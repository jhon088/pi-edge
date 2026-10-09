# PI.Edge

Local runtime of the PI.COMMERCE POS. **The POS is a headless Rust runtime**
(`edge-daemon`); Tauri, the CLI and future hosts are IPC clients of it.

```
ERP / master systems
       |  product master data
       v
PI.COMMERCE Server
       ^   |  versioned catalog snapshot
    Sync   |  (imported today from a file; HTTP sync not built yet)
       ^   v
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
| `crates/edge-catalog` | local projection of the product master: versioned snapshots, lookup, price books and price/tax resolution. Pure Rust |
| `crates/edge-storage` | SQLite adapter, migrations, single-writer runtime lock |
| `crates/edge-protocol` | versioned wire contract. Pure `serde` DTOs; core conversions behind the `core-mapping` feature |
| `crates/edge-ipc` | the only crate that knows what a socket or a pipe is: `EdgeTransport`, JSON Lines framing, local credentials |
| `crates/edge-client` | `EdgeClient` + `IpcEdgeClient`. Cannot reach the core in-process: it does not depend on the application or storage layers |
| `crates/edge-hardware-api` | hardware ports as traits. No adapter implemented yet |
| `apps/edge-daemon` | process lifecycle, IPC listener, dispatch, graceful shutdown |
| `apps/edge-cli` | command line client |
| `apps/tauri-pos` | diagnostic UI (React + TypeScript + Vite + Tauri 2). Excluded from the Cargo workspace on purpose |

## Status — milestone 5 (promotions engine V1)

Done: a deterministic, offline, snapshot-bound **basket** promotion engine.
Promotions arrive upstream in the catalog snapshot — PI.Edge never creates one —
and the engine is a pure function of `(basket, promotions, context)`:

```text
basket + store + effective_at + snapshot promotions  ->  per-line discounts + adjustment records
```

Four rule kinds ship: percentage off, fixed amount off, buy N get M, and a
basket threshold. Every discount comes back as an explicit, auditable
`PromotionAdjustment`, and a tie between two exclusive promotions is
`AMBIGUOUS_PROMOTION`, never a guess.

The basket is re-evaluated from scratch on every change, which is why a basket
built by `add → remove → add` prices exactly like one built by a single `add`:

```bash
edge-cli add-item --barcode 7501000000011 --qty 2
edge-cli change-qty --line 1 --qty 6     # buy-2-get-1 now applies twice
edge-cli remove-item --line 1            # and the discount is retracted
```

Earlier: price books with store overrides, priorities and scheduled validity;
a versioned local catalog imported in resumable chunks and verified by content
hash; barcode/sku/reference lookup; both price modes; sales that keep the
catalog *and the instant* they started with; headless core owned by
`edge-daemon`; Edge Protocol V1 over local IPC with stable error codes; one
runtime per installation; local client authentication; push events; diagnostic
Tauri screen.

**A client sends identity and quantity. It cannot state a price or a discount.**

Not built yet, deliberately: HTTP catalog sync, ERP integration, coupons,
loyalty, customer-specific pricing, product groups and categories, quantity
breaks, inventory, refunds, Mercado Pago, CFDI, iMin adapters, Android service,
updater, telemetry, production UI.

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

# a catalog has to exist before anything can be sold
cargo run -p edge-cli -- import-catalog examples/catalog/demo-v1.json --activate
cargo run -p edge-cli -- catalog-status
cargo run -p edge-cli -- lookup --barcode 7501000000011

cargo run -p edge-cli -- open-business-day
cargo run -p edge-cli -- open-session --register REG-001 --operator CASHIER-001 --opening-cash 100000
cargo run -p edge-cli -- start-sale
cargo run -p edge-cli -- add-item --barcode 7501000000011 --qty 2
cargo run -p edge-cli -- add-item --barcode 7501000000080 --qty 1   # store book price
cargo run -p edge-cli -- add-item --barcode 7501000000059 --qty-thousandths 350
cargo run -p edge-cli -- change-qty --line 1 --qty 3
cargo run -p edge-cli -- remove-item --line 3
cargo run -p edge-cli -- total
cargo run -p edge-cli -- pay-cash --amount 11600
cargo run -p edge-cli -- finalize-sale
cargo run -p edge-cli -- get-state
cargo run -p edge-cli -- close-session
cargo run -p edge-cli -- close-business-day

# 3. the diagnostic UI, consuming the same daemon
cd apps/tauri-pos && pnpm install && PI_EDGE_DB=../../data/pi-edge.db pnpm tauri dev
```

Extras: `remove-item --line N`, `change-qty --line N --qty N`, `core-info`,
`active-sale`, `journal --limit N`, `outbox --limit N`,
`watch` (live core events), `catalog-status`, `import-status --snapshot <id>`,
`activate-catalog`, `abort-catalog`, `lookup --barcode|--sku|--product`,
`--format json`, `--db <path>` (or `PI_EDGE_DB`), `--request-id <uuid>` for
idempotent retries, `PI_EDGE_LOG=info` for structured logs on stderr.

## Catalog

PI.Edge is **not** the master of products: everything comes from upstream as an
immutable, versioned snapshot (ADR 011). A snapshot is transferred in chunks
sized by bytes, each one validated and persisted on its own, and the core
verifies the content hash it computes from its own stored rows before the
snapshot can be used (ADR 012):

```text
BeginCatalogImport -> PutCatalogChunk xN -> CommitCatalogImport -> ActivateCatalogSnapshot
IMPORTING             (never prices a sale)   PENDING               ACTIVE
```

Interrupt an import anywhere — kill the CLI, drop the connection, restart the
daemon — and re-running the same `import-catalog` resumes it.

A sale captures the active snapshot when it starts and keeps it until it is
finalized, so activating a new catalog mid-sale can never move a price
(ADR 013). The next sale uses the new one.

## Pricing

A product does not own a price. A **price book** does, and a sale resolves one
deterministically (ADR 014):

| Book | Scope | Priority | Price |
| --- | --- | --- | --- |
| `BASE` | tenant | 0 | 59.00 |
| `STORE-DEMO` | store `STORE-DEMO` | 0 | 49.90 ← wins |

* Scope decides first: a store book always beats a tenant book. Priority breaks
  ties *within* a scope. A tie at the top is `AMBIGUOUS_PRICE`, never a guess.
* Validity is `[valid_from, valid_until)`, so a scheduled price can start
  exactly when the previous one ends.
* No applicable price is `PRICE_NOT_FOUND`.
* `effective_at` is explicit. A sale pins it when it starts, so a price change
  scheduled for midnight cannot move a price between two scans.

Each **price entry** declares whether its amount already includes tax, which is
how Mexican shelf prices work:

| Mode | 16% on 100.00 | What the customer pays |
| --- | --- | --- |
| `TAX_EXCLUSIVE` | net 100.00 + tax 16.00 | 116.00 |
| `TAX_INCLUSIVE` on 116.00 | net 100.00 + tax 16.00 | 116.00 |

The tax profile states the rate; the price states the mode.

`net + tax == gross` always holds, at every rate, including when the division
is not exact.

## Promotions

Promotions are snapshot content, like products and prices. The engine lives in
the core, works entirely offline, and returns **per-line discount amounts plus
an explicit record of why** (ADR 015):

| Rule | What it does |
| --- | --- |
| `PERCENTAGE_OFF` | a proportion off each matching line |
| `FIXED_AMOUNT_OFF` | a flat amount off each matching line, capped at the line |
| `BUY_N_GET_M` | a line of `buy + get` units earns one application; twice that earns two |
| `BASKET_THRESHOLD` | a flat amount off the basket once it reaches a minimum |

* **Order is deterministic**: priority DESC, then `promotion_id` ASC. Stackable
  promotions accumulate on what is left after the previous one; an exclusive
  promotion applies alone.
* **A tie is a refusal**: two exclusive promotions at the same priority on the
  same target give `AMBIGUOUS_PROMOTION`.
* **Basket discounts are allocated onto the lines** by largest remainder, never
  subtracted from the grand total, so every line still computes its own tax
  from its own discounted amount and `net + tax == gross` survives. The
  allocation is exact to the cent.
* **A threshold is measured after line discounts**, on `Σ (extended − line
  discount)`, which keeps the rule from depending on its own outcome.
* **No line, and no sale, can go below zero.** Every discount is capped.
* **Snapshot-bound**: a sale evaluates against the promotions of the snapshot it
  captured. Activating a new catalog mid-sale changes nothing for that sale.
* **Materialised for audit**: what a sale granted is copied onto the sale —
  code, name, priority, stacking, target, amount, reason — so a historical
  receipt still reads after the snapshot is purged.
* The promotion summary is journalled at `TRANSACTION_TOTALIZED` and only
  there: the moment the totals become official.

```text
candidates (active, in force, this store)
  -> sort: priority DESC, promotion_id ASC
  -> per-line pass   (exclusive arbitration, sequential stacking)
  -> basket pass     (threshold over Σ remaining)
  -> allocate basket discounts onto the lines (largest remainder)
  -> per-line discount + PromotionAdjustment records
```

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
| `crates/*` unit tests | money arithmetic and both rounding modes, transaction lifecycle, rejected transitions, totals, insufficient payment, append-only journal, migration checksums, wire shapes, framing, credentials, runtime lock, daemon state, catalog records, snapshot lifecycle, canonical content hashing |
| `tests/integration` | the full sale against real SQLite; journal and outbox written atomically; cross-aggregate invariants; command idempotency |
| `tests/crash-recovery` | state rebuilt by a brand new application object; a failure before `COMMIT` leaves no finalized sale without its journal + outbox rows |
| `tests/integration/tests/catalog.rs` | barcode/sku/reference lookup, price and tax resolution, quantity and fractional quantity, unknown barcode, inactive and non-sellable products, duplicate barcode, invalid record, unknown tax profile, no active catalog |
| `tests/integration/tests/promotions.rs` | promotions from a real snapshot: the four rules, re-evaluation on add/remove/quantity change, retracted adjustments, retired line numbers, `AMBIGUOUS_PROMOTION`, store scope, inactive promotions, never below zero, tax-inclusive discounts, exact basket allocation, snapshot binding, audit after a catalog switch, journal summary, idempotent replay, lifecycle refusals |
| `crates/edge-catalog` promotion tests | the engine itself: every rule kind, buy-2-get-1 at qty 6, threshold below/at/above, inactive, future, expired, store applicability, priority, exclusive, stackable, ambiguous, rounding boundaries, order independence, idempotence |
| `apps/edge-daemon/tests/promotions.rs` | the promotion surface over real IPC: adjustments on the transaction DTO, `remove-item`/`change-qty`, `AMBIGUOUS_PROMOTION`, promotions in the chunked import, count mismatch, a promotion naming a product from another chunk |
| `tests/integration/tests/pricing.rs` | base price, store override, other store's book ignored, inactive book ignored, priority within a scope, future and expired prices, no price, ambiguous price, overlapping schedules, both price modes, price book in the journal, import-time validation |
| `tests/integration/tests/snapshot_switching.rs` | an open sale keeps its prices across activations; the next sale uses the new catalog; exactly one `ACTIVE` |
| `tests/golden` | deterministic protocol output for known sales, both price modes, rounding boundaries, and one case per promotion kind — percentage, fixed, buy-N-get-M, stacked with a basket threshold, exclusive, tax-inclusive (frozen clock, sequential ids). Regenerate with `UPDATE_GOLDEN=1 cargo test -p edge-golden-tests` |
| `apps/edge-daemon/tests/catalog_import.rs` | chunked import over real IPC: many chunks, idempotent replay, chunk conflict, missing chunk, bad hash, malformed record, daemon killed mid-import and resumed, atomic activation, CLI importer end to end |
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
* PI.Edge never creates product master data or promotions, and no client may
  state a price, a tax rate, a discount or a product description.
* All durable transactional state belongs to the core.
* Exactly one runtime per installation, enforced by an OS lock taken before the
  database is opened.
* Hardware is always behind ports/adapters.
* Public ids are UUIDs; server-owned ids (tenant, store, register, operator,
  product, price book, promotion) are opaque strings PI.Edge never mints.
* No Flutter, no Dart, no Odoo, no ERP logic, no local microservices.

Decisions and their trade-offs live in [`docs/ADR`](docs/ADR).
