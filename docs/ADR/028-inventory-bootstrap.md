# ADR 028 — Bootstrapping the local inventory

Status: accepted (milestone 8)

## Context

A till has to start from somewhere. Upstream knows what the store should have;
PI.Edge needs that position before its ledger means anything. There is no sync
protocol yet, so the position arrives as an imported snapshot.

Two situations look similar and are not:

**A — first provisioning.** No local movements exist. The snapshot simply
establishes the opening balance.

**B — a later snapshot, with local movements already recorded.** Upstream says
10, the local ledger says 8 because two were sold, and the two numbers are both
"right" about different things.

## Decision

**V1 implements A only.** One bootstrap per Edge installation, ever.

```
ImportInventoryBootstrap { source_snapshot_id, generated_at, items[] }
   → one OPENING_BALANCE movement per item, all in one transaction
   → inventory_bootstrap_state records which snapshot did it
```

* Replaying the **same** `source_snapshot_id` is idempotent: it returns the
  recorded state and writes nothing.
* A **different** snapshot is refused with `INVENTORY_ALREADY_BOOTSTRAPPED`.
* **Nothing is ever overwritten.** No movement is edited or deleted, and no
  balance is replaced.
* An active catalog snapshot is required first, and every item's `product_ref`
  must be in it (ADR 026). An unknown product fails the whole import.
* The import is atomic: a crash part-way leaves no inventory at all, not half
  of it.

### What `OPENING_BALANCE` means, exactly

It is **the initial baseline of one Edge installation, and nothing else**:

* It establishes the starting point of this installation's local projection
  (ADR 026). It is not a store-wide census and not a correction.
* **Every quantity is strictly positive.** `Quantity` refuses `<= 0` on
  construction and the table repeats the rule with
  `CHECK (quantity_thousandths > 0)`, so the invariant is stronger than
  non-negative: a zero-stock product is an **omitted line**, not a line of
  zero. There is no movement that says "nothing moved".
* It follows that a bootstrap can never *lower* a position, and can never
  produce a negative one. `OPENING_BALANCE` is `IN`-only by type (ADR 026).
* A second, different baseline stays refused with
  `INVENTORY_ALREADY_BOOTSTRAPPED`.

**A negative position arriving from a future upstream reconciliation is not
resolved through `OPENING_BALANCE`.** It cannot be: the type only goes `IN`,
only accepts positive magnitudes, and only fires once. Representing "upstream
says you have 3 fewer than you think" needs a movement whose direction, cause
and authority are all different — which belongs to the reconciliation workflow
below, still out of scope. Reusing `OPENING_BALANCE` for it would make the
first baseline and a later correction indistinguishable in the ledger, which is
the one thing the ledger exists to prevent.

### Why B is refused rather than guessed

Situation B *is* reconciliation. Resolving it means deciding whether upstream
wins, local wins, or the difference becomes a third movement — and that
decision belongs to a sync protocol with a notion of which movements upstream
has already seen. Picking one silently would mean a till whose stock changed
for a reason nobody can name, which is precisely what the ledger exists to
prevent.

Half a reconciliation is worse than none: a refusal is visible and fixable, a
wrong merge is neither.

## Outbox: nothing new, deliberately

`SALE_FINALIZED` already carries the whole transaction with its lines and
quantities, so PI.COMMERCE Server can derive the sale movements from it.
Emitting an inventory event as well would publish the same fact twice, from two
places that can drift.

`MANUAL_IN` and `MANUAL_OUT` are *not* represented by any existing event — and
they have no consumer either, so no outbox type is added for them now (no
events without consumers, frozen in milestone 7).

**The future responsibility, recorded here so it is not forgotten:** when
inventory sync exists it must publish manual movements and bootstrap, and must
*not* publish sale movements. Until then the ledger itself is the durable
record: it is append-only, so a sync can read it as a replayable log without
needing an outbox mirror of itself.

## Consequences

* Provisioning order is: device → catalog → inventory bootstrap.
* A store that needs a corrected opening position gets it through manual
  movements with stated reasons, which is auditable, rather than through a
  silent re-import.

> **Amended by ADR 041 (milestone 11).** The publication this ADR anticipated is
> built: a **manual** movement now writes an
> `INVENTORY_MOVEMENT_RECORDED` outbox fact. Sale and refund movements are
> still not published — they are derivable from `SALE_FINALIZED` and
> `REFUND_COMPLETED`, and publishing both would hand the consumer the same
> depletion twice. A bootstrap is not published either: it came from upstream.
