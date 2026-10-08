# ADR 012 — Versioned catalog snapshots and chunked import

Status: accepted · 2026-10-08

## Context

A catalog is eventually thousands of products. It has to arrive over a local
IPC transport, be verifiable, survive a crash mid-transfer, and never be
half-visible to a sale. Sending it as one JSON frame fails most of that: the
whole payload sits in memory twice, a retry starts from zero, there is no
progress, and the frame limit becomes a number we keep raising.

## Decision

**Content is immutable and versioned; the local lifecycle is explicit.**

```text
IMPORTING ──commit──▶ PENDING ──activate──▶ ACTIVE ──a newer one activates──▶ SUPERSEDED
    │
    └── abort (row deleted, content cascades)
```

* `IMPORTING` **never** participates in a sale lookup, not even partially.
* Exactly one `ACTIVE` per store, enforced by a partial unique index.
* `SUPERSEDED` content is kept, because a sale that captured it must stay
  priceable (ADR 013).
* There is **no `FAILED` state**. A commit that fails validation leaves the
  import in `IMPORTING` — precisely the state a client resumes from. A dead
  `FAILED` state would need its own cleanup path and would be a second way of
  saying "not usable".

**Transfer is chunked and resumable.**

```text
BeginCatalogImport   header + tax profiles -> IMPORTING, returns progress
PutCatalogChunk  xN  validate -> persist in its own transaction -> ack
CommitCatalogImport  verify, then -> PENDING
ActivateCatalogSnapshot  supersede + activate in one transaction
```

* **Chunks are sized by bytes, not by record count.** The number of records per
  chunk is the client's business; the protocol only requires that a frame fit.
  The CLI importer fills a 256 KiB budget, far below the 4 MiB frame limit,
  which stays where it is.
* **The daemon never holds the snapshot in memory.** Each chunk is validated,
  persisted and acknowledged on its own; stored counts grow per chunk.
* **A chunk is idempotent on `(snapshot_id, chunk_index, chunk_hash)`.** The
  same index with the same content is acknowledged without a second write; the
  same index with different content is refused with
  `CATALOG_CHUNK_CONFLICT`. Chunk records are durable, which is what makes the
  conflict decidable after a restart.
* **A product's barcodes travel inside its record**, so a product and its codes
  are always in the same chunk and therefore the same transaction.
* **Begin is idempotent on `(store, version, declared_content_hash)`** and
  returns the open import with its progress. Resuming after a CLI crash, an IPC
  disconnect or a daemon restart is therefore the ordinary call, not a special
  mode. The same version with *different* content is
  `CATALOG_VERSION_CONFLICT`.
* **Commit verifies before anything becomes usable**: chunk indices contiguous
  from zero, the client's chunk count, the declared record counts, referential
  integrity (a product's tax profile must be declared; a barcode must name a
  product of this snapshot), no duplicate barcode or product, and the content
  hash.
* **The content hash is computed by the core** by streaming its own stored rows
  in canonical order (tax profiles by code, products by `product_ref`, barcodes
  by barcode), length-prefixed so no field value can forge a boundary. The
  client's declared hash is only ever *compared* against it. A mismatch is
  `CATALOG_HASH_MISMATCH` and the import stays open.
* **Activation is one transaction**: the incumbent goes to `SUPERSEDED` and the
  successor to `ACTIVE` together. No reader outside the transaction can observe
  zero or two active snapshots.

Prices live on the product row rather than in a separate price table: a
snapshot already belongs to one store, so there is exactly one price per
product in it. A price-list table is a migration away if a later milestone
needs several.

## Consequences

* The import protocol is five operations instead of one. That cost buys bounded
  memory, resumability, progress, and a verification step the server will reuse
  verbatim when real sync replaces the CLI importer.
* `content_hash` is meaningful to both sides: the server computes it the same
  way, so "is this register running the catalog I published?" is answerable.
* Superseded snapshots accumulate. Retention is a future decision; the rows are
  small and they are what makes an open sale auditable.
* An aborted import deletes its snapshot row and everything cascades, freeing
  the version number.
