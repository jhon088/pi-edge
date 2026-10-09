# ADR 015 — Promotions engine V1

Status: accepted · 2026-10-08

## Context

Catalog, price books and tax are in place, and a sale resolves a deterministic
unit price. Promotions are the next layer, and they are the first thing in this
system that is **not** a property of a line: "spend 500, get 50 off" is a
statement about a basket, and "buy 2 get 1" is a statement about a quantity
across a line. A naive `line.discount = 20` would be unauditable, would make
re-evaluation order-dependent, and would put basket logic inside the
transaction aggregate.

## Where the engine lives

Evaluated before adding a crate, as instructed.

Promotions read products (applicability), resolved prices, quantities, store,
`effective_at` and the snapshot id. They are imported in the *same* chunked
snapshot, validated at the *same* commit, hashed in the *same* canonical
content, and they share the price books' lifecycle exactly: scope, priority,
validity, snapshot-bound.

A separate `edge-promotions` crate would depend heavily on `edge-catalog`
(products, price ids, snapshot counts, content hashing) with nothing flowing
back, and no consumer wants promotions without the catalog. That is a package
boundary without a conceptual boundary.

**Decision: promotions live in `edge-catalog`, as `promotions` and `basket`
modules.** Still pure Rust: no SQLite, no transport, no clock.

The trigger for splitting later is concrete: when promotions acquire data the
catalog does not carry — coupons, loyalty balances, customer segments — they
stop being a projection of the catalog snapshot and earn their own crate.

## The evaluation pipeline

```text
CatalogProduct  ──▶ price book ──▶ ResolvedPrice (unit_price, price_mode)
                                        │
                               PricedBasket (line → extended = unit_price × qty)
                                        │
                               PromotionEngine (pure, deterministic)
                                        │
                      line adjustments + basket adjustments
                                        │
                      allocation: every adjustment becomes a *line* discount
                                        │
                  edge-domain line math: quoted = extended − discount
                                        │
                      split by price_mode ──▶ net, tax, gross
                                        │
                                 transaction totals
```

The engine is a **pure function** of `(basket, promotions, context)`. It never
mutates a price. Nothing accumulates between evaluations: `AddItem`,
`RemoveItem` and `ChangeQuantity` rebuild the basket from the transaction's
current lines and re-run the whole evaluation, so `add → remove → add` lands
exactly where a single `add` would.

## Tax interaction

Reviewed against the existing math before implementing, because this is the
easiest thing in a POS to get quietly wrong.

What already exists, unchanged by this milestone:

```text
extended = unit_price × quantity            (half away from zero)
quoted   = extended − discount
TAX_EXCLUSIVE: net = quoted ;  tax = round(net × bps / 10 000) ;      gross = net + tax
TAX_INCLUSIVE: gross = quoted ; net = round(gross × 10 000 / (10 000 + bps)) ; tax = gross − net
line_total = gross
totals: subtotal = Σ extended · discount_total = Σ discount · net_total = Σ net
        tax_total = Σ tax · grand_total = net_total + tax_total ≡ Σ line_total
```

`net + tax == gross` holds **by construction** in both modes: the exclusive path
adds the tax it just computed, and the inclusive path derives the tax as a
subtraction from the gross. Rounding happens in exactly one helper.

**So promotions are only ever allowed to produce a per-line discount amount.**
They feed the `discount` term of the existing formula and nothing else:

* tax is therefore always computed on the **post-discount** amount, in both
  price modes, with no new arithmetic;
* `net + tax == gross` is preserved per line and in the totals, untouched;
* the rounding policy is not duplicated — a percentage discount rounds with the
  same half-away-from-zero helper, and the tax split keeps using it;
* a basket discount is **never** subtracted from the grand total. That would
  charge tax on an amount the customer did not pay, which is wrong in Mexico
  and destroys the base/tax relationship the receipt has to show.

### Allocating a basket discount

A basket-level discount is distributed across the eligible lines so each line
computes its own tax from its own discounted amount. This matters because lines
carry different rates and different price modes.

Deterministic, integer-only, exact:

1. weight of a line = its remaining base (`extended − discounts already on it`);
2. `share_i = floor(discount × weight_i / Σ weight)`;
3. the residual (`discount − Σ share_i`) is handed out one minor unit at a time
   by **largest fractional remainder**, ties broken by ascending `line_number`;
4. each share is capped at the line's remaining base.

The allocated shares therefore sum to exactly the basket discount, and the
result does not depend on map iteration, JSON order or SQLite row order.

### What a threshold is measured on

`BasketThreshold` compares against **the sum of line amounts after line-level
discounts, in the catalog's own quoted basis** (`Σ (extended − line discount)`).

Using the gross (tax-inclusive) total would be circular: the tax depends on the
discount, which would depend on the tax. For a tax-inclusive catalog — the
Mexican retail norm — the quoted basis *is* what the customer pays, so the rule
reads naturally on the sign in the window. The compared amount is reported in
the result so an operator can see what was evaluated.

## Stacking and priority

Deterministic order for every decision: **priority descending, then
`promotion_id` ascending**. Never JSON order, never row order, never map
iteration.

Per target — each line, and the basket — candidates are the applicable
promotions in that order, and then:

1. if the highest-ranked candidate is `EXCLUSIVE`, it applies **alone** and the
   rest are skipped for that target;
2. two or more `EXCLUSIVE` candidates tied at the top priority →
   **`AMBIGUOUS_PROMOTION`**. Failing beats choosing silently;
3. otherwise the `STACKABLE` candidates apply in order and accumulate; an
   `EXCLUSIVE` candidate encountered later cannot join a stack and is skipped.

Stackable promotions tied at the same priority are *not* ambiguous: they all
apply, so no choice is being made, and `promotion_id` still fixes their order.

## Adjustments are records, not mutations

Every discount is an explicit `PromotionAdjustment`: `adjustment_id`,
`promotion_id`, `promotion_code`, `target`, `amount`, `reason`. Nothing writes
`line.discount = 20` directly.

`adjustment_id` is **deterministic** — a composite of the promotion id and its
target, e.g. `PROMO-10PCT#line:2` — not a minted UUID. A re-evaluation of the
same basket produces byte-identical adjustments, which is what keeps goldens
stable and makes `add → remove → add` provably idempotent.

## Caps

* a discount never exceeds the eligible amount it applies to;
* a line's discounted amount is never negative;
* the two are enforced when the adjustment is produced *and* when it is
  allocated, so no combination of stacked promotions can drive a line below
  zero.

## Snapshot semantics

Promotions belong to the snapshot the sale captured, exactly like prices and
tax. A sale opened under snapshot A keeps A's prices, A's tax profiles **and
A's promotions** until it is finalized, even if B is activated mid-sale. A new
sale uses B. This is ADR 013 extended, not a new rule.

## What the transaction materializes

A historical sale has to stay interpretable after old snapshots are eventually
purged (purging is not implemented, and this ADR is what makes it safe later).
The transaction therefore stores, per applied promotion: `adjustment_id`,
`promotion_id`, `code`, `name`, `priority`, `stackability`, `target`, `amount`,
`reason` and the human `detail` ("2 application(s), 2 free unit(s)") — enough
to print and audit a receipt with no snapshot present.

It does **not** copy the promotion's rule definition. A rule is reconstructible
from the captured snapshot while it exists, and once it is gone the recorded
amount and reason are what an auditor actually needs.

## Import

Promotions travel in the chunked import, in the **same chunk as the product
they name**, for the same reason prices do: a promotion and its product have to
land in one database transaction, and the snapshot's foreign key enforces it. A
promotion that applies to any product has nothing to wait for and rides in the
first chunk.

They are part of the canonical content hash (a sixth section, ascending by
`promotion_id`) and of the declared counts, so a snapshot that discounts a
product is not the snapshot that sells it at list price, and a lost promotion
is a `CATALOG_COUNT_MISMATCH` rather than a silently cheaper basket. The rule
is stored in typed columns with a `CHECK` per rule kind, not as a JSON blob, so
SQLite refuses a malformed rule at write time.

## Consequences

* A line is no longer immutable once priced: its discount changes when the
  basket changes. The line's own arithmetic is always recomputed as a whole, so
  it can never be left half-updated.
* `line_number` is never reused or renumbered. Removing a line leaves a gap, and
  the next line continues from the highest number ever used, so an
  `adjustment_id` always points at the same line.
* The journal records one commercial summary at `TRANSACTION_TOTALIZED`
  (applied promotions, total discount, totals) rather than an event per
  evaluation. The journal is an audit trail, not tracing.
* V1 ships four rule kinds. Applicability is product-or-any today; a
  category/group variant is one enum variant plus one snapshot table, and is
  deliberately not written until there is a group to point at.
* Explicitly out of scope and not stubbed: coupons, loyalty, customer pricing,
  tender- and payment-method promotions.
