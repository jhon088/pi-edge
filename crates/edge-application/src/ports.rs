//! Ports owned by the application layer.
//!
//! Adapters (SQLite, hardware, sync, UI transport) implement these; the
//! application never depends on an adapter crate.

use chrono::Utc;
use edge_cash::{Approval, CashMovement, CloseDeclaration, SafeDrop};
use edge_catalog::{
    AssetRef, CatalogBarcode, CatalogProduct, CatalogSnapshot, Category, Channel,
    ChannelAvailability, ImportProgress, PriceBook, PriceCandidate, PriceEntry,
    ProductPresentation, ProductSelector, Promotion, SelectionGroup, SelectionOption,
    SnapshotCounts, TaxProfile, TaxProfileCode,
};
use edge_domain::prelude::*;
use edge_hardware::{HardwareDeviceKind, HardwareError};
use edge_inventory::{InventoryMovement, InventoryPosition, MovementType};
use edge_payments::{Payment, PaymentAttempt, PaymentView};
use edge_refunds::{
    Refund, RefundAmounts, RefundAttempt, RefundPayment, RefundPaymentView,
    TransactionFiscalMetadata,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

/// Time source. The domain never reads the clock itself.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// Public identifier factory (UUID).
pub trait IdFactory: Send + Sync {
    fn new_uuid(&self) -> Uuid;
}

/// Identity of this edge device, provisioned by PI.COMMERCE Server.
/// Stored locally so the core can restart without the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub currency: Currency,
    /// Whether this installation may sell what it does not have on record.
    ///
    /// On by default: a till whose position is a day old must not refuse sales
    /// over a number it knows is stale. Off means availability is enforced
    /// before any money moves (ADR 029).
    pub allow_negative_inventory: bool,
}

/// Durable record of one accepted import chunk.
///
/// `chunk_hash` is what makes a replay decidable: the same index with the same
/// hash is a no-op, the same index with a different hash is a conflict.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredChunk {
    pub chunk_index: i64,
    pub chunk_hash: String,
    pub product_count: i64,
    pub price_entry_count: i64,
    pub barcode_count: i64,
    pub promotion_count: i64,
    /// Universal catalog extensions a chunk may carry (ADR 044). Zero for
    /// every Retail chunk, which is why `Default` is meaningful here.
    pub presentation_count: i64,
    pub selection_group_count: i64,
    pub selection_option_count: i64,
    pub availability_count: i64,
}

/// Catalog persistence.
///
/// Split from [`UnitOfWork`] as a supertrait so the sale and catalog contexts
/// stay legible, while a use case still gets both through one `&mut dyn
/// UnitOfWork` inside a single store transaction.
pub trait CatalogUnitOfWork {
    fn find_catalog_snapshot(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Option<CatalogSnapshot>, AppError>;
    fn find_catalog_snapshot_by_version(
        &mut self,
        store_id: &StoreId,
        version: i64,
    ) -> Result<Option<CatalogSnapshot>, AppError>;
    /// The snapshot new sales must capture. `None` means the register cannot
    /// sell yet.
    fn find_active_catalog_snapshot(
        &mut self,
        store_id: &StoreId,
    ) -> Result<Option<CatalogSnapshot>, AppError>;
    fn list_catalog_snapshots(
        &mut self,
        store_id: &StoreId,
        limit: i64,
    ) -> Result<Vec<CatalogSnapshot>, AppError>;
    fn insert_catalog_snapshot(
        &mut self,
        snapshot: &CatalogSnapshot,
        now: Timestamp,
    ) -> Result<(), AppError>;
    fn update_catalog_snapshot(
        &mut self,
        snapshot: &CatalogSnapshot,
        now: Timestamp,
    ) -> Result<(), AppError>;
    /// Discard an aborted import. Content cascades away with the snapshot row.
    fn delete_catalog_snapshot(&mut self, snapshot_id: CatalogSnapshotId) -> Result<(), AppError>;

    fn insert_catalog_tax_profiles(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        profiles: &[TaxProfile],
    ) -> Result<(), AppError>;
    fn list_catalog_tax_profiles(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<TaxProfile>, AppError>;
    fn find_catalog_tax_profile(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        code: &TaxProfileCode,
    ) -> Result<Option<TaxProfile>, AppError>;

    fn find_catalog_chunk(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        chunk_index: i64,
    ) -> Result<Option<StoredChunk>, AppError>;
    fn record_catalog_chunk(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        chunk: &StoredChunk,
        now: Timestamp,
    ) -> Result<(), AppError>;
    fn insert_catalog_products(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        products: &[CatalogProduct],
    ) -> Result<(), AppError>;
    fn insert_catalog_barcodes(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        barcodes: &[CatalogBarcode],
    ) -> Result<(), AppError>;

    fn insert_catalog_price_books(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        books: &[PriceBook],
    ) -> Result<(), AppError>;
    fn list_catalog_price_books(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<PriceBook>, AppError>;
    fn insert_catalog_price_entries(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        entries: &[PriceEntry],
    ) -> Result<(), AppError>;

    /// Every price that *could* apply to a product at a store: entries of
    /// active books whose scope covers the store. Validity and ranking are the
    /// domain's job, so the whole rule lives in one place.
    fn price_candidates(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        store_id: &StoreId,
        product_ref: &ProductRef,
    ) -> Result<Vec<PriceCandidate>, AppError>;

    fn insert_catalog_promotions(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        promotions: &[Promotion],
    ) -> Result<(), AppError>;
    fn list_catalog_promotions(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<Promotion>, AppError>;

    /// Every promotion that *could* apply at a store: tenant-wide ones plus
    /// that store's own, active only. Validity is decided in the engine, which
    /// owns the half-open interval rule.
    fn promotion_candidates(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        store_id: &StoreId,
    ) -> Result<Vec<Promotion>, AppError>;

    fn catalog_stored_counts(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<SnapshotCounts, AppError>;
    fn catalog_import_progress(
        &mut self,
        snapshot: &CatalogSnapshot,
    ) -> Result<ImportProgress, AppError>;

    /// Single lookup on the hot path. Must be an index hit.
    fn lookup_catalog_product(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        selector: &ProductSelector,
    ) -> Result<Option<CatalogProduct>, AppError>;

    // ---- universal catalog extensions (ADR 044) ----
    //
    // Channels and categories arrive in the import header, like tax profiles
    // and price books: few of them, and every chunk is validated against them.
    // Presentation, selection groups, selection options and availability are
    // bulk content and travel in the chunks.

    fn insert_catalog_channels(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        channels: &[Channel],
    ) -> Result<(), AppError>;
    fn list_catalog_channels(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<Channel>, AppError>;
    fn insert_catalog_categories(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        categories: &[Category],
    ) -> Result<(), AppError>;
    fn list_catalog_categories(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<Category>, AppError>;

    fn insert_catalog_presentation(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        presentation: &[ProductPresentation],
    ) -> Result<(), AppError>;
    fn list_catalog_presentation(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<ProductPresentation>, AppError>;

    fn insert_catalog_selection_groups(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        groups: &[SelectionGroup],
    ) -> Result<(), AppError>;
    fn insert_catalog_selection_options(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        options: &[SelectionOption],
    ) -> Result<(), AppError>;
    /// Every selection group of the snapshot.
    ///
    /// Bounded by the configurable part of the catalog, not by the catalog:
    /// a retail snapshot has none, and a menu has hundreds. Used once, at
    /// commit, to refuse cycles and dangling owners that no single chunk and
    /// no foreign key can see.
    fn list_catalog_selection_groups(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<SelectionGroup>, AppError>;
    fn list_catalog_selection_options(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<SelectionOption>, AppError>;

    fn insert_catalog_availability(
        &mut self,
        snapshot_id: CatalogSnapshotId,
        rows: &[ChannelAvailability],
    ) -> Result<(), AppError>;
    fn list_catalog_availability(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<ChannelAvailability>, AppError>;

    /// Only the product references the extensions actually name, and only
    /// those that exist. Never the whole product set: a commit must not load
    /// a catalog into memory to validate a menu.
    fn list_catalog_extension_product_refs(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<ProductRef>, AppError>;

    /// Every asset this snapshot's own content references, derived from the
    /// rows and never from a second list that could drift from them
    /// (ADR 045).
    ///
    /// Distinct `(content_hash, media_type, byte_size)` triples in canonical
    /// order. A hash appearing twice means the snapshot declared the same
    /// bytes with two different sizes or types, which the commit refuses:
    /// content-addressed identity allows exactly one answer per hash.
    fn list_catalog_required_assets(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<Vec<AssetRef>, AppError>;

    /// Hash the stored rows in canonical order. Computed by the core, over
    /// what it actually persisted — never taken from the client.
    fn compute_catalog_content_hash(
        &mut self,
        snapshot_id: CatalogSnapshotId,
    ) -> Result<String, AppError>;
}

/// A single durable unit of work. Every method runs inside one store
/// transaction; dropping the unit of work without committing rolls everything
/// back.
/// Payment persistence.
///
/// A supertrait of [`UnitOfWork`], like the catalog, so a use case that moves
/// money and a use case that prices a basket stay separate concerns while
/// sharing one database transaction when they need to.
pub trait PaymentUnitOfWork {
    fn insert_payment(&mut self, payment: &Payment) -> Result<(), AppError>;
    fn update_payment(&mut self, payment: &Payment) -> Result<(), AppError>;
    fn find_payment(&mut self, payment_id: PaymentId) -> Result<Option<PaymentView>, AppError>;
    /// Every payment of a sale with its attempt history, oldest first.
    fn list_transaction_payments(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<PaymentView>, AppError>;

    fn insert_payment_attempt(&mut self, attempt: &PaymentAttempt) -> Result<(), AppError>;
    fn update_payment_attempt(&mut self, attempt: &PaymentAttempt) -> Result<(), AppError>;
    fn find_payment_attempt(
        &mut self,
        attempt_id: Uuid,
    ) -> Result<Option<PaymentAttempt>, AppError>;
    /// Next attempt number for a payment: `max + 1`, never reused.
    fn next_attempt_number(&mut self, payment_id: PaymentId) -> Result<i64, AppError>;
}

/// Cash persistence.
///
/// The ledger is append-only, so there is no `update_cash_movement` and there
/// never will be: a correction is a new movement (ADR 021). Declarations are
/// evidence, so there is no update for those either.
pub trait CashUnitOfWork {
    fn insert_cash_movement(&mut self, movement: &CashMovement) -> Result<(), AppError>;
    /// Every movement of a session, oldest first.
    fn list_cash_movements(&mut self, session_id: SessionId)
        -> Result<Vec<CashMovement>, AppError>;
    /// Whether a cause has already moved this drawer, so a replay cannot.
    fn find_cash_movement_by_source(
        &mut self,
        session_id: SessionId,
        movement_type: edge_cash::MovementType,
        source_ref: &str,
    ) -> Result<Option<CashMovement>, AppError>;

    fn insert_approval(&mut self, approval: &Approval) -> Result<(), AppError>;
    fn update_approval(&mut self, approval: &Approval) -> Result<(), AppError>;
    fn find_approval(&mut self, approval_id: Uuid) -> Result<Option<Approval>, AppError>;

    fn insert_safe_drop(&mut self, drop: &SafeDrop) -> Result<(), AppError>;
    fn update_safe_drop(&mut self, drop: &SafeDrop) -> Result<(), AppError>;
    fn find_safe_drop(&mut self, safe_drop_id: Uuid) -> Result<Option<SafeDrop>, AppError>;
    /// Drops still waiting on a decision. They block a session close and a
    /// business-day close: the money's location is unknown.
    fn list_pending_safe_drops(&mut self, session_id: SessionId)
        -> Result<Vec<SafeDrop>, AppError>;

    fn insert_close_declaration(&mut self, declaration: &CloseDeclaration) -> Result<(), AppError>;
    fn find_close_declaration(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<CloseDeclaration>, AppError>;

    /// Sessions of a day, closed ones included, oldest first. A register
    /// accumulates a chain of them (ADR 023).
    fn list_sessions_for_day(
        &mut self,
        business_day_id: BusinessDayId,
    ) -> Result<Vec<RegisterSession>, AppError>;
    /// One session by id, in any state. Needed to reopen a closed one.
    fn find_register_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<RegisterSession>, AppError>;
}

/// Inventory persistence.
///
/// The ledger is append-only, so there is no `update_inventory_movement` and
/// there never will be: a correction is a new movement (ADR 026). The bootstrap
/// is recorded once for the same reason.
pub trait InventoryUnitOfWork {
    fn insert_inventory_movement(&mut self, movement: &InventoryMovement) -> Result<(), AppError>;
    /// Every movement of one product, oldest first.
    fn list_inventory_movements(
        &mut self,
        store_id: &StoreId,
        product_ref: &ProductRef,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<InventoryMovement>, AppError>;
    /// The position of one product, folded by the database so a long ledger
    /// does not travel just to be summed.
    fn inventory_position(
        &mut self,
        store_id: &StoreId,
        product_ref: &ProductRef,
    ) -> Result<InventoryPosition, AppError>;
    /// Whether a cause has already moved this product, so a replay cannot.
    fn inventory_movement_exists(
        &mut self,
        store_id: &StoreId,
        movement_type: MovementType,
        product_ref: &ProductRef,
        source_ref: &str,
    ) -> Result<bool, AppError>;

    fn find_inventory_bootstrap(
        &mut self,
        store_id: &StoreId,
    ) -> Result<Option<InventoryBootstrapState>, AppError>;
    fn record_inventory_bootstrap(
        &mut self,
        state: &InventoryBootstrapState,
    ) -> Result<(), AppError>;
}

/// Which upstream position this store's ledger started from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryBootstrapState {
    pub store_id: StoreId,
    pub source_snapshot_id: String,
    pub generated_at: Timestamp,
    pub item_count: i64,
    pub bootstrapped_at: Timestamp,
}

/// Refund persistence.
///
/// The refund document and its financial side. Refund attempts are append-only
/// (a reconciliation adds knowledge, it never rewrites what was sent), so
/// there is no delete for those and there never will be.
pub trait RefundUnitOfWork {
    fn insert_refund(&mut self, refund: &Refund) -> Result<(), AppError>;
    fn update_refund(&mut self, refund: &Refund) -> Result<(), AppError>;
    fn find_refund(&mut self, refund_id: RefundId) -> Result<Option<Refund>, AppError>;
    /// Every refund of a sale, oldest first.
    fn list_transaction_refunds(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<Refund>, AppError>;
    /// The live refund of a sale, if any. At most one (ADR 030).
    fn find_refund_in_progress(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Option<Refund>, AppError>;
    /// What earlier refunds already took from each line of one sale, keyed by
    /// original line number: the quantity and the economics.
    fn refunded_by_line(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<RefundedLineTotals>, AppError>;
    /// What earlier refunds already gave back through each original tender.
    fn refunded_by_payment(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<RefundedTenderTotals>, AppError>;

    fn insert_refund_payment(&mut self, payment: &RefundPayment) -> Result<(), AppError>;
    fn update_refund_payment(&mut self, payment: &RefundPayment) -> Result<(), AppError>;
    fn list_refund_payments(
        &mut self,
        refund_id: RefundId,
    ) -> Result<Vec<RefundPaymentView>, AppError>;
    fn insert_refund_attempt(&mut self, attempt: &RefundAttempt) -> Result<(), AppError>;
    fn update_refund_attempt(&mut self, attempt: &RefundAttempt) -> Result<(), AppError>;
    fn next_refund_attempt_number(&mut self, refund_payment_id: Uuid) -> Result<i64, AppError>;

    /// The sale's fiscal metadata. `None` only for a sale that never
    /// finalized: a finalized sale always has a row (ADR 032).
    fn find_transaction_fiscal_metadata(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Option<TransactionFiscalMetadata>, AppError>;
    fn put_transaction_fiscal_metadata(
        &mut self,
        transaction_id: TransactionId,
        metadata: &TransactionFiscalMetadata,
    ) -> Result<(), AppError>;
}

/// What earlier refunds already took from one line of a sale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundedLineTotals {
    pub original_line_number: i64,
    pub quantity_thousandths: i64,
    pub amounts: RefundAmounts,
}

/// What earlier refunds already gave back through one original tender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundedTenderTotals {
    pub original_payment_id: PaymentId,
    /// Requested through this tender by refunds that are not terminal, plus
    /// what completed ones actually returned: the capacity already spoken for.
    pub reserved: Money,
}

/// The hardware action log.
///
/// Append-only: an action is evidence that something physical was attempted,
/// so there is no delete and a trigger in migration 0009 enforces it. An
/// action is *resolved* in place — recording what the device answered is not
/// rewriting what was attempted (ADR 037).
pub trait HardwareUnitOfWork {
    /// Claim an automatic action before the device is touched.
    ///
    /// Answers `None` when this cause already has a pending or completed
    /// action, which is what stops a replayed command from opening the drawer
    /// twice. A *failed* action does not block a retry.
    fn claim_hardware_action(&mut self, action: &HardwareAction) -> Result<Option<Uuid>, AppError>;
    /// Record what the device answered.
    fn resolve_hardware_action(
        &mut self,
        action_id: Uuid,
        outcome: Result<(), &HardwareError>,
        now: Timestamp,
    ) -> Result<(), AppError>;
    /// The action already taken for one cause, if any.
    ///
    /// Recovery's only honest source of truth about whether a device call
    /// began: a `PENDING` row here means the printer may already have the job
    /// (ADR 043).
    fn find_hardware_action_for(
        &mut self,
        kind: HardwareActionKind,
        source_ref: &str,
    ) -> Result<Option<HardwareAction>, AppError>;
    /// The most recent action of one device, for the diagnostic view.
    fn latest_hardware_action(
        &mut self,
        device_kind: HardwareDeviceKind,
    ) -> Result<Option<HardwareAction>, AppError>;
    /// When this device last did something successfully. Survives a restart,
    /// which is why `last_success_at` can be shown after one (ADR 035).
    fn last_hardware_success(
        &mut self,
        device_kind: HardwareDeviceKind,
    ) -> Result<Option<Timestamp>, AppError>;
}

/// One attempted physical action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareAction {
    pub action_id: Uuid,
    pub kind: HardwareActionKind,
    pub device_kind: HardwareDeviceKind,
    /// A `DrawerOpenReason` or a `PrintKind`, as its wire string.
    pub reason: String,
    /// The commercial fact that justified it. `None` for an explicit
    /// diagnostic request, which may legitimately repeat.
    pub source_ref: Option<String>,
    pub status: HardwareActionStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub requested_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareActionKind {
    DrawerOpen,
    Print,
}

impl HardwareActionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            HardwareActionKind::DrawerOpen => "DRAWER_OPEN",
            HardwareActionKind::Print => "PRINT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareActionStatus {
    /// Claimed, and the device not yet answered. Counts as taken: a process
    /// that died here may already have opened the drawer.
    Pending,
    Completed,
    Failed,
}

impl HardwareActionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            HardwareActionStatus::Pending => "PENDING",
            HardwareActionStatus::Completed => "COMPLETED",
            HardwareActionStatus::Failed => "FAILED",
        }
    }
}

/// The receipt obligations of finalized sales.
///
/// Separate from [`HardwareUnitOfWork`] on purpose: that one records what was
/// attempted on a device, this one records what a customer is owed. One sale
/// can produce a failed attempt, a retry and any number of reprints and still
/// owe exactly one receipt (ADR 043).
pub trait ReceiptUnitOfWork {
    /// Take on the obligation. Called in the same transaction that finalizes
    /// the sale, so a closed sale always owes a receipt.
    fn insert_receipt_obligation(&mut self, receipt: &TransactionReceipt) -> Result<(), AppError>;

    fn find_receipt(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Option<TransactionReceipt>, AppError>;

    fn update_receipt(&mut self, receipt: &TransactionReceipt) -> Result<(), AppError>;

    /// Obligations in one state, oldest first. Used by recovery and by the
    /// attention list.
    fn list_receipts_by_status(
        &mut self,
        status: ReceiptStatus,
        limit: i64,
    ) -> Result<Vec<TransactionReceipt>, AppError>;

    /// How many sales still need somebody to look at their receipt.
    ///
    /// The scope is what makes the number actionable: a session close reports
    /// its own till, a day close reports its own day, and the operational
    /// history reports the installation. No close is ever judged by another
    /// one's leftovers (ADR 043).
    fn receipt_attention(
        &mut self,
        scope: ReceiptAttentionScope,
    ) -> Result<ReceiptAttention, AppError>;

    /// Recent finalized sales, newest first, with their receipt state.
    ///
    /// Operational history, not reporting: enough for an operator to find the
    /// sale they just made and reprint it. The money comes from the sale's
    /// own materialised total — nothing is recomputed here (ADR 043).
    fn list_recent_sales(&mut self, limit: i64, offset: i64) -> Result<Vec<RecentSale>, AppError>;
}

/// One line of the operational history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSale {
    pub transaction_id: TransactionId,
    pub register_id: RegisterId,
    pub register_sequence: i64,
    pub finalized_at: Timestamp,
    pub grand_total: Money,
    /// `None` for a sale closed before this installation tracked receipts.
    /// Saying `PRINTED` about one would be inventing evidence (ADR 043).
    pub receipt_status: Option<ReceiptStatus>,
}

/// How many facts sit in each outbox state.
///
/// `failed` is the number that needs a human. It is reported next to the
/// others rather than hidden, because a silently stuck fact is the failure
/// mode this milestone exists to prevent (ADR 041).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutboxCounts {
    pub pending: i64,
    pub sending: i64,
    pub sent: i64,
    pub failed: i64,
}

impl OutboxCounts {
    /// Everything not yet delivered.
    pub fn undelivered(self) -> i64 {
        self.pending + self.sending + self.failed
    }
}

/// Durable state the sync engine needs.
///
/// Separate from [`UnitOfWork`]'s three outbox methods on purpose: those are
/// what *producers* and diagnostics use (enqueue, count, list), while these
/// move facts through the dispatch states and keep the inbound cursor. The
/// split is the same one the catalog and cash contexts already have.
pub trait SyncUnitOfWork {
    /// Facts that may be dispatched at `now`, oldest `seq` first.
    ///
    /// `PENDING` only, and only those whose backoff has elapsed. A row with no
    /// `next_attempt_at` has never been attempted and is due immediately.
    fn list_dispatchable_outbox(
        &mut self,
        now: Timestamp,
        limit: i64,
    ) -> Result<Vec<OutboxEntry>, AppError>;

    /// Aggregates that carry a terminal fact.
    ///
    /// Their later versions must wait: handing the consumer version `n+1` of
    /// an aggregate whose version `n` was permanently rejected would give it a
    /// history with a hole in it.
    fn list_failed_outbox_aggregates(&mut self) -> Result<Vec<String>, AppError>;

    /// `PENDING -> SENDING`, counting the attempt.
    ///
    /// Returns the events actually claimed: a row somebody else already moved
    /// is simply not in the answer.
    fn claim_outbox(
        &mut self,
        event_ids: &[EventId],
        now: Timestamp,
    ) -> Result<Vec<EventId>, AppError>;

    /// `SENDING -> SENT`.
    fn mark_outbox_sent(&mut self, event_id: EventId, now: Timestamp) -> Result<(), AppError>;

    /// `SENDING -> PENDING`, due at `next_attempt_at`, with the reason kept.
    fn reschedule_outbox(
        &mut self,
        event_id: EventId,
        next_attempt_at: Timestamp,
        error_code: &str,
        error_detail: &str,
        now: Timestamp,
    ) -> Result<(), AppError>;

    /// `SENDING -> FAILED`. Terminal.
    fn fail_outbox(
        &mut self,
        event_id: EventId,
        error_code: &str,
        error_detail: &str,
        now: Timestamp,
    ) -> Result<(), AppError>;

    /// Put every `SENDING` row back to `PENDING`, due immediately.
    ///
    /// Run once at startup. A process that died mid-flight left those rows
    /// claimed, and nobody is coming back for them: one daemon owns this
    /// database, so "in flight" after a restart means "never answered". They
    /// are resent with the same `event_id` and the consumer deduplicates —
    /// at-least-once, deliberately (ADR 041).
    fn recover_sending_outbox(&mut self, now: Timestamp) -> Result<i64, AppError>;

    fn outbox_counts(&mut self) -> Result<OutboxCounts, AppError>;

    /// Rows in one state, oldest `seq` first. Used to show what is stuck.
    fn list_outbox_by_status(
        &mut self,
        status: OutboxStatus,
        limit: i64,
    ) -> Result<Vec<OutboxEntry>, AppError>;

    /// How far the inbound stream has been consumed, if at all.
    fn inbound_cursor(&mut self) -> Result<Option<String>, AppError>;

    /// Advance the inbound cursor.
    ///
    /// Only ever called in the same transaction as the effect it accounts for,
    /// so the cursor cannot run ahead of local state (ADR 041).
    fn put_inbound_cursor(&mut self, cursor: &str, now: Timestamp) -> Result<(), AppError>;
}

pub trait UnitOfWork:
    CatalogUnitOfWork
    + PaymentUnitOfWork
    + CashUnitOfWork
    + InventoryUnitOfWork
    + RefundUnitOfWork
    + HardwareUnitOfWork
    + ReceiptUnitOfWork
    + SyncUnitOfWork
{
    fn device_profile(&mut self) -> Result<Option<DeviceProfile>, AppError>;
    fn put_device_profile(
        &mut self,
        profile: &DeviceProfile,
        now: Timestamp,
    ) -> Result<(), AppError>;

    fn find_open_business_day(
        &mut self,
        store_id: &StoreId,
    ) -> Result<Option<BusinessDay>, AppError>;
    fn find_business_day(&mut self, id: BusinessDayId) -> Result<Option<BusinessDay>, AppError>;
    fn insert_business_day(&mut self, day: &BusinessDay, now: Timestamp) -> Result<(), AppError>;
    fn update_business_day(&mut self, day: &BusinessDay, now: Timestamp) -> Result<(), AppError>;

    /// Sessions that are not CLOSED for the given business day.
    fn find_open_sessions(
        &mut self,
        business_day_id: BusinessDayId,
    ) -> Result<Vec<RegisterSession>, AppError>;
    fn find_open_session_for_register(
        &mut self,
        register_id: &RegisterId,
    ) -> Result<Option<RegisterSession>, AppError>;
    fn insert_register_session(
        &mut self,
        session: &RegisterSession,
        now: Timestamp,
    ) -> Result<(), AppError>;
    fn update_register_session(
        &mut self,
        session: &RegisterSession,
        now: Timestamp,
    ) -> Result<(), AppError>;

    /// The non-terminal transaction of a session, if any.
    fn find_active_transaction(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<RetailTransaction>, AppError>;
    fn find_transaction(
        &mut self,
        id: TransactionId,
    ) -> Result<Option<RetailTransaction>, AppError>;
    fn next_register_sequence(&mut self, register_id: &RegisterId) -> Result<i64, AppError>;
    fn insert_transaction(
        &mut self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError>;
    fn update_transaction(
        &mut self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError>;

    fn append_journal(&mut self, event: &JournalEvent) -> Result<(), AppError>;
    fn list_journal(&mut self, limit: i64) -> Result<Vec<JournalEvent>, AppError>;

    fn enqueue_outbox(&mut self, event: &OutboxEvent) -> Result<(), AppError>;
    fn count_pending_outbox(&mut self) -> Result<i64, AppError>;
    fn list_pending_outbox(&mut self, limit: i64) -> Result<Vec<OutboxEvent>, AppError>;

    /// Explicit command idempotency: a replayed `request_id` returns the
    /// recorded result instead of applying the command twice.
    fn find_command_result(&mut self, request_id: Uuid) -> Result<Option<String>, AppError>;
    fn record_command_result(
        &mut self,
        request_id: Uuid,
        command_type: &str,
        result_json: &str,
        now: Timestamp,
    ) -> Result<(), AppError>;
}

/// Durable store able to run a closure inside one ACID transaction.
///
/// Intentionally generic (not object safe): there is exactly one store per
/// process and the concrete type is chosen at the composition root.
pub trait EdgeStore {
    fn transact<T, F>(&self, work: F) -> Result<T, AppError>
    where
        F: FnOnce(&mut dyn UnitOfWork) -> Result<T, AppError>;
}

// ----------------------------------------------- built-in port implementations
//
// The two ports every use case needs have exactly one production
// implementation each, and neither carries a dependency worth isolating. They
// live beside the traits they implement rather than in a file of their own.

/// Wall-clock time in UTC.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_datetime(Utc::now())
    }
}

/// Random UUID v4 factory for public ids.
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidFactory;

impl IdFactory for UuidFactory {
    fn new_uuid(&self) -> Uuid {
        Uuid::new_v4()
    }
}
