/**
 * The only door between React and the core.
 *
 * Each function forwards to a Tauri command that is itself nothing more than an
 * `EdgeClient` call over IPC. No business logic lives on this side of the
 * boundary, and no state is cached here: when something changes, we re-ask the
 * core.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  BridgeError,
  CashPosition,
  CatalogStatus,
  ConnectionStatus,
  CoreInfo,
  EdgeState,
  HardwareStatus,
  InventoryPosition,
  ProductLookup,
  RecentSales,
  RetailTransaction,
  Refund,
  RefundEligibility,
  StateChangedEvent,
  SyncStatus,
  TransactionPayments,
} from "./types";

/** Event the Rust bridge emits when the core says its state changed. */
export const STATE_CHANGED_EVENT = "edge://state-changed";

export function isBridgeError(value: unknown): value is BridgeError {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    "message" in value
  );
}

export function describeError(value: unknown): BridgeError {
  if (isBridgeError(value)) {
    return value;
  }
  return { code: "UNKNOWN", message: String(value) };
}

export function fetchCoreInfo(): Promise<CoreInfo> {
  return invoke<CoreInfo>("core_info");
}

export function fetchEdgeState(): Promise<EdgeState> {
  return invoke<EdgeState>("edge_state");
}

export function fetchConnection(): Promise<ConnectionStatus> {
  return invoke<ConnectionStatus>("connection_status");
}

export function fetchCatalogStatus(): Promise<CatalogStatus> {
  return invoke<CatalogStatus>("catalog_status");
}

/**
 * Every payment of one sale, with its attempts and what they settle.
 *
 * Read-only. All the arithmetic — applied total, remaining balance, change,
 * whether the sale may finalize — is the core's; this only asks for it.
 */
export function fetchTransactionPayments(
  transactionId: string,
): Promise<TransactionPayments> {
  return invoke<TransactionPayments>("transaction_payments", {
    transactionId,
  });
}

/**
 * What the drawer should contain, and the movements that explain it.
 *
 * Read-only, and *not* the cashier's pre-close payload: the blind close uses a
 * separate operation whose answer carries no amount at all.
 */
export function fetchCashPosition(): Promise<CashPosition> {
  return invoke<CashPosition>("cash_position");
}

/**
 * What the ledger says is here, and the movements that explain it.
 *
 * Read-only. The balance is the core's fold over its own append-only ledger;
 * this side adds nothing up.
 */
export function fetchInventoryPosition(
  productRef: string,
  movementLimit: number,
): Promise<InventoryPosition> {
  return invoke<InventoryPosition>("inventory_position", {
    productRef,
    movementLimit,
  });
}

/**
 * What a closed sale can still give back, and the refunds it already has.
 *
 * Read-only, and every refundable figure is the core's: a refund is priced
 * from the original sale's materialised economics, never re-priced here.
 */
export function fetchRefundEligibility(
  transactionId: string,
): Promise<RefundEligibility> {
  return invoke<RefundEligibility>("refund_eligibility", { transactionId });
}

export function fetchTransactionRefunds(
  transactionId: string,
): Promise<Refund[]> {
  return invoke<Refund[]>("transaction_refunds", { transactionId });
}

/**
 * What each device is and what the runtime knows about it.
 *
 * Read-only, and probing is the core's job: this never learns an address.
 */
export function fetchHardwareStatus(): Promise<HardwareStatus> {
  return invoke<HardwareStatus>("hardware_status");
}

/** Recent sales with their receipt state. Read-only. */
export function fetchRecentSales(limit: number): Promise<RecentSales> {
  return invoke<RecentSales>("recent_sales", { limit });
}

/** What sync knows. Read-only: the window never triggers a dispatch cycle. */
export function fetchSyncStatus(): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_status");
}

/** Print a diagnostic page. Carries no bytes and no address. */
export function printTestPage(): Promise<HardwareStatus> {
  return invoke<HardwareStatus>("print_test_page");
}

/** Open the drawer because the operator asked. Audited by the core. */
export function openDrawer(): Promise<HardwareStatus> {
  return invoke<HardwareStatus>("open_drawer");
}

/**
 * Forward one keystroke from a keyboard-wedge scanner.
 *
 * The parser lives in Rust: this only passes the key along, and gets the sale
 * back when — and only when — a complete barcode became an item. No rule about
 * what a scan is exists on this side.
 */
export function scannerKey(key: string): Promise<RetailTransaction | null> {
  return invoke<RetailTransaction | null>("scanner_key", { key });
}

/** Resolve a barcode the way a sale would. The core prices it, not this code. */
export function lookupBarcode(barcode: string): Promise<ProductLookup> {
  return invoke<ProductLookup>("lookup_barcode", { barcode });
}

export function onStateChanged(
  handler: (event: StateChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<StateChangedEvent>(STATE_CHANGED_EVENT, (event) => {
    handler(event.payload);
  });
}
