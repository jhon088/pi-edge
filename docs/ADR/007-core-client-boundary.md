# ADR 007 — The core/client boundary is a contract, not a function call

Status: accepted · 2026-10-08 · **updated 2026-10-08** (transport decision taken, see Update)

## Context

For development speed the first UI will run the core in-process (Tauri
commands calling `EdgeApplication` directly). The architecture must not depend
on that: tomorrow the core should run as `edge-daemon` with Tauri, an Android
host and the CLI as clients, and the Edge state must survive the UI exiting.

## Decision

Clients talk to the core through **`edge-protocol` DTOs only**, never through
domain types:

* Commands are plain data (`edge-application::commands`), results are versioned
  DTOs (`EdgeStateV1`, `RetailTransactionV1`, …). Both are `serde`-serializable,
  so the same call shape works in-process, over IPC, over a pipe or over a local
  socket.
* Every mutating command carries an optional `request_id`, so a client may retry
  a call whose reply was lost — a requirement that only exists because the
  transport will eventually be remote (ADR 004).
* All state needed to repaint a client comes from one query (`GetEdgeState`)
  built from SQLite, not from objects held by the caller.
* There are **no globals and no singletons** in the core, and nothing
  Tauri-specific anywhere in `crates/`. `EdgeApplication` is an ordinary value
  constructed at the composition root (`apps/edge-cli/src/main.rs` today), and
  `edge-cli` proves the core needs no UI process to exist.
* No API is allowed to assume the core lives forever inside the client process.
  Nothing is cached across commands; killing the client loses nothing.

## Update — 2026-10-08: the daemon is the runtime owner

The open question below was closed in favour of option 2. The in-process
option was rejected deliberately: the separation between UI and core is a
property of the product, not an optimisation to retrofit.

**`edge-daemon` is the production runtime.** It owns the single-writer lock,
the SQLite database and `EdgeApplication`. Every UI is an IPC client:

```text
React  ->  Tauri bridge (an IPC client, not the core owner)  ->  EdgeClient
       ->  EdgeTransport  ->  edge-daemon  ->  core  ->  SQLite
```

Consequences of that choice, as implemented:

* Tauri can exit, crash or be replaced while the business day, the session, the
  open sale, the outbox and the journal stay under the core's control.
* `edge-cli` is no longer the owner of `EdgeApplication`: it is an
  `EdgeClient`, exactly like the UI, which is what makes the contract real
  instead of CLI-shaped. The CLI does not depend on `edge-application` or
  `edge-storage` at all, so it cannot reach the core in-process even by
  accident.
* There is **no `InProcessEdgeClient`**. Keeping one "for convenience" would
  have meant a second code path that can open the database and a second way to
  be the runtime; the single-writer lock would refuse it anyway. The existing
  `tests/` suites still drive `EdgeApplication` directly, because there they
  are testing the core, not a client.
* `edge-protocol` lost its dependency on `edge-application`: DTOs are pure
  `serde` types and the core conversions live behind the `core-mapping`
  feature, which only the daemon enables. A UI client links the contract, not
  the application layer.
* Transport choice, authentication and runtime uniqueness get their own
  records: ADR 008, ADR 010 and ADR 009.

## Original open question (kept for the record)

## Open — needs a decision before the Tauri UI is wired

How the Tauri UI reaches the core. Three options, with trade-offs:

1. **In-process, core owned by the Tauri app state.** Fastest to build; one
   process to debug. But the core's lifetime equals the window's, and an
   Android hardware host would need a second instance — which must never
   happen with one SQLite file (ADR 003).
2. **Local daemon (`edge-daemon`) + IPC (Unix socket / named pipe, JSON lines
   or length-prefixed frames).** Matches the target architecture: the UI can
   crash, restart or be replaced while the register stays open; Android's
   hardware host and the CLI are peers, not rivals. Costs a transport layer,
   a service lifecycle and a supervision story.
3. **In-process behind an async "client" facade with the same method shapes as
   the future IPC client.** Keeps today's speed while making option 2 a
   transport swap rather than a rewrite.

Recommendation at the time: (3) now, (2) when a second client appears.

**Decision taken: (2), immediately.** See the Update above.

## Consequences

* The protocol is a versioned artifact. Breaking changes add `V2` types; they do
  not mutate `V1`.
* Some in-process calls serialize data they could have passed by reference. That
  cost buys transport independence.
* Until the facade exists, nobody writes Tauri code that reaches into
  `edge-application` internals.
