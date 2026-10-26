# Replacing the demo artwork

The dataset references each image **by stem**, never by filename or hash:

```json
{ "content_hash": "file:burger-supreme", "media_type": "image/jpeg", "byte_size": 0 }
```

`edge-cli demo-seed` resolves that stem to whichever file in this folder
carries it, hashes the bytes, imports them into the local asset store and
rewrites the catalog with the real `sha256:` reference.

So replacing a photograph is one step:

```bash
cp ~/shoot/burger.jpg demo/assets/burger-supreme.jpg   # keep the stem
edge-cli --db ./data/pi-edge.db demo-seed              # re-seed
```

No hash to edit. No JSON to touch.

## Where each stem appears

One file can serve several places; the content hash is the same, so it is
stored once however many screens show it.

| Stem | Appears in |
| --- | --- |
| `burger-supreme` | Smash Suprema · Hamburguesas tile · **attract hero** |
| `burger-italian` | Italian Burger |
| `pulled-pork` | Pulled Pork |
| `combo-chef` | Combo del Chef · Combos tile |
| `fries` | Papas Fritas · Para acompañar tile · combo side option |
| `onion-rings` | Aros de Cebolla · combo side option |
| `coca-clasica` | Coca-Cola Clásica · combo drink option |
| `limonada` | Limonada · Bebidas y Postres tile |
| `helado-chocolate` | Helado de Chocolate · combo drink option |
| `nuggets` | Nuggets x6 |
| `extra-cheese` | "Queso extra", on all four burgers and the combo |
| `salsa-mayo` | "Salsa mayo hierbas", on the three burgers |
| `extra-carne` | "Carne extra", on the combo |

## What the configurator shows and what it does not

A group is **all** photo cards or **all** text tiles, never half of each —
a grid where some wells are empty reads as a bug rather than a design. A test
enforces it across every group on every dish.

Photographed: the combo's side and drink, and the paid extras.
Textual: sizes (Sencilla / Doble carne, Regular / Grande) and every removal
(`sin cebolla`, `sin pepinillo`, `sin salsa`). There is no photograph of an
absence, and a tile is the right control for one.

## An unsupplied photograph is not an error

Naming a stem with no file behind it does not fail the seed: that image
becomes `null`, the row renders as a tile, and `demo-seed` lists the stem
under `photographs_wanted`. That is what lets the dataset ask for a picture
before anyone has shot it. It currently reports an empty list.

## Shooting notes

Square framing, 1000×1000 or larger, subject centred with even margins.
JPEG, sRGB. Three of the current option photographs (`extra-cheese`,
`extra-carne`, `salsa-mayo`) are around 130px square because that is the size
they were supplied at — they are sharp enough for the small option cards but
will soften on a large kiosk panel, so they are the first worth re-shooting.
