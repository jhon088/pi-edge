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
  tax_amount: Money;
  line_total: Money;
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
  totals: Totals;
  paid_total: Money;
  balance_due: Money;
  change_due: Money;
  created_at: string;
  finalized_at: string | null;
  version: number;
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
