# Frozen decisions

Decisions that are settled. Changing one of these is a new ADR and a
deliberate, reviewed break — not a refactor.

## Runtime and boundaries (milestones 1–2)

* `edge-daemon` is the production runtime owner. UIs and the CLI are IPC
  clients. (ADR 001, ADR 007)
* SQLite belongs to the core. No client opens the database. (ADR 003)
* Exactly one runtime per installation, enforced by an OS lock taken before the
  database is opened. (ADR 009)
* The UI never computes an official total, never decides payment state, and
  never issues a command while disconnected. (ADR 002)
* Edge Protocol V1 is the only way in: versioned DTOs, stable error codes,
  JSON Lines framing over a unix socket or named pipe. (ADR 008)
* Money is `i64` minor units. No floating point anywhere in the core.
* One rounding policy: half away from zero, in
  `edge_domain::money::div_round_half_away_from_zero`.

## Catalog (milestone 3)

* **PI.Edge never creates product master data.** Products, barcodes, prices,
  tax rates and promotions all originate upstream. (ADR 011)
* Identity is `product_ref`, assigned upstream. `external_ref` is traceability
  metadata, never operational identity. (ADR 011)
* Catalog content is immutable and versioned: `IMPORTING → PENDING → ACTIVE →
  SUPERSEDED`, one `ACTIVE` per store, activation atomic. (ADR 012)
* The content hash is computed by the core from the rows it stored, in canonical
  order. A client's declared hash is only ever compared against it. (ADR 012)
* A sale captures its snapshot at `StartTransaction` and keeps it until the sale
  is finalized. (ADR 013)

## Pricing (milestone 4)

* **A product never owns a price.** Prices live in price books. (ADR 014)
* **`price_mode` belongs to the `PriceEntry`**, not to the tax profile: whether
  an amount includes tax is a property of the amount, not of the rate.
* **Validity is `[valid_from, valid_until)`** — lower bound inclusive, upper
  bound exclusive.
* **`STORE` scope outranks `TENANT` scope**, regardless of priority.
* **Higher priority wins within the same scope.**
* **Equal winning priority is `AMBIGUOUS_PRICE`** — the core refuses rather
  than guesses.
* **`effective_at` is captured at `StartTransaction`** and every line of that
  sale resolves at that instant.
* **The domain never reads a wall clock implicitly.** Time arrives as a
  parameter, always.

## Promotions (milestone 5)

* **The promotion engine is pure, deterministic and snapshot-bound.** It is a
  function of `(basket, promotions, context)` with no state between calls, and
  it evaluates against the promotions of the snapshot the sale captured.
  (ADR 015)
* **Promotions produce explicit `PromotionAdjustment` records.** A discount is
  never an anonymous subtraction: it names the promotion, the target, the
  amount and the reason.
* **Basket-level discounts are allocated onto the lines before tax is
  computed**, never subtracted from the grand total, so every line still
  derives its own tax from its own discounted amount and `net + tax == gross`
  holds.
* **Re-evaluation depends only on the current basket plus the snapshot and
  context.** No incremental promotion state is kept, so the same basket always
  produces the same adjustments, with the same ids.
* **What a sale granted is materialised on the transaction** — code, name,
  priority, stacking, target, amount, reason, detail — so a historical sale
  stays auditable after its snapshot is gone.

## Basket mutability and the payment lifecycle (frozen in milestone 5, enforced across 5–6)

* **The basket is mutable only while no payment has started.** `OPEN` and
  `TOTALIZED` are mutable states.
* **A basket change on a `TOTALIZED` transaction returns it to `OPEN`**,
  invalidates the totals and the applied promotions, and requires totalizing
  again before payment.
* **As soon as any payment enters a financially active state, the basket is
  locked**, and `AddItem`, `RemoveItem` and `ChangeQuantity` must be refused
  with a stable error code (`BASKET_LOCKED_BY_PAYMENT`).
* **`PAYMENT_IN_PROGRESS`, `COMMITTED` and `FINALIZED` imply an immutable
  basket.** (`RECEIPT_COMPLETED` was in this list until milestone 12 removed
  the state; see ADR 043.)

The payment-side half of this rule is the subject of milestone 6. It is
recorded here as a decision, not as code: no speculative handling is written
for payment states that do not exist yet.

## Payments (milestone 6)

* **`Payment` and `PaymentAttempt` are two concepts.** A payment is the
  commercial intention to cover part of a sale with one tender; an attempt is
  one concrete interaction with a provider, and attempts are append-only.
  (ADR 016)
* **Cash creates no synthetic attempts.** An attempt models a provider
  interaction and cash has no provider. Cash still uses the same aggregate,
  state machine, settlement arithmetic, persistence and protocol operation.
* **`CARD` is a generic tender.** Mercado Pago will be a *provider of `CARD`*,
  never a tender of its own, so routing a card elsewhere is not a change to the
  sale's commercial meaning.
* **`requested`, `authorized`, `committed`, `tendered`, `applied` and `change`
  are distinct concepts** and are never collapsed. A sale reports what it
  collected, not what the customer handed over.
* **`UNKNOWN` is a first-class state**, and **`PROCESSING` is equally
  unresolved**. An ambiguous outcome is never promoted to `FAILED` and never
  retried automatically: the only way forward is an explicit query (ADR 018).
* **The basket freezes as soon as financial activity starts.** `AddItem`,
  `RemoveItem` and `ChangeQuantity` answer `BASKET_LOCKED_BY_PAYMENT`
  (ADR 019).
* **`PaymentProviderPort` knows nothing about any concrete provider.** Inputs
  and outputs are domain types: amounts, correlation ids and stable status
  codes.
* **`RetailTransaction` is not the source of truth for payments.** The payment
  tables are; `PaymentSummary` on the sale is derived and recomputed on every
  load (ADR 020).
* **Durable before the provider call is mandatory.** The payment, its attempt
  and its correlation identity are committed before anything is sent
  (ADR 020).
* **Reversal is not refund.** A reversal un-applies money on an unfinalized
  sale; a refund returns money for a completed one and is a different domain.
* **No cardholder data is ever stored** — no PAN, CVV, track data or terminal
  payload, in any table, log, journal event or DTO.
* **`COMMIT_CASH_PAYMENT` and the legacy cash path are gone for good.** There
  is exactly one way to take money.

## Cash management, sessions and the day (milestone 7)

* **Four concepts stay separate**: a `Payment` says how a sale was covered, the
  cash ledger says what should be in the drawer, a `RegisterSession` says who is
  responsible, a `BusinessDay` says which period it is. (ADR 025)
* **The cash ledger is append-only.** A correction is a new compensating
  movement, never an edit — enforced by database triggers, not convention.
  (ADR 021)
* **Direction is explicit** (`IN`/`OUT`), never the sign of a `Money`, and each
  movement type moves exactly one way.
* **A cash sale records the net that stayed in the drawer**, carrying
  `tendered` and `change` for audit, with `amount = tendered − change`.
* **Expected cash is derived**, never stored: `Σ(IN) − Σ(OUT)`. The only
  materialised figure is `expected_at_close` on a declaration, so a historical
  variance is never recomputed under future rules. (ADR 022)
* **A committed cash payment writes exactly one movement**, keyed by the payment
  id. A card payment and a card reversal write none.
* **A pending safe drop moves nothing.** Only an approval by a *different*
  identity reduces expected cash, and the domain never evaluates a role — it
  records the identity the application layer already authorised. (ADR 023)
* **The blind close is a contract, not a screen.** The pre-close payload has no
  field capable of carrying an amount, and a declaration is refused unless a
  close was requested first. (ADR 022)
* **Reopening a session creates a new one linked to the closed one**; the first
  close, its declaration and its variance are never mutated. The new float is
  what was *declared*, not what was expected. (ADR 023)
* **A business day is an operational period, never a calendar date**, and
  `business_day_id` is never derived from `DATE(now())`. (ADR 024)
* **A forced end of day invents no counted cash.** The variance stays unknown
  rather than zero, the closure kind is stored rather than inferred, a reason
  and an actor are mandatory, and no transaction, payment, movement or pending
  approval is destroyed. (ADR 024)
* **A movement type arrives with its operation.** No type exists without the
  command, authorisation policy and workflow that produce it.

## Inventory (milestone 8)

* **Three levels of authority, stated rather than assumed** (ADR 026):
  * an **external ERP/WMS** is the enterprise-wide / global inventory of
    record, as the customer's integration defines it;
  * **PI.COMMERCE Server** is the central operational consolidation of retail
    movements and state;
  * **PI.Edge** has durable authority over the movements **originated or
    observed by that Edge installation**, and keeps a local projection of
    `on_hand` from the last known baseline plus those movements.
* **The unit of authority is the installation, not the store.** PI.Edge runs
  per POS and a store may have several, each with its own database and no
  knowledge of the others' movements while offline. **PI.Edge may not claim
  that its `on_hand` is the complete, current inventory of the whole store**
  when several POS operate without coordination. In a single-Edge store the two
  coincide; in a multi-register store they do not. (ADR 026)
* **Strong store-wide consistency is explicitly out of scope.** It will require
  connectivity with PI.COMMERCE Server, a store-level Store Hub, or another
  explicit coordination/reservation strategy. None is implemented and none is
  chosen. (ADR 026)
* **There is no mutable stock field.** `on_hand = Σ(IN) − Σ(OUT)` is derived
  from the ledger on every read, because a quantity that is overwritten cannot
  answer when it changed, why, or because of what. (ADR 026)
* **The movement ledger is append-only**, enforced by database triggers. A
  wrong figure is corrected by a compensating movement and the mistake stays on
  the record.
* **Direction is explicit** (`IN`/`OUT`), never the sign of a quantity, and
  **each movement type moves exactly one way** — checked by the domain and by
  SQL.
* **A movement type arrives with its operation.** V1 has four types and four
  operations; `MANUAL_IN`/`MANUAL_OUT` are named for what they are, not called
  "adjustment".
* **Stock moves at `FINALIZED` and at no other moment.** Not at scan, not at
  totalize, not when money is taken, and not when a receipt prints — the
  receipt is an obligation recorded separately and never a moment in the sale
  (ADR 027, ADR 043).
* **One movement per sale line, in the same database transaction as the
  finalization**, keyed by `{transaction_id}:{line_number}` — so a replay or a
  crash-and-retry can never move stock twice.
* **The physical quantity moves, not the billed one.** A buy-2-get-1 line at
  quantity 3 moves 3. A promotion changes the money, never the goods.
  (ADR 027)
* **Quantities are thousandths**, everywhere, so `0.750` is exact. No float
  ever touches a stock figure.
* **A movement is accepted only for a product the active catalog snapshot
  knows.** PI.Edge still creates no product master data.
* **The opening position is imported once per installation, ever.** The same
  `source_snapshot_id` replayed is a no-op; a different one is refused with
  `INVENTORY_ALREADY_BOOTSTRAPPED`. The import is atomic and overwrites
  nothing. (ADR 028)
* **`OPENING_BALANCE` is the initial baseline and nothing else.** It is
  `IN`-only, fires once, and every quantity is **strictly positive** — a
  zero-stock product is an omitted line, not a line of zero. A bootstrap can
  therefore never lower a position nor produce a negative one. **A negative
  position arriving from a future upstream reconciliation is not resolved
  through `OPENING_BALANCE`**: it belongs to the reconciliation workflow, which
  stays out of scope. (ADR 028)
* **Reconciliation of a second upstream position is refused rather than
  guessed at**, and is a later milestone with its own movement type, command,
  authority and audit. (ADR 028)
* **A position can be negative and is reported as such.** Clamping to zero
  would hide a reportable condition.
* **`allow_negative_inventory` belongs to the installation**, is checked at
  `StartPayment` — before money moves — and is **never** re-checked at
  finalization: after money moves, the ledger records what happened rather than
  enforcing a policy that can no longer change the outcome. (ADR 029)
* **`allow_negative_inventory = false` means**: block the start of payment when
  *this* Edge installation's local projection says existence is insufficient.
  It does **not** mean the store physically holds enough, because other
  registers may be selling offline. That is a known distributed limitation, not
  a bug. (ADR 029)
* **There are no reservations in V1.** The window between the availability
  check and the finalization is documented, not closed. (ADR 029)

## Refunds (milestone 9)

* **A refund is its own document**, never a correction of the sale and never a
  payment reversal. After a refund the original payment is still `COMMITTED`
  and the sale still `FINALIZED`. (ADR 030, ADR 034)
* **A refund needs central authority over the original sale.** A locally
  finalized sale is *not* a centrally accepted one. The domain receives an
  already-resolved `RefundAuthorizationContext`; the wire has no field a client
  could assert it with; and with no authority source configured every refund is
  refused with `REFUND_AUTHORITY_REQUIRED`. (ADR 030)
* **A refund never re-prices anything** — not the current price, promotion, tax
  profile or snapshot. It consumes the original line's materialised
  `extended`, `discount`, `net`, `tax` and `line_total`. (ADR 031)
* **The promotion engine is never re-run for a refund**, and no unit is
  identified as "the free one". A buy-2-get-1 line that charged 232.00 for three
  units gives back a third of 232.00 per unit — neither the shelf price
  (arbitrage) nor zero (shorting the customer). (ADR 031)
* **Partial refunds exhaust a line exactly.** Each slice is prorated against
  what is *left*, so `Σ(partial refunds of a line) == line_total` to the minor
  unit in every split and order, and the last refund absorbs the remainder.
  `Money::prorate(part, whole)` returns the amount unchanged when
  `part == whole`, which is what makes it exact. (ADR 031)
* **`net + tax == gross` on every refund and refund line**, because `gross` is
  computed from the other two rather than rounded a third time.
* **One allocation policy for the whole repo**:
  `edge_domain::money::allocate_proportionally`, largest remainder with caps,
  shared by basket discounts and refund tender spreads. (ADR 031)
* **A tender never gives back more than it took.** A refund is spread pro-rata
  over what each original tender *applied* (`committed_amount`, never
  `requested_amount`), capped per tender by what it has left. A refund the
  tenders cannot cover is refused. Operator-chosen allocation is not built.
  (ADR 031)
* **Commercial acceptance and financial settlement are different facts.**
  `ACCEPTED` means the goods came back and the inventory return is written
  there, exactly once; from then on the refund is irrevocable. Only `CREATED`
  can fail. An unresolved provider refund keeps the goods returned and waits for
  reconciliation. (ADR 033)
* **`REFUND_RETURN` (`IN`) is not `MANUAL_IN`**, and the quantity returned is
  the physical one, exactly as on the way out. **`CASH_REFUND` (`OUT`) is not a
  reversal of a `CASH_SALE`**, and it reduces expected cash exactly once per
  cash refund payment. (ADR 033)
* **V1 restocks everything**, recorded as a value on the refund. There is no
  disposition engine. (ADR 033)
* **`UNKNOWN` is first class for refunds too.** A lost provider answer is never
  promoted to `FAILED` and never retried; starting again answers
  `REFUND_REQUIRES_RECONCILIATION`. The priority is never paying twice.
  (ADR 018, ADR 034)
* **Durable before the provider call**, for refunds as for charges: the refund
  payment, its attempt and its correlation id are committed before anything is
  sent. Refund attempts are append-only. (ADR 020, ADR 034)
* **One refund in progress per original sale**, enforced by a partial unique
  index. Validation and reservation happen in one database transaction. A
  completed refund blocks nothing while quantity remains; a replayed
  `request_id` is not a second refund. (ADR 030)
* **PI.Edge issues no fiscal document.** It records which one is owed:
  `NONE`, `CREDIT_NOTE_PENDING`, `CREDIT_NOTE_REQUESTED`. No CFDI, no stamping,
  no PAC, no SAT. (ADR 032)
* **`CREDIT_NOTE_REQUESTED` is a durable intention, not a result.** It records
  that the credit note was asked for; it never means stamped, issued or
  completed. That is why `CREDIT_NOTE_COMPLETED` and `CREDIT_NOTE_FAILED` do
  not exist yet. (ADR 032)
* **PI.Edge *consumes* fiscal metadata; PI.Edge is not the authority for
  declaring a sale invoiced.** Invoicing happens upstream. There is therefore
  **no production operation** that writes the fiscal state: none in Edge
  Protocol V1, none on `EdgeClient` or `IpcEdgeClient`, no arm in the daemon's
  dispatch, no CLI command. A till only ever meets `NOT_INVOICED`, written when
  the sale finalized. The invoiced case is arranged by a fixture behind the
  `testing` feature of `edge-application`, which the production binary does not
  enable. (ADR 032)
* **The fiscal consequence is constrained by the original sale**: never invoiced
  admits only `NONE`, invoiced must choose explicitly, and the original's fiscal
  state is materialised on the refund so a later invoice cannot rewrite the
  decision. The operator never declares the fiscal *state*, only the *action*.
  (ADR 032)
* **Fiscal metadata lives beside the sale, not on it**, because it has its own
  lifecycle: a sale can be finalized today and invoiced tomorrow. The row is
  written at finalization, so a missing row for a finalized sale is a bug rather
  than a third state. (ADR 032)
* **`PaymentProviderPort` gained `refund_payment` and `query_refund`** as
  methods of their own, beside `reverse_payment`, now that a consumer exists.
  `edge-payments` owns the provider capability; `edge-refunds` owns
  `RefundPayment` and `RefundAttempt`. `PaymentAttempt` is not reused.
  (ADR 034, amending ADR 019)
* **M11 Sync must publish refunds.** No outbox event was added in M9, because
  there is no consumer and the frozen rule is no events without one.

## Hardware (milestone 10)

* **Hardware failure is never a domain failure.** A sale is `FINALIZED` on the
  strength of its payments and its committed transaction. A printer that jams, a
  drawer that stays shut and a device that times out change no money, no stock
  and no sale — they are recorded and reported. (ADR 005, ADR 037)
* **A port arrives with its adapter.** `edge-hardware` holds `PrinterPort` and
  `CashDrawerPort` because both have adapters and consumers. There is no
  `CustomerDisplayPort` and no `PaymentTerminalPort`. (ADR 035)
* **There is no `ScannerPort`**: a scanner is input, not a capability the core
  invokes. (ADR 035, ADR 036)
* **Hardware is injected as trait objects**, not as type parameters on
  `EdgeApplication`. These are bounded I/O calls outside every transaction.
  (ADR 035)
* **A device is touched outside every database transaction**, in three steps:
  claim, call, record. The claim comes first, so a process that dies mid-action
  leaves evidence that it may already have happened. (ADR 020, ADR 037)
* **Configured is not available.** A device nobody has probed is `UNKNOWN`, and
  a restart returns every device to `UNKNOWN`. What survives is what a device
  *did* (`last_success_at`, rebuilt from the action log), never a claim that it
  works. A timeout is `DEGRADED`, not `UNAVAILABLE`: "we asked and we do not
  know" is a different fact. (ADR 035)
* **An automatic physical action happens once per commercial cause**, keyed by
  that cause (`payment_id`, `refund_payment_id`, `transaction_id`) in
  `hardware_actions`. A **diagnostic** request is unkeyed and opens the drawer
  or prints every time it is asked — an operator pressing a button is not a
  replay. (ADR 037)
* **Drawer rules, frozen**: a cash sale opens it once; a cash refund opens it
  once; a card sale and a card refund do not; a diagnostic opens it every time;
  **a reprint never opens it**; a replayed payment or refund opens it once.
  (ADR 037)
* **A reprint prints and changes nothing else** — no cash, no stock, no drawer,
  no change to the sale. The source of a reprint is the sale, never a stored
  job. (ADR 037, ADR 039)
* **Nothing retries a physical action automatically.** A printer retry is a
  human decision; a drawer is never retried in a loop; a scanner has nothing to
  retry. (ADR 037)
* **The scan parser lives once, in Rust**, in `edge-hardware`, and is tested
  without a device. The host that owns the window captures keystrokes and
  decides nothing: no buffer rule, no timeout, no terminator list, no product
  resolution. (ADR 036)
* **A scan's identity is the event, not the barcode.** Two scans of the same
  product are two units; one event delivered twice is one unit. Deduplicating by
  barcode would be a bug. (ADR 036)
* **A scan needs an editable sale.** With none open it answers
  `SCAN_NO_ACTIVE_TRANSACTION`; PI.Edge never opens a sale on its own. A scanned
  barcode then takes the ordinary `AddItem` path — same snapshot, same
  promotions, same basket freeze, and no hardware-specific product lookup.
  (ADR 036)
* **The application expresses a canonical `PrintDocument`; the adapter renders
  the physical protocol.** `edge-hardware::escpos` is the only module that may
  contain an escape sequence. No HTML, no browser printing, no template engine.
  (ADR 039, amending ADR 005)
* **The protocol carries intent, never capability.** No hardware payload has a
  field for a host, a port, an address, a driver, raw bytes or a method name, and
  a contract test asserts it. Configuration is read once in the composition
  root and is unreachable from downstream. (ADR 039)
* **A driver is chosen explicitly and never falls back.** `imin` configured with
  no bridge answers `HARDWARE_NOT_CONFIGURED`; it does not substitute an ESC/POS
  pulse, even when a printer is configured. No configured device implies
  another. (ADR 038)
* **Hardware simulators are explicit development and test adapters. Missing
  hardware configuration never selects a simulator implicitly.** An absent
  driver variable, or `none`, selects the unconfigured adapter, which answers
  `HARDWARE_NOT_CONFIGURED` to everything and reports
  `driver=unconfigured`. A misconfigured runtime must report that it is
  misconfigured rather than report healthy fake hardware — so `AVAILABLE` on a
  `simulator` nobody asked for can never happen. Test harnesses and demos ask
  for simulators explicitly. (ADR 038)
* **`EscPosKickDrawer` working does not close the iMin gate.** They are
  different devices on different wires. Physical validation of the iMin drawer
  from PI.Edge is a Swan gate that has **not** been run; the only iMin baseline
  is the previous POS. (ADR 038)
* **A `PrintDocument` is not a receipt.** M10 proves the transport, the
  rendering and the failure semantics; the receipt's own lifecycle arrived in
  milestone 12 as an obligation of the closed sale, not a state of it
  (ADR 039, ADR 043).

## Sync (milestone 11)

* **PI.Edge works indefinitely offline. Sync is an eventual capability, never a
  requirement to continue a locally permitted sale.** A register with no
  transport configured is not degraded: it sells, writes durable facts, keeps
  them, and closes its day without asking anyone. (ADR 040)
* **The commercial domain knows nothing about sync, and the sync engine knows
  nothing about HTTP.** `edge-sync` is pure policy — batch formation, ordering,
  backoff, outcome classification, message shapes — with no SQLite and no
  sockets. Orchestration and transactions live in
  `edge-application/sync_service.rs`. (ADR 040)
* **A batch is a unit of transport, never a unit of truth.** Every fact is
  answered on its own: acknowledged → `SENT`, transient → `PENDING` with a
  counted attempt and a scheduled retry, permanent → `FAILED`. (ADR 041)
* **`FAILED` is terminal, visible and never retried automatically.** It is not
  deleted, it carries its reason, and it waits for a person. An aggregate with a
  `FAILED` fact is skipped; every other aggregate keeps flowing. (ADR 041)
* **`SENDING` is not a resting state.** At startup every `SENDING` row goes back
  to `PENDING` and is resent with **the same `event_id`**; the consumer
  deduplicates. At-least-once, deliberately. **No leases, no distributed locks,
  no heartbeats, no worker-ownership framework** — there is one writer.
  (ADR 041)
* **`seq` is the durable local order** and governs deterministic batch
  formation. At most one fact per aggregate travels per batch, so version `n+1`
  is never in flight before `n` is acknowledged. No dependency graph.
  (ADR 041)
* **Nothing is purged.** `PENDING`, `SENDING`, `SENT` and `FAILED` are all kept.
  Retention and purge policy are deferred until there is a real operational
  need. (ADR 041)
* **Inbound: the effect commits before the cursor moves, never the reverse.** A
  crash in between re-delivers the message, which is safe because every inbound
  message is idempotent on something the core already had. The first message
  that cannot be applied stops the cycle; nothing after it is applied. An
  incomplete import stays `IMPORTING` and never becomes partially `ACTIVE`.
  (ADR 041)
* **There is exactly one catalog importer.** Inbound catalog drives the existing
  milestone-3 lifecycle — begin, chunk, commit, activate — and catalog, price
  books and promotions are one snapshot, not three channels. (ADR 041)
* **PI.Edge consumes fiscal metadata. PI.Edge does not declare a sale
  invoiced.** The inbound projection is the only production write to
  `transaction_fiscal_metadata`, it only ever moves
  `NOT_INVOICED -> INVOICED`, and there is no local operation, protocol
  message, EdgeClient method, CLI command or UI control that writes it.
  (ADR 032, amended by ADR 041)
* **A refund is published twice: `REFUND_ACCEPTED` (version 1) at `ACCEPTED`,
  `REFUND_COMPLETED` (version 2) at `COMPLETED`.** Acceptance is already a fact
  about the business — the goods are back and the return is irrevocable — and a
  refund can sit in `FINANCIAL_PENDING` indefinitely, so the consumer must not
  learn of the return only when the money resolves. Each event is enqueued in
  the same transaction as the transition it describes.
  `REFUND_FINANCIAL_PENDING` is not published: nothing consumes it. (ADR 041)
* **Three refusals of refund authority, three codes**, because the operator's
  next move differs: `REFUND_AUTHORITY_REQUIRED` (none configured),
  `REFUND_AUTHORITY_UNAVAILABLE` (configured, unreachable),
  `REFUND_NOT_AUTHORIZED` (reached, and it said no). (ADR 042)
* **Production refunds are online.** A refund needs central authorisation *at
  that moment*, for *that* operation. The answer is materialised on the refund
  as evidence and is **never** stored as a standing permission: no
  `sale_refund_authorized`, no reusable "centrally accepted" projection. An
  unreachable authority is the same answer as a refusal. PI.Edge sells offline;
  it does not refund offline. (ADR 042)
* **Hardware is not in the inbound contract.** No printer host, drawer driver,
  iMin bridge or device topology may arrive from the server; nor may identity —
  tenant, store, register and currency are provisioned, not synced. Inbound
  configuration is limited to commercial settings that already exist and already
  have a consumer. (ADR 040, extending ADR 039)
* **Sync simulators are explicit development and test adapters. Missing sync
  configuration never selects a simulator implicitly.** `PI_EDGE_SYNC` absent,
  empty or `none` selects no transport and every sync operation answers
  `SYNC_NOT_CONFIGURED`; an unrecognised value refuses to start. The same rule
  as hardware, for the same reason. (ADR 040, ADR 038)
* **There is no fake PI.COMMERCE Server.** `SimulatorSyncTransport` accepts
  batches, answers them as scripted, remembers the `event_id`s it has seen and
  delivers inbound messages a test put there. It implements none of the future
  server's business rules. And there is no HTTP: no client, no URL, no route, no
  authentication, no tenant routing. (ADR 040)

## Standalone POS closure (milestone 12)

* **`FINALIZED` means the sale is commercially closed**: payments resolved,
  cash already moved by the payment that moved it, stock moved, journal
  written, `SALE_FINALIZED` written. It does **not** mean paper came out, a
  receipt was confirmed, or the hardware is healthy — and it never will.
  (ADR 043)
* **`RECEIPT_COMPLETED` is removed from `RetailTransaction`.** The lifecycle is
  `OPEN → TOTALIZED → PAYMENT_IN_PROGRESS → COMMITTED → FINALIZED`, plus
  `OPEN/TOTALIZED → CANCELLED`. A receipt never changes the sale's state again.
  Migration 0011 refuses to run if any row is still in the removed state: there
  is no conversion to `FINALIZED` and the state is not preserved. (ADR 043)
* **A receipt is an obligation, recorded in `transaction_receipts`**, one row
  per sale, written **in the same transaction** as `FINALIZED`. There is no
  state in which a sale is closed and nobody knows a ticket is owed. It is not
  `hardware_actions`: that records what was attempted on a device, and one sale
  can produce a failed attempt, a retry and any number of reprints while still
  owing exactly one receipt. (ADR 043)
* **The receipt states are about certainty**: `PENDING`, `PRINTED`, `FAILED`
  (the printer certainly did not get the job — an explicit retry is safe) and
  `UNKNOWN` (bytes went out, no confirmation). **`UNKNOWN` is terminal and
  cannot be overwritten, not even by a later success**: the question is closed,
  unanswered, on purpose. (ADR 043)
* **The certainty comes from the adapter, not from a caller's guess.**
  `Unavailable` and `NotConfigured` mean nothing was sent; `Timeout` and `Io`
  mean something may have been. A **connection** that timed out is
  `Unavailable`, because a socket that never opened carried no bytes; `Timeout`
  keeps its M10 meaning of reached-and-silent. (ADR 043, sharpening ADR 035)
* **The core attempts the first receipt after the commercial commit**, outside
  every transaction, claim → call → record, with failure swallowed and
  recorded — exactly like the drawer. A client that dies cannot leave a sale
  without its ticket, and no client decides when a receipt is issued.
  (ADR 043)
* **No automatic retry, ever.** A `FAILED` receipt may be retried only because
  somebody asked; an `UNKNOWN` one may not be retried at all, only reprinted
  explicitly. `HARDWARE_ACTION_ALREADY_COMPLETED` is never used to express
  uncertainty. (ADR 043)
* **A reprint rewrites no history.** Explicit, repeatable, marked as a copy,
  and it changes nothing: no cash, no stock, no drawer, no sale — and **not the
  obligation**. A copy printed today says nothing about yesterday. (ADR 043,
  ADR 037)
* **Recovery distinguishes two `PENDING`s.** No device evidence at all means
  nothing was sent, so it is attempted; a claim with no resolution means the
  printer may already have the job, so it becomes `UNKNOWN` and nothing is
  printed; a resolved claim is simply applied. One deterministic pass at
  startup, no watcher. (ADR 043)
* **A receipt never blocks a close.** A session and a day close with receipts
  `PENDING`, `FAILED` or `UNKNOWN`. No money is unaccounted for because a
  printer jammed. **Both closes report the counts in their own result** —
  `SessionCloseResult` and `BusinessDayCloseResult { day, receipt_attention }`,
  the forced close included — so a caller never needs a second query at the
  moment it closes. `PRINTED` is not counted. **Each count is scoped to what
  that close is responsible for**: a session close to its own session, a day
  close (forced included) to its own day, the operational history to the
  installation — always through the sale's own `session_id` or
  `business_day_id`, never a window of time. Closing one till never reports
  another till's unresolved receipt. `BusinessDay` itself knows nothing about
  receipts or hardware. (ADR 043)
* **A sale with no money in it can be abandoned**, from `OPEN` or `TOTALIZED`
  only, moving nothing at all. Once a payment is applied the way out is a
  reversal or a refund (ADR 019). Without this an abandoned sale blocks the
  session close forever. (ADR 043)
* **The operational history is operational, not reporting.** Recent finalized
  sales, newest first, bounded, each with its own materialised total and its
  receipt state. No analytics, no search, no filters, no aggregates, and no
  money recomputed. (ADR 043)
* **There is no refund receipt lifecycle.** Nothing in the product requires a
  printed refund voucher today; a dedicated refund document is a future product
  decision, not symmetry to be built now. (ADR 043)

## Engineering rules (frozen in milestone 7)

The organisation and quality rules in **`CLAUDE.md`** at the repository root are
permanent instructions, on the same footing as the decisions above: architecture
and boundaries, organisation by domain rather than by task, no file-per-task, no
abstraction without an existing boundary, local helpers, minimal visibility,
clean imports, crates only for real boundaries, no legacy kept "just in case",
and the mandatory gates.

They are recorded there rather than duplicated here, because a rule copied into
two files drifts in one of them.
