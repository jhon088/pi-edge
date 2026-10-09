//! The `EdgeClient` contract.

use std::fmt::Debug;

use edge_protocol::{
    AddItemRequestV1, ApproveSafeDropRequestV1, BeginCatalogImportRequestV1, BusinessDayV1,
    CashMovementRequestV1, CashPositionV1, CatalogImportStatusV1, CatalogSnapshotViewV1,
    CatalogStatusV1, ChangeQuantityRequestV1, CommitCatalogImportRequestV1, CoreInfoV1,
    DeclareClosingCashRequestV1, DeviceV1, EdgeStateV1, ForceCloseBusinessDayRequestV1,
    ImportInventoryBootstrapRequestV1, InitDeviceRequestV1, InventoryBootstrapV1,
    InventoryMovementRequestV1, InventoryMovementV1, InventoryPositionV1, InventoryQueryV1,
    JournalEventV1, OpenRegisterSessionRequestV1, PaymentRequestV1, PaymentResultV1,
    PendingOutboxV1, ProductLookupV1, ProductSelectorV1, PutCatalogChunkRequestV1,
    RegisterSessionV1, RemoveItemRequestV1, ReopenSessionRequestV1, RetailTransactionV1,
    SafeDropV1, SessionCloseRequirementsV1, SessionCloseResultV1, StartPaymentRequestV1,
    TransactionPaymentsV1, TransactionRequestV1,
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
    fn force_close_business_day(
        &self,
        request: ForceCloseBusinessDayRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<BusinessDayV1, ClientError>;
    fn close_business_day(&self, request_id: Option<Uuid>) -> Result<BusinessDayV1, ClientError>;

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

    /// Link diagnostics for the UI's connection indicator.
    fn connection(&self) -> ConnectionStatus;
}
