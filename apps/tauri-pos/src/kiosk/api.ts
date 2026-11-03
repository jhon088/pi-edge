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
  AdminProduct,
  AssetRef,
  Authorization,
  CashGenicSettings,
  CashGenicStatus,
  DayTally,
  BridgeError,
  Menu,
  PrinterConfiguration,
  PaymentGate,
  PrinterSettings,
  PrinterStatus,
  ProductEdit,
  ProductConfiguration,
  RequestedSelection,
  RetailTransaction,
  KioskPreferences,
  Vertical,
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
    case "PRODUCT_NOT_FOUND":
      return "No reconocimos ese código. Intenta de nuevo o pide ayuda.";
    case "CATALOG_INVALID_BARCODE":
      return "Ese código no se leyó bien. Vuelve a pasar el producto.";
    case "SCAN_NO_ACTIVE_TRANSACTION":
      return "Pasa el producto otra vez para empezar.";
    case "AUTHORIZATION_REQUIRED":
    case "PRINTER_UNAVAILABLE":
      // The host composed these from its own gates, in the words the brief
      // asked for. Passed through rather than rewritten here, so there is one
      // wording and the log and the screen agree.
      return value.message;
    case "AUTHORIZATION_PIN_REFUSED":
      return "Ese PIN no es el de este kiosko.";
    case "AUTHORIZATION_REQUEST_UNKNOWN":
      return "El carrito cambió. Vuelve a intentar el pago.";
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
/**
 * May a payment be started at all?
 *
 * Asked before the payment screen opens, so a customer is told what is wrong
 * instead of watching a tender choice fail. Two reasons it can say no — the
 * printer cannot follow through, or the basket is waiting for somebody to
 * authorise it — and both come back as a value rather than a throw, because
 * the screen is choosing what to draw, not handling a failure.
 *
 * `pay` refuses on its own account too, so this is the courteous half of a
 * rule that holds either way.
 */
export function paymentAllowed(): Promise<PaymentGate> {
  return invoke<PaymentGate>("kiosk_payment_allowed");
}

/** What, if anything, the lane is waiting on. */
export function pendingAuthorization(): Promise<Authorization> {
  return invoke<Authorization>("authorization_pending");
}

/**
 * Release the waiting authorization with the PIN an operator typed.
 *
 * Rejects with `AUTHORIZATION_PIN_REFUSED` for a wrong PIN and
 * `AUTHORIZATION_REQUEST_UNKNOWN` when the basket changed underneath — the
 * second is a real case, not a defensive branch: the customer may have
 * scanned something while somebody walked over.
 */
export function approveAuthorization(
  requestId: string,
  pin: string,
): Promise<Authorization> {
  return invoke<Authorization>("authorization_approve", { requestId, pin });
}

export function pay(
  tenderType: string,
  amountMinor: number,
): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("kiosk_pay", { tenderType, amountMinor });
}

// ==================================================================== retail

/**
 * What this box is set up as: which lane, which channel, and whether the
 * retail preview shows the customer their own camera.
 *
 * Asked of the host rather than remembered by the screen, so the answers
 * survive a reload and an operator's change takes effect on the next open.
 */
export function preferences(): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("kiosk_preferences");
}

/**
 * Switch lanes.
 *
 * The host abandons whatever sale is open on the way through: a basket built
 * at a self-checkout must not reappear on a restaurant menu.
 */
export function setVertical(vertical: Vertical): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("set_vertical_setting", { vertical });
}

// ================================================================ showroom
//
// Demo surface: what this box is called, whether it is open, what it has sold
// today, and the note acceptor. None of it is core state — the core owns the
// money and the sale; these are the labels around them.

/** Name this machine: which branch, which kiosk, which cashier. */
export function setIdentity(
  branchId: string,
  kioskId: string,
  cashierId: string,
): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("set_identity", { branchId, kioskId, cashierId });
}

/**
 * Open or close the kiosk to customers.
 *
 * Closed is a sign on the door: the screen will not start a sale, the
 * application keeps running and Admin keeps working.
 */
export function setKioskOpen(open: boolean): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("set_kiosk_open", { open });
}

export function dayTally(): Promise<DayTally> {
  return invoke<DayTally>("day_tally");
}

/**
 * Add a finished sale to today's tally.
 *
 * Called once the core has finalized, never before and never instead of it.
 * If it fails the sale is still a sale; only the showroom's counter is wrong.
 */
export function recordSale(
  tenderType: string,
  amountMinor: number,
): Promise<DayTally> {
  return invoke<DayTally>("record_sale", { tenderType, amountMinor });
}

export function closeDay(): Promise<DayTally> {
  return invoke<DayTally>("close_day");
}

export function openDay(): Promise<DayTally> {
  return invoke<DayTally>("open_day_command");
}

// --------------------------------------------------------- the note acceptor

export function saveCashGenic(
  settings: CashGenicSettings,
): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("cashgenic_save", { settings });
}

/** The "probar conexión" button: ask the device who it is. */
export function probeCashGenic(): Promise<CashGenicStatus> {
  return invoke<CashGenicStatus>("cashgenic_probe");
}

/** What it is doing, against what is owed. Polled while a customer pays. */
export function cashGenicStatus(dueMinor: number): Promise<CashGenicStatus> {
  return invoke<CashGenicStatus>("cashgenic_status", { dueMinor });
}

/** Start taking notes towards an amount the core computed. */
export function cashGenicPay(amountMinor: number): Promise<CashGenicStatus> {
  return invoke<CashGenicStatus>("cashgenic_pay", { amountMinor });
}

export function cashGenicCancel(): Promise<void> {
  return invoke("cashgenic_cancel");
}

export function cashGenicClose(): Promise<void> {
  return invoke("cashgenic_close");
}

/**
 * Turn the customer-facing camera preview on or off.
 *
 * Stored by the host and acted on by the retail shell. Nothing is started
 * here, and nothing asks for permission until the lane actually opens one.
 */
export function setCameraPreview(enabled: boolean): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("set_camera_preview", { enabled });
}

/**
 * One keystroke, on its way to the scan parser in Rust.
 *
 * Answers the sale only when a scan actually completed — so this screen
 * learns nothing it could act on until the core has already acted (ADR 036).
 * The lane opens on the first scan; the host does that, not this.
 */
export function scanKey(key: string): Promise<RetailTransaction | null> {
  return invoke<RetailTransaction | null>("retail_scan_key", { key });
}

/** A whole barcode, as typed on the lane's keypad. */
export function scanBarcode(barcode: string): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("retail_scan", { barcode });
}

/** Add something the customer found by looking for it instead of scanning. */
export function retailAddItem(
  productRef: string,
  quantity: number,
): Promise<RetailTransaction> {
  return invoke<RetailTransaction>("retail_add_item", { productRef, quantity });
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
 * How the printer is.
 *
 * `refresh` asks the device now rather than reading the core's cached answer.
 * Admin asks fresh when an operator presses the button and cached while the
 * panel is just sitting there.
 */
export function printerStatus(refresh: boolean): Promise<PrinterStatus> {
  return invoke<PrinterStatus>("admin_printer_status", { refresh });
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

// ------------------------------------------------- products, prices, the PIN

/**
 * The real products of one experience.
 *
 * The core's own menu for that channel, with this kiosk's edits laid over it.
 * Asked per experience rather than cached, because switching experiences in
 * Admin is a different channel and a different shelf.
 */
export function adminProducts(channel: string): Promise<AdminProduct[]> {
  return invoke<AdminProduct[]>("admin_products", { channelRef: channel });
}

/**
 * Save a batch of product edits, and make the prices real.
 *
 * Batched because each price has to reach the catalog in force, and the only
 * honest way there is a new active snapshot built by the same pipeline a
 * release uses — a second or two, and a catalog version. Eight prices in one
 * sitting is one import.
 *
 * Answers the list read back **from the core**, so what the screen shows next
 * is what the till charges rather than what was typed.
 */
export function saveProducts(edits: ProductEdit[]): Promise<AdminProduct[]> {
  return invoke<AdminProduct[]>("admin_save_products", { edits });
}

/** Set the PIN, or pass `""` to remove it. */
export function setPin(pin: string): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("admin_set_pin", { pin });
}

/** Let the control endpoints answer, or stop them answering. */
export function setRemoteControl(enabled: boolean): Promise<KioskPreferences> {
  return invoke<KioskPreferences>("admin_set_remote_control", { enabled });
}
