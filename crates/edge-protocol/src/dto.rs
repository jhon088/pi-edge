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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentV1 {
    pub payment_id: String,
    pub tender_type: String,
    pub amount: MoneyV1,
    pub status: String,
    pub created_at: String,
    pub committed_at: Option<String>,
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
    pub payments: Vec<PaymentV1>,
    /// Derived by the core: committed payments.
    pub paid_total: MoneyV1,
    /// Derived by the core: what is still owed.
    pub balance_due: MoneyV1,
    /// Derived by the core: cash owed back to the customer.
    pub change_due: MoneyV1,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitCashPaymentRequestV1 {
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitRequestV1 {
    pub limit: i64,
}
