# ADR 001 — The POS is a headless Rust core

Status: accepted · 2026-10-08

## Context

The previous Flutter implementation put transaction state, totals and payment
decisions inside the UI process and its widget tree. That made correctness
depend on the UI being alive, on navigation order and on framework lifecycle,
and it made every new host (Android device, Windows terminal, simulator) a
rewrite.

PI.Edge must complete an offline-eligible sale with no server and no UI, and
must survive a process kill mid-sale.

## Decision

The POS **is** `edge-domain` + `edge-application` + `edge-storage`: a headless
Rust core with no UI dependency. Dependencies point inward only:

```
edge-cli / tauri-pos / android host  ->  edge-protocol  ->  edge-application  ->  edge-domain
                                                                   |
                                                             edge-storage (adapter)
```

* `edge-domain` is pure Rust: no SQLite, no HTTP, no Tauri, no Android, no
  filesystem, no clock. Time and identity are injected.
* `edge-application` owns use cases and declares ports; it never names a
  concrete adapter.
* `edge-storage` is an adapter. So is every UI.
* `edge-cli` is the first client and exists to prove the engine runs with no UI
  at all. It is the executable definition of "headless".

Everything that is not a UI concern (sequence numbers, totals, payment state,
journal, outbox) lives in the core.

## Consequences

* The same core binary logic serves CLI, Tauri, Android host and a future
  `edge-daemon` with no business-logic duplication.
* Business rules are testable without a UI, a device or a server: the suite in
  `tests/` runs on a laptop in milliseconds.
* The core cannot call "up" into a UI. Anything the UI must know is either a
  query result or a future event stream — never a callback into domain code.
* Adding a host means writing an adapter, not porting rules.
* Cost: more crates and more explicit wiring than a single app. Accepted; the
  boundary is the product.
