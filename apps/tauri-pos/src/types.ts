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
  tax_amount: Money;
  line_total: Money;
}

export interface Totals {
  subtotal: Money;
  discount_total: Money;
  tax_total: Money;
  grand_total: Money;
}

export interface RetailTransaction {
  transaction_id: string;
  register_sequence: number;
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
