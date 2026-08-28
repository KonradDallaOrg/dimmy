#!/usr/bin/env python3
"""Generate the Sparkle appcast for a macOS release.

Called by .github/workflows/release.yml. Lives here rather than inline
in the workflow because two separate bugs shipped from that inline
heredoc, and neither was reproducible without cutting a release:

  1. The channel tag was derived from `github.event.release.prerelease`,
     which is empty under an `on: push: tags` trigger. Every release
     candidate was published into the STABLE channel, so Mac users who
     had chosen "stable" were auto-updated to rcs.

  2. The feed carried a single <item> and was clobber-replaced on every
     release. Fixing (1) alone would then EMPTY the stable channel
     whenever the newest build was an rc, stranding stable users on
     "up to date" even when they were behind the last stable release.

So the feed always carries the newest build plus the newest surviving
build of the other channel. Sparkle picks per the client's
`allowedChannels` (see platforms/macos/Dimmy/Services/UpdateService.swift).

Tests: scripts/ci/test_build_appcast.py
"""

from __future__ import annotations

import argparse
import re
import sys
import xml.dom.minidom

PRERELEASE_TAG = "<sparkle:channel>prerelease</sparkle:channel>"

ITEM_TEMPLATE = """    <item>
      <title>Dimmy {build_version}</title>
      <link>{notes_url}</link>
      <sparkle:version>{build_version}</sparkle:version>
      <sparkle:shortVersionString>{version}</sparkle:shortVersionString>
{channel_line}      <pubDate>{pub_date}</pubDate>
      <sparkle:minimumSystemVersion>{min_os}</sparkle:minimumSystemVersion>
      <enclosure
        url="{dmg_url}"
        type="application/octet-stream"
        {signature} />
    </item>"""

FEED_TEMPLATE = """<?xml version="1.0" standalone="yes"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Dimmy</title>
    <link>{dmg_url}</link>
    <description>Dimmy macOS release feed</description>
    <language>en</language>
{items}
  </channel>
</rss>
"""


def item_is_prerelease(item: str) -> bool:
    """True when the item carries the prerelease channel tag.

    Sparkle treats an item with no <sparkle:channel> as belonging to
    the default (stable) channel, which every client sees.
    """
    return PRERELEASE_TAG in item


def carry_over_items(previous_feed: str, publishing_prerelease: bool) -> list[str]:
    """Items to keep from the live feed.

    Only the other channel's newest item survives: the one for the
    channel we are publishing is superseded by this build, and the feed
    is a two-entry pointer, not an archive.
    """
    kept = [
        item
        for item in re.findall(r"[ \t]*<item>.*?</item>", previous_feed, re.S)
        if item_is_prerelease(item) != publishing_prerelease
    ]
    return kept[:1]


def build_item(args: argparse.Namespace, publishing_prerelease: bool) -> str:
    channel_line = f"      {PRERELEASE_TAG}\n" if publishing_prerelease else ""
    return ITEM_TEMPLATE.format(
        build_version=args.build_version,
        version=args.version,
        notes_url=args.notes_url,
        dmg_url=args.dmg_url,
        pub_date=args.pub_date,
        min_os=args.min_os,
        signature=args.signature,
        channel_line=channel_line,
    )


def build_feed(args: argparse.Namespace, previous_feed: str) -> str:
    publishing_prerelease = args.channel == "prerelease"
    items = [build_item(args, publishing_prerelease)]
    items += carry_over_items(previous_feed, publishing_prerelease)
    feed = FEED_TEMPLATE.format(dmg_url=args.dmg_url, items="\n".join(items))
    # A malformed feed breaks auto-update for every Mac user until the
    # next release, and it is published by --clobber. Refuse to emit one.
    xml.dom.minidom.parseString(feed)
    return feed


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--version", required=True,
                   help="Marketing version (CFBundleShortVersionString), e.g. 0.6.73")
    p.add_argument("--build-version", required=True,
                   help="Unique per-build version (CFBundleVersion), e.g. 0.6.73-rc2")
    p.add_argument("--channel", required=True, choices=["stable", "prerelease"])
    p.add_argument("--dmg-url", required=True)
    p.add_argument("--notes-url", required=True)
    p.add_argument("--pub-date", required=True)
    p.add_argument("--min-os", required=True)
    p.add_argument("--signature", required=True,
                   help='sign_update output, e.g. sparkle:edSignature="..." length="123"')
    p.add_argument("--previous", default="",
                   help="Path to the currently published appcast. Missing or "
                        "unreadable means a single-item feed.")
    args = p.parse_args(argv)

    previous_feed = ""
    if args.previous:
        try:
            with open(args.previous, encoding="utf-8") as fh:
                previous_feed = fh.read()
        except OSError as exc:
            print(f"::warning::could not read previous appcast: {exc}", file=sys.stderr)

    feed = build_feed(args, previous_feed)

    other = "stable" if args.channel == "prerelease" else "prerelease"
    if len(re.findall(r"<item>", feed)) < 2:
        print(f"::warning::no item preserved for the {other} channel - that "
              f"channel will see no update until its next release", file=sys.stderr)

    sys.stdout.write(feed)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
