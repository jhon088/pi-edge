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
| `crates/edge-refunds` | the refund document, its lines, the refundable arithmetic over a closed sale, the financial refund lifecycle and the fiscal consequence. Pure Rust |
| `crates/edge-storage` | SQLite adapter, migrations, single-writer runtime lock |
| `crates/edge-protocol` | versioned wire contract. Pure `serde` DTOs; core conversions behind the `core-mapping` feature |
| `crates/edge-ipc` | the only crate that knows what a socket or a pipe is: `EdgeTransport`, JSON Lines framing, local credentials |
| `crates/edge-client` | `EdgeClient` + `IpcEdgeClient`. Cannot reach the core in-process: it does not depend on the application or storage layers |
| `apps/edge-daemon` | process lifecycle, IPC listener, dispatch, graceful shutdown |
| `apps/edge-cli` | command line client |
| `apps/tauri-pos` | diagnostic UI (React + TypeScript + Vite + Tauri 2). Excluded from the Cargo workspace on purpose |

## Status — milestone 9 (refund engine V1)

Done: giving money and goods back is a **document of its own**, not a
correction of the sale:

```text
Original sale (FINALIZED)
      │
      ▼
   Refund ──┬── lines, priced from the sale's own materialised economics
            ├── financial refund  → CASH / CARD, pro-rata over the tenders
            ├── inventory return  → REFUND_RETURN, when the goods come back
            └── fiscal consequence → NONE / CREDIT_NOTE_PENDING / REQUESTED
```

The three consequences settle at **different moments and can disagree** — the
merchandise can be on the shelf while the card refund is still unresolved — so
the lifecycle separates commercial acceptance from financial settlement
(ADR 033):

```text
CREATED ──accept──> ACCEPTED ─────────────────────> COMPLETED
   │                    │                               ▲
  FAILED                └──> FINANCIAL_PENDING ─────────┘
```

* **`ACCEPTED` means the goods came back.** The inventory return is written
  there, once, in the same transaction. From then on the commercial refund is
  irrevocable: an unresolved card refund waits for reconciliation and never
  "fails".
* **Only `CREATED` can fail**, and failing frees the quantities it held.
* **A refund never re-prices anything.** It consumes the original line's
  materialised `net`, `tax`, `discount` and `line_total`, so it is right even
  after the catalog that priced the sale is gone (ADR 031).
* **Promotions are never re-run.** Buy-2-get-1 on three units charged 232.00, so
  one unit gives back a third of that — not the 116.00 shelf price (arbitrage)
  and not zero (shorting the customer). Three single-unit refunds give back
  232.00 exactly.
* **Partial refunds exhaust a line to the cent.** Each slice is prorated against
  what is *left*, and the last one takes precisely the remainder.
* **A tender never gives back more than it took.** The refund is spread
  pro-rata over what each original tender *applied*, capped per tender: a
  300/860 sale refunding 580.00 returns 150.00 cash and 430.00 card.
* **A refund needs central authority over the original sale.** A locally
  finalized sale is not a centrally accepted one, and the wire has no field a
  client could assert it with. With no authority source configured, every refund
  is refused — the production rule by default (ADR 030).
* **One refund in progress per sale**, enforced by a partial unique index, with
  validation and reservation in one transaction.
* **Refund ≠ reversal.** A reversal un-applies a charge that should not have
  stayed, on an open sale; a refund is a new document for a closed one. After a
  refund the original payment is still `COMMITTED` and the sale still
  `FINALIZED` (ADR 034).
* **`UNKNOWN` is first class again.** A lost provider answer is never promoted
  to `FAILED` and never retried: starting again answers
  `REFUND_REQUIRES_RECONCILIATION`. The priority is never paying twice.
* **The fiscal consequence is recorded, never stamped.** A sale that was never
  invoiced admits only `NONE`; an invoiced one must choose explicitly.
  `CREDIT_NOTE_REQUESTED` is a durable intention, not a result. No CFDI, no
  PAC, no SAT (ADR 032).
* **PI.Edge consumes fiscal metadata and never declares it.** Invoicing happens
  upstream, so no operation, client method or CLI command writes the fiscal
  state — a till only ever meets `NOT_INVOICED`. The invoiced case is arranged
  by a test fixture, not by a POS capability (ADR 032).

```bash
edge-cli refund-eligibility --transaction <id>
edge-cli refund-create --transaction <id> --line 1:2000 --reason CUSTOMER_RETURN \
                       --fiscal-action NONE
edge-cli refund-accept --refund <id>     # goods back -> inventory returns
edge-cli refund-pay    --refund <id>     # money back -> drawer / provider
edge-cli refund-reconcile --refund <id>  # resolves an unknown; never refunds again
edge-cli refund-show --refund <id>
```

Refunds are durable locally and in their own tables; **M11 Sync must publish
them.** No outbox event was added, because there is no consumer yet and the
frozen rule is no events without one.

## Previously — milestone 8 (inventory ledger V1)

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
`refund-eligibility`, `refund-create`, `refund-accept`, `refund-pay`,
`refund-reconcile`, `refund-show`, `refunds --transaction <id>`,
`PI_EDGE_REFUND_AUTHORITY=dev-accept-local` to enable refunds in development,
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

## Refunds

```text
                       ┌─ lines         priced from the sale, never re-priced
Refund (own document) ─┼─ financial     pro-rata per original tender, capped
                       ├─ inventory     REFUND_RETURN (IN) at ACCEPTED
                       └─ fiscal        NONE | CREDIT_NOTE_PENDING | REQUESTED
```

**What a unit is worth is already decided.** The sale materialised
`net`, `tax`, `discount` and `line_total` on every line, with basket discounts
already allocated onto the lines before tax (milestone 5). A refund of part of a
line takes that share of those amounts — so the promotion is accounted for
without re-running the promotion engine, and without anyone deciding which unit
"was the free one".

**Slices are taken against the remainder**, so repeated partial refunds of one
line add up to the line to the minor unit, and the last refund absorbs the
remainder. `Money::prorate(part, whole)` returns the amount untouched when
`part == whole`, which is what makes that exact.

**Refund and reversal are different documents:**

```text
ReversePayment   open sale    un-applies a charge that should not have stayed
Refund           closed sale  new document: goods back, money back, credit note owed
```

**The money going back is never more than the money that came in.** Each tender
is capped at what it *applied* — a customer who handed over 200.00 for a 116.00
cash sale is owed 116.00, not 200.00; the change went home with them.

**Authority is resolved by the runtime, not claimed by the till.** In production
PI.COMMERCE Server validates the original sale. Until it exists,
`PI_EDGE_REFUND_AUTHORITY=dev-accept-local` turns on a development stand-in that
marks what it produces; with no switch, refunds are refused.

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
| `tests/golden` | deterministic protocol output for known sales, both price modes, rounding boundaries, one case per promotion kind, one per payment shape — cash exact, cash with change, mixed tender, card unknown, card reconciled, card reversed, partial settlement — and one per inventory shape — the opening position, a ledger with all four movement types, a fractional balance, a negative balance and a promotional sale that moved three units while billing two — and one per refund shape: eligibility, a full cash refund, a promotional partial, a mixed-tender spread, a card refund that came back unknown, and an invoiced sale with its credit note pending (frozen clock, sequential ids, scripted provider). Regenerate with `UPDATE_GOLDEN=1 cargo test -p edge-golden-tests` |
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
| `tests/integration/tests/refunds.rs` | refunds on real SQLite: eligibility and its shrinking as refunds consume the lines, non-finalized and unknown originals, quantity exceeded, duplicate and missing lines, reason required, total and partial refunds, promotional arbitrage, fractional refund, pricing from the sale and not from today's catalog, mixed-tender pro-rata with per-tender ceilings, cash out of the drawer once, replay, inventory returned once and only on acceptance, invoiced vs non-invoiced fiscal choices, one refund at a time, the journal, and refund ≠ reversal |
| `crates/edge-refunds` unit tests | the refundable arithmetic: a full line, a promotional unit, successive partials summing to the line exactly in every split order, `net + tax == gross`, fractional shares, exceeded and exhausted lines, the tender spread with caps; plus the lifecycle, the fiscal choice matrix and the wire round-trips |
| `crates/edge-domain` allocation tests | the single allocation policy: exact sums, proportionality, caps, largest-remainder tie-breaks, and `prorate` slicing a line to the cent in every split |
| `tests/crash-recovery/tests/refund_recovery.rs` | a created refund still holding its quantities; a crash before `COMMIT` leaving no refund, no reservation, no returned goods; goods returned once across a restart; a cash refund emptying the drawer once; a fiscal consequence still pending; and an unresolved card refund resolving through the **same** attempt after a restart, never a second refund |
| `apps/edge-daemon/tests/refunds.rs` | the refund surface over real IPC: **no authority configured means no refund**, the eligibility view a UI renders, a whole refund end to end, a card refund whose answer was lost, **a till being unable to invoice a sale or ask for a credit note**, malformed payloads as `INVALID_ARGUMENT`, and the CLI driving a refund in a separate process |
| `crates/edge-storage` migration tests | the rebuilt ledgers accepting `CASH_REFUND` and `REFUND_RETURN` with their directions and one-per-cause uniqueness, still append-only; the refund invariants (`gross = net + tax`, one live refund per sale, the fiscal pairing); refund attempts append-only with unique correlation ids |
| `apps/edge-daemon/tests/protocol_contract.rs` | pins the stable error codes clients branch on, that the pre-close DTO has no amount-bearing field, that a stock movement crosses the wire as a positive magnitude plus an explicit direction, and that the create-refund payload cannot assert authority or a fiscal state |

## Non-negotiable boundaries

* `edge-domain` has no SQLite, HTTP, Tauri, Android, filesystem or clock, and
  never sees JSON, sockets or pipes.
* The core does not depend on Tauri or Android. The Tauri app is excluded from
  the Cargo workspace so this cannot regress by accident.
* A UI never opens SQLite, never computes official totals, never decides
  payment state, and never issues a command while disconnected. The diagnostic
  window shows payments, the cash drawer, the stock ledger and refunds read-only
  and issues no payment, cash, inventory or refund command.
* A cashier never sees expected cash before declaring the count, and that is
  enforced by the protocol rather than by the interface.
* The cash ledger, the inventory ledger, refund attempts and close declarations
  are append-only in the database, not merely by convention.
* A refund is never a payment reversal, and never re-prices the sale it refunds.
* A refund requires authority over the original sale, resolved by the runtime
  and refused by default. No client can assert it.
* PI.Edge issues no fiscal document, and never declares a sale invoiced. It
  consumes fiscal metadata an upstream system owns, and records which document
  is owed.
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
