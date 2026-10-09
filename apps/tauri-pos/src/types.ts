/**
 * Mirror of the `edge-protocol` V1 DTOs this screen consumes.
 *
 * Hand-written on purpose: the shapes are small, and writing them out keeps the
 * UI honest about what it is allowed to know. Anything not in here is not
 * available to React — in particular there is no way to compute a total or
 * decide a payment state on this side.
 */

export const PROTOCOL_VERSION = 1;

export interface Money {
  minor: number;
  currency: string;
  /** Rendered by the core. Display verbatim; never reformat or recompute. */
  formatted: string;
}

export interface Device {
  tenant_id: string;
  store_id: string;
  currency: string;
}

export interface BusinessDay {
  business_day_id: string;
  tenant_id: string;
  store_id: string;
  status: string;
  opened_at: string;
  closed_at: string | null;
  version: number;
}

export interface RegisterSession {
  session_id: string;
  business_day_id: string;
  register_id: string;
  operator_id: string;
  status: string;
  opening_cash: Money;
  opened_at: string;
  closed_at: string | null;
  version: number;
}

export interface TransactionLine {
  line_id: string;
  line_number: number;
  product_ref: string;
  description: string;
  quantity: string;
  unit_price: Money;
  /** Whether `unit_price` already includes tax. */
  price_mode: string;
  /** Book the price came from. */
  price_book_id: string | null;
  /** What promotions took off this line, allocations included. */
  discount_amount: Money;
  tax_amount: Money;
  line_total: Money;
}

/** One promotion as applied to the sale. Produced entirely by the core. */
export interface AppliedPromotion {
  adjustment_id: string;
  promotion_id: string;
  code: string;
  name: string;
  priority: number;
  /** `STACKABLE` or `EXCLUSIVE`. */
  stackability: string;
  /** `LINE` or `BASKET`. */
  target: string;
  line_number: number | null;
  amount: Money;
  /** Rule kind that produced it, e.g. `BUY_N_GET_M`. */
  reason: string;
  detail: string | null;
}

/** One concrete interaction with a payment provider. Append-only history. */
export interface PaymentAttempt {
  attempt_id: string;
  attempt_number: number;
  /** `CREATED`, `PROCESSING`, `AUTHORIZED`, `UNKNOWN`, `FAILED`, `REVERSAL_PENDING`, `REVERSED`. */
  status: string;
  requested_amount: Money;
  authorized_amount: Money | null;
  provider_kind: string;
  /** Minted by the core before the provider call; what a reconciliation asks about. */
  correlation_id: string;
  /** The provider's own identifier. Never a card number or a terminal payload. */
  provider_reference: string | null;
  /** Stable refusal reason, e.g. `SIMULATED_DECLINED`. */
  failure_code: string | null;
  /** The outcome is still open, so the sale cannot finalize. */
  unresolved: boolean;
  created_at: string;
  updated_at: string;
}

/** One tender's intention to cover part of a sale. */
export interface Payment {
  payment_id: string;
  transaction_id: string;
  /** `CASH` or `CARD`. The processor is `provider_kind`, not a tender. */
  tender_type: string;
  provider_kind: string | null;
  /** What this tender was asked for. For cash, what the customer handed over. */
  requested_amount: Money;
  authorized_amount: Money | null;
  /** What is applied against the sale's debt. */
  committed_amount: Money;
  change_due: Money;
  /** `CREATED`, `IN_PROGRESS`, `COMMITTED`, `FAILED` or `REVERSED`. */
  status: string;
  /** Empty for cash: there is no provider to interact with. */
  attempts: PaymentAttempt[];
  created_at: string;
  updated_at: string;
}

/** What a sale's payments mean for it. Computed entirely by the core. */
export interface Settlement {
  amount_due: Money;
  committed_total: Money;
  remaining_balance: Money;
  tendered_total: Money;
  change_due: Money;
  basket_locked: boolean;
  requires_resolution: boolean;
  requires_reconciliation: boolean;
  can_finalize: boolean;
}

export interface TransactionPayments {
  transaction_id: string;
  payments: Payment[];
  settlement: Settlement;
}

export interface Totals {
  subtotal: Money;
  discount_total: Money;
  /** Net tax base: what a receipt prints above the tax line. */
  net_total: Money;
  tax_total: Money;
  grand_total: Money;
}

export interface RetailTransaction {
  transaction_id: string;
  register_sequence: number;
  /** Snapshot and instant this sale is priced against. */
  catalog_version: number | null;
  pricing_effective_at: string | null;
  status: string;
  currency: string;
  lines: TransactionLine[];
  applied_promotions: AppliedPromotion[];
  totals: Totals;
  /** What the payments applied against the debt. */
  paid_total: Money;
  /** What customers handed over, cash overpayment included. */
  tendered_total: Money;
  balance_due: Money;
  change_due: Money;
  /** The payment lifecycle forbids basket changes. */
  basket_locked: boolean;
  /** A payment outcome is still open, so the sale cannot finalize. */
  requires_payment_resolution: boolean;
  created_at: string;
  finalized_at: string | null;
  version: number;
}

/** One drawer movement. Append-only: there is no update. */
export interface CashMovement {
  movement_id: string;
  /** `OPENING_FLOAT`, `CASH_SALE`, `CASH_IN`, `CASH_OUT`, `SAFE_DROP`. */
  movement_type: string;
  /** `IN` or `OUT`. Explicit, never the sign of the amount. */
  direction: string;
  amount: Money;
  /** `CASH_SALE` only: what the customer handed over. */
  tendered: Money | null;
  /** `CASH_SALE` only: what went back. */
  change: Money | null;
  /** The payment or safe drop that caused it. */
  source_ref: string | null;
  reason_code: string | null;
  approval_id: string | null;
  operator_id: string;
  created_at: string;
}

/** A request to move money to the safe, and its decision. */
export interface SafeDrop {
  safe_drop_id: string;
  approval_id: string;
  amount: Money;
  reason_code: string;
  /** `PENDING_APPROVAL`, `APPROVED` or `REJECTED`. Pending moves nothing. */
  status: string;
  requested_by: string;
  requested_at: string;
  approved_by: string | null;
  approved_at: string | null;
}

/**
 * What the drawer should contain.
 *
 * An operational view. A cashier's close path never receives this: it gets a
 * payload with no amount in it at all, and the expected figure appears only
 * after the count is declared.
 */
export interface CashPosition {
  session_id: string;
  register_id: string;
  operator_id: string;
  session_status: string;
  opening_float: Money;
  /** Net cash from committed cash sales: what stayed in the drawer. */
  cash_sales: Money;
  cash_in: Money;
  cash_out: Money;
  /** Approved drops only. */
  safe_drops: Money;
  /** Derived from the movements, never stored. */
  expected_cash: Money;
  /** Audit only: what customers handed over in cash. */
  tendered_total: Money;
  /** Audit only: what went back as change. */
  change_total: Money;
  movements: CashMovement[];
  pending_safe_drops: SafeDrop[];
}

/** One movement of the inventory ledger. Append-only: nothing edits these. */
export interface InventoryMovement {
  movement_id: string;
  product_ref: string;
  /** `OPENING_BALANCE`, `SALE`, `MANUAL_IN` or `MANUAL_OUT`. */
  movement_type: string;
  /** `IN` or `OUT`. Explicit, never the sign of the quantity. */
  direction: string;
  /** A magnitude, already rendered by the core: `0.750`. */
  quantity: string;
  quantity_thousandths: number;
  source_ref: string;
  transaction_id: string | null;
  line_number: number | null;
  reason_code: string | null;
  occurred_at: string;
}

/**
 * What this installation's ledger says of one product.
 *
 * A projection — last known baseline plus this installation's own movements —
 * not a store-wide count: PI.Edge runs per POS.
 *
 * `on_hand`, not `available`: there are no reservations, so the only honest
 * figure is what this ledger says is here. It can be negative.
 */
export interface InventoryPosition {
  store_id: string;
  product_ref: string;
  /** Rendered by the core, signed when the ledger went below zero. */
  on_hand: string;
  on_hand_thousandths: number;
  movement_count: number;
  last_movement_at: string | null;
  movements: InventoryMovement[];
}

export interface EdgeState {
  protocol_version: number;
  core_version: string;
  core_status: string;
  device: Device | null;
  business_day: BusinessDay | null;
  session: RegisterSession | null;
  active_transaction: RetailTransaction | null;
  pending_outbox: number;
}

export interface SnapshotCounts {
  tax_profiles: number;
  price_books: number;
  products: number;
  price_entries: number;
  barcodes: number;
  promotions: number;
}

export interface CatalogSnapshot {
  snapshot_id: string;
  store_id: string;
  version: number;
  status: string;
  declared_content_hash: string;
  /** Computed by the core from the rows it stored. */
  content_hash: string | null;
  generated_at: string;
  effective_at: string | null;
  expected: SnapshotCounts;
  imported_at: string;
  committed_at: string | null;
  activated_at: string | null;
  superseded_at: string | null;
}

export interface CatalogSnapshotView {
  snapshot: CatalogSnapshot;
  stored: SnapshotCounts;
}

export interface CatalogStatus {
  protocol_version: number;
  active: CatalogSnapshotView | null;
  pending: CatalogSnapshotView[];
  /** Open imports. These never price a sale. */
  importing: CatalogSnapshotView[];
}

export interface TaxProfile {
  code: string;
  name: string;
  rate_bps: number;
}

/** The price the core picked, and the book it came from. */
export interface ResolvedPrice {
  price_book_id: string;
  price_book_code: string;
  unit_price: Money;
  /** Whether `unit_price` already includes tax. */
  price_mode: string;
  effective_at: string;
}

export interface CatalogProduct {
  product_ref: string;
  external_ref: string | null;
  sku: string | null;
  name: string;
  active: boolean;
  sellable: boolean;
  tax_profile: string;
}

export interface ProductLookup {
  snapshot_id: string;
  snapshot_version: number;
  /** Instant the price was resolved at. */
  effective_at: string;
  product: CatalogProduct;
  tax_profile: TaxProfile;
  /** `null` when no price book applies at `effective_at`. */
  price: ResolvedPrice | null;
  /** Stable code explaining a missing price, e.g. `PRICE_NOT_FOUND`. */
  price_issue: string | null;
}

export type CoreStatus = "STARTING" | "READY" | "DEGRADED" | "STOPPING";

export interface CoreInfo {
  core_version: string;
  protocol_version: number;
  status: CoreStatus;
  database_schema_version: number;
  uptime_seconds: number;
  revision: number;
  pid: number;
}

export interface ConnectionStatus {
  connected: boolean;
  endpoint: string;
  mode: string;
  reconnect_in_ms: number | null;
  /** False whenever a mutating command must not be offered to the operator. */
  can_issue_commands: boolean;
}

/** Shape of every failure the bridge returns. `code` is stable; `message` is not. */
export interface BridgeError {
  code: string;
  message: string;
}

export interface StateChangedEvent {
  reason: string;
  revision: number;
}
