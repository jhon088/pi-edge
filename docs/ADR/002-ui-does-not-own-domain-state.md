# ADR 002 — The UI does not own domain state

Status: accepted · 2026-10-08

## Context

In the legacy app the screen held the cart, recomputed totals for display, and
decided when a payment "counted". Two consequences hurt: the number on the
customer-facing display could disagree with the number that was persisted, and
a UI crash lost the sale.

## Decision

The UI is a renderer and an intent emitter. Specifically:

* The UI never opens SQLite. It has no connection, no file path, no schema
  knowledge (ADR 003).
* The UI never computes an official amount. Totals, tax, paid total, balance
  due and change due are computed by the core and shipped in
  `edge-protocol` DTOs as minor units **plus** a core-rendered string
  (`MoneyV1.formatted`), which clients display verbatim.
* The UI never decides payment state. It calls `CommitCashPayment` and reads the
  resulting status; `COMMITTED` vs `PAYMENT_IN_PROGRESS` is the core's verdict.
* All durable transactional state belongs to the core. UI-local state is limited
  to view concerns (focus, scroll, which modal is open).
* Adding an item to a totalized sale returns the transaction to `OPEN` and
  zeroes the totals, so a stale total can never be shown as official.

## Consequences

* A UI bug cannot corrupt a sale or produce a receipt that disagrees with the
  journal.
* The UI can be killed and restarted at any point; `GetEdgeState` rebuilds the
  screen from SQLite.
* Localization/formatting of money is centralized in the core. If a client needs
  a different presentation, it is a protocol change, reviewed as such.
* Cost: one round trip to the core for values a UI could have added up itself.
  Accepted — it is the whole point.
