"""Bake 12 Windows system-tray state ICOs from the brand-kit edge cloud.

Six states × two themes:
  - dark theme  (taskbar is dark)  → WHITE  base cloud + colored dot
  - light theme (taskbar is light) → BLACK  base cloud + colored dot

State colors mirror the macOS StatusBarController palette so the
cross-platform UX is the same: red recording, blue transcribing,
purple processing, green completing, orange paused. `idle` ships
without a dot — it's the "ready / no activity" baseline.

The dot lives in the TOP-RIGHT of the canvas with a small dark halo
so it pops against both light and dark taskbar backgrounds. Sized
in canvas-relative units so all multi-size frames look balanced.

Run from repo root:
    python scripts/dev/bake-win-tray-icons.py
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from PIL import Image, ImageDraw

REPO = Path(__file__).resolve().parents[2]
ASSETS = REPO / "platforms" / "windows" / "Dimmy.Windows" / "Assets"

# Source file: the SOLID white silhouette of the cloud at 1024² (edge-to-
# edge fill, not a thin outline). The current brand-kit's `icon-1024-
# white.png` is an outline-only render — at the 16×16 system-tray size
# the strokes downscale to 1px and visually vanish. The previous brand
# rev shipped a properly filled `icon-1024-edge-white.png` which stays
# legible at small sizes; that is the version Dimmy needs for tray work.
# Override with DIMMY_TRAY_WHITE_SRC=… if a future brand revision
# reintroduces the solid edge variant under a different path.
WHITE_SOURCE = Path(os.environ.get(
    "DIMMY_TRAY_WHITE_SRC",
    str(Path.home() / "Pictures" / "dimmy-brand" / "windows" /
        "icon-1024-edge-white.png"),
))

# Source for the EXE / taskbar-button icon: solid gradient cloud, also
# edge-to-edge. Same rationale as the white source — the latest brand-
# kit's `icon-1024.png` is a thin outline that downscales to invisibility
# at the 24px taskbar size. The previous brand rev's `icon-1024-edge.png`
# (kept in ~/Pictures/dimmy-brand) is the legible solid render.
GRADIENT_SOURCE = Path(os.environ.get(
    "DIMMY_APP_GRADIENT_SRC",
    str(Path.home() / "Pictures" / "dimmy-brand" / "windows" /
        "icon-1024-edge.png"),
))

# (state, hex color or None for no dot)
STATES = [
    ("idle",         None),
    ("recording",    "#FF453A"),  # red
    ("transcribing", "#007AFF"),  # blue
    ("processing",   "#AF52DE"),  # purple
    ("completing",   "#34C759"),  # green
    ("paused",       "#FF9F0A"),  # orange
]

# Sizes Windows wants in a tray-capable ICO. 16/20/24 cover 100/125/150% DPI
# at the small-icon metric; 32/48/64/128/256 satisfy the rest of the shell.
ICO_SIZES = [(16, 16), (20, 20), (24, 24), (32, 32),
             (48, 48), (64, 64), (128, 128), (256, 256)]


def invert_alpha_preserving(rgba: Image.Image) -> Image.Image:
    """Color-invert an RGBA image (white → black) while preserving alpha.

    The brand white cloud is straight-alpha #FFFFFFxx; flipping the RGB
    channels gives a pure black cloud with the same shape and antialiased
    edges. Done in NumPy-free PIL so the script has zero non-PIL deps.
    """
    assert rgba.mode == "RGBA", "expected RGBA source"
    r, g, b, a = rgba.split()
    from PIL import ImageOps
    rgb = Image.merge("RGB", (r, g, b))
    inverted = ImageOps.invert(rgb)
    ri, gi, bi = inverted.split()
    return Image.merge("RGBA", (ri, gi, bi, a))


def parse_hex(h: str) -> tuple[int, int, int]:
    h = h.lstrip("#")
    return int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)


def draw_state_dot(canvas: Image.Image, color_hex: str) -> Image.Image:
    """Overlay a colored dot with a dark halo in the top-right corner.

    Halo = opaque dark ring around the dot; gives contrast on any taskbar
    backdrop (the dot itself would blend on similarly-coloured shells).
    """
    H = canvas.size[0]
    out = canvas.convert("RGBA").copy()
    draw = ImageDraw.Draw(out)

    # Smaller dot (16% of canvas) so the cloud silhouette stays the
    # primary affordance. 20% had the dot eating ~⅓ of the top half at
    # tray size; the user's complaint was the cloud "looks smaller" —
    # shrinking the occluder gives the cloud back its visible footprint.
    r = int(H * 0.16)
    inset = int(H * 0.02)
    cx = H - r - inset
    cy = r + inset                        # TOP-right per user preference

    # Halo: opaque dark ring slightly larger than the dot. Two-step draw
    # so the antialiasing of the halo extends a touch past the dot.
    halo_r = r + max(2, int(H * 0.025))
    draw.ellipse(
        [(cx - halo_r, cy - halo_r), (cx + halo_r, cy + halo_r)],
        fill=(20, 20, 20, 255),
    )

    cr, cg, cb = parse_hex(color_hex)
    draw.ellipse(
        [(cx - r, cy - r), (cx + r, cy + r)],
        fill=(cr, cg, cb, 255),
    )

    return out


def bake_one(base: Image.Image, color_hex: str | None, out_path: Path) -> None:
    """Produce a multi-size ICO from `base` (+ optional state dot)."""
    composed = draw_state_dot(base, color_hex) if color_hex else base
    # PIL expects the largest source frame, then downscales for each ICO size.
    composed.save(
        out_path, format="ICO", sizes=ICO_SIZES,
    )


def main() -> int:
    if not WHITE_SOURCE.exists():
        print(f"FATAL: missing source {WHITE_SOURCE}", file=sys.stderr)
        return 2
    if not ASSETS.exists():
        print(f"FATAL: missing assets dir {ASSETS}", file=sys.stderr)
        return 2

    white = Image.open(WHITE_SOURCE).convert("RGBA")
    black = invert_alpha_preserving(white)

    # Dark theme (taskbar dark) → white base. Light theme (taskbar light)
    # → black base. File names match what TrayService.UpdateState picks
    # based on the resolved system taskbar theme.
    bake_set = [
        ("dark",  white),
        ("light", black),
    ]

    written = []
    for theme, base in bake_set:
        for state, color in STATES:
            out = ASSETS / f"dimmy-tray-{theme}-{state}.ico"
            bake_one(base, color, out)
            written.append(out.name)

    # Also refresh the no-suffix idle ICO as a back-compat alias for any
    # caller that hasn't been updated yet (the old TrayService default).
    bake_one(white, None, ASSETS / "dimmy-tray-idle.ico")

    # And the EXE / taskbar-button icon — same dot-free silhouette but
    # in full gradient. PIL's multi-size ICO save renders one frame per
    # entry of ICO_SIZES so the taskbar gets a 24px native frame instead
    # of a bilinear downscale from 256.
    if GRADIENT_SOURCE.exists():
        gradient = Image.open(GRADIENT_SOURCE).convert("RGBA")
        bake_one(gradient, None, ASSETS / "dimmy.ico")
        print(f"  dimmy.ico (gradient edge-to-edge)")
    else:
        print(f"  WARN: no gradient source at {GRADIENT_SOURCE} — left dimmy.ico untouched")

    print("Baked:")
    for n in written:
        print(f"  {n}")
    print(f"  dimmy-tray-idle.ico (back-compat alias)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
