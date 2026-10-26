# ADR 046 — A sale line can be a configured product, and the kiosk is a client

Status: accepted (milestone 13C-lite / 14-lite / 15 — the Expo MVP)
Extends ADR 044 and ADR 045. Changes no decision in either.

## Context

Milestones 13A and 13B gave the catalog the questions a product asks and the
bytes behind its photographs. What was still missing was the other half: a
*sale line* that represents "Smash Clásica, doble carne, tocino ×2, sin
cebolla" — and a screen a customer can actually touch.

This milestone is explicitly an **MVP for a trade show**, not the enterprise
build. Where a simple answer is defensible it is taken, and said so out loud.

## Decision — a configured line is an ordinary line with its choices attached

```text
TransactionLine
    base_unit_price      what the product costs before any choice
    selections[]         the choices, materialised
    unit_price           base + every chargeable choice  ← the configured price
    quantity, tax, discount, totals … unchanged
```

`unit_price` **is** the configured unit price. There is no second money field
holding the same number, because two fields for one fact drift. The wire
exposes it twice — `base_unit_price` and `configured_unit_price` — purely so a
screen never has to add anything up, and both come from the one stored amount.

Everything downstream is untouched: `extended = unit_price × quantity`, the
tax split, the promotion engine, the totals, the payments, the receipt and the
refund arithmetic all work on a configured line without knowing it is one.
That is the whole point of putting the configuration *inside* the unit price.

**The domain checks the sum.** A line whose base plus chargeable choices is not
the price being charged is refused (`ConfiguredPriceMismatch`), not stored. The
materialised amounts are what a receipt and a refund will read years from now,
so a bug anywhere above the domain cannot persist a sale that lies about its
own price.

## Decision — selections are materialised, like promotions

Each one copies the option's id, its **group's and its own visible name**, the
quantity, the component it billed through, the unit price and the amount. Not
referenced — copied — so the line still reads correctly when the snapshot that
offered the option is long gone (the rule ADR 015 set for promotions).

`component_ref: None` is a semantic choice: *no onions* is not a product, is
not priced, and is still recorded. A kiosk has to show it, and a kitchen has
to read it.

## Decision — the client sends ids, the core sends money

`AddItem` gained `selections: [{option_id, quantity}]` and a
`preparation_note`. That is the entire vocabulary a client has. There is no
field anywhere in the protocol through which a screen could state what a
choice costs, what the line costs, or what the discount is — a chargeable
option is priced by resolving its `component_ref` through the **existing**
price books, at the instant the sale captured (ADR 013).

The core also enforces the questions: an option this product does not offer, a
quantity above what the option allows, a group total outside its range, a
required question left unanswered, and a nested question whose owning answer
was not chosen are all refused with their own stable codes. A kiosk screen
that gets this wrong is told why; it cannot talk its way past it.

## Decision — two reads are the whole kiosk API

`GET_MENU` and `GET_PRODUCT_CONFIGURATION`, both derived from the snapshot in
force. One call draws a home screen; one call draws a configuration screen.
A round trip per product would make a touch feel slow and buy nothing.

Everything else the kiosk needs already existed: start a sale, add, remove,
change quantity, totalize, take a payment, finalize, cancel, read the receipt.
No kiosk-specific transaction, no kiosk-specific payment, no kiosk-specific
promotion.

`READ_ASSET` is the read half of ADR 045: chunked, by content hash, never a
path. The Tauri bridge pulls an image once and hands the webview a `data:`
URL, so a photograph reaches the screen with no HTTP of any kind.

## Decision — the cart's running total is still the core's

`preview_totals()` is the same arithmetic `totalize` uses with no state
change. A kiosk cart shows a live total while the customer is still choosing,
and that number is still computed by the core — a screen that summed its own
lines would be the first crack in ADR 002.

`totals` stays the official, totalized figure; `running_totals` is what the
cart shows. They agree the moment the sale is totalized.

## Decision — the order number is derived, not stored

`K01-0042`, from the register id and the sale's own `register_sequence`, both
already durable. No counter to keep, no gap to explain, nothing to recover
after a crash. It is a demo-grade order number and deliberately not a fiscal
folio.

## Deliberate MVP simplifications

Each of these is a decision, not an oversight, and each has a known cost:

| Simplification | Why it is acceptable now | What it costs later |
| --- | --- | --- |
| A promotion applies to the **configured** line total | Predictable, visible and already correct arithmetic | BASE_ONLY vs CONFIGURED_TOTAL is a real product decision and will need a per-promotion flag |
| Dine-in / take-away lives in the screen | The core has no fulfillment concept and inventing one here would be the platform redesign this milestone is not | It is not on the sale, the journal or the receipt |
| Payment is the deterministic simulator, opt-in and named | There is no real provider yet, and the simulator is the production path for every earlier milestone's tests | No real terminal, no UNKNOWN-outcome UX |
| Channel availability is advisory | The device profile still has no `channel_ref`, which ADR 044 deferred on purpose | A crafted request could add a withheld option; the screens never offer one |
| The demo artwork is generated, not photographed | No network at build time, and the pipeline is what matters | Replace the files in `demo/assets/` and re-run the seed; the dataset references them by name, not by hash |
| One language on screen | The catalog carries `es` + `en` and resolves correctly; the screens are Spanish | A locale switch is UI work, not core work |

## Decision — a list in the contract is always a list

`selections` and `preparation_note` are serialized whether or not they carry
anything. They were originally skipped when empty, which cost a working cart
screen: a product that asks no questions arrived with no `selections` field at
all, the screen read `.length` on `undefined`, the render threw, and React
unmounted the tree — a black screen with no message.

The general rule, now pinned by a test: **an optional value is `null` and an
empty list is `[]`; neither disappears.** A field that vanishes when it is
empty is a field every client has to defend against, and the first one that
forgets gets nothing to read and no error worth the name.

For the same reason the kiosk renders inside an error boundary that *shows*
what failed and offers a way back. A screen that goes black tells nobody
anything.

## Decision — cash and card are one sale, asked for on their own screen

The kiosk asks *how* before it asks *for* the money: a full screen with two
large choices, not a small selector. Both go through the payment engine every
earlier milestone already uses — `START_PAYMENT` with the tender the customer
chose, then `FINALIZE` — so cash needed no second lifecycle, no second
reconciliation path and no second receipt. The tender is recorded on the
sale's payments, which is where "how was this paid" has always lived.

Cash is simulated for the Expo: a short note-feed animation, then approved.
There is no recycler, and the amount is the core's own grand total.

## Decision — what to recommend is catalog content, not a screen's rule

*Amends the earlier decision on this page, which derived suggestions from
categories the cart had not covered.*

That rule was cheap and wrong in a way that only shows up on a real menu: it
could offer a drink to someone buying a drink of another kind, and it had no
way to express "this burger sells fries" because the catalog had nowhere to
say it. Merchandising is a commercial decision, and a commercial decision
belongs in the thing the merchant edits.

So `ProductPresentation` gained `recommended_product_refs`: an ordered list of
products to offer alongside this one, declared per product, travelling in the
snapshot, hashed as content. Order is part of it — what a screen offers first
is the decision, not an implementation detail.

It is **presentation, not a rule and not a price**. Nothing in the list
changes a total, nothing is required to honour it, and a screen that ignores
it still sells correctly. It is also deliberately universal: a burger
recommending fries and a printer recommending toner are the same row, and
nothing in the core knows which is which.

The core does one thing with it beyond storing it — it drops any ref the same
menu does not show, so a withheld, unsaleable or unbrowsable product can never
reach a screen as a suggestion. What is left for a client is presentation:
skip what is already in the basket, keep the declared order, stop at four.

This is still not a recommendation engine. There is no scoring, no history and
no model; there is a list someone wrote down. An engine is a product decision
with data behind it, and inventing one here would be the platform redesign
this milestone is not.

Two surfaces use it. After a configurable dish is added, **"Completa tu
pedido"** offers what that dish recommends — once, never blocking, with "No,
gracias" as large as the suggestions. In the cart, the rail offers what
everything in the basket recommends, minus the basket. A suggestion that needs
configuring opens the product screen and comes **back to the cart**.

## Decision — an option that is a dish is shown as one

`SelectionOption` already carried an `image`; nothing used it. The combo's
side and drink choices *are* products the menu sells, so they now show those
products' photographs and lay out as cards instead of a list. Options with
nothing to show — an extra, a removal — stay as clean tiles rather than
getting an invented placeholder.

The rule is in the data, not the screen: a group whose options carry images
renders visually, and one whose options do not renders as a list.

## Consequences

* Migration 0013 is purely additive: two defaulted columns on
  `transaction_lines` plus `transaction_line_selections`. Existing rows are
  backfilled so an unconfigured line's base price is the price it charged.
* The golden fixtures gained `base_unit_price`, `configured_unit_price`,
  `running_totals` and `order_code`. **No existing money value moved**, and on
  every retail line base = configured = unit price.
* A product that asks a required question can no longer be sold by a bare
  scan. That is correct — and it means a retail catalog must not give a
  scannable product a required group unless a counter UI can answer it.
* The kiosk screens are a *client*. They hold which screen is showing and what
  has been tapped but not yet sent. The sale lives in the core, so navigation
  cannot lose it and a reload picks it back up.
* Migration 0014 adds `catalog_product_recommendations`, additive and empty
  for every snapshot that predates it. The content hash gained a thirteenth
  section that **contributes nothing when no product recommends anything**, so
  a snapshot already activated in a store keeps the hash it was activated
  with. A test pins both halves of that.
* The Tauri window no longer resolves its database from the process working
  directory. `pnpm tauri dev` runs the binary from `apps/tauri-pos/src-tauri`,
  so a relative default pointed the window at
  `apps/tauri-pos/src-tauri/data/.pi-edge/d.sock` while the daemon listened on
  `<repo>/data/.pi-edge/d.sock` — every command failed against a socket
  nothing had created, and the workaround in use was a committed symlink. The
  default is now anchored: the compiled-in checkout in a debug build, the
  executable's own directory in a release one. `PI_EDGE_DB` still wins.
* The window's CSP had `default-src 'self'` and no `img-src`, which blocked
  every product photograph: they arrive as `data:` URLs read out of the local
  asset store. `img-src 'self' data:` is now explicit.
