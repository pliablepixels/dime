#!/usr/bin/env python3
"""Draw DiMe's app icon and build packaging/DiMe.icns. Needs Pillow and macOS iconutil.

The mark is what the app actually shows: an isometric plate with three blocks rising out of it,
in Di's orange, Me's teal and Ru's violet, on the app's own near-black background.
"""
import math, os, shutil, subprocess
from PIL import Image, ImageDraw, ImageFilter

S = 4096                      # drawn big, downsampled at the end, so every edge is smooth
OUT = "packaging"
BG_TOP, BG_BOT = (22, 32, 58), (10, 15, 26)   # matches --bg #0D1321 and its lighter panel tone
ORANGE, TEAL, VIOLET = (255, 122, 61), (79, 209, 197), (201, 179, 255)
PLATE = (38, 52, 86)

def shade(c, f):
    return tuple(max(0, min(255, int(v * f))) for v in c)

def iso(x, y, z, unit, cx, cy):
    """Grid point to screen. x/y run along the plate, z is height."""
    return (cx + (x - y) * unit * math.cos(math.radians(30)),
            cy + (x + y) * unit * math.sin(math.radians(30)) - z * unit)

def quad(d, pts, colour):
    d.polygon(pts, fill=colour)

def block(d, x, y, w, h, z, unit, cx, cy, colour, z0=0.0):
    """An axis-aligned box between heights z0 and z, drawn as its three visible faces."""
    top = [iso(x, y, z, unit, cx, cy), iso(x + w, y, z, unit, cx, cy),
           iso(x + w, y + h, z, unit, cx, cy), iso(x, y + h, z, unit, cx, cy)]
    left = [top[0], top[3], iso(x, y + h, z0, unit, cx, cy), iso(x, y, z0, unit, cx, cy)]
    right = [top[3], top[2], iso(x + w, y + h, z0, unit, cx, cy), iso(x, y + h, z0, unit, cx, cy)]
    quad(d, left, shade(colour, 0.55))
    quad(d, right, shade(colour, 0.75))
    quad(d, top, colour)

def rounded_mask(size, radius):
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return m

def draw():
    img = Image.new("RGB", (S, S), BG_BOT)
    d = ImageDraw.Draw(img)
    for i in range(S):                                    # vertical gradient
        f = i / S
        d.line([(0, i), (S, i)], fill=tuple(int(a + (b - a) * f) for a, b in zip(BG_TOP, BG_BOT)))

    # a soft glow where the blocks sit, so the mark lifts off the background
    glow = Image.new("RGB", (S, S), (0, 0, 0))
    ImageDraw.Draw(glow).ellipse([S * 0.20, S * 0.40, S * 0.80, S * 0.80], fill=shade(ORANGE, 0.55))
    img = Image.blend(img, glow.filter(ImageFilter.GaussianBlur(S // 9)), 0.40)
    d = ImageDraw.Draw(img)

    unit, cx, cy = S * 0.20, S * 0.5, S * 0.575
    R = 1.55
    block(d, -R, -R, 2 * R, 2 * R, 0.0, unit, cx, cy, PLATE, z0=-0.22)  # the map itself, a slab the blocks stand on

    #        x      y     w     h     z     colour
    blocks = [(-1.275, -1.275, 1.15, 1.15, 1.25, ORANGE),   # the big one, at the back so it stays visible
              (0.125, -1.275, 1.15, 1.15, 0.50, VIOLET),
              (-1.275, 0.125, 1.15, 1.15, 0.75, TEAL),
              (0.125, 0.125, 1.15, 1.15, 0.28, shade(PLATE, 1.9))]
    # painter's algorithm: in this projection a bigger x+y is nearer the viewer, so it paints last
    for x, y, w, h, z, colour in sorted(blocks, key=lambda b: b[0] + b[1]):
        block(d, x, y, w, h, z, unit, cx, cy, colour)

    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    icon.paste(img, (0, 0), rounded_mask(S, int(S * 0.225)))   # macOS squircle-ish corner
    return icon

def main():
    art = draw()
    canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))          # macOS insets the art inside the tile
    body = int(S * 0.82)
    canvas.paste(art.resize((body, body), Image.LANCZOS), ((S - body) // 2, (S - body) // 2))

    os.makedirs(OUT, exist_ok=True)
    canvas.resize((1024, 1024), Image.LANCZOS).save(f"{OUT}/icon.png")
    iconset = f"{OUT}/DiMe.iconset"
    shutil.rmtree(iconset, ignore_errors=True)
    os.makedirs(iconset)
    for px in (16, 32, 128, 256, 512):
        canvas.resize((px, px), Image.LANCZOS).save(f"{iconset}/icon_{px}x{px}.png")
        canvas.resize((px * 2, px * 2), Image.LANCZOS).save(f"{iconset}/icon_{px}x{px}@2x.png")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", f"{OUT}/DiMe.icns"], check=True)
    shutil.rmtree(iconset)
    print(f"{OUT}/DiMe.icns")

if __name__ == "__main__":
    main()
