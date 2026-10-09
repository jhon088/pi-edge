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
    CommitCashPayment(CommitCashPaymentRequestV1),
    FinalizeTransaction,
    CloseRegisterSession,
    CloseBusinessDay,

    // catalog import: open, stream, close, put in force
    BeginCatalogImport(BeginCatalogImportRequestV1),
    PutCatalogChunk(PutCatalogChunkRequestV1),
    CommitCatalogImport(CommitCatalogImportRequestV1),
    AbortCatalogImport(CatalogSnapshotRequestV1),
    ActivateCatalogSnapshot(CatalogSnapshotRequestV1),
}

impl OperationV1 {
    /// Stable name used for logging and for the `reason` of an event.
    pub const fn name(&self) -> &'static str {
        match self {
            OperationV1::GetCoreInfo => "GET_CORE_INFO",
            OperationV1::GetEdgeState => "GET_EDGE_STATE",
            OperationV1::GetActiveTransaction => "GET_ACTIVE_TRANSACTION",
            OperationV1::GetPendingOutbox(_) => "GET_PENDING_OUTBOX",
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
            OperationV1::CommitCashPayment(_) => "COMMIT_CASH_PAYMENT",
            OperationV1::FinalizeTransaction => "FINALIZE_TRANSACTION",
            OperationV1::CloseRegisterSession => "CLOSE_REGISTER_SESSION",
            OperationV1::CloseBusinessDay => "CLOSE_BUSINESS_DAY",
            OperationV1::BeginCatalogImport(_) => "BEGIN_CATALOG_IMPORT",
            OperationV1::PutCatalogChunk(_) => "PUT_CATALOG_CHUNK",
            OperationV1::CommitCatalogImport(_) => "COMMIT_CATALOG_IMPORT",
            OperationV1::AbortCatalogImport(_) => "ABORT_CATALOG_IMPORT",
            OperationV1::ActivateCatalogSnapshot(_) => "ACTIVATE_CATALOG_SNAPSHOT",
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
                | OperationV1::CommitCashPayment(_)
                | OperationV1::FinalizeTransaction
                | OperationV1::CloseRegisterSession
                | OperationV1::CloseBusinessDay
                | OperationV1::BeginCatalogImport(_)
                | OperationV1::PutCatalogChunk(_)
                | OperationV1::CommitCatalogImport(_)
                | OperationV1::AbortCatalogImport(_)
                | OperationV1::ActivateCatalogSnapshot(_)
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
    RegisterSession(RegisterSessionV1),
    Transaction(RetailTransactionV1),
    MaybeTransaction(Option<RetailTransactionV1>),
    PendingOutbox(PendingOutboxV1),
    Journal(Vec<JournalEventV1>),
    CatalogStatus(CatalogStatusV1),
    CatalogImport(CatalogImportStatusV1),
    CatalogSnapshot(CatalogSnapshotViewV1),
    ProductLookup(ProductLookupV1),
}

impl ResultV1 {
    pub const fn name(&self) -> &'static str {
        match self {
            ResultV1::Acknowledged => "ACKNOWLEDGED",
            ResultV1::CoreInfo(_) => "CORE_INFO",
            ResultV1::EdgeState(_) => "EDGE_STATE",
            ResultV1::Device(_) => "DEVICE",
            ResultV1::BusinessDay(_) => "BUSINESS_DAY",
            ResultV1::RegisterSession(_) => "REGISTER_SESSION",
            ResultV1::Transaction(_) => "TRANSACTION",
            ResultV1::MaybeTransaction(_) => "MAYBE_TRANSACTION",
            ResultV1::PendingOutbox(_) => "PENDING_OUTBOX",
            ResultV1::Journal(_) => "JOURNAL",
            ResultV1::CatalogStatus(_) => "CATALOG_STATUS",
            ResultV1::CatalogImport(_) => "CATALOG_IMPORT",
            ResultV1::CatalogSnapshot(_) => "CATALOG_SNAPSHOT",
            ResultV1::ProductLookup(_) => "PRODUCT_LOOKUP",
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
            OperationV1::CommitCashPayment(CommitCashPaymentRequestV1 {
                amount_minor: 11_600,
            }),
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"protocol_version":1,"request_id":"00000000-0000-0000-0000-000000000001","operation":"COMMIT_CASH_PAYMENT","payload":{"amount_minor":11600}}"#
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
        })
        .is_command());
        assert!(OperationV1::PutCatalogChunk(PutCatalogChunkRequestV1 {
            promotions: Vec::new(),
            snapshot_id: "s".to_owned(),
            chunk_index: 0,
            chunk_hash: "h".to_owned(),
            products: Vec::new(),
        })
        .is_command());
        assert!(!OperationV1::GetCatalogStatus.is_command());
        assert!(!OperationV1::GetEdgeState.is_command());
        assert!(!OperationV1::Subscribe.is_command());
    }
}
