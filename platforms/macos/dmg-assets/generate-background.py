#!/usr/bin/env python3
"""Generate DMG background image (660x400 PNG) with drag-to-install arrow."""
import struct
import zlib
import os

W, H = 660, 400

def color_at(x, y):
    """Dark gradient background with subtle texture."""
    t = y / H
    r = int(26 + t * (22 - 26))
    g = int(26 + t * (33 - 26))
    b = int(46 + t * (62 - 46))
    return r, g, b

def draw_arrow(pixels):
    """Draw a dashed arrow from left icon area to right icon area."""
    arrow_y = 200
    x_start, x_end = 240, 410
    head_size = 20

    for x in range(x_start, x_end - head_size):
        seg = (x - x_start) // 18
        if seg % 2 == 0:
            for dy in range(-1, 2):
                py = arrow_y + dy
                if 0 <= py < H:
                    alpha = 0.7
                    bx, by, bb = pixels[py][x]
                    pixels[py][x] = (
                        int(bx + (255 - bx) * alpha),
                        int(by + (255 - by) * alpha),
                        int(bb + (255 - bb) * alpha),
                    )

    cx, cy = x_end - head_size, arrow_y
    for dy in range(-head_size, head_size + 1):
        width = int(head_size * (1 - abs(dy) / head_size))
        for dx in range(width):
            px = cx + head_size - dx
            py = cy + dy
            if 0 <= px < W and 0 <= py < H:
                alpha = 0.8 * (1 - abs(dy) / head_size)
                bx, by, bb = pixels[py][px]
                pixels[py][px] = (
                    int(bx + (255 - bx) * alpha),
                    int(by + (255 - by) * alpha),
                    int(bb + (255 - bb) * alpha),
                )

def draw_text_simple(pixels, text, cx, cy):
    """Draw simple dot-matrix text centered at (cx, cy)."""
    glyphs = {
        'D': ["###.", "#..#", "#..#", "#..#", "###."],
        'R': ["###.", "#..#", "###.", "#.#.", "#..#"],
        'A': [".##.", "#..#", "####", "#..#", "#..#"],
        'G': [".###", "#...", "#.##", "#..#", ".##."],
        'T': ["####", ".##.", ".##.", ".##.", ".##."],
        'O': [".##.", "#..#", "#..#", "#..#", ".##."],
        'I': ["###", ".#.", ".#.", ".#.", "###"],
        'N': ["#..#", "##.#", "#.##", "#..#", "#..#"],
        'S': [".###", "#...", ".##.", "...#", "###."],
        'L': ["#...", "#...", "#...", "#...", "####"],
        ' ': ["...", "...", "...", "...", "..."],
    }

    chars = []
    total_w = 0
    for ch in text:
        g = glyphs.get(ch, glyphs[' '])
        w = len(g[0])
        chars.append((g, w))
        total_w += w + 1

    sx = cx - (total_w * 3) // 2
    for g, gw in chars:
        for row_i, row in enumerate(g):
            for col_i, c in enumerate(row):
                if c == '#':
                    for sy in range(3):
                        for ssx in range(3):
                            px = sx + col_i * 3 + ssx
                            py = cy + row_i * 3 + sy
                            if 0 <= px < W and 0 <= py < H:
                                bx, by, bb = pixels[py][px]
                                alpha = 0.35
                                pixels[py][px] = (
                                    int(bx + (255 - bx) * alpha),
                                    int(by + (255 - by) * alpha),
                                    int(bb + (255 - bb) * alpha),
                                )
        sx += (gw + 1) * 3


def make_png(pixels):
    """Create PNG from pixel array."""
    def chunk(ctype, data):
        c = ctype + data
        return struct.pack('>I', len(data)) + c + struct.pack('>I', zlib.crc32(c) & 0xFFFFFFFF)

    raw = b''
    for row in pixels:
        raw += b'\x00'
        for r, g, b in row:
            raw += struct.pack('BBB', r, g, b)

    ihdr = struct.pack('>IIBBBBB', W, H, 8, 2, 0, 0, 0)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b'')


pixels = [[color_at(x, y) for x in range(W)] for y in range(H)]
draw_arrow(pixels)
draw_text_simple(pixels, "DRAG TO INSTALL", 330, 255)

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "dmg-background.png")
with open(out, 'wb') as f:
    f.write(make_png(pixels))
print(f"Generated {out} ({W}x{H})")
