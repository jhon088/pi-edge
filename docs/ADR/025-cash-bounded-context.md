# ADR 025 — Cash management is its own bounded context

Status: accepted (milestone 7)

## Context

Four things were starting to be confused with each other:

| Concept | Question it answers |
| --- | --- |
| `Payment` | how was this sale covered? |
| Cash ledger | how much physical money should be in this drawer, and why? |
| `RegisterSession` | who is responsible for this register, for this period? |
| `BusinessDay` | which operational period are we in? |

A cash payment of 200.00 against a 116.00 sale makes the difference concrete:
the *payment* applied 116.00, the *drawer* gained 116.00 net from 200.00
tendered, the *session* is accountable for the result, and the *day* is the
period it is reported in. Collapsing any two of these produces a number that is
right for one question and wrong for the others.

## Decision

A new pure crate, `crates/edge-cash`, holding the ledger, expected-cash
arithmetic, safe-drop approval and the close declaration. It depends only on
`edge-domain`: no SQLite, no IPC, no Tauri, no Android, no server, no ERP.

The split follows the one that already worked for `edge-payments` (ADR 016):
cash has its own vocabulary (`movement`, `direction`, `float`, `drop`,
`variance`), its own invariants (append-only, direction fixed per type,
`amount = tendered − change`) and its own failure modes — none of which a sale
or a payment needs to know about.

`RegisterSession` and `BusinessDay` **stay in `edge-domain`**. They are
lifecycle aggregates of the sale context, not cash concepts: a session would
exist with no cash at all in a card-only store. What they gain in this milestone
is a reference to a close declaration and a closure kind, not cash arithmetic.

### Direction of dependency

```
edge-domain  ←── edge-cash  ←── edge-application
     ↑                              │
     └────── edge-payments ─────────┘
```

`edge-cash` does not depend on `edge-payments`, and vice versa. A cash payment
becomes a cash movement in the **application layer**, which is the only place
allowed to know both contexts — the same way it is the only place that knows
both the catalog and a sale.

## Consequences

* Expected cash is computed in one place and can be unit-tested without a
  database, a clock or a payment.
* A future `CASH_REFUND`, petty-cash account or multi-drawer register extends
  `edge-cash` without touching the payment engine.
* The application layer grows a third service (`cash_service.rs`). That is the
  intended cost of keeping the contexts apart.
