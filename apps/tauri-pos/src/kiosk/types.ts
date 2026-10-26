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
}

export interface PrinterModel {
  id: string;
  label: string;
  paper_mm: number;
  columns: number;
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
}
