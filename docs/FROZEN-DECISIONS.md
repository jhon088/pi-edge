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

## Engineering rules (frozen in milestone 7)

The organisation and quality rules in **`CLAUDE.md`** at the repository root are
permanent instructions, on the same footing as the decisions above: architecture
and boundaries, organisation by domain rather than by task, no file-per-task, no
abstraction without an existing boundary, local helpers, minimal visibility,
clean imports, crates only for real boundaries, no legacy kept "just in case",
and the mandatory gates.

They are recorded there rather than duplicated here, because a rule copied into
two files drifts in one of them.
