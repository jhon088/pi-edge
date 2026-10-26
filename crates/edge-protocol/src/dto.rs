//! Version 1 data transfer objects.
//!
//! Pure `serde` types: no domain, no application, no transport. Conversions to
//! and from the core live in [`crate::mapping`], behind the `core-mapping`
//! feature.

use std::collections::BTreeMap;

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
    /// What the product costs before any choice. Equal to `unit_price` for an
    /// ordinary line (ADR 046).
    pub base_unit_price: MoneyV1,
    /// Base plus every chargeable choice. The same amount as `unit_price`,
    /// named for what it is so a client never has to add anything up.
    pub configured_unit_price: MoneyV1,
    /// The choices the customer made, materialised onto the line. Empty for
    /// an ordinary line.
    ///
    /// **Always serialized**, empty or not. A field that disappears when it
    /// has nothing in it is a field every client has to defend against, and
    /// one that forgets gets `undefined` where the contract promised a list
    /// (ADR 046).
    #[serde(default)]
    pub selections: Vec<TransactionSelectionV1>,
    /// Always serialized, `null` when there is no note, for the same reason.
    #[serde(default)]
    pub preparation_note: Option<String>,
}

/// One materialised choice on a sale line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionSelectionV1 {
    pub option_id: String,
    pub group_id: String,
    /// What the question was called, as the customer saw it.
    pub group_name: String,
    /// What the answer was called, as the customer saw it.
    pub name: String,
    pub quantity: i64,
    /// The product a chargeable choice billed through, if any.
    pub component_ref: Option<String>,
    pub chargeable: bool,
    pub unit_price: MoneyV1,
    pub amount: MoneyV1,
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
    /// What the sale would total if it were totalized right now, computed by
    /// the core. What a cart screen shows while the customer is still
    /// choosing; `totals` stays the official, totalized figure (ADR 046).
    pub running_totals: TotalsV1,
    /// Short, human order number, for a kiosk screen and for calling an order
    /// out. Derived from the register and this sale's own sequence on it, so
    /// it is stable and needs no counter of its own (ADR 046).
    pub order_code: String,
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
    /// The choices the customer made, as **ids and quantities only**. Empty is
    /// an ordinary scan. There is deliberately no price, delta or total here:
    /// a client never states what a choice costs (ADR 046).
    #[serde(default)]
    pub selections: Vec<RequestedSelectionV1>,
    /// Free text for whoever prepares it. Never priced, never taxed.
    #[serde(default)]
    pub preparation_note: Option<String>,
}

/// One choice a client says the customer made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedSelectionV1 {
    pub option_id: String,
    pub quantity: i64,
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
    /// Universal catalog extensions (ADR 044). Defaulted so a Retail producer
    /// that has never heard of them keeps working unchanged.
    #[serde(default)]
    pub channels: i64,
    #[serde(default)]
    pub categories: i64,
    #[serde(default)]
    pub presentation: i64,
    #[serde(default)]
    pub selection_groups: i64,
    #[serde(default)]
    pub selection_options: i64,
    #[serde(default)]
    pub availability: i64,
}

/// Text in every language the catalog publishes it in.
///
/// `default` is what is shown when nothing matches, and the translations are a
/// map so the wire order of a JSON object cannot change the content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizedTextV1 {
    pub default: String,
    #[serde(default)]
    pub translations: BTreeMap<String, String>,
}

/// A picture, by reference. Never bytes in the catalog: the catalog carries
/// the content hash and the local asset runtime carries the file (ADR 044,
/// ADR 045).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRefV1 {
    /// `sha256:<64 hex>`.
    pub content_hash: String,
    pub media_type: String,
    pub byte_size: i64,
}

/// What this installation has of one asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetStatusV1 {
    /// The declaration this state answers.
    pub asset: AssetRefV1,
    /// `MISSING`, `STAGED`, `READY` or `CORRUPT`.
    pub state: String,
    /// Bytes already on disk while a transfer is in progress. The offset a
    /// resumed import continues from.
    pub bytes_received: i64,
}

/// Naming one asset: used to ask for its state and to commit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRequestV1 {
    pub asset: AssetRefV1,
}

/// One slice of an asset's bytes.
///
/// Bytes travel as lowercase hex because the frame is JSON Lines. Hex doubles
/// the payload and base64 would cost a third less, but it adds no dependency
/// and the digest the core computes is over the decoded bytes either way — so
/// the encoding can change later without touching the asset model (ADR 045).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetChunkRequestV1 {
    /// What the sender claims it is sending, with every chunk, so a retry is
    /// self-describing and the size limit is checked before a byte is written.
    pub asset: AssetRefV1,
    /// Where these bytes belong. Must be exactly what is already staged.
    pub offset: i64,
    /// Lowercase hex of this slice. **There is no field by which a client can
    /// declare the asset valid**: PI.Edge decides, by hashing (ADR 045).
    pub data_hex: String,
}

impl AssetChunkRequestV1 {
    /// Build a chunk from raw bytes.
    pub fn new(asset: AssetRefV1, offset: i64, bytes: &[u8]) -> Self {
        let mut data_hex = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            data_hex.push(HEX_DIGITS[usize::from(byte >> 4)]);
            data_hex.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
        }
        Self {
            asset,
            offset,
            data_hex,
        }
    }

    /// The bytes this chunk carries.
    pub fn data(&self) -> Result<Vec<u8>, AssetChunkError> {
        let raw = self.data_hex.as_bytes();
        if !raw.len().is_multiple_of(2) {
            return Err(AssetChunkError::NotHex);
        }
        let mut bytes = Vec::with_capacity(raw.len() / 2);
        for pair in raw.chunks(2) {
            let high = hex_value(pair[0]).ok_or(AssetChunkError::NotHex)?;
            let low = hex_value(pair[1]).ok_or(AssetChunkError::NotHex)?;
            bytes.push((high << 4) | low);
        }
        if bytes.len() > MAX_ASSET_CHUNK_BYTES {
            return Err(AssetChunkError::TooLarge {
                bytes: bytes.len(),
                limit: MAX_ASSET_CHUNK_BYTES,
            });
        }
        Ok(bytes)
    }
}

const HEX_DIGITS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Why a chunk's payload could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetChunkError {
    #[error("the chunk payload is not lowercase hex")]
    NotHex,
    #[error("the chunk carries {bytes} bytes, above the per-chunk limit of {limit}")]
    TooLarge { bytes: usize, limit: usize },
}

/// Largest slice of an asset one request may carry.
///
/// Hex-encoded this is twice as many bytes on the wire, which leaves a wide
/// margin under the transport's frame limit even with the envelope around it.
/// An asset larger than this is simply more chunks (ADR 045).
pub const MAX_ASSET_CHUNK_BYTES: usize = 512 * 1024;

/// What one snapshot needs locally before it can be put in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAssetsV1 {
    pub snapshot_id: String,
    /// Derived from the snapshot's own content, in canonical order.
    pub assets: Vec<AssetStatusV1>,
    /// How many of them are not ready. Zero is the condition for activation.
    pub pending: i64,
    /// What the whole content store holds, for an operator watching disk.
    pub stored_bytes: i64,
}

/// Ask what a snapshot needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAssetsRequestV1 {
    pub snapshot_id: String,
}

/// A way of selling: a counter, a kiosk, a self-checkout lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelV1 {
    pub channel_ref: String,
    pub name: LocalizedTextV1,
    /// `OFFERED` or `WITHHELD`: what this channel does about products no
    /// availability row mentions.
    pub default_availability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryV1 {
    pub category_ref: String,
    pub name: LocalizedTextV1,
    #[serde(default)]
    pub display_order: i32,
    #[serde(default)]
    pub image: Option<AssetRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductPresentationV1 {
    pub product_ref: String,
    #[serde(default)]
    pub category_ref: Option<String>,
    #[serde(default)]
    pub display_name: Option<LocalizedTextV1>,
    #[serde(default)]
    pub description: Option<LocalizedTextV1>,
    #[serde(default)]
    pub display_order: i32,
    #[serde(default)]
    pub image: Option<AssetRefV1>,
    /// What to offer alongside this product, in the order to offer it.
    ///
    /// Merchandising that travels with the snapshot. Nothing here changes a
    /// price, and an importer that omits it gets an empty list, which is what
    /// every catalog built before this field had.
    #[serde(default)]
    pub recommended_product_refs: Vec<String>,
}

/// One question a product asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionGroupV1 {
    pub group_id: String,
    /// `PRODUCT` or `OPTION`. An `OPTION` owner is how a nested question is
    /// expressed: picking a drink can ask for its size.
    pub owner_kind: String,
    pub owner_ref: String,
    pub name: LocalizedTextV1,
    /// Total units across the group. `min_quantity >= 1` is what required
    /// means; there is no separate flag.
    pub min_quantity: i64,
    pub max_quantity: i64,
    #[serde(default)]
    pub display_order: i32,
}

/// One answer inside a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionOptionV1 {
    pub option_id: String,
    pub group_id: String,
    pub name: LocalizedTextV1,
    /// The product a chargeable option bills through. Absent means the option
    /// is purely semantic and free. **There is no price on the wire here:** a
    /// client never states what an option costs.
    #[serde(default)]
    pub component_product_ref: Option<String>,
    pub max_quantity: i64,
    #[serde(default)]
    pub default_quantity: i64,
    #[serde(default)]
    pub display_order: i32,
    #[serde(default)]
    pub image: Option<AssetRefV1>,
}

/// What one channel offers. Commercial availability, never stock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelAvailabilityV1 {
    pub channel_ref: String,
    /// `PRODUCT`, `CATEGORY` or `SELECTION_OPTION`.
    pub target_kind: String,
    pub target_ref: String,
    /// `OFFERED`, `WITHHELD` or `SNOOZED`.
    pub state: String,
    /// Required exactly when the state is `SNOOZED`.
    #[serde(default)]
    pub snoozed_until: Option<String>,
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
    /// Channels and categories travel in the header, like tax profiles and
    /// price books: few of them, and the chunks reference them (ADR 044).
    #[serde(default)]
    pub channels: Vec<ChannelV1>,
    #[serde(default)]
    pub categories: Vec<CategoryV1>,
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
    /// How the products in this chunk are shown.
    #[serde(default)]
    pub presentation: Vec<ProductPresentationV1>,
    /// The questions they ask and the answers they accept.
    #[serde(default)]
    pub selection_groups: Vec<SelectionGroupV1>,
    #[serde(default)]
    pub selection_options: Vec<SelectionOptionV1>,
    /// What each channel offers. Resolved at commit, so a row may name
    /// something a later chunk delivers.
    #[serde(default)]
    pub availability: Vec<ChannelAvailabilityV1>,
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
    /// Sales whose receipt still needs attention. Never blocks the close.
    pub receipt_attention: ReceiptAttentionV1,
}

/// How many sales still need somebody to look at their receipt.
///
/// `PRINTED` is not counted: it is not an attention item (ADR 043).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReceiptAttentionV1 {
    /// Owed and not yet resolved.
    pub pending: i64,
    /// Certainly did not print: a first receipt may be retried.
    pub failed: i64,
    /// May have printed. Only an explicit reprint, decided by a person.
    pub unknown: i64,
}

/// What closing the business day produced.
///
/// The day and what still needs somebody, in one answer: a client should not
/// have to run a second query to learn that a receipt is outstanding at the
/// moment it closes the day (ADR 043).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessDayCloseV1 {
    pub protocol_version: u32,
    pub day: BusinessDayV1,
    /// Sales **of this day** whose receipt still needs attention. Never
    /// blocks the close.
    pub receipt_attention: ReceiptAttentionV1,
}

/// What a sale owes the customer in paper, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptV1 {
    pub protocol_version: u32,
    pub transaction_id: String,
    /// `PENDING`, `PRINTED`, `FAILED` or `UNKNOWN`.
    pub status: String,
    pub attempt_count: i64,
    pub first_printed_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_error_code: Option<String>,
    /// Human detail. Never a host, a port or raw bytes.
    pub last_error_detail: Option<String>,
}

/// One line of the operational history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSaleV1 {
    pub transaction_id: String,
    pub register_id: String,
    pub register_sequence: i64,
    pub finalized_at: String,
    /// The sale's own materialised total. Nothing is recomputed here.
    pub grand_total: MoneyV1,
    /// `PENDING`, `PRINTED`, `FAILED`, `UNKNOWN` — or absent for a sale closed
    /// before this installation tracked receipts.
    pub receipt_status: Option<String>,
}

/// Recent sales, newest first, with what still needs attention.
///
/// Operational history, not reporting: no analytics, no search, no filters —
/// enough for an operator to find the sale they just made (ADR 043).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSalesV1 {
    pub protocol_version: u32,
    pub attention: ReceiptAttentionV1,
    pub sales: Vec<RecentSaleV1>,
}

/// Abandon a sale that has taken no money.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CancelTransactionRequestV1 {
    /// Why, for the journal. Optional.
    #[serde(default)]
    pub reason_code: Option<String>,
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

/// A bounded page of a list, following the inventory query's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequestV1 {
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn asset() -> AssetRefV1 {
        AssetRefV1 {
            content_hash: format!("sha256:{}", "a".repeat(64)),
            media_type: "image/png".to_owned(),
            byte_size: 4,
        }
    }

    #[test]
    fn a_chunk_round_trips_its_bytes() {
        for bytes in [
            Vec::new(),
            vec![0],
            vec![0xff],
            vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            (0..=255_u8).collect(),
        ] {
            let chunk = AssetChunkRequestV1::new(asset(), 0, &bytes);
            assert_eq!(chunk.data().unwrap(), bytes);
            assert_eq!(
                chunk.data_hex.len(),
                bytes.len() * 2,
                "hex is exactly two characters a byte"
            );
            assert!(
                chunk
                    .data_hex
                    .chars()
                    .all(|character| character.is_ascii_digit()
                        || ('a'..='f').contains(&character)),
                "lowercase hex and nothing else"
            );
        }
    }

    #[test]
    fn a_chunk_survives_the_wire() {
        let bytes: Vec<u8> = (0..64_u8).collect();
        let chunk = AssetChunkRequestV1::new(asset(), 512, &bytes);
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(
            !json.contains('\n'),
            "JSON Lines framing needs one line per frame"
        );
        let parsed: AssetChunkRequestV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, chunk);
        assert_eq!(parsed.data().unwrap(), bytes);
        assert_eq!(parsed.offset, 512);
    }

    #[test]
    fn a_payload_that_is_not_bytes_is_refused() {
        let mut chunk = AssetChunkRequestV1::new(asset(), 0, &[1, 2, 3]);
        chunk.data_hex = "abc".to_owned();
        assert_eq!(chunk.data(), Err(AssetChunkError::NotHex));
        chunk.data_hex = "zz".to_owned();
        assert_eq!(chunk.data(), Err(AssetChunkError::NotHex));
        chunk.data_hex = "../etc".to_owned();
        assert!(chunk.data().is_err());
    }

    #[test]
    fn a_chunk_larger_than_the_limit_is_refused() {
        let oversized = vec![0_u8; MAX_ASSET_CHUNK_BYTES + 1];
        let chunk = AssetChunkRequestV1::new(asset(), 0, &oversized);
        assert!(matches!(
            chunk.data(),
            Err(AssetChunkError::TooLarge { limit, .. }) if limit == MAX_ASSET_CHUNK_BYTES
        ));
        // Hex doubles the payload, so one chunk at the limit stays well under
        // the transport's 4 MiB frame — which this crate deliberately cannot
        // name, since framing belongs to `edge-ipc`.
        const {
            assert!(MAX_ASSET_CHUNK_BYTES * 2 < 4 * 1024 * 1024);
        }
    }

    /// A list in the contract is always a list.
    ///
    /// This is the bug that blacked out the cart: `selections` was omitted
    /// when empty, so an ordinary line arrived with `undefined` where the
    /// contract promised an array, and the first client that read `.length`
    /// threw while rendering (ADR 046).
    #[test]
    fn an_ordinary_line_still_carries_an_empty_selection_list() {
        let money = MoneyV1 {
            minor: 5_900,
            currency: "MXN".to_owned(),
            formatted: "59.00".to_owned(),
        };
        let line = TransactionLineV1 {
            line_id: "l1".to_owned(),
            line_number: 1,
            product_ref: "FRIES".to_owned(),
            sku: None,
            barcode: None,
            description: "Papas".to_owned(),
            quantity: "1".to_owned(),
            quantity_thousandths: 1_000,
            unit_price: money.clone(),
            tax_rate_bps: 1_600,
            price_mode: "TAX_EXCLUSIVE".to_owned(),
            price_book_id: None,
            extended_amount: money.clone(),
            discount_amount: money.clone(),
            taxable_amount: money.clone(),
            tax_amount: money.clone(),
            line_total: money.clone(),
            base_unit_price: money.clone(),
            configured_unit_price: money,
            selections: Vec::new(),
            preparation_note: None,
        };

        let json = serde_json::to_value(&line).unwrap();
        assert_eq!(
            json.get("selections"),
            Some(&serde_json::json!([])),
            "an empty list is still a list on the wire"
        );
        assert_eq!(
            json.get("preparation_note"),
            Some(&Value::Null),
            "and an absent note is still a field"
        );
        // And it survives the round trip it has to survive.
        let parsed: TransactionLineV1 = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, line);
    }

    /// There is no field on the wire by which a client could declare an asset
    /// verified: it says what it is sending, and the core hashes it.
    #[test]
    fn the_wire_has_no_way_to_claim_an_asset_is_valid() {
        let json = serde_json::to_string(&AssetChunkRequestV1::new(asset(), 0, &[1, 2])).unwrap();
        for forbidden in ["valid", "verified", "trusted", "path", "filename", "url"] {
            assert!(
                !json.contains(forbidden),
                "`{forbidden}` has no place in a chunk: {json}"
            );
        }
    }
}

// ----------------------------------------------------------------- the kiosk

/// Which way of selling is asking. The core filters the menu by it; absent
/// means "everything the catalog has" (ADR 044, ADR 046).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuRequestV1 {
    #[serde(default)]
    pub channel_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductConfigurationRequestV1 {
    pub product_ref: String,
    #[serde(default)]
    pub channel_ref: Option<String>,
}

/// Everything a kiosk screen needs to draw the menu in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuV1 {
    pub snapshot_id: String,
    pub currency: String,
    pub categories: Vec<MenuCategoryV1>,
    pub items: Vec<MenuItemV1>,
    /// What a basket can earn, so a screen can advertise it. The amounts stay
    /// the promotion engine's.
    pub promotions: Vec<MenuPromotionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuCategoryV1 {
    pub category_ref: String,
    pub name: String,
    pub display_order: i32,
    pub image: Option<AssetRefV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuItemV1 {
    pub product_ref: String,
    pub category_ref: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub display_order: i32,
    pub image: Option<AssetRefV1>,
    /// What it costs before any choice is made.
    pub from_price: MoneyV1,
    /// Whether tapping it opens a configuration screen or adds it straight
    /// away.
    pub configurable: bool,
    /// What the catalog says to offer alongside this one, in order, already
    /// filtered to products this menu actually shows.
    ///
    /// **Always serialized**, empty or not (ADR 046).
    #[serde(default)]
    pub recommended_product_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuPromotionV1 {
    pub promotion_id: String,
    pub code: String,
    pub name: String,
}

/// The questions one product asks, each answer with its own price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductConfigurationV1 {
    pub product_ref: String,
    pub name: String,
    pub base_price: MoneyV1,
    /// Flat, each group carrying the option it hangs from, so a client renders
    /// the nesting it is told about rather than inferring one.
    pub groups: Vec<MenuSelectionGroupV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuSelectionGroupV1 {
    pub group_id: String,
    /// Set for a nested question: only asked once that option is chosen.
    pub owner_option_id: Option<String>,
    pub name: String,
    pub min_quantity: i64,
    pub max_quantity: i64,
    pub display_order: i32,
    pub options: Vec<MenuSelectionOptionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuSelectionOptionV1 {
    pub option_id: String,
    pub name: String,
    pub component_ref: Option<String>,
    /// Resolved by the core. Zero for a semantic choice, which has no product
    /// and therefore no price.
    pub unit_price: MoneyV1,
    pub max_quantity: i64,
    pub default_quantity: i64,
    pub display_order: i32,
    pub image: Option<AssetRefV1>,
}

/// The receipt printer this installation drives.
///
/// Device configuration, not a sale and not a tenant setting: it says which
/// box is on this desk. The `queue` is opaque above the composition root —
/// the core never resolves it and no client may invent one that was not in
/// `available` (ADR 039).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterSettingsV1 {
    pub enabled: bool,
    /// A model id from `models`, or empty while nothing is chosen.
    pub model: String,
    /// A host print queue name from `available`, or empty.
    pub queue: String,
    /// Characters per line of the configured paper.
    pub columns: i64,
    /// Put a receipt out by itself when a sale finalizes.
    pub auto_print: bool,
}

/// What the admin screen needs to offer a choice: what is configured now,
/// what models this build knows, and what queues this host actually has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterConfigurationV1 {
    pub settings: PrinterSettingsV1,
    pub models: Vec<PrinterModelV1>,
    /// Reported by the host spooler. Empty means it has none — never a
    /// guess, and never an invented device.
    pub available: Vec<PrinterQueueV1>,
    /// What the driver in force calls itself, for the diagnostic line.
    pub driver: String,
    /// Whether the configured printer answered a probe. `None` when there is
    /// nothing configured to probe.
    pub reachable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterModelV1 {
    pub id: String,
    pub label: String,
    pub paper_mm: i64,
    pub columns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterQueueV1 {
    pub name: String,
    pub accepting: bool,
}

/// Ask for part of an asset's bytes.
///
/// A read mirrors the write: chunked, so an asset larger than one frame still
/// crosses it, and never a path — a client names a content hash and gets bytes
/// (ADR 045).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetReadRequestV1 {
    pub asset: AssetRefV1,
    pub offset: i64,
    /// How many bytes at most. Clamped to the per-chunk ceiling.
    pub length: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetChunkV1 {
    pub content_hash: String,
    pub offset: i64,
    /// Lowercase hex of this slice.
    pub data_hex: String,
    /// Total length of the asset, so a client knows when it has all of it.
    pub byte_size: i64,
}

impl AssetChunkV1 {
    pub fn data(&self) -> Result<Vec<u8>, AssetChunkError> {
        AssetChunkRequestV1 {
            asset: AssetRefV1 {
                content_hash: self.content_hash.clone(),
                media_type: "application/octet-stream".to_owned(),
                byte_size: self.byte_size,
            },
            offset: self.offset,
            data_hex: self.data_hex.clone(),
        }
        .data()
    }
}
