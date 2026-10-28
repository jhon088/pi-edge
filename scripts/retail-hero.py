#!/usr/bin/env python3
"""Compose the retail attract screen's hero from real product photographs.

The lane's catalog has no fresh aisle, so a dozen of the supplied photographs —
fruit, bakery, drinks — map to no product and would otherwise go unused. They
are the best material in the folder, so they become the attract screen instead:
a grocery still life on a soft sweep, which is what a self-checkout should look
like when nobody is standing at it.

Source material lives in `examples/`, is read **only here**, and never reaches
the runtime: the output is one image committed under `apps/tauri-pos/src`,
bundled by Vite into `dist`, and served from the app itself.

    python3 scripts/retail-hero.py

Pure Python: PNG in, PNG out, and `sips` compresses the result to JPEG.
"""

from __future__ import annotations

import math
import subprocess
import sys
import tempfile
from pathlib import Path

from retail_imaging import as_png, cut_out, erase, read_png, write_png

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "examples" / "fotos ret"
OUT = ROOT / "apps" / "tauri-pos" / "src" / "kiosk" / "hero-retail.jpg"

WIDTH, HEIGHT = 1500, 1500

# Which photographs, how big, and where. Chosen for colour and silhouette:
# produce in front, taller packages behind, nothing centred dead-on.
#
#   (file, width in pixels, centre x, baseline y, depth, erase)
#
# `erase` blanks rectangles of the source, in fractions of its size, for a
# photograph that carries something the hero should not. The bananas come with
# a shelf sign beside them — "Venta por penca" — and a price tag floating in a
# hero is exactly what makes one look like a scraped screenshot. It cannot be
# cropped away without taking fruit with it, so it is erased where it sits.
CAST = [
    ("aceite.avif", 600, 320, 1105, 2, ()),
    ("powerade.avif", 520, 1190, 1085, 2, ()),
    ("bagel.avif", 680, 745, 975, 1, ()),
    ("platano.avif", 760, 455, 1290, 0, ((0.03, 0.05, 0.52, 0.34),)),
    ("manzana red.avif", 480, 1035, 1320, 0, ()),
    ("limon.avif", 330, 752, 1380, 0, ()),
]


def scaled(width, height, channels, pixels, alpha, target):
    """Box-downsample to `target` pixels square, alpha carried along."""
    out_colour = [(255, 255, 255)] * (target * target)
    out_alpha = [0.0] * (target * target)
    step = min(width, height) / target
    for ty in range(target):
        y0 = int(ty * step)
        y1 = max(int(ty * step) + 1, int((ty + 1) * step))
        for tx in range(target):
            x0 = int(tx * step)
            x1 = max(int(tx * step) + 1, int((tx + 1) * step))
            r = g = b = a = 0.0
            count = 0
            for sy in range(y0, min(y1, height)):
                row = sy * width
                for sx in range(x0, min(x1, width)):
                    index = row + sx
                    weight = alpha[index]
                    base = index * channels
                    r += pixels[base] * weight
                    g += pixels[base + 1] * weight
                    b += pixels[base + 2] * weight
                    a += weight
                    count += 1
            if count == 0:
                continue
            at = ty * target + tx
            out_alpha[at] = a / count
            if a > 0.001:
                out_colour[at] = (r / a, g / a, b / a)
    return out_colour, out_alpha


# ------------------------------------------------------------- the canvas


def background():
    """A light sweep with mint in the upper right, where the brand lives."""
    pixels = []
    for y in range(HEIGHT):
        for x in range(WIDTH):
            u, v = x / WIDTH, y / HEIGHT
            # Mint glow, high and right.
            glow = math.exp(-(((u - 0.78) ** 2) / 0.22 + ((v - 0.2) ** 2) / 0.3))
            # Warm floor, so the still life sits on something.
            floor = max(0.0, (v - 0.55) / 0.45) ** 1.6
            r = 252 - 26 * glow - 10 * floor
            g = 253 - 4 * glow - 8 * floor
            b = 251 - 14 * glow - 9 * floor
            pixels.append((r, g, b))
    return pixels


def shadow(pixels, cx, cy, rx, ry, strength):
    x0, x1 = max(0, int(cx - rx)), min(WIDTH, int(cx + rx))
    y0, y1 = max(0, int(cy - ry)), min(HEIGHT, int(cy + ry))
    for y in range(y0, y1):
        for x in range(x0, x1):
            d = math.hypot((x - cx) / rx, (y - cy) / ry)
            if d >= 1.0:
                continue
            fade = strength * (1.0 - d) ** 2.0
            r, g, b = pixels[y * WIDTH + x]
            pixels[y * WIDTH + x] = (
                r + (74 - r) * fade,
                g + (86 - g) * fade,
                b + (82 - b) * fade,
            )


def place(pixels, colour, alpha, size, cx, baseline):
    left = int(cx - size / 2)
    top = int(baseline - size)
    for y in range(size):
        ty = top + y
        if ty < 0 or ty >= HEIGHT:
            continue
        for x in range(size):
            tx = left + x
            if tx < 0 or tx >= WIDTH:
                continue
            a = alpha[y * size + x]
            if a <= 0.004:
                continue
            sr, sg, sb = colour[y * size + x]
            dr, dg, db = pixels[ty * WIDTH + tx]
            pixels[ty * WIDTH + tx] = (
                dr + (sr - dr) * a,
                dg + (sg - dg) * a,
                db + (sb - db) * a,
            )


def main() -> None:
    if not SOURCE.is_dir():
        sys.exit(f"no source photographs at {SOURCE}")
    pixels = background()

    with tempfile.TemporaryDirectory() as workspace:
        work = Path(workspace)
        prepared = []
        for name, size, cx, baseline, depth, unwanted in CAST:
            source = SOURCE / name
            if not source.is_file():
                sys.exit(f"missing {source}")
            width, height, channels, raw = read_png(as_png(source, work))
            alpha = cut_out(width, height, channels, raw)
            erase(alpha, width, height, unwanted)
            colour, cut = scaled(width, height, channels, raw, alpha, size)
            prepared.append((size, cx, baseline, depth, colour, cut))
            print(f"  cut out {name}")

        # Back to front, so the nearer things overlap the further ones.
        prepared.sort(key=lambda entry: -entry[3])
        for size, cx, baseline, depth, colour, cut in prepared:
            spread = 0.52 - 0.06 * depth
            shadow(
                pixels,
                cx,
                baseline - size * 0.02,
                size * spread,
                size * 0.12,
                0.38 - 0.08 * depth,
            )
            place(pixels, colour, cut, size, cx, baseline)

        draft = work / "hero.png"
        write_png(draft, WIDTH, HEIGHT, pixels)
        subprocess.run(
            ["sips", "-s", "format", "jpeg", "-s", "formatOptions", "86",
             str(draft), "--out", str(OUT)],
            check=True,
            capture_output=True,
        )
    print(f"{OUT.relative_to(ROOT)}  {OUT.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
