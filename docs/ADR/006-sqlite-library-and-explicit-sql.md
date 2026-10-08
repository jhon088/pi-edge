# ADR 006 — rusqlite with bundled SQLite, and explicit SQL

Status: accepted · 2026-10-08

## Context

The core needs embedded, durable, transactional local storage on macOS (dev),
Android and later Windows/Linux, with exact control over pragmas and
transaction boundaries. Candidates considered:

1. **`rusqlite` + `bundled`** — thin, synchronous binding over the C library.
2. **`sqlx`** — async, compile-time checked queries, runtime-agnostic drivers.
3. **`diesel` / `sea-orm`** — ORMs with query builders and migration tooling.
4. **`redb` / `sled`** — pure-Rust embedded key-value stores.

## Decision

`rusqlite 0.40` with the `bundled` feature, and hand-written SQL.

Why:

* **Bundled SQLite compiles the C source with the crate**, so every target
  (including Android) gets the same known SQLite version instead of whatever the
  OS ships. No system dependency, reproducible behaviour.
* **Synchronous API matches the problem.** A register is a single-writer,
  low-concurrency workload. `sqlx`'s async machinery would force a runtime into
  the core for no throughput benefit, and the core must stay runtime-agnostic so
  it can be embedded in a Tauri process, an Android service or a daemon.
* **Direct control of pragmas and transactions.** WAL, `synchronous=FULL`,
  `foreign_keys`, `busy_timeout` and `BEGIN IMMEDIATE` are set explicitly
  (ADR 003). This is exactly the surface an ORM abstracts away.
* **Explicit SQL is an asset here.** The schema carries business invariants
  (partial unique indexes, `CHECK` constraints, append-only triggers). Those are
  easier to read, review and trust in SQL than in a query-builder DSL. A heavy
  ORM adds a model layer that would compete with `edge-domain` for ownership of
  the model — which is precisely the ERP-shaped mistake we are avoiding.
* Key-value stores were rejected: we want relational constraints, indexes and
  ad-hoc queries for recovery and audit.

Migrations are a small in-house runner (`edge-storage/src/migrations.rs`, ~60
lines) with recorded checksums, rather than another dependency.

## Consequences

* Build time includes compiling SQLite once per target. Acceptable.
* SQL is written and reviewed by hand; row mapping is explicit (`Raw*` structs).
  More code than an ORM, but every conversion failure is a typed error instead
  of a silent default.
* If we ever need a second storage backend, `UnitOfWork` is already the seam.
