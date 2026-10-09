# ADR 018 — `UNKNOWN` is a first-class payment outcome

Status: accepted (milestone 6)

## Context

We send a charge request to a provider and the connection dies. The provider
may have charged the customer. It may not have. We cannot tell from here.

Two tempting behaviours are both wrong:

* **`UNKNOWN → FAILED`**: declares the customer was not charged. If they were,
  the sale is short and the customer has paid for nothing.
* **retry automatically**: if the first charge succeeded, the customer is
  charged twice.

The business rule that dominates every other consideration is: **never charge
twice**. Being unable to finish a sale is an inconvenience; double-charging is
a financial incident.

## Decision

`UNKNOWN` is a stored, durable attempt state, and while any attempt of a sale
is unresolved (`PROCESSING`, `UNKNOWN`, or `REVERSAL_PENDING`):

1. **The sale cannot be finalized** — `PAYMENT_REQUIRES_RECONCILIATION`.
2. **No new attempt may be started on that payment** — the only way forward is
   `ReconcilePayment`, which *queries* and never charges.
3. **No second payment may be started for the same tender and amount** while
   an unresolved attempt could already be covering it —
   `PAYMENT_REQUIRES_RECONCILIATION`. A different tender is allowed only once
   the unresolved payment is resolved, because until then the remaining
   balance is not knowable.
4. **The basket stays locked** (ADR 019).
5. Nothing resolves it on a timer. Resolution is an explicit operation,
   initiated by an operator or an orchestrator, and it is always a query.

`ReconcilePayment` is read-only against the provider: `query_payment` by
correlation id. It can resolve `UNKNOWN → AUTHORIZED` or `UNKNOWN → FAILED`,
and an `AUTHORIZED` result proceeds to `COMMITTED` through the ordinary path.
It never creates a payment, an attempt, or a charge.

Because `PROCESSING` is also unresolved (ADR 017), a crash between the provider
call and the result persistence lands in exactly the same place, with the same
single way out.

## Consequences

* A till can be blocked by an unresolved payment. That is intended: the money
  state is genuinely unknown and a human or an orchestrator has to resolve it.
* A future "force-fail after operator confirmation" operation is possible, but
  it must be an explicit, audited decision — not a timeout.
* `PAYMENT_UNKNOWN` and `PAYMENT_REQUIRES_RECONCILIATION` are stable codes a
  UI is expected to branch on.
