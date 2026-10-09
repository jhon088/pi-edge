# ADR 019 — Basket freeze after payment start, and reversal semantics

Status: accepted (milestone 6; the basket half was frozen at the end of M5)

## Context

Once money is in motion, the thing the money is paying for must stop moving.
Otherwise a cashier can authorize 700.00 for a basket, add an item, and
finalize a 900.00 sale against a 700.00 authorization.

Separately, a payment that reached a provider sometimes must not remain
applied — the customer changes their mind about the tender, or the operator
started the wrong one. That is a *reversal*, and it is not a refund.

## Decision — basket freeze

The basket is mutable only while **no payment is in a financially active
state**. Active means any payment that is not terminal (`FAILED`, `REVERSED`),
or equivalently any attempt in `CREATED`, `PROCESSING`, `AUTHORIZED`,
`UNKNOWN`, `REVERSAL_PENDING`, plus any `COMMITTED` payment.

| Transaction status | Basket |
| --- | --- |
| `OPEN` | mutable |
| `TOTALIZED` | mutable — a change returns it to `OPEN` and voids totals + promotions |
| `PAYMENT_IN_PROGRESS` | **locked** |
| `COMMITTED` | **locked** |
| `RECEIPT_COMPLETED` | **locked** |
| `FINALIZED` | **locked** |

`AddItem`, `RemoveItem` and `ChangeQuantity` are refused with
`BASKET_LOCKED_BY_PAYMENT` while locked. The code is distinct from
`INVALID_TRANSACTION_STATE` on purpose: a UI reacts differently to "pay or
reverse first" than to "this sale is finished".

If every payment of a sale ends `FAILED` or `REVERSED`, no payment is active
any more, so the basket unlocks and the sale returns to `TOTALIZED`. That is
the frozen rule applied consistently, and it is what lets a cashier correct a
wrong tender and re-tender.

## Decision — reversal

```
AUTHORIZED  ──> REVERSAL_PENDING ──> REVERSED
COMMITTED   ──> REVERSAL_PENDING ──> REVERSED     (the payment un-applies)
```

* A reversal **un-applies** money that should not have been applied. It is
  always against a specific payment of a specific, unfinalized sale.
* A **refund** returns money for a completed sale. It is a different domain,
  a different document, and a future milestone. Nothing here is named refund,
  and `PaymentProviderPort` has no refund method.
* Reversal is durable-before-call like every other provider interaction
  (ADR 020): `REVERSAL_PENDING` is committed, then the provider is called,
  then `REVERSED` is committed. A crash in between leaves
  `REVERSAL_PENDING`, which blocks finalization and is resolvable by retrying
  the reversal — idempotent at the provider through the attempt's correlation
  id.
* A `FAILED` or already `REVERSED` attempt cannot be reversed:
  `PAYMENT_REVERSAL_NOT_ALLOWED`.
* A reversal on a `FINALIZED` sale is refused. After finalization the
  correction is a refund, not a reversal.

## Consequences

* Reversing the only committed payment of a sale returns the sale to
  `TOTALIZED` with the full balance due again.
* Reversal history is never erased: the attempt stays in the table as
  `REVERSED`, so the audit trail shows that money moved and was undone.
