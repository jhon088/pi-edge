//! Ports owned by the application layer.
//!
//! Adapters (SQLite, hardware, sync, UI transport) implement these; the
//! application never depends on an adapter crate.

use chrono::Utc;
use edge_cash::{Approval, CashMovement, CloseDeclaration, SafeDrop};
use edge_catalog::{
    CatalogBarcode, CatalogProduct, CatalogSnapshot, ImportProgress, PriceBook, PriceCandidate,
    PriceEntry, ProductSelector, Promotion, SnapshotCounts, TaxProfile, TaxProfileCode,
};
use edge_domain::prelude::*;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChunk {
    pub chunk_index: i64,
    pub chunk_hash: String,
    pub product_count: i64,
    pub price_entry_count: i64,
    pub barcode_count: i64,
    pub promotion_count: i64,
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

pub trait UnitOfWork:
    CatalogUnitOfWork + PaymentUnitOfWork + CashUnitOfWork + InventoryUnitOfWork + RefundUnitOfWork
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
