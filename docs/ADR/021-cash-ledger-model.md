# ADR 021 — The cash ledger

Status: accepted (milestone 7)

## Context

With real cash payments in place (ADR 016), two questions start to diverge:

* *how was this sale covered?* — the payment engine answers it,
* *how much physical money should be in this drawer, and why?* — nothing
  answered it.

`RegisterSession.opening_cash` was the only cash fact the core knew. There was
no record of a drawer movement, so "expected cash" could not be computed, let
alone audited.

## Decision

An **append-only ledger of cash movements**, scoped to a register session.

```
CashMovement {
    movement_id, business_day_id, session_id, register_id, operator_id,
    movement_type, direction, amount,
    tendered?, change?,          // CASH_SALE audit, see below
    source_ref?, reason_code?, approval_id?,
    created_at
}
```

V1 types: `OPENING_FLOAT`, `CASH_SALE`, `CASH_IN`, `CASH_OUT`, `SAFE_DROP`.

`CASH_REFUND` and a manual cash adjustment are deliberately absent. A movement
type with no operation, no authorisation policy and no workflow behind it is a
promise, not a model: it would show up in every `match`, every `CHECK` and
every report as a capability that does not exist. Each arrives with its own
use case; adding one is an enum variant, a `CHECK` value and the operation that
produces it.

**Nothing is ever updated or deleted.** A correction is a new, explicit,
compensating movement. The database enforces it: there is no `UPDATE` or
`DELETE` statement against `cash_movements` anywhere in the adapter, and a
trigger refuses both.

### Direction is explicit, never a sign

`Money` stays non-negative everywhere in PI.Edge, so a movement carries a
`direction` of `IN` or `OUT`. Reading a drawer's history must not depend on
noticing a minus sign, and a negative `Money` would be a second way to say the
same thing.

| Type | Direction |
| --- | --- |
| `OPENING_FLOAT` | `IN` |
| `CASH_SALE` | `IN` |
| `CASH_IN` | `IN` |
| `CASH_OUT` | `OUT` |
| `SAFE_DROP` | `OUT` |

Each type pins exactly one direction, and the pairing is checked by the domain
and again by a `CHECK` constraint.

## A cash sale with change

A customer hands over 200.00 for a 116.00 sale and takes 84.00 back. Three
models were considered:

1. **One net movement of +116.00.** Expected cash is right, but the ledger
   forgets that 200.00 came in and 84.00 went out.
2. **Two movements: +200.00 and −84.00.** Auditable, but it needs a seventh
   movement type for the change, and it duplicates what the payment already
   records — with two rows to keep consistent instead of one.
3. **One `CASH_SALE` movement whose amount is the net that stays in the
   drawer, carrying `tendered` and `change` as audit fields.** *(chosen)*

So the row reads `amount = 116.00, tendered = 200.00, change = 84.00`, with the
invariant `amount = tendered − change` enforced in the domain and by a `CHECK`.

Why: expected cash is correct *by construction* rather than by summing a pair
of rows that could drift; there is exactly one movement per payment, which
makes idempotency a `UNIQUE` index on `source_ref` rather than a procedure; and
the tender/change audit lives on the same line of the ledger an auditor is
already reading.

## Expected cash is derived, never stored

```
expected_cash = Σ(IN) − Σ(OUT)
```

over the session's movements. There is no mutable `expected_cash` column: the
movements are the source of truth, exactly as the payment tables are the source
of truth for a sale's settlement (ADR 020).

The one place it is materialised is a session close (ADR 022), where the figure
*used at that moment* is written down so a historical variance is never
recomputed under future rules.

## A cash payment produces exactly one movement

When a `Payment` with tender `CASH` reaches `COMMITTED`, a `CASH_SALE` movement
is written **in the same database transaction**. `source_ref` is the
`payment_id`, and `UNIQUE (session_id, movement_type, source_ref)` makes a
replayed `StartPayment` physically unable to move the drawer twice.

A card payment produces no movement. A card reversal produces none either:
nothing left the drawer, so nothing returns to it.

## Consequences

* `opening_cash` on the session stays as the opening declaration, and is also
  written as the session's first `OPENING_FLOAT` movement, so the ledger alone
  explains the whole drawer.
* Cash reversal is out of scope: undoing a drawer movement is a physical act,
  and the compensating movement it needs must be an audited operator decision
  with its own authorisation policy — not an automatic consequence of a payment
  state change, and not a movement type sitting unused until then.
