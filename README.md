# PI.Edge

Local runtime of the PI.COMMERCE POS. **The POS is a headless Rust runtime**
(`edge-daemon`); Tauri, the CLI and future hosts are IPC clients of it.

```
PI.COMMERCE Server
       ^
    Sync / API (not implemented yet — the outbox contract is fixed)
       ^
edge-daemon ................. the production runtime: owns the single-writer
  edge-application              lock, SQLite and the application core
  edge-domain
  edge-storage
       ^
   Edge Protocol V1 (JSON Lines over a unix socket / named pipe)
       ^
  edge-client (EdgeClient) .... the only way in for a client
       ^
  edge-cli            tauri-pos            Android host (future)
```

Close the window and the register stays open: the business day, the session,
the active sale, the journal and the outbox live in the daemon.

## Crates

| Crate | Responsibility |
| --- | --- |
| `crates/edge-domain` | pure rules, value objects, invariants, state machines. No SQLite, HTTP, Tauri, Android, filesystem or clock |
| `crates/edge-application` | use cases, queries, ports. Knows the domain and the port traits, never an adapter |
| `crates/edge-storage` | SQLite adapter, migrations, single-writer runtime lock |
| `crates/edge-protocol` | versioned wire contract. Pure `serde` DTOs; core conversions behind the `core-mapping` feature |
| `crates/edge-ipc` | the only crate that knows what a socket or a pipe is: `EdgeTransport`, JSON Lines framing, local credentials |
| `crates/edge-client` | `EdgeClient` + `IpcEdgeClient`. Cannot reach the core in-process: it does not depend on the application or storage layers |
| `crates/edge-hardware-api` | hardware ports as traits. No adapter implemented yet |
| `apps/edge-daemon` | process lifecycle, IPC listener, dispatch, graceful shutdown |
| `apps/edge-cli` | command line client |
| `apps/tauri-pos` | diagnostic UI (React + TypeScript + Vite + Tauri 2). Excluded from the Cargo workspace on purpose |

## Status — milestone 2 (daemon + IPC + diagnostic UI)

Done: the process boundary, Edge Protocol V1 with stable error codes, local IPC
transport, client abstraction, CLI migrated to IPC, daemon lifecycle with
health states, local client authentication, one-runtime-per-installation
locking, push events, and a diagnostic Tauri screen.

Not built yet, deliberately: real sync/HTTP, Mercado Pago, CFDI, catalog
snapshot, pricing/promotions engines, inventory ledger, refunds, iMin adapters,
Android service, updater, telemetry, production UI.

## Requirements

* Rust stable (`rust-toolchain.toml` pins the channel); a C compiler — SQLite is
  compiled from source by `rusqlite/bundled`
* For the UI only: Node + pnpm

## Running it

```bash
# 1. the runtime (owns the database; keep it running)
cargo run -p edge-daemon -- --db ./data/pi-edge.db

# 2. a client, in another shell
cargo run -p edge-cli -- init-demo
cargo run -p edge-cli -- open-business-day
cargo run -p edge-cli -- open-session --register REG-001 --operator CASHIER-001 --opening-cash 100000
cargo run -p edge-cli -- start-sale
cargo run -p edge-cli -- add-item --product SKU-001 --description "Producto prueba" \
    --price 10000 --tax-rate 1600 --qty 1
cargo run -p edge-cli -- total
cargo run -p edge-cli -- pay-cash --amount 11600
cargo run -p edge-cli -- finalize-sale
cargo run -p edge-cli -- get-state
cargo run -p edge-cli -- close-session
cargo run -p edge-cli -- close-business-day

# 3. the diagnostic UI, consuming the same daemon
cd apps/tauri-pos && pnpm install && PI_EDGE_DB=../../data/pi-edge.db pnpm tauri dev
```

Extras: `core-info`, `active-sale`, `journal --limit N`, `outbox --limit N`,
`watch` (live core events), `--format json`, `--db <path>` (or `PI_EDGE_DB`),
`--request-id <uuid>` for idempotent retries, `PI_EDGE_LOG=info` for structured
logs on stderr.

All amounts are **minor units** (`10000` = 100.00 MXN) and tax rates are
**basis points** (`1600` = 16.00%). Money is never a float.

Each client command is a separate process, and the daemon is yet another, so
the walkthrough itself proves nothing important lives in a client's memory.

## Runtime layout

Derived from the database path, so one database means one runtime:

```
<db-dir>/pi-edge.db              owned by the daemon; no client opens it
<db-dir>/.pi-edge/               runtime directory, 0700
<db-dir>/.pi-edge/d.sock         IPC endpoint, 0600 (unix)
<db-dir>/.pi-edge/daemon.lock    OS exclusive lock: one runtime per installation
<db-dir>/.pi-edge/daemon.token   local credential, 0600, never logged
```

## Quality gates

```bash
# Rust core (no Node required)
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

# Tauri client
cd apps/tauri-pos
pnpm lint
pnpm typecheck
pnpm build
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

Clippy runs with warnings as errors; `unwrap`/`expect`/`panic!` and float
arithmetic are denied outside tests.

## Tests

| Suite | What it guarantees |
| --- | --- |
| `crates/*` unit tests | money arithmetic and rounding, transaction lifecycle, rejected transitions, totals, insufficient payment, append-only journal, migration checksums, wire shapes, framing, credentials, runtime lock, daemon state |
| `tests/integration` | the full sale against real SQLite; journal and outbox written atomically; cross-aggregate invariants; command idempotency |
| `tests/crash-recovery` | state rebuilt by a brand new application object; a failure before `COMMIT` leaves no finalized sale without its journal + outbox rows |
| `tests/golden` | deterministic protocol output for known sales (frozen clock, sequential ids). Regenerate with `UPDATE_GOLDEN=1 cargo test -p edge-golden-tests` |
| `apps/edge-daemon/tests/milestone_lifecycle.rs` | two clients over one real daemon; UI killed → core survives; core killed → clients degrade safely and reconnect; second runtime refused |
| `apps/edge-daemon/tests/auth.rs` | handshake surface: good, wrong, missing and malformed credentials; per-connection authentication; the credential never reaches a log or an error |
| `apps/edge-daemon/tests/protocol_contract.rs` | pins the stable error codes clients branch on |

## Non-negotiable boundaries

* `edge-domain` has no SQLite, HTTP, Tauri, Android, filesystem or clock, and
  never sees JSON, sockets or pipes.
* The core does not depend on Tauri or Android. The Tauri app is excluded from
  the Cargo workspace so this cannot regress by accident.
* A UI never opens SQLite, never computes official totals, never decides
  payment state, and never issues a command while disconnected.
* All durable transactional state belongs to the core.
* Exactly one runtime per installation, enforced by an OS lock taken before the
  database is opened.
* Hardware is always behind ports/adapters.
* Public ids are UUIDs; server-owned ids (tenant, store, register, operator,
  product) are opaque strings PI.Edge never mints.
* No Flutter, no Dart, no Odoo, no ERP logic, no local microservices.

Decisions and their trade-offs live in [`docs/ADR`](docs/ADR).
