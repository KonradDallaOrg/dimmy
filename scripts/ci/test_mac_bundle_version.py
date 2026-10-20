#!/usr/bin/env python3
"""Tests for mac_bundle_version.py. Run: python3 scripts/ci/test_mac_bundle_version.py

The Mac updater (Sparkle) compares CFBundleVersion / sparkle:version with
SUStandardVersionComparator, which STOPS reading at the first '-'. With the
tag as the bundle version, 0.7.13-rc.3, 0.7.13-rc.5 and 0.7.13 all read as
"0.7.13": no rc ever updated to the next rc or to its stable (reported
2026-10-16, a tester stuck on rc.3 while rc.5 was out). `sparkle_compare`
below is a line-by-line port of that comparator (Sparkle 2.x,
Sparkle/SUStandardVersionComparator.m) so the ordering is checked against
Sparkle's real rules, not ours.
"""
import os
import string
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mac_bundle_version import bundle_version  # noqa: E402


def _type(c):
    if c == ".":
        return "period"
    if c == "-":
        return "dash"
    if c.isdigit():
        return "number"
    if c.isspace():
        return "whitespace"
    if c in string.punctuation:
        return "punctuation"
    return "string"


def _is_sep(t):
    return t in ("period", "punctuation", "whitespace")


def _split(version):
    parts = []
    if not version:
        return parts
    s = version[0]
    old = _type(s)
    for c in version[1:]:
        new = _type(c)
        if new == "dash":
            break
        if old != new or _is_sep(old):
            parts.append(s)
            s = c
        else:
            s += c
        old = new
    parts.append(s)
    return parts


def _leading_number_and_period(parts):
    n = 0
    for p in parts:
        if _type(p[0]) in ("number", "period"):
            n += 1
        else:
            break
    return n


def _balance(a, b):
    na, nb = _leading_number_and_period(a), _leading_number_and_period(b)
    short, long_, ns, nl = (a, b, na, nb) if na < nb else (b, a, nb, na)
    for i in range(ns, nl):
        short.insert(i, "." if _type(long_[i][0]) == "period" else "0")


def sparkle_compare(a, b):
    """>0 when a is newer than b, 0 when Sparkle sees them as the same."""
    pa, pb = _split(a), _split(b)
    _balance(pa, pb)
    for x, y in zip(pa, pb):
        tx, ty = _type(x[0]), _type(y[0])
        if tx == ty or (_is_sep(tx) and _is_sep(ty)):
            if tx == "number" and int(x) != int(y):
                return 1 if int(x) > int(y) else -1
            if tx == "string" and x != y:
                return 1 if x > y else -1
        elif tx != "string" and ty == "string":
            return 1
        elif tx == "string" and ty != "string":
            return -1
        else:
            return 1 if tx == "number" else -1
    if len(pa) != len(pb):
        n = min(len(pa), len(pb))
        missing = (pa if len(pa) > len(pb) else pb)[n]
        a_longer = len(pa) > len(pb)
        if _type(missing[0]) == "string":
            return -1 if a_longer else 1
        return 1 if a_longer else -1
    return 0


class PortMatchesSparkle(unittest.TestCase):
    def test_the_bug_the_port_must_reproduce(self):
        # The field report, as Sparkle sees it.
        self.assertEqual(sparkle_compare("0.7.13-rc.5", "0.7.13-rc.3"), 0)
        self.assertEqual(sparkle_compare("0.7.13", "0.7.13-rc.5"), 0)

    def test_documented_sparkle_orderings(self):
        self.assertGreater(sparkle_compare("1.0.1", "1.0"), 0)
        self.assertEqual(sparkle_compare("1.0", "1.0.0"), 0)
        self.assertGreater(sparkle_compare("1.0", "1.0b1"), 0)
        self.assertGreater(sparkle_compare("1.10", "1.9"), 0)


class BundleVersion(unittest.TestCase):
    def test_shapes(self):
        self.assertEqual(bundle_version("v0.7.13-rc.6"), "0.7.13.0.6")
        self.assertEqual(bundle_version("v0.7.13"), "0.7.13.1")
        self.assertEqual(bundle_version("v0.6.73-rc11"), "0.6.73.0.11")
        self.assertEqual(bundle_version("0.7.14-rc.1"), "0.7.14.0.1")

    def test_no_dash_ever(self):
        for tag in ("v0.7.13-rc.6", "v0.7.13", "v1.2.3-rc.10"):
            self.assertNotIn("-", bundle_version(tag))

    def test_rejects_what_release_yml_must_not_ship(self):
        for bad in ("", "v0.7", "v0.7.13-staging.4", "v0.7.13-beta.1", "vx.y.z", "v0.7.13-rc.0"):
            with self.assertRaises(ValueError, msg=bad):
                bundle_version(bad)

    def test_ordering_through_a_release_line(self):
        line = ["v0.7.12", "v0.7.13-rc.1", "v0.7.13-rc.2", "v0.7.13-rc.9",
                "v0.7.13-rc.10", "v0.7.13", "v0.7.14-rc.1", "v0.7.14", "v1.0.0-rc.1"]
        built = [bundle_version(t) for t in line]
        for older, newer in zip(built, built[1:]):
            self.assertGreater(sparkle_compare(newer, older), 0, f"{newer} must beat {older}")

    def test_rescues_installs_with_the_old_dashed_bundle_version(self):
        # Every Mac that installed an rc before this fix carries
        # CFBundleVersion "X.Y.Z-rc.N", which Sparkle reads as "X.Y.Z".
        # The next rc and the stable must both look newer to it.
        for installed in ("0.7.13-rc.3", "0.7.13-rc.5"):
            self.assertGreater(sparkle_compare(bundle_version("v0.7.13-rc.6"), installed), 0)
            self.assertGreater(sparkle_compare(bundle_version("v0.7.13"), installed), 0)
        self.assertGreater(sparkle_compare(bundle_version("v0.7.13-rc.6"), "0.7.12"), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
