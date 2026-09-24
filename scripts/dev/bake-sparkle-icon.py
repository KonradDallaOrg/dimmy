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

from PIL import Image, ImageDraw, ImageFont

# Matches bake-win-tray-icons.py so the frames line up with the rest of
# the set; 24 px is the one the Win11 taskbar actually picks.
ICO_SIZES = [(16, 16), (20, 20), (24, 24), (32, 32), (48, 48), (64, 64), (256, 256)]

# Supersample, then downscale: a four-pointed star is all diagonals, and
# drawing it straight at 16 px gives stair-stepped spikes.
SS = 16

OUT_DIR = Path(__file__).resolve().parents[2] / "platforms" / "windows" / "Dimmy.Windows" / "Assets"

# The accent Dimmy already uses for "AI did this".
# Sampled from the dot on dimmy-tray-*-processing.ico, not chosen: the
# sparkle replaces that icon for the recap state, so it has to be the same
# purple, exactly. #AF52DE, 11994 pixels of it in the source.
PURPLE = (175, 82, 222, 255)
WHITE = (255, 255, 255, 255)
DARK = (32, 32, 36, 255)


GLYPH_FONT = r"C:\Windows\Fonts\seguiemj.ttf"
SPARKLE = chr(0x2728)


def glyph_master() -> Image.Image:
    """U+2728 as Windows draws it, as a 256-px purple master.

    The shape is taken from the font rather than drawn: an earlier version
    of this script approximated it with two stars, and the real emoji has
    three — the difference is obvious the moment you put them side by side.

    Rendered without embedded_color so we get the monochrome outline, then
    used as an alpha mask and filled with the accent. That keeps our colour
    and the real silhouette at the same time.

    Of the three system fonts that might carry it, only this one is usable:
    Segoe UI Symbol draws a thinner variant with a hollow small star that
    fades at 16 px, and Segoe Fluent Icons has no glyph at all.
    """
    if not Path(GLYPH_FONT).exists():
        raise SystemExit(f"missing {GLYPH_FONT} - cannot bake the sparkle")
    big = 512
    font = ImageFont.truetype(GLYPH_FONT, big)
    mask = Image.new("L", (big * 2, big * 2), 0)
    ImageDraw.Draw(mask).text((big // 2, big // 2), SPARKLE, font=font, fill=255)
    box = mask.getbbox()
    if not box:
        raise SystemExit("the sparkle glyph rendered empty")
    mask = mask.crop(box)
    # Square it so the aspect survives the resize to the icon frames.
    side = max(mask.size)
    square = Image.new("L", (side, side), 0)
    square.paste(mask, ((side - mask.width) // 2, (side - mask.height) // 2))
    square = square.resize((256, 256), Image.LANCZOS)

    out = Image.new("RGBA", (256, 256), (0, 0, 0, 0))
    out.paste(Image.new("RGBA", (256, 256), PURPLE), (0, 0), square)
    return out


def save_ico(img: Image.Image, name: str) -> None:
    path = OUT_DIR / name
    img.save(path, format="ICO", sizes=ICO_SIZES)
    print(f"  {name}  ({path.stat().st_size / 1024:.1f} KB)")


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print(f"writing to {OUT_DIR}")
    master = glyph_master()
    # Purple on BOTH taskbar tones. The rest of the set follows the theme
    # because those icons are the Dimmy silhouette and the theme is all
    # that separates them; here the accent IS the message. Measured, not
    # assumed: 3.80:1 on the dark taskbar, 3.85:1 on the light one.
    save_ico(master, "dimmy-tray-dark-recap.ico")
    save_ico(master, "dimmy-tray-light-recap.ico")
    save_ico(master, "dimmy-sparkle-colour.ico")
    png = OUT_DIR / "dimmy-sparkle-colour.png"
    master.save(png)
    print(f"  {png.name}")


if __name__ == "__main__":
    main()
