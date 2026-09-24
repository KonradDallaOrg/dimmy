#!/usr/bin/env python3
"""Bake the AI sparkle used for "the model is writing the recap".

macOS gets this free as the SF Symbol `sparkles`; Windows has no system
equivalent, so the tray needs a real .ico.

## Where the shape comes from, and why it changed twice

First it was drawn here, with two stars. The emoji and the SF Symbol both
have three, and side by side that is obvious.

Then it was the real U+2728, rasterised from Segoe UI Emoji. Right shape,
wrong licence: rendering a glyph from an installed font is what fonts are
for, but shipping the raster inside a product distributes Microsoft's
artwork, which the Windows font licence does not clearly permit.

Now it is Material Symbols `auto_awesome` — the same three-star mark
under Apache License 2.0, free to ship. The path happens to be straight
segments only, so it renders with plain polygons and this script needs no
SVG rasteriser.

    Material Symbols, Copyright 2022 Google LLC
    Licensed under the Apache License, Version 2.0
    https://github.com/google/material-design-icons

    python scripts/dev/bake-sparkle-icon.py

Writes into platforms/windows/Dimmy.Windows/Assets/.
"""
from __future__ import annotations

import re
from pathlib import Path

from PIL import Image, ImageDraw

# Matches bake-win-tray-icons.py so the frames line up with the rest of
# the set; 24 px is the one the Win11 taskbar actually picks.
ICO_SIZES = [(16, 16), (20, 20), (24, 24), (32, 32), (48, 48), (64, 64), (256, 256)]

# Sampled from the dot on dimmy-tray-*-processing.ico, not chosen: the
# sparkle stands in for that icon while a recap runs, so it has to be the
# same purple exactly. #AF52DE, 11994 pixels of it in the source.
PURPLE = (175, 82, 222, 255)

# Lifts the partially-transparent edge pixels, applied to each frame AFTER
# it is downscaled — on the master it achieves nothing, because every soft
# edge is created by the resize that follows.
#
# A sparkle is geometrically almost all edge, so it composites paler than
# the solid dot it replaces even at an identical colour: its purple pixels
# averaged alpha 130/255 against the dot's 245, and it read as a duller
# purple in the tray. This brings it to roughly 207. Full parity is not
# reachable — it would mean hard-aliased spikes.
ALPHA_GAMMA = 0.62

# Material Symbols `auto_awesome`, viewBox 0 0 24 24. Apache-2.0.
# Straight segments only (m/l/L/Z), hence the small parser below.
SPARKLE_PATH = (
    "m19 9l-1.25-2.75L15 5l2.75-1.25L19 1l1.25 2.75L23 5l-2.75 1.25L19 9Z"
    "m0 14l-1.25-2.75L15 19l2.75-1.25L19 15l1.25 2.75L23 19l-2.75 1.25L19 23Z"
    "M9 20l-2.5-5.5L1 12l5.5-2.5L9 4l2.5 5.5L17 12l-5.5 2.5L9 20Z"
)
VIEWBOX = 24.0

OUT_DIR = (
    Path(__file__).resolve().parents[2]
    / "platforms" / "windows" / "Dimmy.Windows" / "Assets"
)

_NUM = re.compile(r"[-+]?[0-9]*\.?[0-9]+")
_CMD = re.compile(r"([MmLlZz])")


def subpaths(d: str) -> list[list[tuple[float, float]]]:
    """Closed polygons, in viewBox units.

    Only the commands this path uses are handled, deliberately: a general
    SVG parser here would be more code than the icon it draws.
    """
    out: list[list[tuple[float, float]]] = []
    pts: list[tuple[float, float]] = []
    cur = (0.0, 0.0)
    cmd = "M"
    for token in (t for t in _CMD.split(d) if t.strip()):
        if token in "Zz":
            if pts:
                out.append(pts)
                pts = []
            continue
        if token in "MmLl":
            cmd = token
            continue
        nums = [float(n) for n in _NUM.findall(token)]
        for i in range(0, len(nums) - 1, 2):
            x, y = nums[i], nums[i + 1]
            if cmd in "ml":
                x, y = cur[0] + x, cur[1] + y
            cur = (x, y)
            pts.append(cur)
            # After its first pair, an m continues as l (and M as L).
            cmd = "l" if cmd == "m" else ("L" if cmd == "M" else cmd)
    if pts:
        out.append(pts)
    assert len(out) == 3, f"expected three stars, parsed {len(out)}"
    return out


def frame(px: int, supersample: int = 8) -> Image.Image:
    """One icon frame: drawn large, downscaled, then alpha-lifted."""
    n = px * supersample
    mask = Image.new("L", (n, n), 0)
    draw = ImageDraw.Draw(mask)
    k = n / VIEWBOX
    for poly in subpaths(SPARKLE_PATH):
        draw.polygon([(x * k, y * k) for x, y in poly], fill=255)
    mask = mask.resize((px, px), Image.LANCZOS)
    mask = mask.point(lambda a: int(255 * (a / 255) ** ALPHA_GAMMA))
    out = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    out.paste(Image.new("RGBA", (px, px), PURPLE), (0, 0), mask)
    return out


def save_ico(name: str) -> None:
    # LARGEST first: Pillow drops any requested size bigger than the base
    # image, so handing it the 16 px frame first silently produced a
    # single-frame icon. Caught by reading the file back, not by the save
    # succeeding — which it did.
    order = sorted({s[0] for s in ICO_SIZES}, reverse=True)
    frames = [frame(px) for px in order]
    path = OUT_DIR / name
    # Every frame supplied explicitly, so none is re-derived by the
    # encoder and none loses the alpha correction.
    frames[0].save(path, format="ICO", sizes=ICO_SIZES, append_images=frames[1:])
    back = Image.open(path)
    assert len(back.ico.sizes()) == len(order), (
        f"{name} came out with {len(back.ico.sizes())} frames, expected {len(order)}"
    )
    print(f"  {name}  ({path.stat().st_size / 1024:.1f} KB, {len(order)} frames)")


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print(f"writing to {OUT_DIR}")
    # Purple on BOTH taskbar tones. The rest of the set follows the theme
    # because those icons are the Dimmy silhouette and the theme is all
    # that separates them; here the accent IS the message. Measured, not
    # assumed: 3.93:1 on the dark taskbar, 3.72:1 on the light one.
    save_ico("dimmy-tray-dark-recap.ico")
    save_ico("dimmy-tray-light-recap.ico")
    save_ico("dimmy-sparkle-colour.ico")
    png = OUT_DIR / "dimmy-sparkle-colour.png"
    frame(256).save(png)
    print(f"  {png.name}")


if __name__ == "__main__":
    main()
