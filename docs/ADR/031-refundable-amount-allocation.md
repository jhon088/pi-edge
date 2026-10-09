# ADR 031 — What a refund is worth

Status: accepted (milestone 9)

## Context

A customer returns one unit of a line that was sold three-for-the-price-of-two.
What is that unit worth?

Getting this wrong in either direction is a real loss. Give back the shelf
price and the store pays out more than it took — **refund arbitrage**, and it
scales: buy three, return one, repeat. Give back nothing because "the third one
was free" and the customer is shorted, on the arbitrary basis of which unit the
refund happened to name.

## Decision — the sale already answered

A refund **consumes the materialised economics of the original line** and
re-prices nothing. Not the current price, not the current promotion, not the
current tax profile, not the current snapshot. A `TransactionLine` already
carries everything:

```
quantity, extended_amount, discount_amount, taxable_amount, tax_amount, line_total
```

Because basket discounts are allocated onto the lines *before* tax (frozen in
milestone 5), every line's economics are self-contained. The promotion is
already in `discount_amount`, and `line_total` is what the customer paid for
that line.

So there is no "which unit was free" question to answer. Buy-2-get-1 on three
units of 100.00 at 16% is a line of `net 200.00 + tax 32.00 = 232.00`, and one
unit is worth a third of that.

**The promotion engine is never re-run for a refund.** It is a function of
`(basket, promotions, context)` and the refund has no basket, no snapshot and
no context — only a sale that already closed.

## Decision — slice against the remainder, not the original

Each component is prorated against **what is left**, not against the original:

```
remaining  = original − already refunded        (per component)
share      = remaining.prorate(slice, remaining_quantity)
```

`Money::prorate(part, whole)` returns the amount **unchanged** when
`part == whole`, with no rounding step at all. That one property is what makes
a sequence of partial refunds exact: the refund that takes the last units takes
precisely what is left.

Three single-unit refunds of that 232.00 line give back `77.34 + 77.34 + 77.32`
— 232.00 to the cent, in any order, for any split. A property test walks every
split of several awkward totals and asserts it.

`net` and `tax` each take their own share and each rounds once; `gross` is then
`net + tax` rather than a third independent rounding. Two roundings of the same
amount is how a cent goes missing, and `net + tax == gross` is an invariant a
client can rely on.

Rounding is the core's single policy, half away from zero. There is no second
one.

## Decision — one allocation policy, shared

Spreading an amount over weighted shares now has two real consumers: a basket
discount over its lines, and a refund over the original tenders. So the
largest-remainder-with-caps allocator moved from `edge-catalog` into
`edge_domain::money::allocate_proportionally`, where the rounding policy
already lives, and both call it.

This touched milestone 5. **The gate was that every promotional golden stays
byte-identical**, and it did: the suite passed against the recorded fixtures
with no regeneration, twice.

## Decision — the tender spread

A refund goes back **pro-rata over what each original tender actually applied,
capped at what it has left to give**. One policy, deterministic, no operator
input:

```
sale:   300.00 cash + 860.00 card  (1160.00)
refund: 580.00  ->  150.00 cash + 430.00 card
```

The cap is what makes it safe: a tender never returns more than it took, and a
tender that earlier refunds already drained contributes nothing more. A refund
the tenders cannot cover is refused with `REFUND_TENDER_EXCEEDED` rather than
silently paid out of the wrong tender.

`committed_amount` is the ceiling, never `requested_amount`: a customer who
handed over 200.00 for a 116.00 cash sale is owed 116.00 back, not 200.00. The
change already went home with them.

Operator-chosen allocation is **not** built. When a policy or a UX needs it, the
command grows an optional explicit distribution validated against the same
caps. Until then there is exactly one canonical answer.

## Consequences

* A refund is correct after the catalog that priced the sale is gone.
* `Σ(partial refunds of a line) == line_total` exactly, always.
* No refund can return more economic value than the sale captured.
* Eligibility is a core query: a client never computes a refundable figure.
