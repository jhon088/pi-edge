# ADR 022 — Blind close

Status: accepted (milestone 7)

## Context

If a cashier can see what the drawer *should* contain before declaring what it
*does* contain, the count stops being evidence. Any shortage can be made to
disappear by typing the expected figure.

Hiding the number in the UI is not a control. A diagnostic screen, a CLI, a
future integration or a mistake in a React component would all expose it again,
and nothing in the protocol would have been violated.

## Decision

**The blind close is enforced by the contract, not by the interface.**

Two operations, two DTOs, and the expected figure exists in only one of them:

```
RequestSessionClose  -> SessionCloseRequirementsV1
                        { session_id, currency, movement_count,
                          blockers[], requires_declaration: true }
                        ← no expected cash, no totals, no movement amounts

DeclareClosingCash   -> SessionCloseResultV1
   { counted_amount }   { declared, expected_at_close, variance, status }
                        ← the expected figure appears only after the
                          declaration is durable
```

`SessionCloseRequirementsV1` has no field that could carry an amount: not
`expected_cash`, not a movement list, not a running total. A serialization test
asserts that the rendered JSON of a session with a known non-zero expected
figure contains neither the field name nor the amount, so the leak cannot be
reintroduced by adding a field later.

The ordering is enforced too: `DeclareClosingCash` is refused unless the
session is already `CLOSE_REQUESTED`, so the declaration can never be a reply
to a request that also answered it.

## Variance

```
variance = counted − expected_at_close
```

Negative is a shortage, positive an overage. Both are recorded; neither is
corrected automatically, and neither blocks the close. A close that silently
"fixed" a shortage would destroy the only evidence that it happened.

`expected_at_close` is **materialised** on the declaration. Expected cash is
normally derived from the ledger (ADR 021), but a historical variance must stay
exactly what it was computed against: a future movement type, a corrected
rounding rule or a compensating entry must not be able to rewrite yesterday's
variance.

## Declarations are not editable

One declaration per close attempt, enforced by a unique index. Changing a
declared figure is not an update: it is a new close attempt after a reopen
(ADR 023), which leaves the first declaration visible forever.

## Consequences

* A future administrative query may expose expected cash before the count —
  that is a different operation for a different actor, and it will not be the
  one a cashier's close path calls.
* The requirements DTO is deliberately thin. If a client needs richer
  pre-close context, it gets a new DTO with an explicit audience, not a field
  bolted onto this one.
