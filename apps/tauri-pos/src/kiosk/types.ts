/**
 * Wire types the kiosk reads.
 *
 * Mirrors the `V1` DTOs of `edge-protocol`. Money is always `{ minor,
 * currency, formatted }` and the kiosk renders `formatted` verbatim: it never
 * divides anything by 100 and never adds two amounts together.
 */

export interface Money {
  minor: number;
  currency: string;
  formatted: string;
}

export interface AssetRef {
  content_hash: string;
  media_type: string;
  byte_size: number;
}

export interface MenuCategory {
  category_ref: string;
  name: string;
  display_order: number;
  image: AssetRef | null;
}

export interface MenuItem {
  product_ref: string;
  category_ref: string | null;
  name: string;
  description: string | null;
  display_order: number;
  image: AssetRef | null;
  from_price: Money;
  configurable: boolean;
  /**
   * What the catalog says to offer alongside this one, in the order to offer
   * it, already narrowed to products this menu shows. The screens read it;
   * they do not work it out (ADR 046).
   */
  recommended_product_refs: string[];
}

export interface MenuPromotion {
  promotion_id: string;
  code: string;
  name: string;
}

export interface Menu {
  snapshot_id: string;
  currency: string;
  categories: MenuCategory[];
  items: MenuItem[];
  promotions: MenuPromotion[];
}

export interface SelectionOption {
  option_id: string;
  name: string;
  component_ref: string | null;
  unit_price: Money;
  max_quantity: number;
  default_quantity: number;
  display_order: number;
  image: AssetRef | null;
}

export interface SelectionGroup {
  group_id: string;
  owner_option_id: string | null;
  name: string;
  min_quantity: number;
  max_quantity: number;
  display_order: number;
  options: SelectionOption[];
}

export interface ProductConfiguration {
  product_ref: string;
  name: string;
  base_price: Money;
  groups: SelectionGroup[];
}

export interface RequestedSelection {
  option_id: string;
  quantity: number;
}

export interface TransactionSelection {
  option_id: string;
  group_id: string;
  group_name: string;
  name: string;
  quantity: number;
  component_ref: string | null;
  chargeable: boolean;
  unit_price: Money;
  amount: Money;
}

export interface TransactionLine {
  line_id: string;
  line_number: number;
  product_ref: string;
  description: string;
  quantity: string;
  quantity_thousandths: number;
  unit_price: Money;
  base_unit_price: Money;
  configured_unit_price: Money;
  selections: TransactionSelection[];
  preparation_note: string | null;
  extended_amount: Money;
  discount_amount: Money;
  line_total: Money;
}

export interface Totals {
  subtotal: Money;
  discount_total: Money;
  net_total: Money;
  tax_total: Money;
  grand_total: Money;
}

export interface AppliedPromotion {
  adjustment_id: string;
  promotion_id: string;
  code: string;
  name: string;
  amount: Money;
}

export interface RetailTransaction {
  transaction_id: string;
  status: string;
  currency: string;
  lines: TransactionLine[];
  applied_promotions: AppliedPromotion[];
  totals: Totals;
  running_totals: Totals;
  order_code: string;
  balance_due: Money;
}

/**
 * Which lane this installation opens in.
 *
 * One build, one core, one catalog: the vertical selects a shell and the
 * channel it reads the menu for, and nothing else (ADR 044).
 */
export type Vertical = "RESTAURANT" | "TIM_HORTONS" | "SBARRO" | "RETAIL";

export interface CashGenicSettings {
  enabled: boolean;
  host: string;
  port: number;
  username: string;
  password: string;
}

export interface KioskPreferences {
  vertical: Vertical;
  /** The channel this lane sells from. Decided by the host, not the screen. */
  channel_ref: string;
  /** Whether the retail lane shows the customer their own camera. */
  camera_preview: boolean;
  /** What the stand calls this machine. Free text, typed in by an operator. */
  branch_id: string;
  kiosk_id: string;
  cashier_id: string;
  /** Whether the kiosk will take an order at all. */
  kiosk_open: boolean;
  cashgenic: CashGenicSettings;
}

/** What this screen has put through since the day was opened. */
export interface DayTally {
  state: "OPEN" | "CLOSED";
  transaction_count: number;
  sales_total_minor: number;
  cash_total_minor: number;
  card_total_minor: number;
  opened_at: string | null;
  closed_at: string | null;
}

/** What the note acceptor is doing, as the payment screen draws it. */
export interface CashGenicStatus {
  online: boolean;
  device: string | null;
  state: string;
  deposited_minor: number;
  outstanding_minor: number;
  change_minor: number;
  cash_total_minor: number | null;
  detail: string | null;
}

export interface BridgeError {
  code: string;
  message: string;
}

// ===================================================================== admin

export interface PrinterSettings {
  enabled: boolean;
  model: string;
  queue: string;
  columns: number;
  auto_print: boolean;
  /**
   * Where to ask this printer how it is. Empty, `usb`, `usb:<fragment>`, or
   * `host[:port]`.
   *
   * A separate channel from the queue that prints, because a spooler queue is
   * write-only: there is no way to get a `DLE EOT` reply back out of one.
   * `usb` and an address are the real bidirectional channels — the firmware's
   * own answer, including paper near its end. Empty falls back to what the
   * host spooler reports, which is less but is not nothing.
   */
  status_channel: string;
}

/**
 * How the printer is, right now.
 *
 * Every field is something that answered, or the `UNKNOWN` that says nothing
 * did. `blocks_payment` is the core's own answer and the screen renders it
 * rather than re-deriving it from the flags.
 */
export interface PrinterStatus {
  online: boolean;
  state: "ONLINE" | "OFFLINE" | "ERROR" | "NO_RESPONSE" | "UNKNOWN";
  cover_open: boolean;
  paper: "OK" | "NEAR_END" | "END" | "UNKNOWN";
  cutter_error: boolean;
  /** A recoverable error: on a receipt printer, a jam or a lifted head. */
  jammed: boolean;
  unrecoverable_error: boolean;
  /** Which source can answer for this printer, not which one answered last. */
  status_source: "DLE_EOT" | "SPOOLER" | "NONE";
  /** `null` until the first read. */
  status_checked_at: string | null;
  status_error: string | null;
  blocks_payment: boolean;
}

export interface PrinterModel {
  id: string;
  label: string;
  paper_mm: number;
  columns: number;
  /** What this model's firmware can report about itself. */
  reports_paper: boolean;
  reports_jam: boolean;
  reports_online: boolean;
  /** The paper states it can produce. `["UNKNOWN"]` when it reports nothing. */
  paper_states: string[];
}

export interface PrinterQueue {
  name: string;
  accepting: boolean;
}

export interface PrinterConfiguration {
  settings: PrinterSettings;
  models: PrinterModel[];
  /** What the host spooler reports. Empty means it has none. */
  available: PrinterQueue[];
  driver: string;
  /** `null` when nothing is configured, so "unreachable" is not implied. */
  reachable: boolean | null;
  /**
   * USB printers this host exposes a **readable** interface for.
   *
   * What a `status_channel` of `usb` can be pointed at. Empty where the host
   * has none, or where the platform has no such interface at all.
   */
  status_devices: string[];
}
