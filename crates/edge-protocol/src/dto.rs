//! Version 1 data transfer objects.
//!
//! Pure `serde` types: no domain, no application, no transport. Conversions to
//! and from the core live in [`crate::mapping`], behind the `core-mapping`
//! feature.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A monetary amount as seen by clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyV1 {
    pub minor: i64,
    pub currency: String,
    /// Core-rendered representation. Clients display this verbatim.
    pub formatted: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceV1 {
    pub tenant_id: String,
    pub store_id: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessDayV1 {
    pub business_day_id: String,
    pub tenant_id: String,
    pub store_id: String,
    pub status: String,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterSessionV1 {
    pub session_id: String,
    pub business_day_id: String,
    pub register_id: String,
    pub operator_id: String,
    pub status: String,
    pub opening_cash: MoneyV1,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionLineV1 {
    pub line_id: String,
    pub line_number: i64,
    pub product_ref: String,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub description: String,
    pub quantity: String,
    pub quantity_thousandths: i64,
    pub unit_price: MoneyV1,
    pub tax_rate_bps: u32,
    /// Whether `unit_price` already includes tax.
    pub price_mode: String,
    /// Price book the unit price came from.
    pub price_book_id: Option<String>,
    /// `unit_price * quantity`, in the quoted price mode.
    pub extended_amount: MoneyV1,
    pub discount_amount: MoneyV1,
    /// Net tax base after discount.
    pub taxable_amount: MoneyV1,
    pub tax_amount: MoneyV1,
    pub line_total: MoneyV1,
}

/// One promotion as applied to one sale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedPromotionV1 {
    /// Deterministic composite of promotion and target: re-evaluating the same
    /// basket produces the same id.
    pub adjustment_id: String,
    pub promotion_id: String,
    pub code: String,
    pub name: String,
    pub priority: i32,
    /// `STACKABLE` or `EXCLUSIVE`.
    pub stackability: String,
    /// `LINE` or `BASKET`.
    pub target: String,
    /// Set exactly when `target` is `LINE`.
    pub line_number: Option<i64>,
    /// Positive discount amount.
    pub amount: MoneyV1,
    /// Rule kind that produced it, e.g. `BUY_N_GET_M`.
    pub reason: String,
    /// Human detail, e.g. `2 application(s), 2 free unit(s)`.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotalsV1 {
    /// Sum of quoted extended amounts, before discount.
    pub subtotal: MoneyV1,
    pub discount_total: MoneyV1,
    /// Sum of net tax bases: the subtotal a receipt prints above the tax line.
    pub net_total: MoneyV1,
    pub tax_total: MoneyV1,
    pub grand_total: MoneyV1,
}

/// One tender's intention to cover part of a sale, with its attempt history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentV1 {
    pub payment_id: String,
    pub transaction_id: String,
    /// `CASH` or `CARD`. The processor is `provider_kind`, not a tender.
    pub tender_type: String,
    /// Set exactly for tenders settled through a provider.
    pub provider_kind: Option<String>,
    /// What this tender was asked to cover. For cash, what the customer
    /// handed over.
    pub requested_amount: MoneyV1,
    /// What the provider approved, when it has answered.
    pub authorized_amount: Option<MoneyV1>,
    /// What is applied against the sale's debt.
    pub committed_amount: MoneyV1,
    /// What comes back to the customer for this tender.
    pub change_due: MoneyV1,
    /// `CREATED`, `IN_PROGRESS`, `COMMITTED`, `FAILED` or `REVERSED`.
    pub status: String,
    /// Empty for cash: there is no provider to interact with.
    pub attempts: Vec<PaymentAttemptV1>,
    pub created_at: String,
    pub updated_at: String,
}

/// One concrete interaction with a provider. Append-only history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentAttemptV1 {
    pub attempt_id: String,
    pub attempt_number: i64,
    /// `CREATED`, `PROCESSING`, `AUTHORIZED`, `UNKNOWN`, `FAILED`,
    /// `REVERSAL_PENDING` or `REVERSED`.
    pub status: String,
    pub requested_amount: MoneyV1,
    pub authorized_amount: Option<MoneyV1>,
    pub provider_kind: String,
    /// Minted by the core before the provider was called; what a
    /// reconciliation asks about.
    pub correlation_id: String,
    pub provider_reference: Option<String>,
    /// Stable refusal reason. Never a raw provider payload.
    pub failure_code: Option<String>,
    /// The financial outcome is still open, so the sale cannot finalize.
    pub unresolved: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// What a sale's payments mean for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementV1 {
    pub amount_due: MoneyV1,
    pub committed_total: MoneyV1,
    pub remaining_balance: MoneyV1,
    /// What customers handed over, cash overpayment included.
    pub tendered_total: MoneyV1,
    pub change_due: MoneyV1,
    /// The basket cannot change while this is true.
    pub basket_locked: bool,
    /// A payment has an open outcome, so the sale cannot finalize.
    pub requires_resolution: bool,
    /// The way out is an explicit provider query.
    pub requires_reconciliation: bool,
    pub can_finalize: bool,
}

/// What every payment command answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentResultV1 {
    pub payment: PaymentV1,
    pub transaction: RetailTransactionV1,
    pub settlement: SettlementV1,
}

/// Every payment of one sale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionPaymentsV1 {
    pub transaction_id: String,
    pub payments: Vec<PaymentV1>,
    pub settlement: SettlementV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetailTransactionV1 {
    pub transaction_id: String,
    pub business_day_id: String,
    pub session_id: String,
    pub store_id: String,
    pub register_id: String,
    pub operator_id: String,
    pub register_sequence: i64,
    pub status: String,
    pub currency: String,
    /// Snapshot that priced this sale, captured when it started.
    pub catalog_snapshot_id: Option<String>,
    pub catalog_version: Option<i64>,
    /// Instant every line of this sale is priced at.
    pub pricing_effective_at: Option<String>,
    pub lines: Vec<TransactionLineV1>,
    /// Every promotion this sale granted, in the order the engine applied
    /// them. Materialised on the sale, so it stays readable after the snapshot
    /// that produced it is gone.
    pub applied_promotions: Vec<AppliedPromotionV1>,
    pub totals: TotalsV1,
    /// Derived by the core: what the payments applied against the debt. The
    /// payments themselves are a separate query — a sale does not own them
    /// (ADR 016).
    pub paid_total: MoneyV1,
    /// Derived by the core: what customers handed over, overpayment included.
    pub tendered_total: MoneyV1,
    /// Derived by the core: what is still owed.
    pub balance_due: MoneyV1,
    /// Derived by the core: cash owed back to the customer.
    pub change_due: MoneyV1,
    /// Derived by the core: the payment lifecycle forbids basket changes.
    pub basket_locked: bool,
    /// Derived by the core: a payment outcome is still open, so the sale
    /// cannot finalize until it is reconciled.
    pub requires_payment_resolution: bool,
    pub created_at: String,
    pub updated_at: String,
    pub finalized_at: Option<String>,
    pub version: i64,
}

/// Diagnostic snapshot of the core: what `get-state` prints and what the Tauri
/// diagnostic screen renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeStateV1 {
    pub protocol_version: u32,
    pub core_version: String,
    pub core_status: String,
    pub device: Option<DeviceV1>,
    pub business_day: Option<BusinessDayV1>,
    pub session: Option<RegisterSessionV1>,
    pub active_transaction: Option<RetailTransactionV1>,
    pub pending_outbox: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEventV1 {
    pub event_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub aggregate_version: i64,
    pub status: String,
    pub attempt_count: i64,
    pub occurred_at: String,
    pub next_attempt_at: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOutboxV1 {
    pub protocol_version: u32,
    pub pending_count: i64,
    pub events: Vec<OutboxEventV1>,
}

/// What sync knows, for an operator and for a diagnostics panel.
///
/// `driver` is an adapter name — `unconfigured`, `simulator` — and never a
/// host, a port, a URL or a credential. A diagnostic view must not become a
/// way to read the installation's configuration (ADR 040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatusV1 {
    pub protocol_version: u32,
    pub driver: String,
    pub pending: i64,
    pub sending: i64,
    pub sent: i64,
    /// Facts the server rejected for good. Each one needs a person.
    pub failed: i64,
    /// When the oldest undelivered fact happened: how far behind this till is.
    pub oldest_pending_at: Option<String>,
    pub next_attempt_at: Option<String>,
    pub inbound_cursor: Option<String>,
    pub failed_facts: Vec<FailedFactV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedFactV1 {
    pub seq: i64,
    pub event_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub attempt_count: i64,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
}

/// What one dispatch cycle did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCycleV1 {
    pub protocol_version: u32,
    pub driver: String,
    /// `IDLE`, `PROGRESSED`, `NOT_CONFIGURED` or `TRANSPORT_FAILED`.
    pub outcome: String,
    pub attempted: i64,
    pub acknowledged: i64,
    pub rescheduled: i64,
    pub failed: i64,
    pub inbound_applied: i64,
    /// Present only when the transport failed.
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEventV1 {
    pub event_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub business_day_id: Option<String>,
    pub session_id: Option<String>,
    pub transaction_id: Option<String>,
    pub operator_id: Option<String>,
    pub occurred_at: String,
    pub payload: Value,
}

/// Lifecycle state of the daemon runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CoreStatusV1 {
    /// Acquiring the single-writer lock, opening and migrating the database.
    Starting,
    /// Serving requests.
    Ready,
    /// Serving, but the last database operation failed.
    Degraded,
    /// Draining in-flight work; no new connections are accepted.
    Stopping,
}

impl CoreStatusV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            CoreStatusV1::Starting => "STARTING",
            CoreStatusV1::Ready => "READY",
            CoreStatusV1::Degraded => "DEGRADED",
            CoreStatusV1::Stopping => "STOPPING",
        }
    }

    /// Whether mutating operations may be accepted in this state.
    pub const fn accepts_commands(self) -> bool {
        matches!(self, CoreStatusV1::Ready | CoreStatusV1::Degraded)
    }
}

/// Answer to `GET_CORE_INFO`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreInfoV1 {
    pub core_version: String,
    pub protocol_version: u32,
    pub status: CoreStatusV1,
    pub database_schema_version: i64,
    pub uptime_seconds: u64,
    /// Monotonic within one daemon lifetime; bumped on every applied command.
    /// Diagnostic staleness signal only — never an aggregate version and never
    /// an input to business logic.
    pub revision: u64,
    pub pid: u32,
}

// ------------------------------------------------------------ request payloads

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitDeviceRequestV1 {
    pub tenant_id: String,
    pub store_id: String,
    pub currency: String,
    /// Whether this installation may sell what it does not have on record.
    /// Defaults to permissive: a stale position must not block the counter.
    #[serde(default = "default_true")]
    pub allow_negative_inventory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRegisterSessionRequestV1 {
    pub register_id: String,
    pub operator_id: String,
    pub opening_cash_minor: i64,
}

/// How a client names a product. Exactly one way, explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "value", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProductSelectorV1 {
    Barcode(String),
    Sku(String),
    /// Canonical catalog reference assigned upstream.
    ProductRef(String),
}

/// Identity and quantity. Description, price, tax rate and price mode are
/// resolved by the core from the snapshot the sale captured — a client cannot
/// state a price, so it cannot be wrong about one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddItemRequestV1 {
    pub product: ProductSelectorV1,
    pub quantity_thousandths: i64,
}

/// The line to remove, by its number. Numbers are never reused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveItemRequestV1 {
    pub line_number: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeQuantityRequestV1 {
    pub line_number: i64,
    pub quantity_thousandths: i64,
}

// ------------------------------------------------------------------- catalog

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SnapshotCountsV1 {
    pub tax_profiles: i64,
    pub price_books: i64,
    pub products: i64,
    pub price_entries: i64,
    pub barcodes: i64,
    #[serde(default)]
    pub promotions: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxProfileV1 {
    pub code: String,
    pub name: String,
    pub rate_bps: u32,
}

/// Where a price book applies: tenant-wide, or one store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBookV1 {
    pub price_book_id: String,
    pub code: String,
    pub name: String,
    pub currency: String,
    /// `TENANT` or `STORE`.
    pub scope: String,
    /// Set exactly when `scope` is `STORE`.
    #[serde(default)]
    pub store_id: Option<String>,
    /// Tiebreaker within a scope: higher wins.
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_true")]
    pub active: bool,
}

/// One price, optionally scheduled. `valid_from` is inclusive and
/// `valid_until` exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceEntryV1 {
    pub price_book_id: String,
    pub product_ref: String,
    pub amount: MoneyV1,
    pub price_mode: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
}

/// The price the core picked, and the book it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPriceV1 {
    pub price_book_id: String,
    pub price_book_code: String,
    pub unit_price: MoneyV1,
    pub price_mode: String,
    pub effective_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogProductV1 {
    pub product_ref: String,
    /// Source-system code: traceability metadata, never identity.
    pub external_ref: Option<String>,
    pub sku: Option<String>,
    pub name: String,
    pub active: bool,
    pub sellable: bool,
    pub tax_profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshotV1 {
    pub snapshot_id: String,
    pub store_id: String,
    pub version: i64,
    pub status: String,
    pub declared_content_hash: String,
    /// Computed by the core from the rows it stored. `null` until committed.
    pub content_hash: Option<String>,
    pub generated_at: String,
    pub effective_at: Option<String>,
    pub expected: SnapshotCountsV1,
    pub imported_at: String,
    pub committed_at: Option<String>,
    pub activated_at: Option<String>,
    pub superseded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshotViewV1 {
    pub snapshot: CatalogSnapshotV1,
    /// What is actually stored, as counted by the core.
    pub stored: SnapshotCountsV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogStatusV1 {
    pub protocol_version: u32,
    pub active: Option<CatalogSnapshotViewV1>,
    pub pending: Vec<CatalogSnapshotViewV1>,
    /// Open imports. These never price a sale.
    pub importing: Vec<CatalogSnapshotViewV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProgressV1 {
    pub snapshot_id: String,
    pub status: String,
    pub version: i64,
    pub received_chunks: Vec<i64>,
    pub received_records: i64,
    /// Lowest index the client still has to send.
    pub next_chunk_index: i64,
    pub expected: SnapshotCountsV1,
    pub stored: SnapshotCountsV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogImportStatusV1 {
    pub snapshot: CatalogSnapshotV1,
    pub progress: ImportProgressV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductLookupV1 {
    pub snapshot_id: String,
    pub snapshot_version: i64,
    /// Instant the price was resolved at.
    pub effective_at: String,
    pub product: CatalogProductV1,
    pub tax_profile: TaxProfileV1,
    /// `null` when no price applies at `effective_at`.
    pub price: Option<ResolvedPriceV1>,
    /// Stable code explaining a missing price.
    pub price_issue: Option<String>,
}

// ------------------------------------------------- catalog import requests

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeginCatalogImportRequestV1 {
    /// Monotonic version assigned upstream.
    pub version: i64,
    /// Currency every price in this snapshot is quoted in. Checked against the
    /// device, so a mismatched catalog fails with a clear error instead of a
    /// hash mismatch.
    pub currency: String,
    /// Hash the producer claims the content has; verified, never trusted.
    pub content_hash: String,
    pub generated_at: String,
    #[serde(default)]
    pub effective_at: Option<String>,
    pub expected: SnapshotCountsV1,
    pub tax_profiles: Vec<TaxProfileV1>,
    /// A snapshot must declare at least one price book.
    pub price_books: Vec<PriceBookV1>,
}

/// One price for the product that carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceRecordV1 {
    /// Book this price belongs to. Must be declared in the import header.
    pub price_book: String,
    pub amount_minor: i64,
    pub price_mode: String,
    /// Inclusive lower bound.
    #[serde(default)]
    pub valid_from: Option<String>,
    /// Exclusive upper bound.
    #[serde(default)]
    pub valid_until: Option<String>,
}

/// One product record. Its barcodes and its prices travel with it, which is
/// what keeps a product, its codes and its prices inside the same chunk and
/// the same transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogProductRecordV1 {
    pub product_ref: String,
    #[serde(default)]
    pub external_ref: Option<String>,
    #[serde(default)]
    pub sku: Option<String>,
    pub name: String,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default = "default_true")]
    pub sellable: bool,
    pub tax_profile: String,
    #[serde(default)]
    pub barcodes: Vec<String>,
    /// Prices for this product. A product with none cannot be sold.
    #[serde(default)]
    pub prices: Vec<PriceRecordV1>,
}

/// One promotion, as the snapshot carries it.
///
/// The rule is adjacently tagged on `rule`, so each kind declares exactly the
/// fields it uses and an unknown kind is a parse error rather than a silently
/// ignored promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionRecordV1 {
    pub promotion_id: String,
    pub code: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub active: bool,
    /// Higher wins on the same target.
    #[serde(default)]
    pub priority: i32,
    /// `STACKABLE` or `EXCLUSIVE`.
    pub stackability: String,
    /// `TENANT` or `STORE`.
    pub scope: String,
    /// Set exactly when `scope` is `STORE`.
    #[serde(default)]
    pub store_id: Option<String>,
    /// Inclusive lower bound.
    #[serde(default)]
    pub valid_from: Option<String>,
    /// Exclusive upper bound.
    #[serde(default)]
    pub valid_until: Option<String>,
    pub rule: PromotionRuleV1,
}

/// What a promotion does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PromotionRuleV1 {
    PercentageOff {
        /// Absent means every product.
        #[serde(default)]
        product_ref: Option<String>,
        percentage_bps: u32,
    },
    FixedAmountOff {
        #[serde(default)]
        product_ref: Option<String>,
        amount_minor: i64,
    },
    BuyNGetM {
        #[serde(default)]
        product_ref: Option<String>,
        buy_thousandths: i64,
        get_thousandths: i64,
    },
    BasketThreshold {
        minimum_minor: i64,
        amount_minor: i64,
    },
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutCatalogChunkRequestV1 {
    pub snapshot_id: String,
    pub chunk_index: i64,
    /// Hash of this chunk's canonical content, for replay detection.
    pub chunk_hash: String,
    pub products: Vec<CatalogProductRecordV1>,
    /// Promotions for the products in this chunk. One that names a product
    /// must travel with it, exactly as a price does.
    #[serde(default)]
    pub promotions: Vec<PromotionRecordV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitCatalogImportRequestV1 {
    pub snapshot_id: String,
    /// Number of chunks sent, so a lost tail is detected.
    pub chunk_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshotRequestV1 {
    pub snapshot_id: String,
}

// ------------------------------------------------------------------ inventory

/// One movement of the inventory ledger. Append-only: there is no update DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryMovementV1 {
    pub movement_id: String,
    pub product_ref: String,
    /// `OPENING_BALANCE`, `SALE`, `MANUAL_IN` or `MANUAL_OUT`.
    pub movement_type: String,
    /// `IN` or `OUT`. Explicit, never the sign of the quantity.
    pub direction: String,
    /// A magnitude, in thousandths and rendered: `0.750`.
    pub quantity: String,
    pub quantity_thousandths: i64,
    /// What caused it: a bootstrap snapshot, a `transaction:line`, or the
    /// request that asked for a manual movement.
    pub source_ref: String,
    pub transaction_id: Option<String>,
    pub line_number: Option<i64>,
    pub reason_code: Option<String>,
    pub occurred_at: String,
}

/// What one Edge installation's ledger says of one product.
///
/// A projection — last known baseline plus this installation's own movements —
/// not a store-wide count: PI.Edge runs per POS (ADR 026).
///
/// `on_hand` rather than `available`: there are no reservations, so the only
/// honest figure is what this ledger says is here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryPositionV1 {
    pub store_id: String,
    pub product_ref: String,
    /// Rendered the way every other quantity is: `9.250`, and signed when this
    /// installation sold what it did not have on record.
    pub on_hand: String,
    pub on_hand_thousandths: i64,
    pub movement_count: i64,
    pub last_movement_at: Option<String>,
    /// Empty unless movements were asked for.
    pub movements: Vec<InventoryMovementV1>,
}

/// Which upstream baseline this installation's ledger started from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryBootstrapV1 {
    pub store_id: String,
    pub source_snapshot_id: String,
    pub generated_at: String,
    pub item_count: i64,
    pub bootstrapped_at: String,
}

/// One line of an opening baseline. The quantity is strictly positive: a
/// zero-stock product is an omitted line, not a line of zero (ADR 028).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryBootstrapItemV1 {
    pub product_ref: String,
    pub quantity_thousandths: i64,
}

/// Import the opening baseline. One per installation, ever (ADR 028).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportInventoryBootstrapRequestV1 {
    pub source_snapshot_id: String,
    pub generated_at: String,
    pub items: Vec<InventoryBootstrapItemV1>,
}

/// Record a manual stock movement. A blank reason is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryMovementRequestV1 {
    pub product_ref: String,
    pub quantity_thousandths: i64,
    pub reason_code: String,
}

/// Ask about one product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryQueryV1 {
    pub product_ref: String,
    /// How many movements to include. Zero for the position alone.
    #[serde(default)]
    pub movement_limit: i64,
    #[serde(default)]
    pub offset: i64,
}

// ------------------------------------------------------------------- refunds

/// What one original line can still give back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundableLineV1 {
    pub line_number: i64,
    pub product_ref: String,
    pub description: String,
    /// Rendered: `3.000`.
    pub sold_quantity: String,
    pub refunded_quantity: String,
    pub remaining_quantity: String,
    pub remaining_quantity_thousandths: i64,
    /// What the whole line was worth.
    pub original_gross: MoneyV1,
    /// What is still available, after earlier refunds took their share.
    pub remaining_net: MoneyV1,
    pub remaining_tax: MoneyV1,
    pub remaining_gross: MoneyV1,
    pub remaining_discount: MoneyV1,
}

/// One original tender and what it can still refund.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundableTenderV1 {
    pub payment_id: String,
    /// `CASH` or `CARD`.
    pub tender_type: String,
    pub committed: MoneyV1,
    pub remaining: MoneyV1,
}

/// Everything a client needs to offer a refund, computed by the core.
///
/// A client never works out a refundable quantity or amount of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundEligibilityV1 {
    pub transaction_id: String,
    /// The original sale's status. Only `FINALIZED` is refundable.
    pub original_status: String,
    pub register_sequence: i64,
    pub original_total: MoneyV1,
    /// `NOT_INVOICED` or `INVOICED`: what fiscal consequences a refund may
    /// choose depends on it.
    pub fiscal_state: String,
    pub invoice_ref: Option<String>,
    pub lines: Vec<RefundableLineV1>,
    pub tenders: Vec<RefundableTenderV1>,
    pub refundable_total: MoneyV1,
    /// While a refund is live, a second cannot be created.
    pub refund_in_progress: Option<String>,
    pub refund_ids: Vec<String>,
}

/// One line of a refund, priced by the core from the original sale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundLineV1 {
    pub refund_line_id: String,
    pub original_line_number: i64,
    pub product_ref: String,
    pub description: String,
    pub quantity: String,
    pub quantity_thousandths: i64,
    pub net: MoneyV1,
    pub tax: MoneyV1,
    pub gross: MoneyV1,
    pub discount: MoneyV1,
}

/// One tender's share of a refund, with its attempt history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundPaymentV1 {
    pub refund_payment_id: String,
    pub original_payment_id: String,
    pub tender_type: String,
    pub requested: MoneyV1,
    pub refunded: MoneyV1,
    /// `CREATED`, `PENDING`, `REFUNDED` or `FAILED`.
    pub status: String,
    pub attempts: Vec<RefundAttemptV1>,
}

/// One concrete interaction with a provider to give money back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundAttemptV1 {
    pub attempt_id: String,
    pub attempt_number: i64,
    /// `CREATED`, `PROCESSING`, `UNKNOWN`, `REFUNDED` or `FAILED`.
    pub status: String,
    pub requested: MoneyV1,
    pub refunded: Option<MoneyV1>,
    /// What the provider is asked about during reconciliation. Never a
    /// cardholder detail.
    pub correlation_id: String,
    pub provider_reference: Option<String>,
    pub failure_code: Option<String>,
    pub created_at: String,
}

/// A refund and everything that explains it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundV1 {
    pub refund_id: String,
    pub original_transaction_id: String,
    pub store_id: String,
    pub business_day_id: String,
    pub session_id: String,
    pub register_id: String,
    pub operator_id: String,
    /// `CREATED`, `ACCEPTED`, `FINANCIAL_PENDING`, `COMPLETED` or `FAILED`.
    pub status: String,
    /// Who said the original sale could be refunded. `CENTRAL_ACCEPTED`.
    pub authority: String,
    pub authorization_ref: Option<String>,
    pub lines: Vec<RefundLineV1>,
    pub net: MoneyV1,
    pub tax: MoneyV1,
    pub gross: MoneyV1,
    pub discount: MoneyV1,
    /// `NONE`, `CREDIT_NOTE_PENDING` or `CREDIT_NOTE_REQUESTED`. PI.Edge
    /// issues no CFDI: this records what is owed fiscally, nothing more.
    pub fiscal_status: String,
    pub original_fiscal_state: String,
    pub reason_code: String,
    pub restock: bool,
    pub payments: Vec<RefundPaymentV1>,
    pub remaining_to_refund: MoneyV1,
    /// True from `ACCEPTED` onwards: the goods are back on the ledger.
    pub inventory_returned: bool,
    /// True while a refund attempt is unresolved. The way out is reconciling
    /// that attempt, never a second refund.
    pub requires_reconciliation: bool,
    pub created_at: String,
    pub accepted_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Which quantity of which original line to give back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundLineRequestV1 {
    pub original_line_number: i64,
    pub quantity_thousandths: i64,
}

/// Open a refund.
///
/// There is deliberately **no authority field**: the runtime resolves who says
/// the original sale is refundable, so a client cannot assert it (ADR 030).
/// There is no fiscal *state* field either — only the fiscal *action*, which
/// the core then validates against the original sale (ADR 032).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRefundRequestV1 {
    pub transaction_id: String,
    pub lines: Vec<RefundLineRequestV1>,
    pub reason_code: String,
    /// `NONE`, `CREDIT_NOTE_PENDING` or `CREDIT_NOTE_REQUESTED`.
    pub fiscal_action: String,
}

/// Address one refund.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundRequestV1 {
    pub refund_id: String,
}

// ------------------------------------------------------------------- hardware

/// One device's health.
///
/// Carries no host, port, serial number or driver internals beyond the driver's
/// name: a diagnostic view must not become a way to read the installation's
/// configuration (ADR 035).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareDeviceV1 {
    /// `SCANNER`, `PRINTER` or `CASH_DRAWER`.
    pub kind: String,
    /// Opaque. One device per kind in V1, but the contract does not say so.
    pub device_id: String,
    /// Which driver is configured, by name only.
    pub driver: Option<String>,
    /// `AVAILABLE`, `UNAVAILABLE`, `DEGRADED` or `UNKNOWN`. A device nobody has
    /// probed in this process lifetime is `UNKNOWN`, never `AVAILABLE`.
    pub state: String,
    pub last_checked_at: Option<String>,
    /// Survives a restart: it is rebuilt from the hardware action log.
    pub last_success_at: Option<String>,
    pub last_error_code: Option<String>,
    /// Human detail. Never a stack trace, never a payload, never a host.
    pub last_error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareStatusV1 {
    pub scanner: HardwareDeviceV1,
    pub printer: HardwareDeviceV1,
    pub drawer: HardwareDeviceV1,
}

/// A completed scan, as the host's parser produced it.
///
/// `event_id` identifies *this keypress sequence*, not the barcode: two scans
/// of the same product are two events and two units (ADR 036).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitScanRequestV1 {
    pub event_id: String,
    pub barcode: String,
    /// Which scanner, opaque. Optional: one is the common case.
    #[serde(default)]
    pub device_id: Option<String>,
}

/// Print a finalized sale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintTransactionRequestV1 {
    pub transaction_id: String,
    /// A reprint puts paper out and changes nothing else (ADR 037).
    #[serde(default)]
    pub reprint: bool,
}

// ------------------------------------------------------------ cash management

/// One drawer movement. Append-only: there is no update DTO (ADR 021).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashMovementV1 {
    pub movement_id: String,
    /// `OPENING_FLOAT`, `CASH_SALE`, `CASH_IN`, `CASH_OUT` or `SAFE_DROP`.
    pub movement_type: String,
    /// `IN` or `OUT`. Explicit, never the sign of the amount.
    pub direction: String,
    /// What the drawer gained or lost. Never negative.
    pub amount: MoneyV1,
    /// `CASH_SALE` only: what the customer handed over.
    pub tendered: Option<MoneyV1>,
    /// `CASH_SALE` only: what went back.
    pub change: Option<MoneyV1>,
    /// The payment or safe drop that caused it.
    pub source_ref: Option<String>,
    pub reason_code: Option<String>,
    pub approval_id: Option<String>,
    pub operator_id: String,
    pub created_at: String,
}

/// What a drawer should contain, and the figures that explain it.
///
/// An operational read. A cashier's pre-close path receives
/// [`SessionCloseRequirementsV1`] instead, which cannot carry an amount at all
/// (ADR 022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashPositionV1 {
    pub session_id: String,
    pub register_id: String,
    pub operator_id: String,
    pub session_status: String,
    pub opening_float: MoneyV1,
    /// Net cash from committed cash sales: what stayed in the drawer.
    pub cash_sales: MoneyV1,
    pub cash_in: MoneyV1,
    pub cash_out: MoneyV1,
    /// Approved drops only. A pending drop has moved nothing.
    pub safe_drops: MoneyV1,
    /// Cash handed back for commercial refunds.
    pub cash_refunds: MoneyV1,
    /// Derived from the movements, never stored.
    pub expected_cash: MoneyV1,
    /// Audit only: what customers handed over in cash.
    pub tendered_total: MoneyV1,
    /// Audit only: what went back as change.
    pub change_total: MoneyV1,
    pub movements: Vec<CashMovementV1>,
    pub pending_safe_drops: Vec<SafeDropV1>,
}

/// A request to move money to the safe, and its decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeDropV1 {
    pub safe_drop_id: String,
    pub approval_id: String,
    pub amount: MoneyV1,
    pub reason_code: String,
    /// `PENDING_APPROVAL`, `APPROVED` or `REJECTED`. A pending drop has not
    /// reduced expected cash.
    pub status: String,
    pub requested_by: String,
    pub requested_at: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
}

/// What a session still needs before it can close.
///
/// **This type must never gain a field that reveals expected cash** — not the
/// figure, not a movement list, not a running total. A contract test asserts
/// that its rendered JSON contains neither the words nor the amount (ADR 022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCloseRequirementsV1 {
    pub session_id: String,
    pub currency: String,
    /// A count of movements. Never an amount.
    pub movement_count: i64,
    /// Everything standing in the way, together.
    pub blockers: Vec<String>,
    pub requires_declaration: bool,
}

/// What a close produced, once the declaration is durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCloseResultV1 {
    pub session_id: String,
    /// `DECLARED` or `FORCED`.
    pub closure_kind: String,
    /// Absent for a forced close: no count was taken.
    pub declared_cash: Option<MoneyV1>,
    /// The ledger figure used at this close, materialised for audit.
    pub expected_at_close: MoneyV1,
    /// `declared − expected`. Absent exactly when the count is.
    pub variance: Option<MoneyV1>,
    /// `BALANCED`, `SHORTAGE`, `OVERAGE` or `UNDECLARED`, so an unknown
    /// variance never renders as zero.
    pub variance_label: String,
}

/// Record a manual drawer movement. A blank reason is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashMovementRequestV1 {
    pub amount_minor: i64,
    pub reason_code: String,
}

/// Decide a pending safe drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproveSafeDropRequestV1 {
    pub safe_drop_id: String,
    /// The approving identity. Must not be the requester.
    pub approver: String,
    #[serde(default = "default_true")]
    pub approve: bool,
}

/// Declare the counted cash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclareClosingCashRequestV1 {
    pub counted_minor: i64,
}

/// Continue a closed session under a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReopenSessionRequestV1 {
    pub session_id: String,
    pub reason: String,
    /// Who takes over. May differ from the previous operator.
    pub operator_id: String,
}

/// End the day over open sessions, under an explicit policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceCloseBusinessDayRequestV1 {
    pub reason: String,
    /// Who or what ordered it.
    pub actor: String,
}

/// Start a payment. The core decides what the tender may cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartPaymentRequestV1 {
    /// `CASH` or `CARD`.
    pub tender_type: String,
    /// What this tender is asked to cover, in minor units. For cash, what the
    /// customer handed over.
    pub amount_minor: i64,
}

/// Address one existing payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRequestV1 {
    pub payment_id: String,
}

/// Address one sale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionRequestV1 {
    pub transaction_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitRequestV1 {
    pub limit: i64,
}
