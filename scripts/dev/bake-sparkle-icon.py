#!/usr/bin/env python3
"""Bake the AI sparkle mark used for "the model is working on it".

macOS gets this for free as the SF Symbol `sparkles`; Windows has no
system equivalent, so the tray needs a real .ico. This produces one, in
three variants, and wires up nothing: the tray keeps using the standard
`processing` icon until someone decides this one is better.

Drawn rather than traced from the brand kit on purpose. The frozen-icon
rule in CLAUDE.md exists because the brand kit's thin outline renders
vanish at 16 px, and a sparkle is mostly empty space already — so every
shape here is SOLID and sized as a fraction of the canvas, which is what
survives the downscale.

    python scripts/dev/bake-sparkle-icon.py

Writes into platforms/windows/Dimmy.Windows/Assets/.
"""
from __future__ import annotations

import math
from pathlib import Path

from PIL import Image, ImageDraw

# Matches bake-win-tray-icons.py so the frames line up with the rest of
# the set; 24 px is the one the Win11 taskbar actually picks.
ICO_SIZES = [(16, 16), (20, 20), (24, 24), (32, 32), (48, 48), (64, 64), (256, 256)]

# Supersample, then downscale: a four-pointed star is all diagonals, and
# drawing it straight at 16 px gives stair-stepped spikes.
SS = 16

OUT_DIR = Path(__file__).resolve().parents[2] / "platforms" / "windows" / "Dimmy.Windows" / "Assets"

# The accent Dimmy already uses for "AI did this".
PURPLE = (138, 96, 232, 255)
WHITE = (255, 255, 255, 255)
DARK = (32, 32, 36, 255)


def four_point_star(draw: ImageDraw.ImageDraw, cx: float, cy: float, r: float, colour) -> None:
    """A concave four-pointed star: long spikes, pinched waist.

    `waist` is what makes it read as a sparkle rather than a diamond. At
    0.27 the spikes stay legible once the whole thing is 6 px across.
    """
    waist = r * 0.27
    pts = []
    for i in range(8):
        angle = math.pi / 2 * (i / 2.0)
        radius = r if i % 2 == 0 else waist
        pts.append((cx + radius * math.cos(angle), cy - radius * math.sin(angle)))
    draw.polygon(pts, fill=colour)


def render(colour, bg=None) -> Image.Image:
    """One 256-px master: a big star with a small companion, the shape
    every AI mark has converged on."""
    n = 256 * SS
    img = Image.new("RGBA", (n, n), bg or (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Big star slightly low and left, small one up and right — the
    # asymmetry is what stops it reading as a plus sign at 16 px.
    four_point_star(d, n * 0.42, n * 0.56, n * 0.40, colour)
    four_point_star(d, n * 0.76, n * 0.26, n * 0.20, colour)
    return img.resize((256, 256), Image.LANCZOS)


def save_ico(img: Image.Image, name: str) -> None:
    path = OUT_DIR / name
    img.save(path, format="ICO", sizes=ICO_SIZES)
    print(f"  {name}  ({path.stat().st_size / 1024:.1f} KB)")


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print(f"writing to {OUT_DIR}")
    # Coloured, for wherever the accent is wanted.
    save_ico(render(PURPLE), "dimmy-sparkle-colour.ico")
    # Monochrome pair, the shape the tray actually needs: white on a dark
    # taskbar, dark on a light one. Same rule as the rest of the set.
    save_ico(render(WHITE), "dimmy-sparkle-dark.ico")
    save_ico(render(DARK), "dimmy-sparkle-light.ico")
    # A PNG too, for looking at it outside a tray.
    png = OUT_DIR / "dimmy-sparkle-colour.png"
    render(PURPLE).save(png)
    print(f"  {png.name}")


if __name__ == "__main__":
    main()
