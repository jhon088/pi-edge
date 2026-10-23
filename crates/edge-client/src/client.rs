//! The `EdgeClient` contract.

use std::fmt::Debug;

use edge_protocol::{
    AddItemRequestV1, ApproveSafeDropRequestV1, BeginCatalogImportRequestV1, BusinessDayCloseV1,
    BusinessDayV1, CashMovementRequestV1, CashPositionV1, CatalogImportStatusV1,
    CatalogSnapshotViewV1, CatalogStatusV1, ChangeQuantityRequestV1, CommitCatalogImportRequestV1,
    CoreInfoV1, CreateRefundRequestV1, DeclareClosingCashRequestV1, DeviceV1, EdgeStateV1,
    ForceCloseBusinessDayRequestV1, HardwareStatusV1, ImportInventoryBootstrapRequestV1,
    InitDeviceRequestV1, InventoryBootstrapV1, InventoryMovementRequestV1, InventoryMovementV1,
    InventoryPositionV1, InventoryQueryV1, JournalEventV1, OpenRegisterSessionRequestV1,
    PaymentRequestV1, PaymentResultV1, PendingOutboxV1, PrintTransactionRequestV1, ProductLookupV1,
    ProductSelectorV1, PutCatalogChunkRequestV1, ReceiptV1, RecentSalesV1, RefundEligibilityV1,
    RefundRequestV1, RefundV1, RegisterSessionV1, RemoveItemRequestV1, ReopenSessionRequestV1,
    RetailTransactionV1, SafeDropV1, SessionCloseRequirementsV1, SessionCloseResultV1,
    StartPaymentRequestV1, SubmitScanRequestV1, SyncCycleV1, SyncStatusV1, TransactionPaymentsV1,
    TransactionRequestV1,
};
use uuid::Uuid;

use crate::error::ClientError;

/// What a client knows about its link to the core. Diagnostics only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionStatus {
    /// A live channel *and* no open backoff window. False as soon as the
    /// client has any reason to believe the core is gone, so a UI never shows
    /// "connected" over a dead runtime.
    pub connected: bool,
    /// Endpoint identity, never a credential.
    pub endpoint: String,
    /// How this client reaches the core, e.g. `IPC`.
    pub mode: &'static str,
    /// Remaining backoff before the next connection attempt, if any.
    pub reconnect_in_ms: Option<u64>,
}

/// Everything a UI or CLI can ask of the core.
///
/// Mutating operations take an optional `request_id`. When supplied it travels
/// in the request envelope and becomes the core's idempotency key, so a client
/// that lost a reply can replay the identical request instead of risking a
/// second sale. When omitted the client generates a fresh one.
pub trait EdgeClient: Debug + Send + Sync {
    // queries
    fn core_info(&self) -> Result<CoreInfoV1, ClientError>;
    fn get_edge_state(&self) -> Result<EdgeStateV1, ClientError>;
    fn get_active_transaction(&self) -> Result<Option<RetailTransactionV1>, ClientError>;
    fn get_pending_outbox(&self, limit: i64) -> Result<PendingOutboxV1, ClientError>;
    /// What sync knows: how much is waiting, how much is stuck, how far the
    /// inbound stream got.
    fn get_sync_status(&self) -> Result<SyncStatusV1, ClientError>;
    /// What one sale owes the customer in paper, and what became of it.
    fn get_receipt(&self, transaction_id: &str) -> Result<ReceiptV1, ClientError>;
    /// Recent sales, newest first, with their receipt state.
    fn list_recent_sales(&self, limit: i64, offset: i64) -> Result<RecentSalesV1, ClientError>;
    fn get_journal(&self, limit: i64) -> Result<Vec<JournalEventV1>, ClientError>;
    fn catalog_status(&self) -> Result<CatalogStatusV1, ClientError>;
    fn catalog_import_status(
        &self,
        snapshot_id: &str,
    ) -> Result<CatalogImportStatusV1, ClientError>;
    /// Diagnostic product lookup. Adding to a sale goes through `add_item`.
    fn lookup_product(&self, selector: ProductSelectorV1) -> Result<ProductLookupV1, ClientError>;

    // commands
    fn init_device(&self, request: InitDeviceRequestV1) -> Result<DeviceV1, ClientError>;
    fn open_business_day(&self, request_id: Option<Uuid>) -> Result<BusinessDayV1, ClientError>;
    fn open_session(
        &self,
        request: OpenRegisterSessionRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RegisterSessionV1, ClientError>;
    fn start_transaction(
        &self,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError>;
    fn add_item(
        &self,
        request: AddItemRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError>;
    fn remove_item(
        &self,
        request: RemoveItemRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError>;
    fn change_quantity(
        &self,
        request: ChangeQuantityRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError>;
    fn totalize(&self, request_id: Option<Uuid>) -> Result<RetailTransactionV1, ClientError>;
    /// Start a payment. Cash settles immediately; a card goes through its
    /// provider and may come back unresolved.
    fn start_payment(
        &self,
        request: StartPaymentRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<PaymentResultV1, ClientError>;
    /// Ask the provider what happened to an unresolved payment. Never charges.
    fn reconcile_payment(
        &self,
        request: PaymentRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<PaymentResultV1, ClientError>;
    /// Undo a payment that must not remain applied.
    fn reverse_payment(
        &self,
        request: PaymentRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<PaymentResultV1, ClientError>;
    fn get_payment(&self, request: PaymentRequestV1) -> Result<PaymentResultV1, ClientError>;
    fn get_transaction_payments(
        &self,
        request: TransactionRequestV1,
    ) -> Result<TransactionPaymentsV1, ClientError>;
    fn finalize_transaction(
        &self,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError>;
    // -------------------------------------------------------------- hardware

    /// What each device is and what we know about it. Probes; prints nothing
    /// and opens nothing.
    fn hardware_status(&self) -> Result<HardwareStatusV1, ClientError>;
    /// A completed scan becomes an item on the active sale.
    fn submit_scan(&self, request: SubmitScanRequestV1)
        -> Result<RetailTransactionV1, ClientError>;
    /// A diagnostic page. Touches no commercial data.
    fn print_test_page(&self, request_id: Option<Uuid>) -> Result<HardwareStatusV1, ClientError>;
    /// Print a finalized sale, or print it again. A reprint changes nothing.
    fn print_transaction(
        &self,
        request: PrintTransactionRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<HardwareStatusV1, ClientError>;
    /// Open the drawer because an operator asked. Audited, and it opens every
    /// time it is asked.
    fn open_drawer(&self, request_id: Option<Uuid>) -> Result<HardwareStatusV1, ClientError>;

    // --------------------------------------------------------------- refunds

    /// What a sale can still give back. The core computes every figure.
    fn refund_eligibility(
        &self,
        request: TransactionRequestV1,
    ) -> Result<RefundEligibilityV1, ClientError>;
    /// Open a refund. The runtime resolves the authority over the original
    /// sale; a client cannot assert it (ADR 030).
    fn create_refund(
        &self,
        request: CreateRefundRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RefundV1, ClientError>;
    /// The goods came back: writes the inventory return, exactly once.
    fn accept_refund(
        &self,
        request: RefundRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RefundV1, ClientError>;
    /// Give the money back, spread over the original tenders by the core.
    fn start_refund_payment(
        &self,
        request: RefundRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RefundV1, ClientError>;
    /// Ask the provider about an unresolved refund. Never refunds again.
    fn reconcile_refund(
        &self,
        request: RefundRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RefundV1, ClientError>;
    fn get_refund(&self, request: RefundRequestV1) -> Result<RefundV1, ClientError>;
    fn transaction_refunds(
        &self,
        request: TransactionRequestV1,
    ) -> Result<Vec<RefundV1>, ClientError>;
    // ------------------------------------------------------------- inventory

    /// Import the opening baseline. One per installation, ever (ADR 028).
    fn import_inventory_bootstrap(
        &self,
        request: ImportInventoryBootstrapRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<InventoryBootstrapV1, ClientError>;
    /// What the ledger says is here, optionally with the movements that
    /// explain it.
    fn inventory_position(
        &self,
        request: InventoryQueryV1,
    ) -> Result<InventoryPositionV1, ClientError>;
    fn inventory_movements(
        &self,
        request: InventoryQueryV1,
    ) -> Result<Vec<InventoryMovementV1>, ClientError>;
    fn inventory_in(
        &self,
        request: InventoryMovementRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<InventoryPositionV1, ClientError>;
    fn inventory_out(
        &self,
        request: InventoryMovementRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<InventoryPositionV1, ClientError>;

    // ------------------------------------------------------- cash management

    /// What the drawer should contain, and the movements that explain it.
    fn cash_position(&self) -> Result<CashPositionV1, ClientError>;
    fn cash_in(
        &self,
        request: CashMovementRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<CashPositionV1, ClientError>;
    fn cash_out(
        &self,
        request: CashMovementRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<CashPositionV1, ClientError>;
    /// Ask for a safe drop. Moves no money on its own.
    fn request_safe_drop(
        &self,
        request: CashMovementRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<SafeDropV1, ClientError>;
    fn approve_safe_drop(
        &self,
        request: ApproveSafeDropRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<SafeDropV1, ClientError>;
    /// Begin a close. The answer never carries the expected cash (ADR 022).
    fn request_session_close(
        &self,
        request_id: Option<Uuid>,
    ) -> Result<SessionCloseRequirementsV1, ClientError>;
    /// Declare the counted cash. Only now does the core reveal the variance.
    fn declare_closing_cash(
        &self,
        request: DeclareClosingCashRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<SessionCloseResultV1, ClientError>;
    fn reopen_session(
        &self,
        request: ReopenSessionRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RegisterSessionV1, ClientError>;
    /// End the day over open sessions. Answers the day plus the receipts of
    /// that day still needing attention.
    fn force_close_business_day(
        &self,
        request: ForceCloseBusinessDayRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<BusinessDayCloseV1, ClientError>;
    /// Close the day. Answers the day plus the receipts of that day still
    /// needing attention — never a reason the close was refused (ADR 043).
    fn close_business_day(
        &self,
        request_id: Option<Uuid>,
    ) -> Result<BusinessDayCloseV1, ClientError>;

    // catalog import: open, stream, close, put in force
    fn begin_catalog_import(
        &self,
        request: BeginCatalogImportRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<CatalogImportStatusV1, ClientError>;
    /// Idempotent on `(snapshot_id, chunk_index, chunk_hash)`, so a resend
    /// after a disconnect is safe and a different payload for the same index is
    /// refused.
    fn put_catalog_chunk(
        &self,
        request: PutCatalogChunkRequestV1,
    ) -> Result<CatalogImportStatusV1, ClientError>;
    fn commit_catalog_import(
        &self,
        request: CommitCatalogImportRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<CatalogSnapshotViewV1, ClientError>;
    fn abort_catalog_import(
        &self,
        snapshot_id: &str,
        request_id: Option<Uuid>,
    ) -> Result<(), ClientError>;
    fn activate_catalog_snapshot(
        &self,
        snapshot_id: &str,
        request_id: Option<Uuid>,
    ) -> Result<CatalogSnapshotViewV1, ClientError>;

    /// Abandon the active sale. Refused once any money has been applied.
    fn cancel_transaction(
        &self,
        reason_code: Option<String>,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError>;

    /// Run one dispatch cycle now. Answers `NOT_CONFIGURED` and changes
    /// nothing when this installation has no sync transport.
    fn run_sync_cycle(&self, request_id: Option<Uuid>) -> Result<SyncCycleV1, ClientError>;

    /// Link diagnostics for the UI's connection indicator.
    fn connection(&self) -> ConnectionStatus;
}
