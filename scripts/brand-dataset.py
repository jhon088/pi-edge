#!/usr/bin/env python3
"""The Tim Hortons and Sbarro shelves, for the showroom.

Two more restaurant lanes on the same core, the same catalog and the same
payment journey as the one that was already there — a channel and a shell, not
a second application (ADR 047). Each lane declares its own channel, its own
categories and its own products, and withholds every other lane's aisles.

Names, prices and photographs all come from the captures in `examples/`, which
are read when preparing an asset and never at runtime.

    python3 scripts/brand-photos.py      # the photographs first
    python3 scripts/brand-dataset.py     # then the datasets

Writes `demo/tim/menu.json` and `demo/sbarro/menu.json`.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from lanes import LANES, rows_for

ROOT = Path(__file__).resolve().parent.parent

TIM = {
    "slug": "tim",
    "channel": "KIOSK-TIM",
    "channel_name": ("Tim Hortons", "Tim Hortons"),
    "categories": [
        ("T-DESAYUNO", "Desayunos", "Breakfast"),
        ("T-PANADERIA", "Donas y Panadería", "Donuts & bakery"),
        ("T-CAFE", "Café", "Coffee"),
        ("T-FRIAS", "Bebidas Frías", "Cold drinks"),
    ],
    # product_ref, name, category, price, stem, what it suggests
    "products": [
        ("T-CROISSANT-HASH", "Croissant + Papa Hashbrown", "T-DESAYUNO", 104.00,
         "croissant-hashbrown", ["T-CAFE-DARK", "T-DONA-HONEY", "T-ICED-CAPP"]),
        ("T-BISCUIT-JAMON", "Biscuit — Jamón, Huevo y Queso", "T-DESAYUNO", 92.00,
         "biscuit-jamon", ["T-CAFE-MEXICANO", "T-DONA-HONEY", "T-TIMBITS-10"]),
        ("T-CROISSANT-SALCH", "Croissant — Salchicha, Huevo y Queso", "T-DESAYUNO", 72.00,
         "croissant-salchicha", ["T-LATTE", "T-DONA-HONEY", "T-FRAPPE-CHOCO"]),
        ("T-DONA-HONEY", "Dona Honey Dip", "T-PANADERIA", 31.00,
         "dona-honey-dip", ["T-CAFE-DARK", "T-LATTE", "T-ICED-CAPP"]),
        ("T-TIMBITS-10", "Timbits — Paquete de 10", "T-PANADERIA", 94.40,
         "timbits-10", ["T-CAFE-DARK", "T-CAFE-MEXICANO", "T-FRAPPE-CHOCO"]),
        ("T-CAFE-DARK", "Café Dark Roast", "T-CAFE", 47.00,
         "cafe-dark-roast", ["T-DONA-HONEY", "T-CROISSANT-HASH", "T-TIMBITS-10"]),
        ("T-CAFE-MEXICANO", "Café Mexicano", "T-CAFE", 42.00,
         "cafe-mexicano", ["T-DONA-HONEY", "T-BISCUIT-JAMON", "T-TIMBITS-10"]),
        ("T-LATTE", "Latte", "T-CAFE", 38.00,
         "latte", ["T-DONA-HONEY", "T-CROISSANT-SALCH", "T-TIMBITS-10"]),
        ("T-VANILLA-LATTE", "French Vanilla Latte", "T-CAFE", 38.00,
         "french-vanilla-latte", ["T-DONA-HONEY", "T-CROISSANT-HASH", "T-TIMBITS-10"]),
        ("T-ICED-CAPP", "Iced Capp Original", "T-FRIAS", 75.00,
         "iced-capp", ["T-DONA-HONEY", "T-TIMBITS-10", "T-CROISSANT-HASH"]),
        ("T-FRAPPE-CHOCO", "Frappé Chocolate", "T-FRIAS", 69.20,
         "frappe-chocolate", ["T-DONA-HONEY", "T-TIMBITS-10", "T-BISCUIT-JAMON"]),
        ("T-REFRESHER-JAM", "Refresher Tasty Jamaica", "T-FRIAS", 75.00,
         "refresher-jamaica", ["T-DONA-HONEY", "T-CROISSANT-HASH", "T-TIMBITS-10"]),
        ("T-QUENCHER-MORA", "Quencher Moratonic", "T-FRIAS", 75.00,
         "quencher-moratonic", ["T-DONA-HONEY", "T-BISCUIT-JAMON", "T-TIMBITS-10"]),
    ],
}

SBARRO = {
    "slug": "sbarro",
    "channel": "KIOSK-SBARRO",
    "channel_name": ("Sbarro", "Sbarro"),
    "categories": [
        ("S-PIZZAS", "Pizzas NY 14\"", "NY pizzas"),
        ("S-REBANADAS", "Rebanadas", "Slices"),
        ("S-ENSALADAS", "Ensaladas y Acompañamientos", "Salads & sides"),
        ("S-BEBIDAS", "Bebidas", "Drinks"),
    ],
    # The shelf follows the photographs: one dish per picture, and no product
    # wearing a picture of something else. The combos and the slices nobody
    # photographed went with their images rather than being dressed in a
    # neighbour's (a card whose picture does not match its label is worse than
    # no card). The combo is still sold — as a modifier on every pizza.
    "products": [
        ("S-PIZZA-PEPPERONI", "Pizza Pepperoni 14\"", "S-PIZZAS", 389.00,
         "pizza-pepperoni", ["S-BEB-COCA", "S-PAPAS", "S-PAN-AJO"]),
        ("S-PIZZA-QUESO", "Pizza Queso 14\"", "S-PIZZAS", 389.00,
         "pizza-queso", ["S-BEB-COCA", "S-PAN-AJO", "S-ENSALADA-CESAR"]),
        ("S-PIZZA-CARNE", "Pizza Carne Suprema 14\"", "S-PIZZAS", 389.00,
         "pizza-carne-suprema", ["S-BEB-COCA-ZERO", "S-PAPAS", "S-ENSALADA-GRIEGA"]),
        ("S-PIZZA-HAWAIIAN", "Pizza Hawaiian 14\"", "S-PIZZAS", 389.00,
         "pizza-hawaiian", ["S-BEB-FANTA", "S-PAPAS", "S-PAN-AJO"]),
        ("S-PIZZA-CHAMP", "Pizza Champiñones 14\"", "S-PIZZAS", 389.00,
         "pizza-champinones", ["S-BEB-SPRITE", "S-ENSALADA-CESAR", "S-PAN-AJO"]),
        ("S-REB-PEPPERONI", "Rebanada de Pepperoni", "S-REBANADAS", 129.00,
         "rebanada-pepperoni", ["S-BEB-COCA", "S-PAPAS", "S-ENSALADA-CESAR"]),
        ("S-ENSALADA-CESAR", "Ensalada César", "S-ENSALADAS", 99.00,
         "ensalada-cesar", ["S-REB-PEPPERONI", "S-PAN-AJO", "S-BEB-SPRITE"]),
        ("S-ENSALADA-GRIEGA", "Ensalada Griega", "S-ENSALADAS", 139.00,
         "ensalada-griega", ["S-REB-PEPPERONI", "S-PAN-AJO", "S-BEB-COCA-ZERO"]),
        ("S-PAPAS", "Papas Sbarro", "S-ENSALADAS", 49.00,
         "papas-sbarro", ["S-REB-PEPPERONI", "S-BEB-COCA", "S-PAN-AJO"]),
        ("S-PAN-AJO", "Pan de Ajo", "S-ENSALADAS", 79.00,
         "pan-de-ajo", ["S-REB-PEPPERONI", "S-PIZZA-QUESO", "S-BEB-COCA"]),
        ("S-BEB-COCA", "Coca-Cola 600 ml", "S-BEBIDAS", 45.00,
         "bebida-coca", ["S-REB-PEPPERONI", "S-PAPAS", "S-PIZZA-PEPPERONI"]),
        ("S-BEB-COCA-ZERO", "Coca-Cola Zero 600 ml", "S-BEBIDAS", 45.00,
         "bebida-coca-zero", ["S-REB-PEPPERONI", "S-ENSALADA-CESAR", "S-PAPAS"]),
        ("S-BEB-SPRITE", "Sprite 600 ml", "S-BEBIDAS", 45.00,
         "bebida-sprite", ["S-REB-PEPPERONI", "S-PAN-AJO", "S-PIZZA-QUESO"]),
        ("S-BEB-FANTA", "Fanta 600 ml", "S-BEBIDAS", 45.00,
         "bebida-fanta", ["S-REB-PEPPERONI", "S-PAPAS", "S-PIZZA-HAWAIIAN"]),
    ],
}


# --------------------------------------------------------------- modifiers
#
# A priced choice is backed by a **component product**: an ordinary catalog
# product with a price and no presentation, so it never appears on a menu but
# the core can price the line that names it (ADR 046). A free choice — "sin
# azúcar", "masa tradicional" — needs none.
#
#   component_ref, name, price in pesos
COMPONENTS = [
    # Tim Hortons
    ("T-X-GRANDE", "Tamaño grande", 8.00),
    ("T-X-GRANDE-FRIA", "Tamaño grande", 10.00),
    ("T-X-DESLAC", "Leche deslactosada", 6.00),
    ("T-X-VEGETAL", "Leche vegetal", 12.00),
    ("T-X-SHOT", "Shot extra de espresso", 14.00),
    ("T-X-CREMA", "Crema batida", 10.00),
    ("T-X-COMBO", "Combo: café y hashbrown", 35.00),
    # Sbarro
    ("S-X-RELLENA", "Orilla rellena de queso", 60.00),
    ("S-X-PEPP", "Pepperoni extra", 35.00),
    ("S-X-QUESO", "Queso extra", 30.00),
    ("S-X-CHAMP", "Champiñones", 25.00),
    ("S-X-COMBO", "Combo: 2 bebidas y pan de ajo", 95.00),
    ("S-X-BEBIDA", "Agrega una bebida", 35.00),
]

# group_id, owner product(s), name, min, max, [(option_id, label, component|None,
#                                               max per option, default)]
#
# Written per brand and applied to every product named, because a coffee asks
# the same three questions whichever coffee it is.
MODIFIERS = [
    # --- Tim Hortons: hot drinks -----------------------------------------
    ("G-T-SIZE", ["T-CAFE-DARK", "T-CAFE-MEXICANO", "T-LATTE", "T-VANILLA-LATTE"],
     "Tamaño", 1, 1,
     [("O-T-MED", "Mediano", None, 1, 1),
      ("O-T-GDE", "Grande", "T-X-GRANDE", 1, 0)]),
    ("G-T-MILK", ["T-LATTE", "T-VANILLA-LATTE", "T-CAFE-MEXICANO"],
     "Leche", 1, 1,
     [("O-T-ENTERA", "Entera", None, 1, 1),
      ("O-T-DESLAC", "Deslactosada", "T-X-DESLAC", 1, 0),
      ("O-T-VEGETAL", "Vegetal", "T-X-VEGETAL", 1, 0)]),
    ("G-T-SUGAR", ["T-CAFE-DARK", "T-CAFE-MEXICANO", "T-LATTE", "T-VANILLA-LATTE"],
     "Azúcar", 0, 1,
     [("O-T-SINAZ", "Sin azúcar", None, 1, 0),
      ("O-T-POCA", "Poca azúcar", None, 1, 0)]),
    ("G-T-EXTRAS", ["T-CAFE-DARK", "T-CAFE-MEXICANO", "T-LATTE", "T-VANILLA-LATTE"],
     "Extras", 0, 3,
     [("O-T-SHOT", "Shot extra de espresso", "T-X-SHOT", 2, 0),
      ("O-T-CREMA", "Crema batida", "T-X-CREMA", 1, 0)]),
    # --- Tim Hortons: cold drinks ----------------------------------------
    ("G-T-SIZE-F", ["T-ICED-CAPP", "T-FRAPPE-CHOCO", "T-REFRESHER-JAM", "T-QUENCHER-MORA"],
     "Tamaño", 1, 1,
     [("O-T-MED-F", "Mediano", None, 1, 1),
      ("O-T-GDE-F", "Grande", "T-X-GRANDE-FRIA", 1, 0)]),
    ("G-T-EXTRAS-F", ["T-ICED-CAPP", "T-FRAPPE-CHOCO"],
     "Extras", 0, 2,
     [("O-T-SHOT-F", "Shot extra de espresso", "T-X-SHOT", 2, 0),
      ("O-T-CREMA-F", "Crema batida", "T-X-CREMA", 1, 0)]),
    # --- Tim Hortons: breakfast ------------------------------------------
    ("G-T-COMBO", ["T-CROISSANT-HASH", "T-BISCUIT-JAMON", "T-CROISSANT-SALCH"],
     "¿Lo hacemos combo?", 0, 1,
     [("O-T-COMBO", "Agrega café y hashbrown", "T-X-COMBO", 1, 0)]),
    ("G-T-REMOVE", ["T-CROISSANT-HASH", "T-BISCUIT-JAMON", "T-CROISSANT-SALCH"],
     "Quitar ingredientes", 0, 2,
     [("O-T-NOQUESO", "Sin queso", None, 1, 0),
      ("O-T-NOSALSA", "Sin salsa", None, 1, 0)]),
    # --- Sbarro: whole pizzas --------------------------------------------
    ("G-S-MASA", ["S-PIZZA-QUESO", "S-PIZZA-CARNE", "S-PIZZA-CHAMP",
                  "S-PIZZA-HAWAIIAN", "S-PIZZA-PEPPERONI"],
     "Masa", 1, 1,
     [("O-S-TRAD", "Tradicional", None, 1, 1),
      ("O-S-DELGADA", "Delgada", None, 1, 0),
      ("O-S-RELLENA", "Orilla rellena de queso", "S-X-RELLENA", 1, 0)]),
    ("G-S-EXTRAS", ["S-PIZZA-QUESO", "S-PIZZA-CARNE", "S-PIZZA-CHAMP",
                    "S-PIZZA-HAWAIIAN", "S-PIZZA-PEPPERONI"],
     "Ingredientes extra", 0, 4,
     [("O-S-PEPP", "Pepperoni extra", "S-X-PEPP", 2, 0),
      ("O-S-QUESO", "Queso extra", "S-X-QUESO", 2, 0),
      ("O-S-CHAMP", "Champiñones", "S-X-CHAMP", 1, 0)]),
    ("G-S-COMBO", ["S-PIZZA-QUESO", "S-PIZZA-CARNE", "S-PIZZA-CHAMP",
                   "S-PIZZA-HAWAIIAN", "S-PIZZA-PEPPERONI"],
     "¿Lo hacemos combo?", 0, 1,
     [("O-S-COMBO", "Agrega 2 bebidas y pan de ajo", "S-X-COMBO", 1, 0)]),
    # --- Sbarro: slices ---------------------------------------------------
    ("G-S-EXTRAS-REB", ["S-REB-PEPPERONI"],
     "Ingredientes extra", 0, 2,
     [("O-S-PEPP-R", "Pepperoni extra", "S-X-PEPP", 1, 0),
      ("O-S-QUESO-R", "Queso extra", "S-X-QUESO", 1, 0)]),
    ("G-S-BEBIDA", ["S-REB-PEPPERONI"],
     "¿Agregamos bebida?", 0, 1,
     [("O-S-BEBIDA", "Agrega una bebida", "S-X-BEBIDA", 1, 0)]),
    # --- Sbarro: salads and sides ----------------------------------------
    ("G-S-ADEREZO", ["S-ENSALADA-CESAR", "S-ENSALADA-GRIEGA"],
     "Aderezo", 1, 1,
     [("O-S-CESAR", "César", None, 1, 1),
      ("O-S-ITAL", "Italiano", None, 1, 0),
      ("O-S-RANCH", "Ranch", None, 1, 0)]),
    ("G-S-SALSA", ["S-PAPAS", "S-PAN-AJO"],
     "Salsas", 0, 2,
     [("O-S-MARINARA", "Marinara", None, 1, 0),
      ("O-S-RANCH-D", "Ranch", None, 1, 0)]),
]


def modifiers_for(products: set[str]) -> tuple[list, list, set]:
    """The groups, options and component refs this brand's products need.

    A group is written once per product that asks it, because the catalog
    hangs a question off a product rather than off a kind of product.
    """
    groups, options, components = [], [], set()
    order = 0
    for group_id, owners, name, low, high, choices in MODIFIERS:
        for owner in owners:
            if owner not in products:
                continue
            scoped = f"{group_id}-{owner}"
            groups.append({
                "group_id": scoped,
                "owner_kind": "PRODUCT",
                "owner_ref": owner,
                "name": text(name, name),
                "min_quantity": low,
                "max_quantity": high,
                "display_order": order,
            })
            for index, (option_id, label, component, most, default) in enumerate(choices):
                entry = {
                    "option_id": f"{option_id}-{owner}",
                    "group_id": scoped,
                    "name": text(label, label),
                    "max_quantity": most,
                    "default_quantity": default,
                    "display_order": index,
                }
                if component:
                    entry["component_product_ref"] = component
                    components.add(component)
                options.append(entry)
            order += 1
    return groups, options, components


def text(default: str, english: str) -> dict:
    return {"default": default, "translations": {"en": english}}


def image(stem: str) -> dict:
    return {"content_hash": f"file:{stem}", "media_type": "image/jpeg", "byte_size": 0}


def build(brand: dict) -> dict:
    channel = brand["channel"]
    assets = ROOT / "demo" / brand["slug"] / "assets"
    for product in brand["products"]:
        stem = product[4]
        if not (assets / f"{stem}.jpg").is_file():
            sys.exit(f"{brand['slug']}: no photograph for {stem}; run brand-photos.py")

    first_of: dict[str, str] = {}
    for _, _, category, _, stem, _ in brand["products"]:
        first_of.setdefault(category, stem)

    # This lane declares what *it* does not sell, and nothing about any other
    # channel: two datasets claiming the same channel/target pair is a
    # duplicate the core refuses, correctly.
    availability = rows_for(channel)

    mine = {ref for ref, *_ in brand["products"]}
    groups, options, needed = modifiers_for(mine)
    # Component products carry the price of a choice and nothing else: no
    # presentation, so they are never a card on a menu.
    priced = {ref: (name, price) for ref, name, price in COMPONENTS}
    components = [
        {
            "product_ref": ref,
            "sku": ref,
            "name": priced[ref][0],
            "tax_profile": "IVA16",
            "barcodes": [],
            "prices": [{
                "price_book": "BASE",
                "amount_minor": round(priced[ref][1] * 100),
                "price_mode": "TAX_EXCLUSIVE",
            }],
        }
        for ref in sorted(needed)
    ]

    return {
        "version": 1,
        "vertical": brand["slug"],
        "channels": [{
            "channel_ref": channel,
            "name": text(*brand["channel_name"]),
            "default_availability": "OFFERED",
        }],
        "categories": [
            {
                "category_ref": ref,
                "name": text(spanish, english),
                "display_order": order,
                "image": image(first_of[ref]),
            }
            for order, (ref, spanish, english) in enumerate(brand["categories"])
        ],
        "products": [
            {
                "product_ref": ref,
                "sku": ref,
                "name": name,
                "tax_profile": "IVA16",
                "barcodes": [],
                "prices": [{
                    "price_book": "BASE",
                    "amount_minor": round(price * 100),
                    "price_mode": "TAX_EXCLUSIVE",
                }],
            }
            for ref, name, _, price, _, _ in brand["products"]
        ] + components,
        "selection_groups": groups,
        "selection_options": options,
        "presentation": [
            {
                "product_ref": ref,
                "category_ref": category,
                "display_name": text(name, name),
                "display_order": order,
                "image": image(stem),
                "recommended_product_refs": suggests,
            }
            for order, (ref, name, category, _, stem, suggests)
            in enumerate(brand["products"], start=1)
        ],
        "availability": availability,
    }


def restaurant_availability() -> None:
    """Give the original restaurant lane its own rows.

    `demo/menu.json` is hand-written and has no generator, but the lane it
    describes still has to withhold the other three. Only the `availability`
    array is touched.
    """
    target = ROOT / "demo" / "menu.json"
    dataset = json.loads(target.read_text(encoding="utf-8"))
    dataset["availability"] = rows_for("KIOSK")
    with target.open("w", encoding="utf-8") as handle:
        json.dump(dataset, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    print(f"{target.relative_to(ROOT)}  {len(dataset['availability'])} availability rows")


def main() -> None:
    known = set(LANES)
    for brand in (TIM, SBARRO):
        if brand["channel"] not in known:
            sys.exit(f"{brand['channel']} is not in the lane matrix")
    restaurant_availability()
    for brand in (TIM, SBARRO):
        dataset = build(brand)
        target = ROOT / "demo" / brand["slug"] / "menu.json"
        with target.open("w", encoding="utf-8") as handle:
            json.dump(dataset, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
        print(
            f"{target.relative_to(ROOT)}  "
            f"{len(dataset['products'])} products, "
            f"{len(dataset['categories'])} categories, "
            f"{len(dataset['availability'])} availability rows"
        )


if __name__ == "__main__":
    main()
