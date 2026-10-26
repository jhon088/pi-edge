//! Envelopes and frames of Edge Protocol V1.
//!
//! Wire encoding is JSON Lines: one compact JSON object per `\n`-terminated
//! line. `serde_json` escapes newlines inside strings, so a line break is an
//! unambiguous frame delimiter. The transport owns the bytes; this module owns
//! the shapes.
//!
//! Direction of travel:
//!
//! ```text
//! client -> daemon : ClientFrameV1  (HELLO once, then REQUEST*)
//! daemon -> client : ServerFrameV1  (WELCOME, then RESPONSE* / EVENT*)
//! ```
//!
//! Requests and responses are correlated by `request_id`. For mutating
//! operations that same `request_id` is also the core's idempotency key, so a
//! client that loses a reply can replay the exact request safely.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::dto::*;
use crate::PROTOCOL_VERSION;

/// First frame of every connection. Authentication happens once per
/// connection; the token is never repeated on subsequent frames.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloV1 {
    pub protocol_version: u32,
    pub client_name: String,
    pub client_version: String,
    /// Local credential. Never logged, never echoed, never shown in a UI.
    pub auth_token: String,
}

/// Hand-written so the credential cannot leak through `{:?}` in a log line,
/// a `tracing` field or an error message.
impl std::fmt::Debug for HelloV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelloV1")
            .field("protocol_version", &self.protocol_version)
            .field("client_name", &self.client_name)
            .field("client_version", &self.client_version)
            .field("auth_token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClientFrameV1 {
    Hello(HelloV1),
    Request(RequestEnvelopeV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelopeV1 {
    pub protocol_version: u32,
    pub request_id: Uuid,
    #[serde(flatten)]
    pub operation: OperationV1,
}

impl RequestEnvelopeV1 {
    pub fn new(request_id: Uuid, operation: OperationV1) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            operation,
        }
    }
}

/// Everything a client can ask the core to do.
///
/// Queries are safe to retry; commands are not, unless the client reuses the
/// same `request_id` (see [`RequestEnvelopeV1`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "payload",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
pub enum OperationV1 {
    // queries
    GetCoreInfo,
    GetEdgeState,
    GetActiveTransaction,
    GetPendingOutbox(LimitRequestV1),
    GetSyncStatus,
    /// What one sale owes the customer in paper, and what became of it.
    GetReceipt(TransactionRequestV1),
    /// Recent sales, newest first, with their receipt state. Operational
    /// history: no analytics, no search, no filters.
    ListRecentSales(PageRequestV1),
    GetJournal(LimitRequestV1),
    /// Turns this connection into an event stream. No further requests are
    /// accepted on it.
    Subscribe,

    GetCatalogStatus,
    GetCatalogImportStatus(CatalogSnapshotRequestV1),
    LookupProduct(ProductSelectorV1),

    // commands
    InitDevice(InitDeviceRequestV1),
    OpenBusinessDay,
    OpenRegisterSession(OpenRegisterSessionRequestV1),
    StartTransaction,
    AddItem(AddItemRequestV1),
    RemoveItem(RemoveItemRequestV1),
    ChangeQuantity(ChangeQuantityRequestV1),
    Totalize,
    /// Abandon a sale that has taken no money.
    CancelTransaction(CancelTransactionRequestV1),
    StartPayment(StartPaymentRequestV1),
    ReconcilePayment(PaymentRequestV1),
    ReversePayment(PaymentRequestV1),
    GetPayment(PaymentRequestV1),
    GetTransactionPayments(TransactionRequestV1),
    FinalizeTransaction,
    GetHardwareStatus,
    SubmitScan(SubmitScanRequestV1),
    PrintTestPage,
    /// What receipt printer this installation is configured to drive, and
    /// what models and host queues it could be pointed at.
    GetPrinterSettings,
    /// Point it at a different one. Device configuration, never a sale.
    SetPrinterSettings(PrinterSettingsV1),
    PrintTransaction(PrintTransactionRequestV1),
    OpenDrawerDiagnostic,
    GetRefundEligibility(TransactionRequestV1),
    CreateRefund(CreateRefundRequestV1),
    AcceptRefund(RefundRequestV1),
    StartRefundPayment(RefundRequestV1),
    ReconcileRefund(RefundRequestV1),
    GetRefund(RefundRequestV1),
    GetTransactionRefunds(TransactionRequestV1),
    ImportInventoryBootstrap(ImportInventoryBootstrapRequestV1),
    GetInventoryPosition(InventoryQueryV1),
    GetInventoryMovements(InventoryQueryV1),
    InventoryIn(InventoryMovementRequestV1),
    InventoryOut(InventoryMovementRequestV1),
    GetCashPosition,
    CashIn(CashMovementRequestV1),
    CashOut(CashMovementRequestV1),
    RequestSafeDrop(CashMovementRequestV1),
    ApproveSafeDrop(ApproveSafeDropRequestV1),
    RequestSessionClose,
    DeclareClosingCash(DeclareClosingCashRequestV1),
    ReopenSession(ReopenSessionRequestV1),
    ForceCloseBusinessDay(ForceCloseBusinessDayRequestV1),
    CloseBusinessDay,

    /// Run one dispatch cycle now instead of waiting for the daemon's own
    /// tick. Safe to call with no transport configured: it answers
    /// `NOT_CONFIGURED` and changes nothing.
    RunSyncCycle,

    // catalog import: open, stream, close, put in force
    /// Boxed, like the chunk below: an import header carries the tax
    /// profiles, price books, channels and categories of a whole snapshot.
    BeginCatalogImport(Box<BeginCatalogImportRequestV1>),
    /// Boxed: a chunk carries bulk catalog content and is by far the largest
    /// thing a client sends, so every other operation would otherwise pay for
    /// its size. `Box<T>` serializes exactly as `T`, so the wire is unchanged.
    PutCatalogChunk(Box<PutCatalogChunkRequestV1>),
    CommitCatalogImport(CommitCatalogImportRequestV1),
    AbortCatalogImport(CatalogSnapshotRequestV1),
    ActivateCatalogSnapshot(CatalogSnapshotRequestV1),

    // local asset runtime: what a snapshot needs, and the bytes for it
    /// What one snapshot references and what of it is here. A client calls
    /// this to learn what to send, and after a refused activation to learn
    /// why (ADR 045).
    GetSnapshotAssets(SnapshotAssetsRequestV1),
    GetAssetStatus(AssetRequestV1),
    /// Stream part of an asset's bytes. The read half of the asset runtime,
    /// so a kiosk draws a photo with no network and no path (ADR 045).
    ReadAsset(AssetReadRequestV1),
    /// The menu in force, for one way of selling.
    GetMenu(MenuRequestV1),
    /// The questions one product asks, each answer with its own price.
    GetProductConfiguration(ProductConfigurationRequestV1),
    /// Boxed: a chunk is bulk bytes, like a catalog chunk.
    PutAssetChunk(Box<AssetChunkRequestV1>),
    CommitAsset(AssetRequestV1),
}

impl OperationV1 {
    /// Stable name used for logging and for the `reason` of an event.
    pub const fn name(&self) -> &'static str {
        match self {
            OperationV1::GetCoreInfo => "GET_CORE_INFO",
            OperationV1::GetEdgeState => "GET_EDGE_STATE",
            OperationV1::GetActiveTransaction => "GET_ACTIVE_TRANSACTION",
            OperationV1::GetPendingOutbox(_) => "GET_PENDING_OUTBOX",
            OperationV1::GetSyncStatus => "GET_SYNC_STATUS",
            OperationV1::GetReceipt(_) => "GET_RECEIPT",
            OperationV1::ListRecentSales(_) => "LIST_RECENT_SALES",
            OperationV1::CancelTransaction(_) => "CANCEL_TRANSACTION",
            OperationV1::RunSyncCycle => "RUN_SYNC_CYCLE",
            OperationV1::GetJournal(_) => "GET_JOURNAL",
            OperationV1::GetCatalogStatus => "GET_CATALOG_STATUS",
            OperationV1::GetCatalogImportStatus(_) => "GET_CATALOG_IMPORT_STATUS",
            OperationV1::LookupProduct(_) => "LOOKUP_PRODUCT",
            OperationV1::Subscribe => "SUBSCRIBE",
            OperationV1::InitDevice(_) => "INIT_DEVICE",
            OperationV1::OpenBusinessDay => "OPEN_BUSINESS_DAY",
            OperationV1::OpenRegisterSession(_) => "OPEN_REGISTER_SESSION",
            OperationV1::StartTransaction => "START_TRANSACTION",
            OperationV1::AddItem(_) => "ADD_ITEM",
            OperationV1::RemoveItem(_) => "REMOVE_ITEM",
            OperationV1::ChangeQuantity(_) => "CHANGE_QUANTITY",
            OperationV1::Totalize => "TOTALIZE",
            OperationV1::StartPayment(_) => "START_PAYMENT",
            OperationV1::ReconcilePayment(_) => "RECONCILE_PAYMENT",
            OperationV1::ReversePayment(_) => "REVERSE_PAYMENT",
            OperationV1::GetPayment(_) => "GET_PAYMENT",
            OperationV1::GetTransactionPayments(_) => "GET_TRANSACTION_PAYMENTS",
            OperationV1::FinalizeTransaction => "FINALIZE_TRANSACTION",
            OperationV1::GetHardwareStatus => "GET_HARDWARE_STATUS",
            OperationV1::SubmitScan(_) => "SUBMIT_SCAN",
            OperationV1::PrintTestPage => "PRINT_TEST_PAGE",
            OperationV1::GetPrinterSettings => "GET_PRINTER_SETTINGS",
            OperationV1::SetPrinterSettings(_) => "SET_PRINTER_SETTINGS",
            OperationV1::PrintTransaction(_) => "PRINT_TRANSACTION",
            OperationV1::OpenDrawerDiagnostic => "OPEN_DRAWER_DIAGNOSTIC",
            OperationV1::GetRefundEligibility(_) => "GET_REFUND_ELIGIBILITY",
            OperationV1::CreateRefund(_) => "CREATE_REFUND",
            OperationV1::AcceptRefund(_) => "ACCEPT_REFUND",
            OperationV1::StartRefundPayment(_) => "START_REFUND_PAYMENT",
            OperationV1::ReconcileRefund(_) => "RECONCILE_REFUND",
            OperationV1::GetRefund(_) => "GET_REFUND",
            OperationV1::GetTransactionRefunds(_) => "GET_TRANSACTION_REFUNDS",
            OperationV1::ImportInventoryBootstrap(_) => "IMPORT_INVENTORY_BOOTSTRAP",
            OperationV1::GetInventoryPosition(_) => "GET_INVENTORY_POSITION",
            OperationV1::GetInventoryMovements(_) => "GET_INVENTORY_MOVEMENTS",
            OperationV1::InventoryIn(_) => "INVENTORY_IN",
            OperationV1::InventoryOut(_) => "INVENTORY_OUT",
            OperationV1::GetCashPosition => "GET_CASH_POSITION",
            OperationV1::CashIn(_) => "CASH_IN",
            OperationV1::CashOut(_) => "CASH_OUT",
            OperationV1::RequestSafeDrop(_) => "REQUEST_SAFE_DROP",
            OperationV1::ApproveSafeDrop(_) => "APPROVE_SAFE_DROP",
            OperationV1::RequestSessionClose => "REQUEST_SESSION_CLOSE",
            OperationV1::DeclareClosingCash(_) => "DECLARE_CLOSING_CASH",
            OperationV1::ReopenSession(_) => "REOPEN_SESSION",
            OperationV1::ForceCloseBusinessDay(_) => "FORCE_CLOSE_BUSINESS_DAY",
            OperationV1::CloseBusinessDay => "CLOSE_BUSINESS_DAY",
            OperationV1::BeginCatalogImport(_) => "BEGIN_CATALOG_IMPORT",
            OperationV1::PutCatalogChunk(_) => "PUT_CATALOG_CHUNK",
            OperationV1::CommitCatalogImport(_) => "COMMIT_CATALOG_IMPORT",
            OperationV1::AbortCatalogImport(_) => "ABORT_CATALOG_IMPORT",
            OperationV1::ActivateCatalogSnapshot(_) => "ACTIVATE_CATALOG_SNAPSHOT",
            OperationV1::GetSnapshotAssets(_) => "GET_SNAPSHOT_ASSETS",
            OperationV1::GetAssetStatus(_) => "GET_ASSET_STATUS",
            OperationV1::ReadAsset(_) => "READ_ASSET",
            OperationV1::GetMenu(_) => "GET_MENU",
            OperationV1::GetProductConfiguration(_) => "GET_PRODUCT_CONFIGURATION",
            OperationV1::PutAssetChunk(_) => "PUT_ASSET_CHUNK",
            OperationV1::CommitAsset(_) => "COMMIT_ASSET",
        }
    }

    /// Whether the operation changes durable state. Mutating operations are
    /// never retried automatically by a client.
    pub const fn is_command(&self) -> bool {
        matches!(
            self,
            OperationV1::InitDevice(_)
                | OperationV1::OpenBusinessDay
                | OperationV1::OpenRegisterSession(_)
                | OperationV1::StartTransaction
                | OperationV1::AddItem(_)
                | OperationV1::RemoveItem(_)
                | OperationV1::ChangeQuantity(_)
                | OperationV1::Totalize
                | OperationV1::CancelTransaction(_)
                | OperationV1::StartPayment(_)
                | OperationV1::ReconcilePayment(_)
                | OperationV1::ReversePayment(_)
                | OperationV1::FinalizeTransaction
                | OperationV1::SubmitScan(_)
                | OperationV1::PrintTestPage
                | OperationV1::SetPrinterSettings(_)
                | OperationV1::PrintTransaction(_)
                | OperationV1::OpenDrawerDiagnostic
                | OperationV1::CreateRefund(_)
                | OperationV1::AcceptRefund(_)
                | OperationV1::StartRefundPayment(_)
                | OperationV1::ReconcileRefund(_)
                | OperationV1::ImportInventoryBootstrap(_)
                | OperationV1::InventoryIn(_)
                | OperationV1::InventoryOut(_)
                | OperationV1::CashIn(_)
                | OperationV1::CashOut(_)
                | OperationV1::RequestSafeDrop(_)
                | OperationV1::ApproveSafeDrop(_)
                | OperationV1::RunSyncCycle
                | OperationV1::RequestSessionClose
                | OperationV1::DeclareClosingCash(_)
                | OperationV1::ReopenSession(_)
                | OperationV1::ForceCloseBusinessDay(_)
                | OperationV1::CloseBusinessDay
                | OperationV1::BeginCatalogImport(_)
                | OperationV1::PutCatalogChunk(_)
                | OperationV1::CommitCatalogImport(_)
                | OperationV1::AbortCatalogImport(_)
                | OperationV1::ActivateCatalogSnapshot(_)
                | OperationV1::PutAssetChunk(_)
                | OperationV1::CommitAsset(_)
        )
    }
}

// Variant sizes differ a lot because responses carry whole aggregates. These
// values exist to be serialized onto a socket straight away, so the extra
// stack bytes never outlive a frame; boxing each payload would only add
// indirection to every match site.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServerFrameV1 {
    /// Handshake accepted.
    Welcome(WelcomeV1),
    Response(ResponseEnvelopeV1),
    Event(EventEnvelopeV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WelcomeV1 {
    pub protocol_version: u32,
    pub core_version: String,
    pub status: CoreStatusV1,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEnvelopeV1 {
    pub protocol_version: u32,
    pub request_id: Uuid,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorV1>,
}

impl ResponseEnvelopeV1 {
    pub fn ok(request_id: Uuid, result: ResultV1) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            success: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failed(request_id: Uuid, error: ErrorV1) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            success: false,
            result: None,
            error: Some(error),
        }
    }
}

// Same reasoning as `ServerFrameV1`: these are wire payloads, not long-lived
// in-memory values.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "result",
    content = "payload",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
pub enum ResultV1 {
    Acknowledged,
    CoreInfo(CoreInfoV1),
    EdgeState(EdgeStateV1),
    Device(DeviceV1),
    BusinessDay(BusinessDayV1),
    /// The result of closing the day: the day plus what still needs somebody.
    BusinessDayClose(BusinessDayCloseV1),
    RegisterSession(RegisterSessionV1),
    Transaction(RetailTransactionV1),
    MaybeTransaction(Option<RetailTransactionV1>),
    PendingOutbox(PendingOutboxV1),
    SyncStatus(SyncStatusV1),
    Receipt(ReceiptV1),
    RecentSales(RecentSalesV1),
    SyncCycle(SyncCycleV1),
    Journal(Vec<JournalEventV1>),
    CatalogStatus(CatalogStatusV1),
    CatalogImport(CatalogImportStatusV1),
    CatalogSnapshot(CatalogSnapshotViewV1),
    ProductLookup(ProductLookupV1),
    Payment(PaymentResultV1),
    TransactionPayments(TransactionPaymentsV1),
    CashPosition(CashPositionV1),
    /// What receipt printer this installation drives, and what it could.
    PrinterConfiguration(PrinterConfigurationV1),
    SafeDrop(SafeDropV1),
    SessionCloseRequirements(SessionCloseRequirementsV1),
    SessionCloseResult(SessionCloseResultV1),
    InventoryPosition(InventoryPositionV1),
    InventoryMovements(Vec<InventoryMovementV1>),
    InventoryBootstrap(InventoryBootstrapV1),
    RefundEligibility(RefundEligibilityV1),
    Refund(RefundV1),
    Refunds(Vec<RefundV1>),
    HardwareStatus(HardwareStatusV1),
    AssetStatus(AssetStatusV1),
    SnapshotAssets(SnapshotAssetsV1),
    AssetChunk(AssetChunkV1),
    Menu(MenuV1),
    ProductConfiguration(ProductConfigurationV1),
}

impl ResultV1 {
    pub const fn name(&self) -> &'static str {
        match self {
            ResultV1::Acknowledged => "ACKNOWLEDGED",
            ResultV1::CoreInfo(_) => "CORE_INFO",
            ResultV1::EdgeState(_) => "EDGE_STATE",
            ResultV1::Device(_) => "DEVICE",
            ResultV1::BusinessDay(_) => "BUSINESS_DAY",
            ResultV1::BusinessDayClose(_) => "BUSINESS_DAY_CLOSE",
            ResultV1::RegisterSession(_) => "REGISTER_SESSION",
            ResultV1::Transaction(_) => "TRANSACTION",
            ResultV1::MaybeTransaction(_) => "MAYBE_TRANSACTION",
            ResultV1::PendingOutbox(_) => "PENDING_OUTBOX",
            ResultV1::SyncStatus(_) => "SYNC_STATUS",
            ResultV1::Receipt(_) => "RECEIPT",
            ResultV1::RecentSales(_) => "RECENT_SALES",
            ResultV1::SyncCycle(_) => "SYNC_CYCLE",
            ResultV1::Journal(_) => "JOURNAL",
            ResultV1::CatalogStatus(_) => "CATALOG_STATUS",
            ResultV1::CatalogImport(_) => "CATALOG_IMPORT",
            ResultV1::CatalogSnapshot(_) => "CATALOG_SNAPSHOT",
            ResultV1::ProductLookup(_) => "PRODUCT_LOOKUP",
            ResultV1::Payment(_) => "PAYMENT",
            ResultV1::TransactionPayments(_) => "TRANSACTION_PAYMENTS",
            ResultV1::CashPosition(_) => "CASH_POSITION",
            ResultV1::PrinterConfiguration(_) => "PRINTER_CONFIGURATION",
            ResultV1::SafeDrop(_) => "SAFE_DROP",
            ResultV1::SessionCloseRequirements(_) => "SESSION_CLOSE_REQUIREMENTS",
            ResultV1::SessionCloseResult(_) => "SESSION_CLOSE_RESULT",
            ResultV1::InventoryPosition(_) => "INVENTORY_POSITION",
            ResultV1::InventoryMovements(_) => "INVENTORY_MOVEMENTS",
            ResultV1::InventoryBootstrap(_) => "INVENTORY_BOOTSTRAP",
            ResultV1::RefundEligibility(_) => "REFUND_ELIGIBILITY",
            ResultV1::Refund(_) => "REFUND",
            ResultV1::Refunds(_) => "REFUNDS",
            ResultV1::HardwareStatus(_) => "HARDWARE_STATUS",
            ResultV1::AssetStatus(_) => "ASSET_STATUS",
            ResultV1::SnapshotAssets(_) => "SNAPSHOT_ASSETS",
            ResultV1::AssetChunk(_) => "ASSET_CHUNK",
            ResultV1::Menu(_) => "MENU",
            ResultV1::ProductConfiguration(_) => "PRODUCT_CONFIGURATION",
        }
    }
}

/// Machine-readable failure. Clients branch on `code` (see
/// [`crate::error_codes`]) and never on `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorV1 {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl ErrorV1 {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelopeV1 {
    pub protocol_version: u32,
    #[serde(flatten)]
    pub event: EventV1,
}

impl EventEnvelopeV1 {
    pub fn new(event: EventV1) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            event,
        }
    }
}

/// Server-to-client notifications.
///
/// Deliberately coarse in V1: a thin signal that something changed, carrying no
/// business payload. The client answers by re-running `GET_EDGE_STATE`, which
/// keeps the core the only source of truth (ADR 002). Fine-grained events
/// (`TransactionChanged`, `PaymentChanged`, `OutboxChanged`,
/// `HardwareStateChanged`, …) are added when a client actually needs to react
/// differently to each one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "event",
    content = "payload",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
pub enum EventV1 {
    EdgeStateChanged(EdgeStateChangedV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeStateChangedV1 {
    /// Operation that caused the change, e.g. `ADD_ITEM`.
    pub reason: String,
    /// Monotonic within one daemon lifetime. A client whose last known
    /// revision is lower is stale and should re-query.
    pub revision: u64,
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_envelope_is_flat_on_the_wire() {
        let envelope = RequestEnvelopeV1::new(
            Uuid::from_u128(1),
            OperationV1::StartPayment(StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: 11_600,
            }),
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"protocol_version":1,"request_id":"00000000-0000-0000-0000-000000000001","operation":"START_PAYMENT","payload":{"tender_type":"CASH","amount_minor":11600}}"#
        );
        assert_eq!(
            serde_json::from_str::<RequestEnvelopeV1>(&json).unwrap(),
            envelope
        );
    }

    #[test]
    fn operations_without_a_payload_omit_the_payload_field() {
        let envelope = RequestEnvelopeV1::new(Uuid::from_u128(2), OperationV1::GetEdgeState);
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"protocol_version":1,"request_id":"00000000-0000-0000-0000-000000000002","operation":"GET_EDGE_STATE"}"#
        );
        assert_eq!(
            serde_json::from_str::<RequestEnvelopeV1>(&json).unwrap(),
            envelope
        );
    }

    #[test]
    fn frames_round_trip() {
        let hello = ClientFrameV1::Hello(HelloV1 {
            protocol_version: PROTOCOL_VERSION,
            client_name: "edge-cli".to_owned(),
            client_version: "0.1.0".to_owned(),
            auth_token: "deadbeef".to_owned(),
        });
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.starts_with(r#"{"frame":"HELLO""#));
        assert_eq!(serde_json::from_str::<ClientFrameV1>(&json).unwrap(), hello);

        let event = ServerFrameV1::Event(EventEnvelopeV1::new(EventV1::EdgeStateChanged(
            EdgeStateChangedV1 {
                reason: "ADD_ITEM".to_owned(),
                revision: 7,
            },
        )));
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"frame":"EVENT","protocol_version":1,"event":"EDGE_STATE_CHANGED","payload":{"reason":"ADD_ITEM","revision":7}}"#
        );
        assert_eq!(serde_json::from_str::<ServerFrameV1>(&json).unwrap(), event);
    }

    #[test]
    fn hello_debug_never_reveals_the_token() {
        let hello = HelloV1 {
            protocol_version: PROTOCOL_VERSION,
            client_name: "edge-cli".to_owned(),
            client_version: "0.1.0".to_owned(),
            auth_token: "super-secret-token".to_owned(),
        };
        let rendered = format!("{hello:?}");
        assert!(!rendered.contains("super-secret-token"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn json_lines_framing_is_unambiguous() {
        // A newline inside a payload string must be escaped, never emitted raw.
        let envelope = RequestEnvelopeV1::new(
            Uuid::from_u128(3),
            OperationV1::AddItem(AddItemRequestV1 {
                product: ProductSelectorV1::Barcode("750\n123".to_owned()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            }),
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(!json.contains('\n'));
        assert!(json.contains(r"750\n123"));
    }

    #[test]
    fn commands_and_queries_are_classified() {
        assert!(OperationV1::AddItem(AddItemRequestV1 {
            product: ProductSelectorV1::Sku("SKU-1".to_owned()),
            quantity_thousandths: 1,
            selections: Vec::new(),
            preparation_note: None,
        })
        .is_command());
        assert!(
            OperationV1::PutCatalogChunk(Box::new(PutCatalogChunkRequestV1 {
                promotions: Vec::new(),
                snapshot_id: "s".to_owned(),
                chunk_index: 0,
                chunk_hash: "h".to_owned(),
                products: Vec::new(),
                presentation: Vec::new(),
                selection_groups: Vec::new(),
                selection_options: Vec::new(),
                availability: Vec::new(),
            }))
            .is_command()
        );
        assert!(!OperationV1::GetCatalogStatus.is_command());
        assert!(!OperationV1::GetEdgeState.is_command());
        assert!(!OperationV1::Subscribe.is_command());
    }
}
