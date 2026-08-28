#!/usr/bin/env python3
"""Tests for build_appcast.py. Run: python3 scripts/ci/test_build_appcast.py

Covers the two bugs that shipped from the old inline heredoc, which
could only be observed by cutting a real release:
  - an rc landing in the stable channel;
  - the stable channel going empty when the newest build is an rc.
"""

import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from build_appcast import (  # noqa: E402
    PRERELEASE_TAG,
    build_feed,
    carry_over_items,
    item_is_prerelease,
)


class Args:
    """Stand-in for the argparse namespace."""

    def __init__(self, version, build_version, channel):
        self.version = version
        self.build_version = build_version
        self.channel = channel
        self.dmg_url = f"https://example.invalid/v{build_version}/Dimmy.dmg"
        self.notes_url = f"https://example.invalid/tag/v{build_version}"
        self.pub_date = "Thu, 27 Aug 2026 15:53:35 +0000"
        self.min_os = "12.0"
        self.signature = 'sparkle:edSignature="abc==" length="47440784"'


def feed_for(version, build_version, channel, previous=""):
    return build_feed(Args(version, build_version, channel), previous)


STABLE_FEED = feed_for("0.6.72", "0.6.72", "stable")
RC_FEED = feed_for("0.6.73", "0.6.73-rc2", "prerelease", previous=STABLE_FEED)


class ChannelTagging(unittest.TestCase):
    def test_stable_build_carries_no_channel_tag(self):
        # Sparkle shows an untagged item to every client, which is what
        # "stable" means. A tag here would hide the release from everyone.
        self.assertNotIn(PRERELEASE_TAG, STABLE_FEED)

    def test_prerelease_build_is_tagged(self):
        # The bug: this tag was never emitted, so every rc reached
        # users who had explicitly chosen stable.
        self.assertIn(PRERELEASE_TAG, RC_FEED)

    def test_item_channel_detection(self):
        self.assertTrue(item_is_prerelease(f"<item>{PRERELEASE_TAG}</item>"))
        self.assertFalse(item_is_prerelease("<item>plain</item>"))


class ChannelPreservation(unittest.TestCase):
    def test_publishing_an_rc_keeps_the_stable_item(self):
        # Fixing the channel tag alone would strand stable users here.
        self.assertEqual(len(re.findall(r"<item>", RC_FEED)), 2)
        self.assertIn("0.6.72", RC_FEED)
        self.assertIn("0.6.73-rc2", RC_FEED)

    def test_publishing_a_stable_keeps_the_rc_item(self):
        feed = feed_for("0.6.73", "0.6.73", "stable", previous=RC_FEED)
        self.assertIn("0.6.73-rc2", feed)
        self.assertIn(PRERELEASE_TAG, feed)

    def test_same_channel_item_is_superseded_not_accumulated(self):
        # Two rcs in a row must not leave both in the feed.
        rc3 = feed_for("0.6.73", "0.6.73-rc3", "prerelease", previous=RC_FEED)
        self.assertNotIn("0.6.73-rc2", rc3)
        self.assertIn("0.6.73-rc3", rc3)
        self.assertIn("0.6.72", rc3)  # the stable item still survives
        self.assertEqual(len(re.findall(r"<item>", rc3)), 2)

    def test_feed_never_grows_past_two_items(self):
        feed = STABLE_FEED
        for n in range(1, 6):
            feed = feed_for("0.6.73", f"0.6.73-rc{n}", "prerelease", previous=feed)
            feed = feed_for("0.6.7%d" % (2 + n), "0.6.7%d" % (2 + n), "stable",
                            previous=feed)
            self.assertLessEqual(len(re.findall(r"<item>", feed)), 2)

    def test_missing_previous_feed_still_produces_a_valid_single_item(self):
        feed = feed_for("0.6.73", "0.6.73-rc1", "prerelease", previous="")
        self.assertEqual(len(re.findall(r"<item>", feed)), 1)

    def test_carry_over_ignores_unparseable_leftovers(self):
        self.assertEqual(carry_over_items("not xml at all", True), [])


class VersionFields(unittest.TestCase):
    def test_sparkle_version_is_unique_per_build(self):
        # CFBundleVersion used to come from Cargo.toml, so rc2 and the
        # stable that followed both said 0.6.73 and Sparkle never
        # offered the stable to anyone sitting on the rc.
        rc = feed_for("0.6.73", "0.6.73-rc2", "prerelease")
        self.assertIn("<sparkle:version>0.6.73-rc2</sparkle:version>", rc)
        self.assertIn("<sparkle:shortVersionString>0.6.73</sparkle:shortVersionString>", rc)

    def test_stable_build_version_matches_marketing_version(self):
        self.assertIn("<sparkle:version>0.6.72</sparkle:version>", STABLE_FEED)


class Wellformedness(unittest.TestCase):
    def test_generated_feeds_parse(self):
        # build_feed() parses its own output and raises otherwise; these
        # assertions document that the guard is load-bearing.
        import xml.dom.minidom
        for feed in (STABLE_FEED, RC_FEED):
            xml.dom.minidom.parseString(feed)

    def test_enclosure_carries_the_signature(self):
        self.assertIn('sparkle:edSignature="abc=="', RC_FEED)


if __name__ == "__main__":
    unittest.main(verbosity=2)
