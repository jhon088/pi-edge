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
| `crates/edge-catalog` | local projection of the product master: versioned snapshots, lookup, price books, price/tax resolution and the promotion engine. Pure Rust |
| `crates/edge-payments` | tenders, the payment and attempt state machines, settlement arithmetic, the provider port and a deterministic simulator. Pure Rust: no SQLite, no HTTP, no provider SDK |
| `crates/edge-cash` | the append-only drawer ledger, expected-cash arithmetic, safe-drop approval and the blind close declaration. Pure Rust |
| `crates/edge-inventory` | the append-only stock ledger, its movement types and directions, and the derived on-hand fold. Pure Rust |
| `crates/edge-storage` | SQLite adapter, migrations, single-writer runtime lock |
| `crates/edge-protocol` | versioned wire contract. Pure `serde` DTOs; core conversions behind the `core-mapping` feature |
| `crates/edge-ipc` | the only crate that knows what a socket or a pipe is: `EdgeTransport`, JSON Lines framing, local credentials |
| `crates/edge-client` | `EdgeClient` + `IpcEdgeClient`. Cannot reach the core in-process: it does not depend on the application or storage layers |
| `apps/edge-daemon` | process lifecycle, IPC listener, dispatch, graceful shutdown |
| `apps/edge-cli` | command line client |
| `apps/tauri-pos` | diagnostic UI (React + TypeScript + Vite + Tauri 2). Excluded from the Cargo workspace on purpose |

## Status — milestone 8 (inventory ledger V1)

Done: PI.Edge knows what *it* has recorded, locally and offline. The model is a
ledger, not a number:

```text
InventoryMovement ──append──> InventoryLedger ──fold──> on_hand
```

**There is no stock column anywhere.** `on_hand = Σ(IN) − Σ(OUT)`, derived on
every read, because a mutable quantity cannot answer *when* it changed, *why*,
or *because of what* (ADR 026).

| Movement | Direction | Cause |
| --- | --- | --- |
| `OPENING_BALANCE` | `IN` | the upstream opening baseline, once per installation |
| `SALE` | `OUT` | a sale that reached `FINALIZED` |
| `MANUAL_IN` | `IN` | an operator movement, with a stated reason |
| `MANUAL_OUT` | `OUT` | an operator movement, with a stated reason |

* **Three levels of authority, stated rather than assumed.** An external
  ERP/WMS is the enterprise-wide record, PI.COMMERCE Server is the central
  operational consolidation, and PI.Edge has durable authority over the
  movements **originated or observed by that installation** — keeping a local
  projection of `on_hand` from the last known baseline plus those movements
  (ADR 026).
* **The unit is the installation, not the store.** PI.Edge runs per POS and a
  store may have several, each with its own database and no knowledge of the
  others while offline, so `on_hand` is *this till's* projection and not a
  store-wide census. The two coincide in a single-Edge store; they do not in a
  multi-register one, and strong store-wide consistency is explicitly out of
  scope (ADR 026).
* **Stock moves when the sale finalizes, and at no other moment.** Not when an
  item is scanned, not when it is totalized, not when money is taken. A
  basket is not a commitment, and `RECEIPT_COMPLETED` is optional (ADR 027).
* **One movement per sale line**, written in the *same* database transaction
  as the finalization, keyed by `{transaction_id}:{line_number}` — so a replay
  or a crash-and-retry cannot move stock twice.
* **The physical quantity moves, not the billed one.** A buy-2-get-1 line at
  quantity 3 is three units leaving the shelf; the promotion only touched the
  money (ADR 027).
* **Direction is explicit** (`IN`/`OUT`), never the sign of a quantity, and
  each type moves exactly one way — checked by the domain *and* by SQL.
* **Quantities are thousandths**, so `0.750 kg` is exact. No float ever touches
  a stock figure, for the same reason none touches a price.
* **The ledger is append-only**, enforced by database triggers. A wrong
  write-off is corrected by a compensating movement, and the mistake stays on
  the record.
* **The opening baseline is imported once per installation, ever.** The same snapshot
  replayed is a no-op; a *different* one is refused with
  `INVENTORY_ALREADY_BOOTSTRAPPED` rather than guessed at, and the import is
  atomic (ADR 028).
* **Selling into negative stock is a policy of the installation.**
  `allow_negative_inventory = false` blocks the *start of payment* when this
  installation's local projection says existence is insufficient — before money
  moves, because discovering a shortfall *after* charging the customer is the
  one outcome nobody can undo cleanly. It does **not** promise the store
  physically holds enough: other registers may be selling offline, which is a
  known distributed limitation rather than a bug. After money moves, the ledger
  records what happened: a movement is never suppressed to keep a balance tidy
  (ADR 029).
* **A movement only for a product the active catalog knows.** PI.Edge still
  creates no product master data.
* **No reservations.** There is no soft-allocation between the check and the
  finalization; the window is documented, not hidden (ADR 029).

```bash
edge-cli inventory-import examples/inventory/opening-v1.json
edge-cli inventory-position   --product P-LECHE-1L
edge-cli inventory-movements  --product P-LECHE-1L --limit 20
edge-cli inventory-in  --product P-JAMON-GRANEL --qty 2500 --reason RECEIPT
edge-cli inventory-out --product P-JAMON-GRANEL --qty 500  --reason DAMAGE
```

No cloud sync yet: the ledger is local to one installation, and consolidation
with PI.COMMERCE Server — which is also what would make a position store-wide
rather than per-POS — is a later milestone that this schema is shaped for.

## Previously — milestone 7 (cash management + sessions + EOD V1)

Done: the operational responsibility of a till. Four things that were starting
to be confused are now separate (ADR 025):

| Concept | Question it answers |
| --- | --- |
| `Payment` | how was this sale covered? |
| Cash ledger | how much physical money should be in this drawer, and why? |
| `RegisterSession` | who is responsible for this register, for this period? |
| `BusinessDay` | which operational period are we in? |

* **An append-only cash ledger.** `OPENING_FLOAT`, `CASH_SALE`, `CASH_IN`,
  `CASH_OUT` and `SAFE_DROP`, each with exactly one `IN`/`OUT` direction. Nothing is ever updated or deleted — a correction is a new
  compensating movement, and the database enforces it (ADR 021).
* **Expected cash is derived**, never stored: `Σ(IN) − Σ(OUT)`.
* **A committed cash payment writes exactly one movement**, keyed by the
  payment id, so a replay cannot move the drawer twice. A card payment writes
  none.
* **Safe drops need a second person.** A pending drop moves nothing; only an
  approval by a different identity reduces expected cash (ADR 023).
* **Blind close.** A cashier declares the count *before* learning the
  expectation, enforced by the contract: the pre-close payload has no field
  that could carry an amount (ADR 022).
* **Reopening a session creates a new one linked to the closed one**, so the
  first close and its variance survive (ADR 023).
* **Forced end of day** ends a day over open tills under an explicit policy,
  with a reason and an actor, and **never invents a counted amount** — the
  variance stays unknown rather than zero (ADR 024).

```bash
edge-cli cash-position
edge-cli cash-in  --amount 50000 --reason CHANGE_FUND
edge-cli cash-out --amount 10000 --reason PETTY_CASH
edge-cli safe-drop --amount 80000 --reason DRAWER_LIMIT
edge-cli approve-safe-drop --id <id> --approver SUPERVISOR-01
edge-cli request-close-session        # never reveals the expected cash
edge-cli declare-close --amount 111600
edge-cli reopen-session --session <id> --reason recount
edge-cli force-close-business-day --reason "central EOD"
```

## Previously — milestone 6 (payment engine V1)

Done: payments are a bounded context with a real lifecycle. A **`Payment`** is
one tender's intention to cover part of a sale; a **`PaymentAttempt`** is one
concrete interaction with a provider, and attempts are append-only, so a
retry, a timeout or a reconciliation never erases history (ADR 016).

```text
Transaction ──amount due──> Payment Engine ──> PaymentProviderPort
                              CASH   (no provider, no attempts)
                              CARD   (provider-backed, 1..n attempts)
```

* **`CASH` and `CARD`**, mixed freely. Cash may overpay and produce change; a
  card may not exceed the remaining balance.
* **`UNKNOWN` is a first-class outcome.** A charge whose result was lost is
  never assumed failed and never retried automatically: it blocks the sale and
  the basket until an explicit reconciliation *queries* the provider
  (ADR 018).
* **Durable before the provider call.** The payment and its attempt are
  committed, with a correlation id, before anything is sent — so a process
  death can never leave a charged customer with no local record (ADR 020).
* **Reversal, not refund.** An authorization that must not remain applied is
  reversed, which un-applies the money and returns the sale to payable
  (ADR 019).
* **The basket freezes** the moment a payment becomes financially active:
  `AddItem`, `RemoveItem` and `ChangeQuantity` answer
  `BASKET_LOCKED_BY_PAYMENT`.

```bash
edge-cli start-payment --tender CASH --amount 30000
edge-cli start-payment --tender CARD --amount 70000
edge-cli payments
edge-cli reconcile-payment --payment <id>   # queries; never charges
edge-cli reverse-payment   --payment <id>
```

There is no real provider yet: `CARD` settles through a deterministic
`SimulatorPaymentProvider`. Mercado Pago will be a *provider of `CARD`*, never
a tender of its own.

## Previously — milestone 5 (promotions engine V1)

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
breaks, inventory, commercial refunds, real Mercado Pago, physical terminals,
CFDI, iMin adapters, Android service, updater, telemetry, production checkout
UI, PCI cardholder data handling.

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

# the opening stock baseline, once per installation (idempotent on its snapshot id)
cargo run -p edge-cli -- inventory-import examples/inventory/opening-v1.json
cargo run -p edge-cli -- inventory-position --product P-LECHE-1L

cargo run -p edge-cli -- open-business-day
cargo run -p edge-cli -- open-session --register REG-001 --operator CASHIER-001 --opening-cash 100000
cargo run -p edge-cli -- start-sale
cargo run -p edge-cli -- add-item --barcode 7501000000011 --qty 2
cargo run -p edge-cli -- add-item --barcode 7501000000080 --qty 1   # store book price
cargo run -p edge-cli -- add-item --barcode 7501000000059 --qty-thousandths 350
cargo run -p edge-cli -- change-qty --line 1 --qty 3
cargo run -p edge-cli -- remove-item --line 3
cargo run -p edge-cli -- total
cargo run -p edge-cli -- start-payment --tender CASH --amount 30000
cargo run -p edge-cli -- start-payment --tender CARD --amount 70000
cargo run -p edge-cli -- payments
cargo run -p edge-cli -- finalize-sale
cargo run -p edge-cli -- get-state
cargo run -p edge-cli -- cash-position
cargo run -p edge-cli -- inventory-position --product P-LECHE-1L   # fewer units
cargo run -p edge-cli -- request-close-session
cargo run -p edge-cli -- declare-close --amount 111600
cargo run -p edge-cli -- close-business-day

# 3. the diagnostic UI, consuming the same daemon
cd apps/tauri-pos && pnpm install && PI_EDGE_DB=../../data/pi-edge.db pnpm tauri dev
```

Extras: `remove-item --line N`, `change-qty --line N --qty N`,
`reconcile-payment --payment <id>`, `reverse-payment --payment <id>`,
`core-info`, `active-sale`, `journal --limit N`, `outbox --limit N`,
`watch` (live core events), `catalog-status`, `import-status --snapshot <id>`,
`activate-catalog`, `abort-catalog`, `lookup --barcode|--sku|--product`,
`inventory-movements --product <ref> --limit N --offset N`,
`inventory-in|inventory-out --product <ref> --qty <thousandths> --reason <code>`,
`init-demo --block-negative-inventory` for a till that refuses to oversell,
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

## Payments

A payment is not a field on a sale; it is its own aggregate with its own state
machine (ADR 017):

```text
Payment   CREATED ──> IN_PROGRESS ──> COMMITTED ──> REVERSED
                          └──────────> FAILED

Attempt   CREATED ──> PROCESSING ──┬──> AUTHORIZED ──> REVERSAL_PENDING ──> REVERSED
                                   ├──> FAILED
                                   └──> UNKNOWN ──┬──> AUTHORIZED
                                                  └──> FAILED
```

Three amounts are kept apart on purpose:

| Amount | Meaning |
| --- | --- |
| `requested` | what this tender was asked for — for cash, what the customer handed over |
| `authorized` | what the provider approved (card only) |
| `committed` | what is applied against the sale's debt |

So a 200.00 note against a 116.00 sale gives `requested 200.00`,
`committed 116.00`, `change 84.00` — and the sale reports collecting 116.00,
not 200.00.

**Unresolved means blocked.** While any attempt is `PROCESSING`, `UNKNOWN` or
`REVERSAL_PENDING`:

* the sale cannot finalize,
* no equivalent charge may be started,
* the basket stays frozen,
* the only way forward is `reconcile-payment`, which queries and never charges.

`PROCESSING` counts as unresolved, which is why a process that dies around a
provider call needs no startup repair: the stored state already says "we asked
and we do not know".

**Nothing here stores cardholder data.** The provider port carries amounts,
correlation ids and stable status codes. There is no column, field or event for
a PAN, a CVV, track data or a terminal payload.

For development, `PI_EDGE_PAYMENT_SIM` scripts the simulator — for example
`default:approve,amount:70000=unknown,query:approve`. The daemon refuses to
start on an unparseable script rather than falling back silently.

## Cash, sessions and the day

```text
BusinessDay ──┬── RegisterSession ──┬── OPENING_FLOAT
              │   (one operator,    ├── CASH_SALE ← a committed cash payment
              │    one register,    ├── CASH_IN / CASH_OUT
              │    one period)      ├── SAFE_DROP ← only once approved
              │                     └── close declaration → variance
              └── RegisterSession … (a register runs a chain of them)
```

**Tender, applied and change are three different numbers.** A customer hands
over 200.00 for a 116.00 sale: the payment applies 116.00, the drawer gains
116.00 net, and the ledger keeps `tendered 200.00` / `change 84.00` on the same
row so the audit exists without distorting the expectation. The invariant is
`amount = tendered − change`, checked in the domain and by a SQL `CHECK`.

**Expected cash is never stored.** It is `Σ(IN) − Σ(OUT)` over the session's
movements. The one place it is materialised is a close declaration, so a
historical variance cannot be recomputed under future rules.

**The blind close is a contract, not a screen:**

```text
RequestSessionClose  -> { session_id, currency, movement_count, blockers[],
                          requires_declaration }      ← no amount, anywhere
DeclareClosingCash   -> { declared, expected_at_close, variance, label }
   { counted_amount }                                 ← only now
```

`variance = counted − expected`; a shortage is negative, an overage positive,
and neither is corrected automatically. A forced close has an expected figure,
no count, and **no variance** — `UNDECLARED`, never `BALANCED`.

A business day is an operational period, not a date: it can span midnight and
survive an offline stretch, and `business_day_id` is never derived from
`DATE(now())`.

## Inventory

```text
                   ┌─ OPENING_BALANCE  (IN)  ← upstream position, once
InventoryLedger ───┼─ SALE             (OUT) ← at FINALIZED, one per line
 (append-only)     ├─ MANUAL_IN        (IN)  ← reason required
                   └─ MANUAL_OUT       (OUT) ← reason required
                            │
                            └─ fold ─→ on_hand = Σ(IN) − Σ(OUT)
```

**Three levels of authority, and the unit of the third one is the POS.**

```text
External ERP / WMS    enterprise-wide inventory of record
PI.COMMERCE Server    central operational consolidation
PI.Edge               durable authority over the movements originated or
                      observed by THIS installation, plus a local projection
                      of on_hand = last known baseline + those movements

STORE
├── POS-01 / Edge DB A   ─┐  offline, these three know nothing of
├── POS-02 / Edge DB B    │  each other's movements
└── POS-03 / Edge DB C   ─┘
```

A movement recorded here is a **fact**, true whether or not anything upstream
is reachable, and nothing upstream may invalidate it. `on_hand`, by contrast,
is a **projection**: it answers "what does this installation know?", not "what
does the store physically hold?". **PI.Edge may not claim its `on_hand` is the
complete, current inventory of the whole store** when several POS run without
coordination. In a single-Edge store the two coincide.

Store-wide strong consistency will need connectivity with PI.COMMERCE Server,
a Store Hub (one PI.Edge shared at store level), or another explicit
coordination/reservation strategy. None of those is implemented, and none is
chosen here (ADR 026).

**One moment moves stock: `FINALIZED`.** Scanning, totalizing and taking money
all leave the shelf alone; a basket is not a commitment and a payment can still
be reversed. The movements are written in the same database transaction as the
finalization, so there is no state in which a sale closed without its stock
moving, or stock moved for a sale that never closed (ADR 027).

**What moved is what left this counter.** A buy-2-get-1 line at quantity 3
bills two units and moves three. The promotion changed the money, not the
goods.

**A position can be negative, and says so.** There is no clamping: a till that
sold more than its ledger knew it received is a reportable condition, and
`−0.750` is the honest answer. Whether such a sale is *allowed* is
`allow_negative_inventory`, a property of the installation, checked against
this installation's projection before any money moves (ADR 029).

**`OPENING_BALANCE` is a baseline, not a correction.** It is `IN`-only, fires
once per installation, and every quantity is strictly positive — a zero-stock
product is an omitted line, not a line of zero — so a bootstrap can never lower
a position or produce a negative one.

**Reconciliation is a later milestone.** A second, different baseline is
refused today rather than guessed at, and a negative position arriving from a
future upstream reconciliation will *not* be expressed as an
`OPENING_BALANCE`: it belongs to a reconciliation workflow with its own
movement type, command, authority and audit — not a silent overwrite
(ADR 028).

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
| `tests/golden` | deterministic protocol output for known sales, both price modes, rounding boundaries, one case per promotion kind, one per payment shape — cash exact, cash with change, mixed tender, card unknown, card reconciled, card reversed, partial settlement — and one per inventory shape: the opening position, a ledger with all four movement types, a fractional balance, a negative balance and a promotional sale that moved three units while billing two (frozen clock, sequential ids, scripted provider). Regenerate with `UPDATE_GOLDEN=1 cargo test -p edge-golden-tests` |
| `apps/edge-daemon/tests/catalog_import.rs` | chunked import over real IPC: many chunks, idempotent replay, chunk conflict, missing chunk, bad hash, malformed record, daemon killed mid-import and resumed, atomic activation, CLI importer end to end |
| `apps/edge-daemon/tests/milestone_lifecycle.rs` | two clients over one real daemon; UI killed → core survives; core killed → clients degrade safely and reconnect; second runtime refused |
| `apps/edge-daemon/tests/auth.rs` | handshake surface: good, wrong, missing and malformed credentials; per-connection authentication; the credential never reaches a log or an error |
| `tests/integration/tests/payments.rs` | the payment engine on real SQLite: cash exact/partial/overpaid, card approved/declined/unknown, mixed tender, card cannot overpay, reconcile to authorized and to failed, reversal, unconfirmed reversal, idempotent replay of start/reconcile/reverse, basket freeze, journal exactly once, pricing and promotions untouched by paying |
| `crates/edge-payments` unit tests | tender rules, both state machines, settlement arithmetic, cash/card split, unresolved propagation, deterministic simulator |
| `tests/crash-recovery/tests/payment_recovery.rs` | `UNKNOWN`, `PROCESSING`, `AUTHORIZED`, committed cash, partial mixed tender and a pending reversal all rebuilt by a brand new application object; a restart never decides an outcome |
| `apps/edge-daemon/tests/payments.rs` | the payment surface over real IPC, plus a daemon **killed between the provider call and the result persistence**: the attempt comes back unresolved, never failed, with no second charge |
| `tests/integration/tests/cash.rs` | the drawer on real SQLite: float as a movement, cash sales net of change, cash in/out, replay idempotency, card payments moving nothing, pending vs approved safe drops, self-approval refused, blind close exact/shortage/overage, declaration ordering, materialised expected figure, close blockers, sequential sessions, reopen, midnight-crossing day, forced close inventing no count |
| `crates/edge-cash` unit tests | movement rules and directions, the `amount = tendered − change` invariant, expected-cash arithmetic, safe-drop approval and self-approval, declaration and variance, the pre-close payload carrying no amount |
| `tests/crash-recovery/tests/cash_recovery.rs` | exact position after a restart, a pending drop still pending, an approved drop not applied twice, a session still awaiting its declaration, a declaration not duplicated, a forced close keeping its unknown variance |
| `apps/edge-daemon/tests/cash.rs` | the cash surface over real IPC, including the contract-level proof that the pre-close payload cannot carry the expected cash |
| `tests/integration/tests/inventory.rs` | the stock ledger on real SQLite: an empty store, the opening position per product, replay and competing snapshots, atomic import, duplicate and unknown items, manual in/out and the reason requirement, replay idempotency, fractional and negative positions, stock moving only at `FINALIZED`, one movement per line, buy-2-get-1 physical quantity, two products and two stores independent, both negative-inventory policies, the post-payment window, corrections as new movements, the fold, paging, the journal |
| `crates/edge-inventory` unit tests | movement rules and directions, the sale↔transaction pairing, reason requirements, the `Quantity` positive-magnitude invariant, the on-hand fold, negative rendering, `covers` |
| `tests/crash-recovery/tests/inventory_recovery.rs` | an exact position after a restart; a crash before `COMMIT` leaving no opening position, no manual movement and no half-finalized sale; a replayed finalization across a restart moving stock once; the policy surviving a restart |
| `apps/edge-daemon/tests/inventory.rs` | the inventory surface over real IPC: rendered quantities, explicit direction, movements as their own operation, a position without movements, a refused competing snapshot, a strict till refusing the payment, and the CLI importer reaching the same ledger |
| `crates/edge-storage` migration tests | the inventory triggers against raw SQL: `UPDATE`/`DELETE` refused, a bootstrap recorded once, a movement contradicting its own type refused, one movement per cause per product |
| `apps/edge-daemon/tests/protocol_contract.rs` | pins the stable error codes clients branch on, that the pre-close DTO has no amount-bearing field, and that a stock movement crosses the wire as a positive magnitude plus an explicit direction |

## Non-negotiable boundaries

* `edge-domain` has no SQLite, HTTP, Tauri, Android, filesystem or clock, and
  never sees JSON, sockets or pipes.
* The core does not depend on Tauri or Android. The Tauri app is excluded from
  the Cargo workspace so this cannot regress by accident.
* A UI never opens SQLite, never computes official totals, never decides
  payment state, and never issues a command while disconnected. The diagnostic
  window shows payments, the cash drawer and the stock ledger read-only and
  issues no payment, cash or inventory command.
* A cashier never sees expected cash before declaring the count, and that is
  enforced by the protocol rather than by the interface.
* The cash ledger, the inventory ledger and close declarations are append-only
  in the database, not merely by convention.
* There is no mutable stock field. On hand is always a fold over movements.
* An inventory position is a **projection of one installation**, never a claim
  about a whole store's shelves. PI.Edge runs per POS.
* PI.Edge never receives cardholder data. No PAN, CVV, track data or terminal
  payload is stored, logged or journalled.
* PI.Edge never creates product master data or promotions, and no client may
  state a price, a tax rate, a discount or a product description.
* All durable transactional state belongs to the core.
* Exactly one runtime per installation, enforced by an OS lock taken before the
  database is opened.
* Hardware will always sit behind ports injected at the composition root. The
  port traits arrive with the first adapter, not before: a crate of traits
  nothing implements is an abstraction waiting for a boundary, not a boundary.
* Public ids are UUIDs; server-owned ids (tenant, store, register, operator,
  product, price book, promotion) are opaque strings PI.Edge never mints.
* No Flutter, no Dart, no Odoo, no ERP logic, no local microservices.

Decisions and their trade-offs live in [`docs/ADR`](docs/ADR).
