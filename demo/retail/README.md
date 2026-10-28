# The retail lane's dataset

The second vertical: a supermarket self-checkout, in the same installation and
the same catalog as the restaurant kiosk (ADR 047).

## It is additive

This file carries **only** the retail lane's own rows. `edge-cli demo-seed`
reads `demo/menu.json`, appends every list section it finds here, and imports
**one snapshot**:

```text
demo/
  menu.json          the restaurant
  assets/            its photographs
  retail/
    menu.json        ← this lane: channels, categories, products,
    assets/            presentation, availability
```

Delete `demo/retail/` and the restaurant seeds alone, unchanged. Nothing fails.

A store may have exactly one active catalog, so the two lanes are told apart by
**channel availability**, not by swapping catalogs:

| Channel | Shows |
| --- | --- |
| `KIOSK` | the restaurant's four categories |
| `KIOSK-RETAIL` | the eight retail aisles |
| `COUNTER` | everything the shop sells |

Both lanes default to `OFFERED` and withhold the other's categories explicitly.
That is load-bearing: availability resolves product **and** category, so a
product with no row of its own falls back to the channel default — a channel
that defaults to `WITHHELD` shows nothing at all, however many of its categories
are offered.

## One table generates the shelf

`scripts/retail-dataset.py` holds every product — name, SKU, barcode, price,
aisle and photograph — and writes three things from it, so they cannot drift
apart:

```text
demo/retail/assets/        the photograph each product is sold by
demo/retail/menu.json      the dataset `demo-seed` imports
demo/retail/barcodes.csv   the same rows, for printing test labels
```

Change the table, run the script, re-seed. Anything no longer on the shelf is
deleted from `assets/`, so the installer carries the lane as it is rather than
as it used to be.

### Photographs

Eighteen of the twenty-two products carry **real photographs**, prepared from
`examples/fotos ret/`. Products are named after the photograph rather than the
other way round: a card whose picture does not match its label is worse than a
card with a drawing on it.

`examples/` is source material and nothing else. It is read when preparing an
asset and never at runtime, and `the_dataset_references_nothing_outside_the_installation`
fails on any dataset that mentions it, or any URL or absolute path.

The sources are square packshots on white, so preparing one is a re-encode and
nothing more. `erase` blanks a rectangle of a source that carries something a
product card should not — the bananas arrive with a "Venta por penca" shelf
sign beside them, and somebody else's signage in a card reads as a price.

The remaining four — `leche`, `yogurt`, `cafe`, `papel` — are **placeholders**:
generic packaging rendered by `scripts/retail-packshots.py`, with no wordmark
on the artwork (the kiosk prints the name under the card). Each one goes the
moment a photograph of that product arrives: drop it in `examples/fotos ret/`,
name it in the table, and re-run.

`every_retail_product_has_real_asset` checks what actually reached the store:
the bytes are there, they decode, they are at least 600×600, and they are
square enough not to letterbox in the grid.

### Barcodes

`750` (Mexico's GS1 prefix), the product's index padded to nine digits, and an
EAN-13 check digit:

```text
750 000000001 5     R-COCA-3L
750 000000002 2     R-POWERADE
750 000000003 9     R-BELIGHT
```

They are real EAN-13s, which means **a scanner accepts them**: print
`barcodes.csv` and the whole shelf can be scanned by hand. They can also be
typed into the lane's keypad where there is no reader.

A wrong check digit is the failure that hides — the reader simply never emits
the code, nothing reaches a log, and the demo looks broken. So
`every_demo_barcode_is_a_valid_ean13_and_belongs_to_one_product` recomputes
all of them, and `scanning_a_demo_barcode_resolves_the_product_it_was_printed_for`
puts every one through the core.

## The attract screen's hero

`apps/tauri-pos/src/kiosk/hero-retail.jpg`, composed by
`scripts/retail-hero.py` from six of the photographs in `examples/fotos ret/` —
fruit, bakery, a drink — cut out and stacked on a mint sweep. It is the lane's
own artwork rather than catalog content, so it ships with the frontend and
needs no asset store round trip before the first frame.

Re-run the script after changing which photographs it uses; `CAST` at the top
says what goes where, and `erase` blanks anything a photograph carries that a
hero should not (one of them arrives with a shelf price sign on it).

## What the lane does not have

No configurator, and no product here asks a question: a scan has to be able to
add anything on the shelf in one move, and a test asserts that nothing in the
retail menu is `configurable`. No recommendations either — an upsell screen
between a scan and the next scan would be in the customer's way.
