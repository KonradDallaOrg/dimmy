#!/usr/bin/env python3
"""The Mac CFBundleVersion (and appcast sparkle:version) for a release tag.

    python3 scripts/ci/mac_bundle_version.py v0.7.13-rc.6   ->  0.7.13.0.6
    python3 scripts/ci/mac_bundle_version.py v0.7.13        ->  0.7.13.1

Sparkle's comparator stops reading a version at the first '-', so the tag
itself ("0.7.13-rc.6") cannot be the bundle version: every rc of a line and
its stable would compare equal and nobody on an rc would ever be updated.
All-numeric instead: rc N is X.Y.Z.0.N, the stable is X.Y.Z.1, so
rc.1 < rc.2 < ... < rc.10 < stable < next line's rc.1, and any install that
still carries the old dashed value (read as X.Y.Z) sees all of them as newer.

Tests: scripts/ci/test_mac_bundle_version.py
"""
import re
import sys

_TAG = re.compile(r"^v?(\d+)\.(\d+)\.(\d+)(?:-rc\.?(\d+))?$")


def bundle_version(tag):
    m = _TAG.match(tag)
    if not m:
        raise ValueError(f"not a release tag (vX.Y.Z or vX.Y.Z-rc.N): {tag!r}")
    major, minor, patch, rc = m.groups()
    base = f"{int(major)}.{int(minor)}.{int(patch)}"
    if rc is None:
        return f"{base}.1"
    if int(rc) < 1:
        raise ValueError(f"rc numbers start at 1: {tag!r}")
    return f"{base}.0.{int(rc)}"


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: mac_bundle_version.py <tag>")
    try:
        print(bundle_version(sys.argv[1]))
    except ValueError as e:
        sys.exit(str(e))
