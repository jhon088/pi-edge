//! Read models returned by the query use cases.
//!
//! These are plain application-level views. Wire formatting lives in
//! `edge-protocol`.

use edge_assets::AssetState;
use edge_cash::{CashMovement, CashPosition, SafeDrop};
use edge_catalog::{
    AssetRef, CatalogProduct, CatalogSnapshot, ImportProgress, PromotionId, ResolvedPrice,
    SnapshotCounts, TaxProfile,
};
use edge_domain::prelude::*;
use edge_hardware::HardwareDeviceStatus;
use edge_inventory::{InventoryMovement, InventoryPosition};
use edge_refunds::{FiscalState, LineRefundable, Refund, RefundPaymentView, TenderRefundable};
use serde::{Deserialize, Serialize};

use crate::ports::{DeviceProfile, OutboxCounts, RecentSale};

/// Everything a client needs to render the POS after a cold start.
/// Rebuilt entirely from SQLite: nothing lives only in memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeState {
    pub core_version: String,
    pub device: Option<DeviceProfile>,
    pub business_day: Option<BusinessDay>,
    pub session: Option<RegisterSession>,
    pub active_transaction: Option<RetailTransaction>,
    pub pending_outbox: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOutbox {
    pub pending_count: i64,
    pub events: Vec<OutboxEvent>,
}

/// One snapshot plus what is actually stored for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshotView {
    pub snapshot: CatalogSnapshot,
    pub stored: SnapshotCounts,
}

/// What the register knows about its catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogStatus {
    /// The snapshot new sales capture. `None` means the register cannot sell.
    pub active: Option<CatalogSnapshotView>,
    /// Committed and waiting for activation.
    pub pending: Vec<CatalogSnapshotView>,
    /// Open imports. These never price a sale.
    pub importing: Vec<CatalogSnapshotView>,
    /// Highest version this store has *ever* held, superseded ones included.
    /// `0` when it has never held a catalog.
    ///
    /// A version is an identity: the core refuses one that already exists with
    /// different content, and it is right to. So a producer deciding what to
    /// call its next snapshot has to know which numbers are taken — and the
    /// lists above cannot tell it, because a superseded snapshot appears in
    /// none of them while still owning its version forever.
    pub highest_version: i64,
}

/// Progress of an open import, for resume and for a progress indicator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogImportStatus {
    pub snapshot: CatalogSnapshot,
    pub progress: ImportProgress,
}

/// A product as resolved from one snapshot, with the tax profile that prices
/// it. Diagnostic read: adding to a sale goes through `AddItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductLookup {
    pub snapshot_id: CatalogSnapshotId,
    pub snapshot_version: i64,
    /// Instant the price was resolved at.
    pub effective_at: Timestamp,
    pub product: CatalogProduct,
    pub tax_profile: TaxProfile,
    /// `None` when no price applies. A diagnostic lookup reports that instead
    /// of failing, so an operator can see *why* a scan would be refused.
    pub price: Option<ResolvedPrice>,
    /// Stable code explaining a missing price, e.g. `PRICE_NOT_FOUND`.
    pub price_issue: Option<String>,
}

/// What a drawer should contain, and the movements that explain it.
///
/// An operational read. The cashier's pre-close path uses
/// [`edge_cash::SessionCloseRequirements`] instead, which cannot carry an
/// amount at all (ADR 022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashPositionView {
    pub session_id: SessionId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub session_status: RegisterSessionStatus,
    pub position: CashPosition,
    pub movements: Vec<CashMovement>,
    /// Requested drops nobody has decided yet. They have moved no money.
    pub pending_safe_drops: Vec<SafeDrop>,
}

/// What a close produced, once the declaration is durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCloseResult {
    pub session_id: SessionId,
    pub closure_kind: ClosureKind,
    /// Absent for a forced close: no count was taken.
    pub declared_cash: Option<Money>,
    pub expected_at_close: Money,
    /// `declared − expected`. Absent exactly when the count is.
    pub variance: Option<Money>,
    /// `BALANCED`, `SHORTAGE`, `OVERAGE` or `UNDECLARED` — so a report never
    /// renders an unknown variance as zero.
    pub variance_label: String,
    /// Sales whose receipt still needs somebody. **Never a blocker**: a
    /// receipt is an operational obligation, not a financial one, so the till
    /// closes and the operator is told (ADR 043).
    pub receipt_attention: ReceiptAttention,
}

/// What closing the business day produced.
///
/// The day *and* what still needs somebody, in one answer: a caller must not
/// have to run a second query to learn that a receipt is outstanding at the
/// moment it closes the day. The attention never blocks the close (ADR 043).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessDayCloseResult {
    pub day: BusinessDay,
    /// Sales **of this day** whose receipt is `PENDING`, `FAILED` or
    /// `UNKNOWN`. `PRINTED` is not an attention item.
    pub receipt_attention: ReceiptAttention,
}

/// A product's position, with the movements that explain it.
///
/// `on_hand` rather than `available`: there are no reservations, so the only
/// honest figure is what the ledger says is here (ADR 029).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryPositionView {
    pub position: InventoryPosition,
    /// Rendered the way every other quantity is: `9.250`.
    pub on_hand_display: String,
    /// Empty unless the caller asked for movements.
    pub movements: Vec<InventoryMovement>,
}

// ------------------------------------------------------------------ refunds

/// What a sale can still give back, computed by the core.
///
/// A client never works out a refundable quantity or amount: everything here
/// is the core's arithmetic over the original sale's materialised economics
/// and the refunds that already happened (ADR 031).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundEligibilityView {
    pub transaction_id: TransactionId,
    /// The original sale's status. Only `FINALIZED` is refundable.
    pub original_status: String,
    pub register_sequence: i64,
    pub currency: Currency,
    pub original_total: Money,
    /// Whether the original sale was invoiced, which decides what fiscal
    /// consequences a refund may choose (ADR 032).
    pub fiscal_state: FiscalState,
    pub invoice_ref: Option<String>,
    pub lines: Vec<LineRefundable>,
    pub tenders: Vec<TenderRefundable>,
    /// What all the remaining lines together would give back.
    pub refundable_total: Money,
    /// The live refund of this sale, if any. While one exists, a second cannot
    /// be created (ADR 030).
    pub refund_in_progress: Option<RefundId>,
    /// Refunds already recorded against this sale, newest last.
    pub refund_ids: Vec<RefundId>,
}

/// A refund with everything that explains it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundView {
    pub refund: Refund,
    pub payments: Vec<RefundPaymentView>,
    /// What the financial side still owes the customer.
    pub remaining_to_refund: Money,
    /// Whether the goods were returned to stock. True from `ACCEPTED` onwards.
    pub inventory_returned: bool,
    /// Whether the only way forward is reconciling a refund attempt.
    pub requires_reconciliation: bool,
}

// ----------------------------------------------------------------- hardware

/// What each device is and what we know about it.
///
/// Carries no host, port, serial or driver internals: a diagnostic view must
/// not become a way to read the installation's configuration (ADR 035).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareStatusView {
    pub scanner: HardwareDeviceStatus,
    pub printer: HardwareDeviceStatus,
    pub drawer: HardwareDeviceStatus,
}

// --------------------------------------------------------------------- sync

/// What sync knows: how much is waiting, how much is stuck, how far the
/// inbound stream got.
///
/// `driver` names the adapter, never a host, a port or a credential — the same
/// rule the hardware view follows (ADR 035, ADR 040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatusView {
    pub driver: String,
    pub outbox: OutboxCounts,
    /// When the oldest undelivered fact happened. How far behind this till is.
    pub oldest_pending_at: Option<Timestamp>,
    /// When the next attempt is due, if one is waiting out a backoff.
    pub next_attempt_at: Option<Timestamp>,
    pub inbound_cursor: Option<String>,
    /// The facts the consumer rejected for good. Shown, not hidden: each one
    /// needs a person (ADR 041).
    pub failed: Vec<FailedFactView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedFactView {
    pub seq: i64,
    pub event_id: EventId,
    pub event_type: OutboxEventType,
    pub aggregate_id: String,
    pub attempt_count: i64,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
}

/// What one dispatch cycle did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCycleReport {
    pub driver: String,
    pub outcome: SyncCycleOutcome,
    /// Facts claimed and handed to the transport.
    pub attempted: i64,
    pub acknowledged: i64,
    pub rescheduled: i64,
    pub failed: i64,
    pub inbound_applied: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE", tag = "status")]
pub enum SyncCycleOutcome {
    /// Nothing was waiting in either direction.
    Idle,
    /// Facts moved, messages were applied, or both.
    Progressed,
    /// No transport is configured. Not a failure: the till keeps selling and
    /// the facts keep accumulating (ADR 040).
    NotConfigured,
    /// The transport failed. Whatever was in flight is pending again.
    TransportFailed { code: String, detail: String },
}

// ------------------------------------------------------------------ receipts

/// Recent sales plus what still needs attention.
///
/// The attention counts travel with the list because they answer the same
/// question from the other end: "is there anything I have to deal with?"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSalesView {
    pub attention: ReceiptAttention,
    pub sales: Vec<RecentSale>,
}

/// What a receipt recovery pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReceiptRecovery {
    /// Obligations with no device evidence at all: safe to attempt now.
    pub resumed: i64,
    /// Obligations whose printer call had begun and never answered.
    pub marked_unknown: i64,
    /// Obligations settled from a device answer the crash had not applied.
    pub resolved_from_evidence: i64,
}

/// What this installation has of one asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetStatusView {
    /// The declaration this state answers. A state means nothing without it:
    /// "a file of 4096 bytes exists" is only `READY` for an asset that
    /// declared 4096 bytes.
    pub asset: AssetRef,
    pub state: AssetState,
}

/// What one snapshot needs locally before it can be put in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAssetsView {
    pub snapshot_id: CatalogSnapshotId,
    /// Derived from the snapshot's own content rows, in canonical order.
    pub assets: Vec<AssetStatusView>,
    /// What the whole content store holds, for an operator watching disk.
    pub stored_bytes: i64,
}

impl SnapshotAssetsView {
    /// Whether every asset this snapshot needs is present and of the declared
    /// length. Activation goes further and re-reads the bytes.
    pub fn is_ready(&self) -> bool {
        self.assets.iter().all(|entry| entry.state.is_ready())
    }

    pub fn pending(&self) -> usize {
        self.assets
            .iter()
            .filter(|entry| !entry.state.is_ready())
            .count()
    }
}

// ------------------------------------------------------------------ the menu

/// Everything a kiosk screen needs to draw the menu in force (ADR 046).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuView {
    pub snapshot_id: CatalogSnapshotId,
    pub currency: Currency,
    pub categories: Vec<MenuCategoryView>,
    pub items: Vec<MenuItemView>,
    /// What a basket can earn, so a screen can advertise it. The amounts stay
    /// the promotion engine's.
    pub promotions: Vec<MenuPromotionView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuCategoryView {
    pub category_ref: CategoryRef,
    pub name: String,
    pub display_order: i32,
    pub image: Option<AssetRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItemView {
    pub product_ref: ProductRef,
    pub category_ref: Option<CategoryRef>,
    pub name: String,
    pub description: Option<String>,
    pub display_order: i32,
    pub image: Option<AssetRef>,
    /// What it costs before any choice is made.
    pub from_price: Money,
    /// Whether tapping it opens a configuration screen or adds it straight
    /// away.
    pub configurable: bool,
    /// What the catalog says to offer alongside this one, in declared order.
    ///
    /// Already filtered to products this same menu shows: a suggestion a
    /// screen cannot render is not a suggestion, and the filtering belongs
    /// here rather than in each client.
    pub recommended_product_refs: Vec<ProductRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuPromotionView {
    pub promotion_id: PromotionId,
    pub code: String,
    pub name: String,
}

/// The questions one product asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductConfigurationView {
    pub product_ref: ProductRef,
    pub name: String,
    pub base_price: Money,
    /// Flat, each group carrying the option it hangs from, so a client renders
    /// the nesting it is told about rather than inferring one.
    pub groups: Vec<SelectionGroupView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionGroupView {
    pub group_id: SelectionGroupId,
    /// `Some` for a nested question: it is only asked once that option is
    /// chosen.
    pub owner_option_id: Option<SelectionOptionId>,
    pub name: String,
    pub min_quantity: i64,
    pub max_quantity: i64,
    pub display_order: i32,
    pub options: Vec<SelectionOptionView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionOptionView {
    pub option_id: SelectionOptionId,
    pub name: String,
    pub component_ref: Option<ProductRef>,
    /// Resolved by the core from the price books. Zero for a semantic choice,
    /// which has no product and therefore no price.
    pub unit_price: Money,
    pub max_quantity: i64,
    pub default_quantity: i64,
    pub display_order: i32,
    pub image: Option<AssetRef>,
}
