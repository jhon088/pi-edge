# ADR 013 — A sale keeps the catalog it started with

Status: accepted · 2026-10-08

## Context

Catalogs change during business hours. A new snapshot can be imported and
activated while a cashier has a sale open. If a line resolved against
"whatever is active right now", activating a snapshot mid-sale would silently
move prices between two scans of the same product — the kind of defect that is
invisible in testing and infuriating at a till.

## Decision

**A transaction captures its catalog snapshot when it starts, and resolves
against that snapshot for its whole life.**

* `StartTransaction` requires an `ACTIVE` snapshot and records
  `(snapshot_id, version)` on the aggregate. No active catalog means no sale:
  `NO_ACTIVE_CATALOG`.
* Every `AddItem` resolves the product, its price and its tax profile from the
  **captured** snapshot, never from the currently active one.
* A `SUPERSEDED` snapshot still resolves sales, which is why activation
  supersedes rather than deletes. An `IMPORTING` snapshot never resolves
  anything.
* The captured `(snapshot_id, version)` is persisted with the transaction and
  survives a restart, so a recovered sale prices exactly as it did before the
  process died.
* The journal records the snapshot version and the selector that resolved each
  line, so "what priced this line?" is answerable after the fact.
* A new sale started after an activation captures the new snapshot. Catalog
  updates therefore take effect at a sale boundary, which is the only boundary
  a cashier can reason about.

Alternatives considered:

* **Resolve from the active snapshot at add-time.** Simplest, and wrong: the
  same sale could contain two prices for one product.
* **Copy resolved values onto the line only** (what milestone 1 effectively
  did). The line is already immutable once priced, but without the captured
  reference there is nothing to re-resolve against, so any later operation
  (re-price, void-and-rescan, audit) would silently use a different catalog.
* **Block activation while a sale is open.** Rejected: it makes a catalog
  update hostage to a till that an operator walked away from.

## Consequences

* An open sale can outlive several activations and still be correct; a test
  asserts exactly that for two generations.
* `retail_transactions.catalog_snapshot_id` is nullable, because databases
  created before migration 0002 have sales with no catalog. The use case
  requires one for every new sale, so `NULL` only ever means "older than the
  catalog".
* A snapshot cannot be deleted while a sale references it. Nothing deletes
  committed snapshots today; whatever retention we add later has to respect
  this.
* "Effective at" is carried on the snapshot but not yet enforced: scheduled
  activation is a later decision, and this ADR is what makes it safe to add.
