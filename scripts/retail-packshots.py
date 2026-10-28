#!/usr/bin/env python3
"""Render the retail lane's placeholder packshots.

Every one of these is a stand-in for a photograph that has not been supplied
yet. The dataset references its images by *stem*, so replacing one is a copy
and a re-seed — see `demo/retail/README.md`. Nothing here is a brand: the
shapes are generic packaging, there is no wordmark, and the product's name is
drawn by the kiosk under the card rather than printed on the artwork.

Pure Python on purpose: no Pillow, no ImageMagick, nothing to install. Shapes
are signed distance fields, which gives real anti-aliasing and soft shadows
for the cost of a subtraction, and each one only touches the pixels inside its
own bounding box.

    python3 scripts/retail-packshots.py            # writes demo/retail/assets
"""

from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

SIZE = 1000
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "demo" / "retail" / "assets"


# --------------------------------------------------------------- the canvas


class Canvas:
    """A float RGB image, composited front to back by coverage."""

    def __init__(self, size: int):
        self.size = size
        self.pixels = [[1.0, 1.0, 1.0] for _ in range(size * size)]

    def background(self) -> None:
        """A studio sweep: barely grey at the top, white at the base."""
        for y in range(self.size):
            t = y / (self.size - 1)
            # Brightest just below centre, where a product's own shadow sits.
            shade = 0.957 + 0.043 * min(1.0, t * 1.6)
            row = y * self.size
            for x in range(self.size):
                self.pixels[row + x] = [shade, shade, shade]

    def paint(self, bbox, coverage, colour) -> None:
        """Composite one shape.

        `coverage(x, y)` answers 0..1, `colour(x, y)` answers an RGB triple.
        Only the pixels inside `bbox` are visited.
        """
        x0, y0, x1, y1 = bbox
        x0 = max(0, int(x0))
        y0 = max(0, int(y0))
        x1 = min(self.size, int(x1) + 1)
        y1 = min(self.size, int(y1) + 1)
        for y in range(y0, y1):
            row = y * self.size
            for x in range(x0, x1):
                alpha = coverage(x + 0.5, y + 0.5)
                if alpha <= 0.0:
                    continue
                if alpha > 1.0:
                    alpha = 1.0
                source = colour(x + 0.5, y + 0.5)
                under = self.pixels[row + x]
                self.pixels[row + x] = [
                    under[channel] + (source[channel] - under[channel]) * alpha
                    for channel in range(3)
                ]

    def png(self, path: Path) -> None:
        raw = bytearray()
        for y in range(self.size):
            raw.append(0)
            row = y * self.size
            for x in range(self.size):
                for channel in self.pixels[row + x]:
                    value = int(round(max(0.0, min(1.0, channel)) * 255.0))
                    raw.append(value)

        def chunk(kind: bytes, payload: bytes) -> bytes:
            return (
                struct.pack(">I", len(payload))
                + kind
                + payload
                + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
            )

        header = struct.pack(">IIBBBBB", self.size, self.size, 8, 2, 0, 0, 0)
        path.write_bytes(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b"")
        )


# ------------------------------------------------------------- distance fields


def rounded_box(cx, cy, half_w, half_h, radius):
    """Signed distance to a rounded rectangle: negative inside."""
    radius = min(radius, half_w, half_h)
    inner_w = half_w - radius
    inner_h = half_h - radius

    def distance(x, y):
        dx = abs(x - cx) - inner_w
        dy = abs(y - cy) - inner_h
        ax = max(dx, 0.0)
        ay = max(dy, 0.0)
        outside = math.hypot(ax, ay)
        inside = min(max(dx, dy), 0.0)
        return outside + inside - radius

    return distance


def ellipse(cx, cy, rx, ry):
    def distance(x, y):
        # Close enough to a true distance for shading, and exact at the edge.
        nx = (x - cx) / rx
        ny = (y - cy) / ry
        value = math.hypot(nx, ny)
        return (value - 1.0) * min(rx, ry)

    return distance


def taper(top_half, bottom_half, cy, half_h):
    """A body whose width changes with height, for cones and bags."""

    def half_width(y):
        t = (y - (cy - half_h)) / (2 * half_h)
        t = max(0.0, min(1.0, t))
        return top_half + (bottom_half - top_half) * t

    return half_width


def union(*fields):
    def distance(x, y):
        return min(field(x, y) for field in fields)

    return distance


def coverage_of(field, softness=0.75):
    """Anti-aliasing: one pixel of ramp across the edge."""

    def coverage(x, y):
        return max(0.0, min(1.0, 0.5 - field(x, y) / (2 * softness)))

    return coverage


# ------------------------------------------------------------------- shading


def shade(base, *, cx, half_w, top, bottom, gloss=0.55, wrap=True):
    """Cylindrical shading with a specular stripe, or a flat panel."""
    red, green, blue = base

    def colour(x, y):
        if wrap:
            # Across the body: dark at the edges, bright where the light is.
            u = (x - cx) / half_w
            u = max(-1.0, min(1.0, u))
            curve = math.cos(u * 1.25)
            lit = 0.66 + 0.34 * max(0.0, curve)
            # One broad softbox left of centre, and a thin rim light on the
            # right — which is what stops a shape reading as a flat decal.
            highlight = math.exp(-(((u + 0.38) / 0.46) ** 2)) * gloss * 0.42
            highlight += math.exp(-(((u - 0.88) / 0.1) ** 2)) * gloss * 0.3
        else:
            lit = 1.0
            highlight = 0.0
        # Along the body: a touch darker at the very top and bottom.
        v = (y - top) / max(1.0, bottom - top)
        v = max(0.0, min(1.0, v))
        lit *= 0.93 + 0.1 * math.sin(math.pi * v)
        return [
            min(1.0, red * lit + highlight),
            min(1.0, green * lit + highlight),
            min(1.0, blue * lit + highlight),
        ]

    return colour


def flat(colour):
    def shader(_x, _y):
        return list(colour)

    return shader


def contact_shadow(canvas, cx, cy, rx, ry, strength=0.3):
    """The soft ellipse a product leaves on a white sweep."""

    def coverage(x, y):
        nx = (x - cx) / rx
        ny = (y - cy) / ry
        d = math.hypot(nx, ny)
        if d >= 1.0:
            return 0.0
        return strength * (1.0 - d) ** 2.1

    canvas.paint((cx - rx, cy - ry, cx + rx, cy + ry), coverage, flat((0.30, 0.33, 0.33)))


def label(canvas, cx, top, half_w, half_h, tint=(0.95, 0.95, 0.94)):
    """A blank label band. No wordmark: the kiosk prints the name itself.

    Deliberately quiet. A bright white panel across the middle of every
    package is the thing that made these read as clip-art.
    """
    half_w *= 0.82
    half_h *= 0.76
    field = rounded_box(cx, top + half_h, half_w, half_h, 8)
    canvas.paint(
        (cx - half_w - 2, top - 2, cx + half_w + 2, top + 2 * half_h + 2),
        coverage_of(field),
        shade(tint, cx=cx, half_w=half_w, top=top, bottom=top + 2 * half_h, gloss=0.05),
    )


# ------------------------------------------------------------------ packages
#
# Each builder draws one kind of packaging. They take a colour and nothing
# else, because a placeholder's job is to read as "a bottle", "a carton", "a
# bag" from across a room.


def bottle(canvas, colour, *, cap=(0.16, 0.17, 0.18), slim=False):
    cx = SIZE / 2
    half_w = 150 if slim else 172
    top, bottom = 250, 880
    contact_shadow(canvas, cx, bottom - 8, half_w * 1.35, 46)

    neck_half = half_w * 0.36
    shoulder_top = top + 90

    # The column starts *above* the visible shoulder, so its rounded top
    # corner is hidden behind the taper instead of pinching the join.
    column = rounded_box(
        cx,
        (shoulder_top - 80 + bottom) / 2,
        half_w,
        (bottom - (shoulder_top - 80)) / 2,
        half_w * 0.4,
    )

    def body(x, y):
        if y >= shoulder_top:
            return column(x, y)
        if y <= top - 130:
            return 1e3
        # Above it the walls draw in towards the neck, reaching the column's
        # full width exactly where the column takes over.
        t = max(0.0, min(1.0, (shoulder_top - y) / 120.0))
        half = half_w - (half_w - neck_half * 1.02) * (t**1.6)
        return abs(x - cx) - half
    neck = rounded_box(cx, top - 40, neck_half, 70, 18)
    canvas.paint(
        (cx - half_w - 2, top - 180, cx + half_w + 2, bottom + 2),
        coverage_of(union(body, neck)),
        shade(colour, cx=cx, half_w=half_w, top=top - 110, bottom=bottom),
    )
    closure = rounded_box(cx, 150, neck_half * 1.22, 42, 14)
    canvas.paint(
        (cx - 90, 100, cx + 90, 200),
        coverage_of(closure),
        shade(cap, cx=cx, half_w=neck_half * 1.22, top=108, bottom=192, gloss=0.3),
    )
    label(canvas, cx, 470, half_w * 0.88, 95)


def can(canvas, colour):
    cx = SIZE / 2
    half_w = 155
    top, bottom = 230, 830
    contact_shadow(canvas, cx, bottom - 6, half_w * 1.3, 44)
    body = rounded_box(cx, (top + bottom) / 2, half_w, (bottom - top) / 2, 34)
    canvas.paint(
        (cx - half_w - 2, top - 2, cx + half_w + 2, bottom + 2),
        coverage_of(body),
        shade(colour, cx=cx, half_w=half_w, top=top, bottom=bottom, gloss=0.62),
    )
    for y in (top + 26, bottom - 26):
        rim = rounded_box(cx, y, half_w * 0.97, 13, 8)
        canvas.paint(
            (cx - half_w, y - 20, cx + half_w, y + 20),
            coverage_of(rim),
            shade((0.78, 0.80, 0.82), cx=cx, half_w=half_w, top=y - 13, bottom=y + 13),
        )
    label(canvas, cx, 440, half_w * 0.9, 92)


def carton(canvas, colour):
    cx = SIZE / 2
    half_w = 168
    top, bottom = 230, 860
    contact_shadow(canvas, cx, bottom - 6, half_w * 1.3, 42)
    body = rounded_box(cx, (top + bottom) / 2, half_w, (bottom - top) / 2, 16)
    canvas.paint(
        (cx - half_w - 2, top - 2, cx + half_w + 2, bottom + 2),
        coverage_of(body),
        shade(colour, cx=cx, half_w=half_w, top=top, bottom=bottom, gloss=0.22),
    )
    # The folded gable, as a lighter wedge across the top.
    gable = rounded_box(cx, top + 54, half_w * 0.97, 54, 12)
    canvas.paint(
        (cx - half_w, top, cx + half_w, top + 120),
        coverage_of(gable),
        shade(
            [min(1.0, channel * 1.12 + 0.05) for channel in colour],
            cx=cx,
            half_w=half_w,
            top=top,
            bottom=top + 108,
            gloss=0.18,
        ),
    )
    label(canvas, cx, 470, half_w * 0.86, 100)


def box(canvas, colour, *, wide=False):
    cx = SIZE / 2
    half_w = 212 if wide else 182
    top, bottom = (300, 830) if wide else (230, 860)
    contact_shadow(canvas, cx, bottom - 6, half_w * 1.26, 40)
    face = rounded_box(cx - 16, (top + bottom) / 2, half_w, (bottom - top) / 2, 18)
    canvas.paint(
        (cx - half_w - 20, top - 2, cx + half_w, bottom + 2),
        coverage_of(face),
        shade(colour, cx=cx - 16, half_w=half_w, top=top, bottom=bottom, gloss=0.0, wrap=False),
    )
    # A narrow side panel, so it reads as a box rather than a card.
    side = rounded_box(cx + half_w - 30, (top + bottom) / 2 + 6, 42, (bottom - top) / 2 - 10, 12)
    canvas.paint(
        (cx + half_w - 80, top, cx + half_w + 20, bottom),
        coverage_of(side),
        flat([channel * 0.74 for channel in colour]),
    )
    label(canvas, cx - 16, (top + bottom) / 2 - 40, half_w * 0.74, 88)


def pouch(canvas, colour):
    cx = SIZE / 2
    top, bottom = 220, 850
    contact_shadow(canvas, cx, bottom - 10, 230, 44)
    width = taper(168, 196, (top + bottom) / 2, (bottom - top) / 2)

    def field(x, y):
        half = width(y)
        # A gentle bulge at the middle, the way a bag of crisps sits, and
        # shoulders that round into the crimp instead of being cut off square.
        t = (y - top) / (bottom - top)
        if t < 0.0 or t > 1.0:
            return 1e3
        bulge = 26 * math.sin(math.pi * t)
        shoulder = 1.0 - (max(0.0, abs(t - 0.5) * 2 - 0.72) / 0.28) ** 2 * 0.52
        return abs(x - cx) - (half + bulge) * shoulder

    canvas.paint(
        (cx - 240, top - 2, cx + 240, bottom + 2),
        coverage_of(field),
        shade(colour, cx=cx, half_w=200, top=top, bottom=bottom, gloss=0.46),
    )
    # The crimped seams: the same film, pressed flat, so slightly darker.
    for y in (top + 16, bottom - 16):
        crimp = rounded_box(cx, y, 120, 13, 6)
        canvas.paint(
            (cx - 140, y - 20, cx + 140, y + 20),
            coverage_of(crimp),
            shade(
                [channel * 0.82 for channel in colour],
                cx=cx,
                half_w=120,
                top=y - 13,
                bottom=y + 13,
                gloss=0.2,
            ),
        )
    label(canvas, cx, 430, 150, 92)


def tube(canvas, colour):
    cx = SIZE / 2
    top, bottom = 230, 820
    contact_shadow(canvas, cx, bottom - 6, 180, 40)
    width = taper(58, 132, (top + bottom) / 2, (bottom - top) / 2)

    def field(x, y):
        if y < top or y > bottom:
            return 1e3
        return abs(x - cx) - width(y)

    canvas.paint(
        (cx - 150, top - 2, cx + 150, bottom + 2),
        coverage_of(field),
        shade(colour, cx=cx, half_w=130, top=top, bottom=bottom, gloss=0.5),
    )
    cap = rounded_box(cx, top - 44, 62, 48, 16)
    canvas.paint(
        (cx - 80, top - 110, cx + 80, top + 10),
        coverage_of(cap),
        shade([channel * 0.72 for channel in colour], cx=cx, half_w=62, top=top - 92, bottom=top),
    )
    label(canvas, cx, 520, 104, 86)


def tub(canvas, colour):
    cx = SIZE / 2
    top, bottom = 330, 790
    contact_shadow(canvas, cx, bottom - 6, 220, 42)
    width = taper(186, 150, (top + bottom) / 2, (bottom - top) / 2)

    def field(x, y):
        if y < top or y > bottom:
            return 1e3
        return abs(x - cx) - width(y)

    canvas.paint(
        (cx - 210, top - 2, cx + 210, bottom + 2),
        coverage_of(field),
        shade(colour, cx=cx, half_w=186, top=top, bottom=bottom, gloss=0.42),
    )
    lid = ellipse(cx, top + 6, 196, 40)
    canvas.paint(
        (cx - 210, top - 46, cx + 210, top + 52),
        coverage_of(lid),
        shade((0.95, 0.96, 0.97), cx=cx, half_w=196, top=top - 34, bottom=top + 46, gloss=0.3),
    )
    label(canvas, cx, 520, 148, 76)


def tin(canvas, colour):
    cx = SIZE / 2
    cy = 560
    half_w, half_h = 206, 118
    contact_shadow(canvas, cx, cy + half_h - 4, half_w * 1.22, 36)
    body = rounded_box(cx, cy, half_w, half_h, 40)
    canvas.paint(
        (cx - half_w - 2, cy - half_h - 2, cx + half_w + 2, cy + half_h + 2),
        coverage_of(body),
        shade(colour, cx=cx, half_w=half_w, top=cy - half_h, bottom=cy + half_h, gloss=0.6),
    )
    lid = ellipse(cx, cy - half_h + 26, half_w * 0.94, 34)
    canvas.paint(
        (cx - half_w, cy - half_h - 20, cx + half_w, cy - half_h + 70),
        coverage_of(lid),
        shade((0.80, 0.82, 0.84), cx=cx, half_w=half_w, top=cy - half_h, bottom=cy - half_h + 54),
    )
    label(canvas, cx, cy - 16, half_w * 0.74, 60)


def roll(canvas, colour):
    cx = SIZE / 2
    top, bottom = 290, 820
    half_w = 200
    contact_shadow(canvas, cx, bottom - 6, half_w * 1.24, 42)
    body = rounded_box(cx, (top + bottom) / 2, half_w, (bottom - top) / 2, 26)
    canvas.paint(
        (cx - half_w - 2, top - 2, cx + half_w + 2, bottom + 2),
        coverage_of(body),
        shade(colour, cx=cx, half_w=half_w, top=top, bottom=bottom, gloss=0.46),
    )
    # Two wrapped rolls showing through the pack.
    for offset in (-96, 96):
        seam = rounded_box(cx + offset, (top + bottom) / 2, 4, (bottom - top) / 2 - 30, 3)
        canvas.paint(
            (cx + offset - 8, top, cx + offset + 8, bottom),
            coverage_of(seam),
            flat([channel * 0.86 for channel in colour]),
        )
    label(canvas, cx, 470, half_w * 0.78, 96)


def bar(canvas, colour):
    cx = SIZE / 2
    cy = 540
    half_w, half_h = 224, 150
    contact_shadow(canvas, cx, cy + half_h - 4, half_w * 1.2, 38)
    body = rounded_box(cx, cy, half_w, half_h, 26)
    canvas.paint(
        (cx - half_w - 2, cy - half_h - 2, cx + half_w + 2, cy + half_h + 2),
        coverage_of(body),
        shade(colour, cx=cx, half_w=half_w, top=cy - half_h, bottom=cy + half_h, gloss=0.3),
    )
    label(canvas, cx, cy - 54, half_w * 0.7, 62)


def jar(canvas, colour):
    cx = SIZE / 2
    top, bottom = 300, 830
    half_w = 184
    contact_shadow(canvas, cx, bottom - 6, half_w * 1.26, 42)
    body = rounded_box(cx, (top + bottom) / 2, half_w, (bottom - top) / 2, 46)
    canvas.paint(
        (cx - half_w - 2, top - 2, cx + half_w + 2, bottom + 2),
        coverage_of(body),
        shade(colour, cx=cx, half_w=half_w, top=top, bottom=bottom, gloss=0.5),
    )
    lid = rounded_box(cx, top - 26, half_w * 0.86, 56, 18)
    canvas.paint(
        (cx - half_w, top - 90, cx + half_w, top + 40),
        coverage_of(lid),
        shade((0.22, 0.23, 0.24), cx=cx, half_w=half_w * 0.86, top=top - 82, bottom=top + 30),
    )
    label(canvas, cx, 500, half_w * 0.82, 92)


# -------------------------------------------------------------- the shelf
#
# Stem -> how it is packaged and what colour it is. The stems are the ones
# `demo/retail/menu.json` references; nothing here is a brand.

SHELF = {
    "cola": (bottle, (0.42, 0.07, 0.09)),
    "cola-zero": (bottle, (0.10, 0.11, 0.13)),
    "agua": (bottle, (0.55, 0.78, 0.88)),
    "agua-mineral": (bottle, (0.24, 0.52, 0.66)),
    "jugo": (carton, (0.93, 0.56, 0.11)),
    "papas": (pouch, (0.92, 0.70, 0.13)),
    "totopos": (pouch, (0.76, 0.21, 0.12)),
    "galletas": (box, (0.36, 0.20, 0.44)),
    "chocolate": (bar, (0.33, 0.19, 0.12)),
    "cereal": (box, (0.90, 0.35, 0.12)),
    "cafe": (jar, (0.30, 0.17, 0.11)),
    "pasta": (box, (0.11, 0.33, 0.55)),
    "arroz": (box, (0.86, 0.82, 0.70)),
    "atun": (tin, (0.24, 0.45, 0.70)),
    "leche": (carton, (0.95, 0.96, 0.97)),
    "yogurt": (tub, (0.86, 0.33, 0.47)),
    "shampoo": (bottle, (0.28, 0.63, 0.74)),
    "jabon": (bar, (0.52, 0.76, 0.58)),
    "pasta-dental": (tube, (0.16, 0.42, 0.72)),
    "detergente": (box, (0.17, 0.38, 0.68)),
    "papel": (roll, (0.60, 0.78, 0.86)),
    "limpiador": (bottle, (0.42, 0.70, 0.36)),
}


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for stem, (draw, colour) in sorted(SHELF.items()):
        canvas = Canvas(SIZE)
        canvas.background()
        draw(canvas, colour)
        path = OUT / f"{stem}.png"
        canvas.png(path)
        print(f"{path.relative_to(ROOT)}  {path.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
