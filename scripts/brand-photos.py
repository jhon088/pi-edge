#!/usr/bin/env python3
"""Cut product photographs out of the brand screenshots in `examples/`.

Showroom material. The sources are screen captures of a delivery app, so the
photograph of each dish sits on a white page next to its name and price. What
this does is find that photograph and save it square, on white, at the size a
kiosk card wants.

Two shapes of source, two ways of finding the picture:

* **A product page** — one large photograph in the upper left. Found by its
  own bounding box: the page is white, so the non-white content in that
  quadrant *is* the picture.
* **A menu listing** — a grid of small thumbnails, each on a coloured tile.
  Found by their geometry, which is regular, and taken at whatever resolution
  the capture has. Sbarro's are about a hundred pixels square; that is the
  resolution the material exists at, and upscaling is the honest limit of it.

    python3 scripts/brand-photos.py

Writes into `demo/tim/assets` and `demo/sbarro/assets`.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

from retail_imaging import as_png, read_png, write_png

ROOT = Path(__file__).resolve().parent.parent
TIM_SOURCE = ROOT / "examples" / "TIM"
SBARRO_SOURCE = ROOT / "examples" / "SBARRO"

# Sources are named by capture time, which says nothing. Index them in the
# order the folder lists them and name what each one holds.
#
#   stem, which capture (1-based), where to look
TIM_PAGES = [
    ("croissant-hashbrown", 2),
    ("cafe-dark-roast", 3),
    ("cafe-mexicano", 4),
    ("latte", 5),
    ("french-vanilla-latte", 6),
    ("timbits-10", 7),
    ("dona-honey-dip", 8),
    ("iced-capp", 9),
    ("refresher-jamaica", 10),
    ("frappe-chocolate", 11),
    ("biscuit-jamon", 20),
    ("croissant-salchicha", 21),
    ("quencher-moratonic", 24),
]

# Sbarro's photographs, supplied ready to use: square, on the brand's red, one
# dish each. They are indexed in the order the folder lists them because the
# filenames carry only a timestamp.
#
#   stem, which file (1-based)
SBARRO_PHOTOS = [
    ("pizza-pepperoni", 1),
    ("pizza-queso", 2),
    ("pizza-carne-suprema", 3),
    ("pizza-hawaiian", 4),
    ("pizza-champinones", 5),
    ("rebanada-pepperoni", 6),
    ("ensalada-cesar", 7),
    ("ensalada-griega", 8),
    ("papas-sbarro", 9),
    ("pan-de-ajo", 10),
    ("bebida-coca", 11),
    ("bebida-coca-zero", 12),
    ("bebida-sprite", 13),
    ("bebida-fanta", 14),
]

def captures(folder: Path) -> list[Path]:
    """The screen captures, in the order the folder lists them."""
    return sorted(p for p in folder.iterdir() if p.suffix.lower() == ".png")


def bounding_box(width, height, channels, pixels, region, threshold=244):
    """The box around everything that is not page-white inside `region`."""
    x0f, y0f, x1f, y1f = region
    left, right = int(x0f * width), int(x1f * width)
    top, bottom = int(y0f * height), int(y1f * height)
    found = [width, height, 0, 0]
    for y in range(top, bottom):
        row = y * width
        for x in range(left, right):
            at = (row + x) * channels
            if min(pixels[at], pixels[at + 1], pixels[at + 2]) < threshold:
                if x < found[0]:
                    found[0] = x
                if y < found[1]:
                    found[1] = y
                if x > found[2]:
                    found[2] = x
                if y > found[3]:
                    found[3] = y
    if found[2] <= found[0] or found[3] <= found[1]:
        return None
    return found


def square_crop(width, height, channels, pixels, box, size, pad=0.06):
    """A square crop around `box`, padded, scaled to `size`, on white.

    Padded rather than tight: a dish photographed to the edge of its frame
    looks cramped on a card, and the sources all have air around them.
    """
    x0, y0, x1, y1 = box
    side = max(x1 - x0, y1 - y0)
    side = int(side * (1 + pad * 2))
    cx, cy = (x0 + x1) // 2, (y0 + y1) // 2
    left, top = cx - side // 2, cy - side // 2

    out = []
    step = side / size
    for ty in range(size):
        sy = top + int(ty * step)
        for tx in range(size):
            sx = left + int(tx * step)
            if sx < 0 or sy < 0 or sx >= width or sy >= height:
                out.append((255, 255, 255))
                continue
            at = (sy * width + sx) * channels
            out.append((pixels[at], pixels[at + 1], pixels[at + 2]))
    return out


def encode(draft: Path, target: Path, quality: str = "88", longest: int | None = None) -> None:
    command = ["sips", "-s", "format", "jpeg", "-s", "formatOptions", quality]
    if longest:
        command += ["-Z", str(longest)]
    command += [str(draft), "--out", str(target)]
    subprocess.run(command, check=True, capture_output=True)


def main() -> None:
    for folder in (TIM_SOURCE, SBARRO_SOURCE):
        if not folder.is_dir():
            sys.exit(f"no source captures at {folder}")

    tim_out = ROOT / "demo" / "tim" / "assets"
    sbarro_out = ROOT / "demo" / "sbarro" / "assets"
    tim_out.mkdir(parents=True, exist_ok=True)
    sbarro_out.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory() as workspace:
        work = Path(workspace)

        tim = captures(TIM_SOURCE)
        for stem, index in TIM_PAGES:
            source = tim[index - 1]
            width, height, channels, pixels = read_png(as_png(source, work))
            # The photograph lives in the upper left of a product page; the
            # right-hand column is the name, the price and the options.
            box = bounding_box(width, height, channels, pixels, (0.0, 0.0, 0.42, 0.52))
            if box is None:
                print(f"  ! {stem}: found nothing in the upper left")
                continue
            draft = work / f"{stem}.png"
            write_png(draft, 760, 760,
                      square_crop(width, height, channels, pixels, box, 760))
            encode(draft, tim_out / f"{stem}.jpg")
            print(f"  tim/{stem}.jpg  <- capture {index}  box={box}")

        sbarro = captures(SBARRO_SOURCE)
        if len(sbarro) < len(SBARRO_PHOTOS):
            sys.exit(
                f"{SBARRO_SOURCE} holds {len(sbarro)} images; "
                f"{len(SBARRO_PHOTOS)} are mapped"
            )
        for stem, index in SBARRO_PHOTOS:
            source = sbarro[index - 1]
            # Already square and already composed. Downscaled to the size a
            # card draws and re-encoded, so the installer is not carrying two
            # and a half megabytes per dish.
            encode(source, sbarro_out / f"{stem}.jpg", quality="86", longest=760)
            size = (sbarro_out / f"{stem}.jpg").stat().st_size // 1024
            print(f"  sbarro/{stem}.jpg  <- {source.name}  {size} KB")

        wanted = {f"{stem}.jpg" for stem, _ in SBARRO_PHOTOS}
        for existing in sorted(sbarro_out.iterdir()):
            if existing.is_file() and existing.name not in wanted:
                existing.unlink()
                print(f"  removed {existing.name}")


if __name__ == "__main__":
    main()
