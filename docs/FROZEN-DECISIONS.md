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
* **`PAYMENT_IN_PROGRESS`, `COMMITTED`, `RECEIPT_COMPLETED` and `FINALIZED`
  imply an immutable basket.**

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
  totalize, not when money is taken. `RECEIPT_COMPLETED` is not a competing
  moment. (ADR 027)
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

## Engineering rules (frozen in milestone 7)

The organisation and quality rules in **`CLAUDE.md`** at the repository root are
permanent instructions, on the same footing as the decisions above: architecture
and boundaries, organisation by domain rather than by task, no file-per-task, no
abstraction without an existing boundary, local helpers, minimal visibility,
clean imports, crates only for real boundaries, no legacy kept "just in case",
and the mandatory gates.

They are recorded there rather than duplicated here, because a rule copied into
two files drifts in one of them.
