# ADR 016 — Payment and PaymentAttempt are two concepts

Status: accepted (milestone 6)

## Context

Until M5, a payment was a value object with two states (`CREATED`,
`COMMITTED`) created and committed in the same call, because the only tender
was cash and cash is instantaneous. Card, wallets and terminals are not: a
single commercial payment can involve several interactions with a provider,
any of which may fail, time out, or leave us not knowing what happened.

Overwriting one `status` field would destroy exactly the history an operator
and an auditor need: *what did we ask, how many times, and what came back*.

## Decision

Two concepts, in a new pure crate `edge-payments`:

**`Payment`** — the commercial intention to cover part of the amount due with
one tender. It owns the money that is applied to the sale.

```
payment_id, transaction_id, tender_type, provider_kind?,
requested_amount, authorized_amount?, committed_amount,
status, created_at, updated_at
```

**`PaymentAttempt`** — one concrete interaction with a payment provider.
Append-only: a new attempt never rewrites an older one.

```
attempt_id, payment_id, attempt_number, status,
requested_amount, authorized_amount?,
provider_kind, provider_reference?, correlation_id,
failure_code?, created_at, updated_at
```

### Three amounts, deliberately distinct

| Amount | Meaning |
| --- | --- |
| `requested_amount` | what this tender was asked to cover — for cash, what the customer handed over |
| `authorized_amount` | what the provider actually authorized (card only) |
| `committed_amount` | what is applied against the sale's debt |

For cash, `committed = min(requested, remaining_balance)` and the difference is
change. For card, `committed = authorized` and the engine refuses a request
larger than the remaining balance, so a card can never create change.

### Cash has no attempts

An attempt models an interaction with a provider. Cash has no provider: the
drawer movement is physical and immediate. So a cash `Payment` carries **zero**
attempts and goes `CREATED → COMMITTED` in one database transaction.

This is not a parallel code path. Cash and card share the same aggregate, the
same state machine, the same settlement arithmetic, the same persistence and
the same protocol operation. What differs is how many provider interactions
each needs — which is the honest difference between them. Inventing a
synthetic "attempt" for cash would add a row that records nothing.

## Consequences

* `edge_domain::payment` no longer defines `Payment`, `PaymentStatus` or
  `TenderType`; those move to `edge-payments`. The transaction aggregate stops
  owning payments (ADR 020).
* Attempt history is queryable and survives restarts, which is what makes
  `UNKNOWN` resolvable (ADR 018).
* A provider reference is unique per provider, so the same provider operation
  can never be recorded twice under two attempts.
