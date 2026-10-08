# ADR 003 — SQLite is owned by the edge core

Status: accepted · 2026-10-08

## Context

A POS terminal must finish a sale with no network. The local database is the
system of record until the outbox drains. Multiple writers (UI process, a sync
worker, a hardware service) over one SQLite file is a known source of
corruption, `SQLITE_BUSY` storms and half-written aggregates.

## Decision

One process owns the database, through one connection, inside `edge-storage`.

* `SqliteEdgeStore` holds a single `Connection` behind a `Mutex`. A POS has one
  writer; serializing units of work end to end is correct and simple.
* Every use case runs in exactly one `BEGIN IMMEDIATE` … `COMMIT` via
  `EdgeStore::transact`. Returning `Err` drops the transaction and rolls back
  everything, so aggregate + journal + outbox are atomic (ADR 004).
* Connection settings are fixed at open time: `journal_mode=WAL`,
  `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5s`.
* Schema changes are forward-only migrations with recorded checksums; editing an
  applied migration is a hard error, not silent drift.
* Invariants are enforced twice — in the use case (typed error, good message)
  and in the schema (partial unique indexes for "one open business day per
  store" and "one live session per register", `UNIQUE(register_id,
  register_sequence)`, `CHECK` constraints, append-only triggers).
* No client ever opens the file. There are no local microservices around it.

## Consequences

* `synchronous=FULL` costs an fsync per commit. At POS transaction rates this is
  irrelevant, and it is what makes "the sale is committed" true after a power
  cut.
* Concurrency is intentionally limited. If a future daemon needs concurrent
  readers, WAL already allows them; writers stay serialized.
* The database is a private implementation detail of the core, so its schema can
  evolve without a client release.
