/**
 * The kiosk's only door to the core.
 *
 * Every call is a Tauri command that forwards one `EdgeClient` request over
 * IPC. Nothing is cached here except decoded images, and nothing is computed:
 * prices, discounts and totals all arrive from the core already decided
 * (ADR 002, ADR 046).
 */
import { invoke } from "@tauri-apps/api/core";

import type {
  AssetRef,
  BridgeError,
  Menu,
  PrinterConfiguration,
  PrinterSettings,
  ProductConfiguration,
  RequestedSelection,
  RetailTransaction,
} from "./types";

export function isBridgeError(value: unknown): value is BridgeError {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    "message" in value
  );
}

/** What to put in front of a customer when something goes wrong. */
export function describe(value: unknown): string {
  if (!isBridgeError(value)) {
    return "Algo salió mal. Intenta de nuevo.";
  }
  switch (value.code) {
    case "CATALOG_SELECTION_REQUIRED":
      return "Falta elegir una opción obligatoria.";
    case "CATALOG_SELECTION_QUANTITY_REFUSED":
      return "Esa cantidad no está disponible para esta opción.";
    case "CATALOG_OPTION_NOT_OFFERED":
      return "Esa opción ya no está disponible.";
    case "NO_ACTIVE_TRANSACTION":
      return "Tu pedido se cerró. Empecemos de nuevo.";
    case "NO_OPEN_REGISTER_SESSION":
    case "NO_OPEN_BUSINESS_DAY":
      return "El kiosko no está abierto todavía.";
    case "CATALOG_NO_ACTIVE_SNAPSHOT":
      return "El menú aún no se ha cargado en este kiosko.";
    case "NOT_CONNECTED":
      return "Sin conexión con la caja. Avisa a un miembro del equipo.";
    default:
      return value.message;
  }
}

export function fetchMenu(channel: string): Promise<Menu> {
  return invoke<Menu>("kiosk_menu", { channelRef: channel });
}

export function fetchConfiguration(
  productRef: string,
  channel: string,
): Promise<ProductConfiguration> {
  return invoke<ProductConfiguration>("kiosk_product_configuration", {
    productRef,
    channelRef: channel,
  });
}

export function activeTransaction(): Promise<RetailTransaction | null> {
  return invoke<RetailTransaction | null>("kiosk_active_transaction");
}

export function startTransaction(): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_start_transaction");
}

export function addItem(
  productRef: string,
  quantity: number,
  selections: RequestedSelection[],
  preparationNote: string | null,
): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_add_item", {
    productRef,
    quantity,
    selections,
    preparationNote,
  });
}

export function removeItem(lineNumber: number): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_remove_item", { lineNumber });
}

export function changeQuantity(
  lineNumber: number,
  quantity: number,
): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_change_quantity", {
    lineNumber,
    quantity,
  });
}

export function cancelTransaction(reason: string): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_cancel_transaction", {
    reasonCode: reason,
  });
}

export function totalize(): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_totalize");
}

/**
 * Take the money and close the sale.
 *
 * The amount is the core's own grand total, handed straight back: the screen
 * never decides what is owed, and never decides that it was paid.
 */
export function pay(
  tenderType: string,
  amountMinor: number,
): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_pay", { tenderType, amountMinor });
}

/** Decoded images, for the life of the window. */
const assets = new Map<string, Promise<string>>();

export function assetUrl(asset: AssetRef | null): Promise<string | null> {
  if (!asset) {
    return Promise.resolve(null);
  }
  const cached = assets.get(asset.content_hash);
  if (cached) {
    return cached;
  }
  const pending = invoke<string>("kiosk_asset", { asset }).catch(() => "");
  assets.set(asset.content_hash, pending);
  return pending.then((url) => url || null);
}

// ===================================================================== admin

export function fetchPrinterSettings(): Promise<PrinterConfiguration> {
  return invoke<PrinterConfiguration>("admin_printer_settings");
}

export function savePrinterSettings(
  settings: PrinterSettings,
): Promise<PrinterConfiguration> {
  return invoke<PrinterConfiguration>("admin_save_printer_settings", {
    settings,
  });
}

export function testPrint(): Promise<unknown> {
  return invoke("admin_test_print");
}

/**
 * Put the receipt out for a sale that is already closed.
 *
 * Answers whether paper came out, never throws for a printer problem: the
 * sale is finalized before this runs and a jam is not the customer's problem.
 */
export function printReceipt(transactionId: string): Promise<boolean> {
  return invoke<boolean>("admin_print_receipt", { transactionId });
}

/**
 * Put the kiosk back to a clean state between visitors.
 *
 * Abandons whatever order is open and nothing else: the menu, the
 * photographs and the printer settings all survive it.
 */
export function resetDemo(): Promise<void> {
  return invoke("admin_reset_demo");
}
