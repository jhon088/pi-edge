#!/usr/bin/env python3
"""The retail lane's shelf: its products, its photographs and its barcodes.

One table, three outputs, so they cannot disagree:

    demo/retail/assets/*          the photograph each product is sold by
    demo/retail/menu.json         the dataset `demo-seed` imports
    demo/retail/barcodes.csv      the same rows, for printing test labels

The photographs come from `examples/fotos ret/`, which is **source material**:
it is read here and never at runtime, and a test asserts that no dataset
mentions it. Products are named after the photograph rather than the other way
round — a shelf where the picture does not match the label is worse than a
shelf of drawings.

Barcodes are generated, not transcribed: `750` (Mexico's GS1 prefix), the
product's index padded to nine digits, and an EAN-13 check digit. So every one
is a code a real scanner accepts, and the whole shelf can be printed and
scanned by hand.

    python3 scripts/retail-dataset.py
"""

from __future__ import annotations

import csv
import json
import subprocess
import sys
import tempfile
from pathlib import Path

from retail_imaging import as_png, read_png, write_png

ROOT = Path(__file__).resolve().parent.parent
PHOTOS = ROOT / "examples" / "fotos ret"
ASSETS = ROOT / "demo" / "retail" / "assets"
DATASET = ROOT / "demo" / "retail" / "menu.json"
BARCODES = ROOT / "demo" / "retail" / "barcodes.csv"

CHANNEL = "KIOSK-RETAIL"
# The restaurant's aisles, withheld from this lane. Named here because the
# availability rule is a conjunction: a lane offers by default and withholds
# the other lane's categories (ADR 047).
RESTAURANT_CATEGORIES = ["C-BURGERS", "C-COMBOS", "C-SIDES", "C-DRINKS"]

CATEGORIES = [
    ("R-BEBIDAS", "Bebidas", "Drinks"),
    ("R-SNACKS", "Snacks", "Snacks"),
    ("R-PANADERIA", "Panadería", "Bakery"),
    ("R-FRUTAS", "Frutas y Verduras", "Produce"),
    ("R-CARNES", "Carnes y Pollo", "Meat & poultry"),
    ("R-LACTEOS", "Lácteos", "Dairy"),
    ("R-DESPENSA", "Despensa", "Pantry"),
    ("R-HOGAR", "Hogar y Limpieza", "Home & cleaning"),
]

# product_ref, sku, name, category, price in pesos, image stem, source photo
#
# `source` is None where no photograph of that product exists yet: the stem
# then resolves to the rendered placeholder that is already on disk, and the
# product is listed as a placeholder in the report rather than dropped.
#
# `erase` blanks a rectangle of the source, in fractions of its size — the
# bananas arrive with a shelf sign beside them, and a sign in a product card
# reads as somebody else's price.
SHELF = [
    # product_ref,     sku,          name,                                              category,      price,  stem,            source,                   erase
    ("R-COCA-3L",      "SKU-RT-1001", "Coca-Cola Sabor Original 3 L",                    "R-BEBIDAS",    42.00, "coca",          "coca.avif",              ()),
    ("R-POWERADE",     "SKU-RT-1002", "Powerade Sabor Moras 1 L",                        "R-BEBIDAS",    24.00, "powerade",      "powerade.avif",          ()),
    ("R-BELIGHT",      "SKU-RT-1003", "Be Light Agua Saborizada 1.5 L",                  "R-BEBIDAS",    21.00, "belight",       "belight.avif",           ()),
    ("R-JUMEX",        "SKU-RT-1004", "Jumex Néctar de Naranja 1 L",                     "R-BEBIDAS",    27.50, "jumex",         "jumex.avif",             ()),
    ("R-RUFFLES",      "SKU-RT-2001", "Ruffles Queso 185 g",                             "R-SNACKS",     38.00, "ruffles",       "rufles.jpeg",            ()),
    ("R-BAGELS",       "SKU-RT-3001", "Bagels Bimbo Original 4 pz",                      "R-PANADERIA",  46.00, "bagels",        "bagel.avif",             ()),
    ("R-MANZANA",      "SKU-RT-4001", "Manzana Red Delicious 1 kg",                      "R-FRUTAS",     59.90, "manzana",       "manzana red.avif",       ()),
    ("R-PLATANO",      "SKU-RT-4002", "Plátano Orgánico por Penca 1 kg",                 "R-FRUTAS",     19.90, "platano",       "platano.avif",           ((0.03, 0.05, 0.52, 0.34),)),
    ("R-LIMON",        "SKU-RT-4003", "Limón con Semilla 1 kg",                          "R-FRUTAS",     32.90, "limon",         "limon.avif",             ()),
    ("R-ARRACHERA",    "SKU-RT-5001", "Arrachera Marinada SuKarne 600 g",                "R-CARNES",    189.00, "arrachera",     "arrachera sukarne.avif", ()),
    ("R-MOLIDA",       "SKU-RT-5002", "Carne Molida de Res 90/10 500 g",                 "R-CARNES",     98.00, "molida",        "molida de res.avif",     ()),
    ("R-ALITAS",       "SKU-RT-5003", "Alitas de Pollo Bachoco Picositas",               "R-CARNES",     89.00, "alitas",        "alitas bachoco.avif",    ()),
    ("R-POLLO",        "SKU-RT-5004", "Pollo Rostizado Entero",                          "R-CARNES",    139.00, "pollo",         "pollo rostizado.avif",   ()),
    ("R-LECHE",        "SKU-RT-6001", "Leche Entera 1 L",                                "R-LACTEOS",    26.50, "leche",         None,                     ()),
    ("R-YOGURT",       "SKU-RT-6002", "Yogurt Natural 900 g",                            "R-LACTEOS",    34.00, "yogurt",        None,                     ()),
    ("R-ACEITE",       "SKU-RT-7001", "Aceite Vegetal Cártamus 900 ml",                  "R-DESPENSA",   48.50, "aceite",        "aceite.avif",            ()),
    ("R-ATUN",         "SKU-RT-7002", "Atún Tuny en Agua 140 g",                         "R-DESPENSA",   22.50, "atun",          "atun.avif",              ()),
    ("R-CAFE",         "SKU-RT-7003", "Café Molido 250 g",                               "R-DESPENSA",   89.00, "cafe",          None,                     ()),
    ("R-CLOROX",       "SKU-RT-8001", "Cloro Clorox Triple Acción 1.89 L",               "R-HOGAR",      38.50, "clorox",        "clorox.avif",            ()),
    ("R-PERSIL-LIQ",   "SKU-RT-8002", "Detergente Líquido Persil Universal 1.66 L",      "R-HOGAR",     112.00, "persil",        "persil.avif",            ()),
    ("R-PERSIL-POL",   "SKU-RT-8003", "Detergente en Polvo Persil Universal 4.5 kg",     "R-HOGAR",     189.00, "persil-polvo",  "detergente polvo.avif",  ()),
    ("R-PAPEL",        "SKU-RT-8004", "Papel Higiénico 4 rollos",                        "R-HOGAR",      42.00, "papel",         None,                     ()),
]


def ean13(index: int) -> str:
    """`750` + the index, padded, + the digit a scanner checks."""
    body = f"750{index:09d}"
    total = sum((3 if at % 2 else 1) * int(digit) for at, digit in enumerate(body))
    return body + str((10 - total % 10) % 10)


def prepare_photo(source: Path, stem: str, boxes, work: Path) -> str:
    """One source photograph, as the square JPEG the lane is sold by.

    The sources are already square packshots on white, so there is nothing to
    crop and nothing to letterbox: the only work is erasing anything the
    photograph carries that a product card should not, and re-encoding.
    """
    width, height, channels, pixels = read_png(as_png(source, work))
    if width != height:
        sys.exit(f"{source.name} is {width}x{height}; the cards expect a square")

    flat = []
    masked = [
        (int(x0 * width), int(y0 * height), int(x1 * width), int(y1 * height))
        for x0, y0, x1, y1 in boxes
    ]
    for y in range(height):
        row = y * width
        for x in range(width):
            if any(x0 <= x < x1 and y0 <= y < y1 for x0, y0, x1, y1 in masked):
                flat.append((255, 255, 255))
                continue
            at = (row + x) * channels
            flat.append((pixels[at], pixels[at + 1], pixels[at + 2]))

    draft = work / f"{stem}.flat.png"
    write_png(draft, width, height, flat)
    target = ASSETS / f"{stem}.jpg"
    subprocess.run(
        ["sips", "-s", "format", "jpeg", "-s", "formatOptions", "88",
         str(draft), "--out", str(target)],
        check=True,
        capture_output=True,
    )
    return target.name


def text(default: str, english: str) -> dict:
    return {"default": default, "translations": {"en": english}}


def main() -> None:
    if not PHOTOS.is_dir():
        sys.exit(f"no source photographs at {PHOTOS}")
    ASSETS.mkdir(parents=True, exist_ok=True)

    # --- the photographs ---------------------------------------------------
    wanted: set[str] = set()
    media: dict[str, str] = {}
    with tempfile.TemporaryDirectory() as workspace:
        work = Path(workspace)
        for *_, stem, source, boxes in SHELF:
            if source is None:
                placeholder = ASSETS / f"{stem}.png"
                if not placeholder.is_file():
                    sys.exit(f"{stem} has neither a photograph nor a placeholder")
                wanted.add(placeholder.name)
                media[stem] = "image/png"
                continue
            path = PHOTOS / source
            if not path.is_file():
                sys.exit(f"missing {path}")
            wanted.add(prepare_photo(path, stem, boxes, work))
            media[stem] = "image/jpeg"
            print(f"  {stem}.jpg  <- {source}")

    # Anything not on the shelf any more goes: the installer should carry the
    # lane as it is, not as it used to be.
    for existing in sorted(ASSETS.iterdir()):
        if existing.is_file() and existing.name not in wanted:
            existing.unlink()
            print(f"  removed {existing.name}")

    # --- the dataset -------------------------------------------------------
    first_of: dict[str, str] = {}
    for _, _, _, category, _, stem, _, _ in SHELF:
        first_of.setdefault(category, stem)

    def image(stem: str) -> dict:
        return {"content_hash": f"file:{stem}", "media_type": media[stem], "byte_size": 0}

    dataset = {
        "version": 1,
        "vertical": "retail",
        "channels": [
            {
                "channel_ref": CHANNEL,
                "name": text("Autoservicio", "Self-checkout"),
                "default_availability": "OFFERED",
            }
        ],
        "categories": [
            {
                "category_ref": ref,
                "name": text(spanish, english),
                "display_order": order,
                "image": image(first_of[ref]),
            }
            for order, (ref, spanish, english) in enumerate(CATEGORIES)
        ],
        "products": [
            {
                "product_ref": ref,
                "sku": sku,
                "name": name,
                "tax_profile": "IVA16",
                "barcodes": [ean13(index)],
                "prices": [
                    {
                        "price_book": "BASE",
                        "amount_minor": round(price * 100),
                        "price_mode": "TAX_EXCLUSIVE",
                    }
                ],
            }
            for index, (ref, sku, name, _, price, _, _, _) in enumerate(SHELF, start=1)
        ],
        "presentation": [
            {
                "product_ref": ref,
                "category_ref": category,
                "display_name": text(name, name),
                "display_order": index,
                "image": image(stem),
                "recommended_product_refs": [],
            }
            for index, (ref, _, name, category, _, stem, _, _) in enumerate(SHELF, start=1)
        ],
        "availability": [
            {
                "channel_ref": CHANNEL,
                "target_kind": "CATEGORY",
                "target_ref": ref,
                "state": "WITHHELD",
            }
            for ref in RESTAURANT_CATEGORIES
        ]
        + [
            {
                "channel_ref": "KIOSK",
                "target_kind": "CATEGORY",
                "target_ref": ref,
                "state": "WITHHELD",
            }
            for ref, _, _ in CATEGORIES
        ],
    }
    with DATASET.open("w", encoding="utf-8") as handle:
        json.dump(dataset, handle, ensure_ascii=False, indent=2)
        handle.write("\n")

    # --- the labels to print -----------------------------------------------
    names = {ref: spanish for ref, spanish, _ in CATEGORIES}
    with BARCODES.open("w", encoding="utf-8", newline="") as handle:
        out = csv.writer(handle)
        out.writerow(
            ["product_ref", "sku", "barcode", "name", "price_mxn", "category", "image"]
        )
        for index, (ref, sku, name, category, price, stem, source, _) in enumerate(
            SHELF, start=1
        ):
            out.writerow(
                [
                    ref,
                    sku,
                    ean13(index),
                    name,
                    f"{price:.2f}",
                    names[category],
                    f"{stem}.jpg" if source else f"{stem}.png",
                ]
            )

    real = sum(1 for *_, source, _ in SHELF if source)
    print(
        f"\n{len(SHELF)} products, {len(CATEGORIES)} categories, "
        f"{real} photographs, {len(SHELF) - real} placeholders"
    )
    print(f"{DATASET.relative_to(ROOT)}")
    print(f"{BARCODES.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
