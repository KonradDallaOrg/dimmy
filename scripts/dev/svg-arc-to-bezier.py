#!/usr/bin/env python3
"""Convert the Google-supplied Gemini SVG path (which uses elliptical arc
commands that macOS Asset Catalog's SVG renderer silently drops) to an
equivalent path that uses only line + cubic-Bezier commands.

Standard SVG arc-to-bezier conversion per the W3C implementation note
(https://www.w3.org/TR/SVG11/implnote.html#ArcImplementationNotes).

Output is the new `d=` attribute body."""

import math
from typing import List, Tuple


def arc_to_beziers(x1, y1, x2, y2, rx, ry, phi_deg, large_arc, sweep):
    """Return a list of (cp1x, cp1y, cp2x, cp2y, ex, ey) cubic-bezier
    segments approximating an SVG elliptical arc from (x1,y1) to
    (x2,y2). Handles arcs > 90° by subdivision."""
    if (x1, y1) == (x2, y2):
        return []
    phi = math.radians(phi_deg)
    cos_phi, sin_phi = math.cos(phi), math.sin(phi)

    # Step 1: primed coordinates
    dx = (x1 - x2) / 2.0
    dy = (y1 - y2) / 2.0
    x1p = cos_phi * dx + sin_phi * dy
    y1p = -sin_phi * dx + cos_phi * dy

    # Step 2: ensure radii are large enough
    rx, ry = abs(rx), abs(ry)
    lam = (x1p ** 2) / (rx ** 2) + (y1p ** 2) / (ry ** 2)
    if lam > 1:
        s = math.sqrt(lam)
        rx *= s
        ry *= s

    rx2, ry2 = rx ** 2, ry ** 2
    x1p2, y1p2 = x1p ** 2, y1p ** 2

    # Step 3: compute center'
    radicand = (rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2) / (rx2 * y1p2 + ry2 * x1p2)
    if radicand < 0:
        radicand = 0
    coef = math.sqrt(radicand)
    if large_arc == sweep:
        coef = -coef
    cxp = coef * (rx * y1p) / ry
    cyp = -coef * (ry * x1p) / rx

    # Step 4: real center
    cx = cos_phi * cxp - sin_phi * cyp + (x1 + x2) / 2
    cy = sin_phi * cxp + cos_phi * cyp + (y1 + y2) / 2

    # Step 5: angles
    def ang(ux, uy, vx, vy):
        d = math.sqrt((ux*ux + uy*uy) * (vx*vx + vy*vy))
        c = (ux * vx + uy * vy) / d
        c = max(-1.0, min(1.0, c))
        a = math.acos(c)
        if ux * vy - uy * vx < 0:
            a = -a
        return a

    theta1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    delta = ang((x1p - cxp) / rx, (y1p - cyp) / ry,
                (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if sweep == 0 and delta > 0:
        delta -= 2 * math.pi
    elif sweep == 1 and delta < 0:
        delta += 2 * math.pi

    # Step 6: split into ≤90° subsegments and bezier-approximate each
    n = max(1, math.ceil(abs(delta) / (math.pi / 2)))
    sub = delta / n
    beziers = []

    def to_world(px, py):
        return (
            cos_phi * (rx * px) - sin_phi * (ry * py) + cx,
            sin_phi * (rx * px) + cos_phi * (ry * py) + cy,
        )

    for i in range(n):
        a1 = theta1 + i * sub
        a2 = theta1 + (i + 1) * sub
        c1x = math.cos(a1); c1y = math.sin(a1)
        c2x = math.cos(a2); c2y = math.sin(a2)
        t1x = -math.sin(a1); t1y = math.cos(a1)
        t2x = -math.sin(a2); t2y = math.cos(a2)
        k = 4.0 / 3.0 * math.tan(sub / 4.0)
        cp1x = c1x + k * t1x; cp1y = c1y + k * t1y
        cp2x = c2x - k * t2x; cp2y = c2y - k * t2y
        cp1 = to_world(cp1x, cp1y)
        cp2 = to_world(cp2x, cp2y)
        end = to_world(c2x, c2y)
        beziers.append((cp1[0], cp1[1], cp2[0], cp2[1], end[0], end[1]))
    return beziers


# Manually decoded segments from the Google Gemini SVG path:
#   M20.616 10.835
#   a14.147 14.147 0 01-4.45-3.001
#   14.111 14.111 0 01-3.678-6.452
#   .503.503 0 00-.975 0
#   14.134 14.134 0 01-3.679 6.452
#   14.155 14.155 0 01-4.45 3.001
#   c-.65.28-1.318.505-2.002.678
#   .502.502 0 000 .975
#   c.684.172 1.35.397 2.002.677
#   14.147 14.147 0 014.45 3.001
#   14.112 14.112 0 013.679 6.453
#   .502.502 0 00.975 0
#   c.172-.685.397-1.351.677-2.003
#   14.145 14.145 0 013.001-4.45
#   14.113 14.113 0 016.453-3.678
#   .503.503 0 000-.975
#   13.245 13.245 0 01-2.003-.678 z
#
# Each "a"/implicit arc is (rx, ry, phi, large, sweep, dx, dy). Each "c"
# is a relative cubic-bezier (dx1, dy1, dx2, dy2, dx, dy). Tracking the
# pen position as we go.

start = (20.616, 10.835)
pen = list(start)

# (kind, params). kind in {"A", "C"}.
# For A: (rx, ry, phi, large, sweep, dx, dy)
# For C: (dx1, dy1, dx2, dy2, dx, dy)
segs = [
    ("A", (14.147, 14.147, 0, 0, 1, -4.45, -3.001)),
    ("A", (14.111, 14.111, 0, 0, 1, -3.678, -6.452)),
    ("A", (0.503,  0.503,  0, 0, 0, -0.975, 0)),
    ("A", (14.134, 14.134, 0, 0, 1, -3.679, 6.452)),
    ("A", (14.155, 14.155, 0, 0, 1, -4.45,  3.001)),
    ("C", (-0.65, 0.28, -1.318, 0.505, -2.002, 0.678)),
    ("A", (0.502, 0.502, 0, 0, 0, 0, 0.975)),
    ("C", (0.684, 0.172, 1.35, 0.397, 2.002, 0.677)),
    ("A", (14.147, 14.147, 0, 0, 1, 4.45, 3.001)),
    ("A", (14.112, 14.112, 0, 0, 1, 3.679, 6.453)),
    ("A", (0.502, 0.502, 0, 0, 0, 0.975, 0)),
    ("C", (0.172, -0.685, 0.397, -1.351, 0.677, -2.003)),
    ("A", (14.145, 14.145, 0, 0, 1, 3.001, -4.45)),
    ("A", (14.113, 14.113, 0, 0, 1, 6.453, -3.678)),
    ("A", (0.503, 0.503, 0, 0, 0, 0, -0.975)),
    ("A", (13.245, 13.245, 0, 0, 1, -2.003, -0.678)),
]


def fmt(x): return f"{x:.4f}".rstrip("0").rstrip(".") if "." in f"{x:.4f}" else f"{x:.4f}"


parts = [f"M{fmt(start[0])} {fmt(start[1])}"]
for kind, p in segs:
    if kind == "C":
        dx1, dy1, dx2, dy2, dx, dy = p
        cp1 = (pen[0] + dx1, pen[1] + dy1)
        cp2 = (pen[0] + dx2, pen[1] + dy2)
        end = (pen[0] + dx,  pen[1] + dy)
        parts.append(
            f"C{fmt(cp1[0])} {fmt(cp1[1])} {fmt(cp2[0])} {fmt(cp2[1])} {fmt(end[0])} {fmt(end[1])}"
        )
        pen = list(end)
    else:
        rx, ry, phi, large, sweep, dx, dy = p
        end = (pen[0] + dx, pen[1] + dy)
        bz = arc_to_beziers(pen[0], pen[1], end[0], end[1],
                            rx, ry, phi, large, sweep)
        for (cp1x, cp1y, cp2x, cp2y, ex, ey) in bz:
            parts.append(
                f"C{fmt(cp1x)} {fmt(cp1y)} {fmt(cp2x)} {fmt(cp2y)} {fmt(ex)} {fmt(ey)}"
            )
        pen = list(end)
parts.append("Z")
print(" ".join(parts))
