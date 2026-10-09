# ADR 034 — A refund is not a payment reversal

Status: accepted (milestone 9)

## Context

Both give money back, so the temptation to implement one with the other is
real — and `PaymentProviderPort` already had `reverse_payment` from milestone 6,
whose documentation said a refund method was *deliberately* absent because
nothing consumed it (ADR 019). Milestone 9 gives it a consumer. This amends
that decision rather than pretending it never existed.

## Decision — two concepts, kept apart

| | Reversal | Refund |
| --- | --- | --- |
| What it corrects | a payment that should not have stayed applied | nothing; it is a new commercial fact |
| Belongs to | the original `Payment`'s lifecycle | its own `Refund` document |
| When | the sale is still open | the sale is closed |
| Effect on the sale | un-applies money, sale returns to payable | none; the sale stays `FINALIZED` |
| Inventory | none; stock never moved | goods come back (ADR 033) |
| Fiscal | none | a credit note may be owed (ADR 032) |
| Authority | the till's own | central, over the original sale (ADR 030) |

An integration test pins the consequence: after a refund, the original payment
is still `COMMITTED`, the settlement still shows what it collected, and the sale
is still `FINALIZED`. A refund adds a document; it does not rewrite one.

## Decision — the provider port gains a refund, not a rename

```rust
fn refund_payment(&self, request: &RefundRequest) -> Result<RefundOutcome, PaymentError>;
fn query_refund(&self, query: &ProviderQuery) -> Result<RefundOutcome, PaymentError>;
```

Separate from `reverse_payment` and `query_payment`, so no provider
implementation can quietly serve one with the other. `RefundOutcome` is
`Refunded | Declined | Unknown` — its own type, because a reversal's outcome is
`Reversed | Unknown` and the two are not interchangeable.

The simulator answers all three, scripted by `refund:` and `refund-query:`.

## Decision — where the refund's financial records live

`edge-payments` owns the **provider capability**: the port, the request, the
outcome, the correlation semantics. That is its subject matter.

`edge-refunds` owns the **records**: `RefundPayment` (one tender's share of a
refund) and `RefundAttempt` (one interaction with a provider). They belong to the
refund document, not to the sale's payment.

`PaymentAttempt` is **not** reused. It is keyed by `payment_id` and belongs to a
`Payment`; giving it a nullable payment id, or inventing a synthetic payment to
hang it from, would blur exactly the boundary this ADR draws. What *is* reused
is the shape — created, sent, unresolved, settled — and the rules:

* **durable before the provider call**: the refund payment, its attempt and its
  correlation id are committed before anything is sent (ADR 020);
* **`UNKNOWN` is first class**: a lost answer is never promoted to `FAILED` and
  never retried automatically; starting a refund payment again answers
  `REFUND_REQUIRES_RECONCILIATION` (ADR 018);
* **append-only attempts**: a reconciliation adds what the provider said, it
  never rewrites what was sent. A database trigger refuses the delete.

The priority, when the two could conflict: **never give money back twice**. That
is why an unresolved refund blocks a second one instead of retrying, and why a
crash between the provider call and the result leaves a reconcilable attempt
rather than a failed one.

## Amendment to ADR 019

ADR 019 said `PaymentProviderPort` has no refund method because a refund is a
different document and a different domain. The first half stands and this ADR
depends on it. The second half — that it therefore has no place on the port — is
superseded: the *document* is a different domain, the *provider interaction* is
not, and now that a consumer exists the capability belongs where every other
provider capability lives.

## Consequences

* `COMMIT_CASH_PAYMENT` stayed deleted; `reverse_payment` keeps its meaning.
* A client cannot reach a refund through a payment operation, or a reversal
  through a refund operation. The operation names never overlap.
