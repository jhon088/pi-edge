# ADR 026 — The inventory ledger, and who owns inventory

Status: accepted (milestone 8)

## Context

A POS that keeps stock in a mutable `quantity` column cannot answer the only
questions that matter when something is wrong: *when did this change, why, and
because of what*. It also cannot survive the thing PI.Edge is built for — a
till that keeps selling with no network — because two writers of one number
have no way to merge.

## Decision — authority

PI.Edge runs **per installation**, which means per POS, and a store can have
several:

```
STORE
├── POS-01 / Edge DB A
├── POS-02 / Edge DB B
└── POS-03 / Edge DB C
```

While those installations are offline they have no knowledge of each other's
movements. That single fact is what fixes the wording below: the unit of
authority is the *installation*, never the store.

| Who | Authority |
| --- | --- |
| External ERP / WMS | the enterprise-wide / global inventory of record, as the customer's integration defines it |
| PI.COMMERCE Server | the central operational consolidation of retail movements and state |
| **PI.Edge** | **durable authority over the movements originated or observed by *this* Edge installation, plus a local projection of `on_hand` from the last known baseline and those movements** |

So PI.Edge owns two different kinds of statement, and they are not equally
strong:

* **Its movements are facts.** A sale took two units out here; an operator put
  five in here. They are true whether or not anything upstream is reachable,
  nothing upstream may invalidate them, and they are durable the moment they
  are written.
* **Its `on_hand` is a projection**, not a census. It is `baseline + my own
  movements`, so it answers "what does *this* installation know?" — not "what
  does the store physically hold?"

**PI.Edge cannot claim that its `on_hand` is the complete, current inventory of
the whole store when several POS are operating without coordination.** In a
single-Edge store the two coincide. In a multi-register store they diverge
exactly as much as the other registers have sold without being consolidated.

Strong store-wide consistency is therefore *not* a property of this milestone.
Achieving it later needs one of:

* connectivity with PI.COMMERCE Server;
* a Store Hub — one PI.Edge shared at store level;
* some other explicit coordination or reservation strategy.

None of those is implemented now, and this ADR deliberately does not pick one.
What it does is stop the code and the documentation from implying the problem
is already solved.

So: **a sale never waits for the ERP.** Registering a sale and its inventory
movement is a local, durable, offline-capable operation, and reconciliation
with upstream — and with the other registers — happens afterwards.

## Decision — an append-only ledger

```
InventoryMovement  ──>  the ledger  ──>  on_hand = Σ(IN) − Σ(OUT)
```

Movements are written once and never updated or deleted — enforced by database
triggers, not by convention, exactly as the cash ledger is (ADR 021). A
correction is a new compensating movement.

`on_hand` is **derived**, not stored. There is no materialised balance table in
V1: a local till accumulates thousands of movements a day, and a `SUM` over an
index on `(store_id, product_ref)` is not a performance problem. A projection
would be a second thing to keep true. The trigger for adding one is a measured
query cost, not a guess.

### Direction is explicit

`Quantity` is a magnitude, always positive, in thousandths — the same type and
the same semantics the rest of the core uses, so `0.750 kg` means here exactly
what it means on a transaction line. Which way stock moved is a `direction` of
`IN` or `OUT`, never the sign of the quantity.

| Type | Direction | What produces it |
| --- | --- | --- |
| `OPENING_BALANCE` | `IN` | the one-time bootstrap (ADR 028) |
| `SALE` | `OUT` | a finalized sale (ADR 027) |
| `MANUAL_IN` | `IN` | an operator putting stock in, with a reason |
| `MANUAL_OUT` | `OUT` | an operator taking stock out, with a reason |

Each type moves exactly one way, checked in the domain and again by a SQL
`CHECK`.

**Four types, four operations.** There is no `ADJUSTMENT`: it has no command,
no authorisation policy and no workflow, and a movement type with none of those
is a promise rather than a model (frozen in milestone 7). `REFUND`, `TRANSFER`,
`RECEIPT` and `COUNT` are absent for the same reason.

### `MANUAL_IN` and `MANUAL_OUT`, not "adjustment"

A generic adjustment loses the one thing an auditor needs: whether stock
physically arrived or physically left. Both require a non-blank `reason_code`
and a positive quantity.

## Decision — a movement is only for a known product

A movement is accepted only for a `product_ref` the store's **active catalog
snapshot** knows. Inventory does not keep its own product list — that would be
a second master (ADR 011) — so the catalog has to be in place before inventory
is. `SALE` movements are valid by construction: the product came from the
snapshot the sale captured.

## Consequences

* `catalog_products` has no stock column and never will.
* Two products accumulate independently: the ledger is keyed by
  `(store_id, product_ref)`.
* So do two *installations*, even when they carry the same `store_id`. The key
  is store-scoped because that is the operational grain a future consolidation
  will group by — not because one Edge database can see another's movements.
  Two POS in one store hold two ledgers and two projections.
* A future sync reads the ledger directly. It is an append-only log, so it is
  replayable without an outbox mirror of itself (ADR 028).
