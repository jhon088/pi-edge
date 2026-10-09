//! Read models returned by the query use cases.
//!
//! These are plain application-level views. Wire formatting lives in
//! `edge-protocol`.

use edge_cash::{CashMovement, CashPosition, SafeDrop};
use edge_catalog::{
    CatalogProduct, CatalogSnapshot, ImportProgress, ResolvedPrice, SnapshotCounts, TaxProfile,
};
use edge_domain::prelude::*;
use edge_inventory::{InventoryMovement, InventoryPosition};
use serde::{Deserialize, Serialize};

use crate::ports::DeviceProfile;

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
