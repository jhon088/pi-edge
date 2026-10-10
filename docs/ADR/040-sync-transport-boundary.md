# ADR 040 — Where sync stops being the domain's business

Status: accepted (milestone 11)
Amends ADR 004.

## Context

ADR 004 fixed the durable contract — a transactional outbox, at-least-once
delivery deduplicated on `event_id` — and deliberately built no dispatcher and
no client, because neither would have had behaviour to own. Milestone 11 builds
the dispatcher. PI.COMMERCE Server still does not exist.

That is not a reason to wait, and it is not a reason to invent a server. The
question worth answering now is where the seam goes, so that the day an HTTP
adapter appears, nothing above it changes.

## Decision — three layers, and none of them knows the next one's business

```text
commercial domain ──facts──> outbox (SQLite) ──> edge-sync policy ──> SyncTransport
   knows no sync            durable, ordered      pure, no I/O          ├─ Unconfigured (default)
                                                                        ├─ Simulator (opt-in)
                                                                        └─ HTTP (not built)
```

* **`edge-sync` is pure.** Batch formation, deterministic ordering, backoff,
  outcome classification and the message shapes. No SQLite, no sockets, no HTTP,
  no POS domain. It is a crate rather than a module because it is a real
  boundary: it is the only place that decides *what travels and in what order*,
  and it is the only part of sync worth testing exhaustively.
* **`edge-application/sync_service.rs` orchestrates.** It owns the transactions,
  calls the transport with no transaction open, and applies the answers. It is
  one cohesive file for one context, like `cash_service.rs` next to it.
* **`SyncTransport` has three operations** — push facts, pull messages, ask the
  authority about one refund — and no transport vocabulary. There is no URL, no
  header, no status code, no retry and no connection in the port. Retry is the
  engine's policy; the connection is the adapter's problem.

**The commercial domain knows nothing about sync.** Finalizing a sale writes an
outbox row because ADR 004 said so, and that is the whole of its involvement. No
use case asks whether a server exists, and no sale is ever refused because one
does not.

## Decision — no sync configured is a valid, healthy installation

A register with no transport is not degraded. It sells, writes durable facts,
keeps them, closes its day and reopens tomorrow, indefinitely. Every sync
operation answers `SYNC_NOT_CONFIGURED`, which is a statement of fact and not an
error about the business.

**A simulator is never selected implicitly.** `PI_EDGE_SYNC` absent, empty or
`none` selects no transport; `simulator` selects the simulator; anything else
refuses to start. This is ADR 038's rule for hardware, applied here for the same
reason: a runtime that was misconfigured must say so rather than report healthy
traffic to a server that does not exist.

## Decision — what is deliberately not built

* **No HTTP.** No client, no URL, no route, no base address, no TLS, no
  authentication, no tenant routing. When there is a server, it arrives as one
  more arm of `sync_config::transport_of`.
* **No fake server.** `SimulatorSyncTransport` accepts batches, answers them the
  way a test scripted, remembers which `event_id`s it has seen, and hands over
  inbound messages a test put there. It implements **none** of the future
  server's business rules: it prices nothing, validates no sale, keeps no
  catalog and decides no refund on merit.
* **No hardware in the inbound contract.** A printer host, a drawer driver, an
  iMin bridge or a device topology is local to the composition root (ADR 039)
  and is not something a server may set. Inbound configuration is limited to
  commercial settings that already exist and already have a consumer.
* **No new identity.** Tenant, store, register and currency are provisioned, not
  synced.

## Consequences

* The server must be idempotent on `event_id`. Unchanged from ADR 004, and now
  load-bearing: PI.Edge *will* send duplicates (ADR 041).
* Diagnostics expose a driver name — `unconfigured`, `simulator` — and never a
  host, a port or a credential. A diagnostic view must not become a way to read
  the installation's configuration.
* The HTTP adapter, when it exists, is testable against the same policy tests
  that exist today, because the policy never knew it was missing.
