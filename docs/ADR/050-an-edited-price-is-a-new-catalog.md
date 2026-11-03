# ADR 050 — An edited price is a new catalog, and a restricted product is not a catalog fact at all

Status: accepted (showroom — PX Kiosk)
Extends ADR 044, ADR 047 and ADR 048. Changes no decision in any of them.

## Context

The showroom needed two things from an Admin screen: change a price, and mark
a product as one a customer cannot pay for unsupervised. They look like the
same feature — two editable fields on one row — and they are not the same kind
of thing at all. Getting them into the same file without blurring that is what
this ADR is about.

A price is **the core's**. A sale is priced by the active catalog snapshot and
nothing else; that is the invariant that makes "what did this sale cost"
answerable, and it is why a snapshot is immutable and content-hashed. So a
price typed into a screen and kept beside the screen is **cosmetic** — the
list would show one number and the till would charge another, which is the one
outcome worse than not having the feature.

"Somebody has to come over before this is paid for" is **not** the core's.
There is no commercial attribute it belongs to, no operation it parameterises,
and inventing one would put a demo's policy into the catalog contract.

## Decision

### One file, two kinds of thing, said out loud

`<data-dir>/overrides.json`, keyed by `product_ref`:

```json
{
  "T-LATTE":     { "price_minor": 5900 },
  "R-ARRACHERA": { "price_minor": 28900, "authorization_required": true }
}
```

They share a file because they share an editor — one Admin screen, one list of
products, one place an operator's changes survive a restart. They do not share
a destination:

* `price_minor` is applied to the dataset and **re-imported through the same
  `demo-seed` pipeline a release uses**, producing a new active snapshot. The
  number the till charges *is* this number, because there is no other number.
* `authorization_required` never leaves the host. The core is not told, and has
  no field to be told in.

### The order is the correctness

`admin_save_products` writes the file **first**, then rebuilds the catalog from
it. Reversed, a reseed would import the old prices. Skip the rebuild and a
number sits on a screen that no sale charges. If the rebuild fails, the file is
still right and the next attempt — or the next launch, since `ensure_demo`
passes the same path — applies it.

That last part is also what makes an edited price survive an **upgrade**: the
overrides travel with every seed, so a release that re-imports its dataset
carries them rather than quietly restoring the shipped prices.

### Batched, because the honest path costs something

Each price change is a catalog import and a version. One button for the whole
batch, so eight prices adjusted in one sitting is one import and one version
rather than eight of each. The screen reads the list back **from the core**
after it, never from what was typed: what an operator sees next is what the
till charges.

### Admin's product list is the core's menu

No second list of products, no cached copy, no "catalog for Admin". The list
is `GET_MENU` for the chosen channel with the overrides laid over it, and
`GET /demo/products` is the *same function* — so a floor app and the operator
standing at the machine cannot disagree about what is on the shelf.

`available` is `true` for every row, and that is not a stub: the list **is**
what the channel offers, because it is the channel's menu. A withheld product
is not on it.

## The authorization gate

Three rules, and each one is a decision rather than an implementation detail:

* **Scanning is never blocked.** A customer fills their basket normally and
  finds out at the till. Blocking the scan would be easier and would hide the
  behaviour being demonstrated.
* **An approval belongs to one sale and one set of products.** Keyed on the
  sale's own id and the refs it covered, so it cannot reach the next
  customer's basket, and a restricted product added to an approved basket asks
  again. Removing one of two leaves the rest approved.
* **Retail only.** The counter brands have somebody standing behind them. A
  restaurant lane that started refusing money because a stored flag mentioned
  one of its dishes would be a surprise nobody asked for.

Held in memory for the life of the process, deliberately: an approval that
outlived a restart is an approval nobody remembers giving.

This is **not** the core's authority model. Nothing here authorises a refund, a
discount, a safe drop or a session close — those are the core's and are decided
online (ADR 030, ADR 042). The PIN releases a screen, and the ADR 022 rule that
a close cannot carry an amount is untouched.

## The control endpoints, and why they are off by default

`GET /demo/status` has always answered anyone who could reach port 8777,
because it only ever told you how a kiosk is. `POST /demo/control/kiosk` and
`POST /demo/control/authorization` are the **first write path from the network
into this kiosk**: one closes it, the other releases a payment.

A four-digit PIN is thin protection to leave switched on in a customer's
building, so three gates stand in front of them, in the order that leaks least:

1. **off unless an operator turned it on**, which is what a kiosk ships as;
2. **a private address only** — loopback, the three private IPv4 ranges, IPv4
   link-local, IPv6 loopback and unique-local. A showroom's equipment is on a
   LAN; a routed address asking to close a kiosk is not the store app;
3. **then** the PIN. A kiosk with the feature off says so without revealing
   whether the PIN would have been right, and one with no PIN set refuses
   everything — an empty stored PIN is never satisfied by an empty offered one.

The reads stay open, as they always were. `remote_control` is published in
`/demo/status` so a floor app can tell "it refused my PIN" from "it is not
listening for me" without guessing from a `403`.

Every control answer is `success`, `error` and `request_id` — all three,
always, never a null — so a caller branches on one and prints the other.

## Consequences

* A price edit takes a second or two and raises the catalog version. That is
  the cost of the number being real, and it is visible on the button.
* An operator can price an experience the kiosk is **not** running: somebody
  setting up a stand prices all four, so the shelf on screen is its own state,
  separate from the lane in use.
* A price is parsed as text, never through a float, and strictly: interior
  whitespace is refused because `"5 9"` is somebody still typing, and a comma
  must be a well-formed thousands separator because `"59,5"` means two
  different amounts to two halves of the world. The first version of that
  parser turned a 59-peso coffee into a 595-peso one, which is why both rules
  have a test naming them.
* The PIN is stored in the clear beside the database, like the acceptor's
  credentials and for the same stated reason. It is not an operator identity
  and authorises nothing in the core.
