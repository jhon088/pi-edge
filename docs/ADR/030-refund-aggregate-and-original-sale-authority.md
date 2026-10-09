# ADR 030 — The refund document, and who says a sale may be refunded

Status: accepted (milestone 9)

## Context

A refund moves goods, money and a tax consequence, and the three do not settle
at the same moment. Modelling it as a correction of the sale would make the
sale mutable; modelling it as a payment reversal would collapse two different
commercial facts (ADR 034). So a refund is **its own document**, with its own
lifecycle, lines, authority and audit.

## Decision — the aggregate

```
Refund {
    refund_id, original_transaction_id,
    store_id, business_day_id, session_id, register_id, operator_id,
    status, authority, authorization_ref,
    lines[], total{extended, discount, net, tax, gross},
    fiscal_status, original_fiscal_state,
    reason_code, restock,
    created_at, updated_at, accepted_at?, completed_at?, version
}
```

The day, session and register are the **refund's**, not the sale's: a refund is
money and goods moving today, and the drawer it comes out of is today's drawer.
The link to the sale is `original_transaction_id`, and the link to the goods is
each line's `original_line_number` — never reused, so it always points at the
same goods (ADR 005).

`restock` exists as a value, set to `true` in V1 and never anything else. It is
one column rather than a disposition engine: when a non-restockable disposition
has a real requirement, it becomes another value, not a schema change
(ADR 033).

## Decision — authority over the original sale

**A locally finalized sale is not a refundable sale.** The product rule is that
a production refund needs central authority over the original: PI.COMMERCE
Server validates it and returns a decision. PI.Edge must not quietly treat its
own `FINALIZED` row as that decision.

So the domain receives an **already-resolved** authorisation and asks no
questions about where it came from:

```rust
RefundAuthorizationContext {
    original_transaction_id,
    authority: CENTRAL_ACCEPTED,
    authorization_ref: Option<String>,
}
```

One authority value, because one exists. `CREATE_REFUND` has **no authority
field on the wire**: a client cannot assert it, and a contract test pins that
the payload has no way to.

Where it is resolved: the **composition root**, next to the payment simulator
and for the same reason. `PI_EDGE_REFUND_AUTHORITY=dev-accept-local` turns on a
development stand-in that marks every context it produces with `DEV-LOCAL:`.
**With no switch, every refund is refused** with `REFUND_AUTHORITY_REQUIRED` —
the production rule preserved by default rather than weakened by one.

There is no `RefundServerPort`. A trait with one development implementation and
no server to talk to would be an abstraction waiting for a boundary. When M11
arrives, the resolver in the composition root makes a call instead of reading an
environment variable, and nothing downstream changes.

## Decision — one refund at a time per sale

Refundable quantity is `sold − already spoken for`, where "spoken for" includes
**non-terminal** refunds as well as completed ones. A refund therefore reserves
what it names simply by existing, and the reservation is enforced by a partial
unique index:

```sql
CREATE UNIQUE INDEX ux_refunds_in_progress
    ON refunds (original_transaction_id)
    WHERE status NOT IN ('COMPLETED', 'FAILED');
```

Validation and reservation happen in **one** database transaction, so two tills
cannot both pass a check and both write. A `COMPLETED` refund blocks nothing: a
later refund may take whatever quantity remains. A replay by `request_id`
returns the first refund and is not a second one.

The V1 rule — *one refund in progress per original sale* — is a simplification,
and a deliberate one: it makes the race impossible with an index rather than
with a lock, and nothing in retail needs two simultaneous returns of the same
ticket.

## Consequences

* A refund survives a crash as `CREATED` with its quantities still reserved; a
  crash before `COMMIT` leaves no refund and no reservation at all.
* `REFUND_AUTHORITY_REQUIRED` is the answer a till gets today unless it was
  explicitly configured otherwise. That is correct, not a gap.
* Reading what *could* be refunded needs no authority. Only refunding does.
