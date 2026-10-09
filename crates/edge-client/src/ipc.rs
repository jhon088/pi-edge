//! `EdgeClient` over an `EdgeTransport`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use edge_ipc::{DesktopLocalTransport, EdgeEventStream, EdgeTransport, IpcError};
use edge_protocol::{
    AddItemRequestV1, BeginCatalogImportRequestV1, BusinessDayV1, CatalogImportStatusV1,
    CatalogSnapshotRequestV1, CatalogSnapshotViewV1, CatalogStatusV1, ChangeQuantityRequestV1,
    CommitCatalogImportRequestV1, CoreInfoV1, DeviceV1, EdgeStateV1, InitDeviceRequestV1,
    JournalEventV1, LimitRequestV1, OpenRegisterSessionRequestV1, OperationV1, PaymentRequestV1,
    PaymentResultV1, PendingOutboxV1, ProductLookupV1, ProductSelectorV1, PutCatalogChunkRequestV1,
    RegisterSessionV1, RemoveItemRequestV1, RequestEnvelopeV1, ResponseEnvelopeV1, ResultV1,
    RetailTransactionV1, StartPaymentRequestV1, TransactionPaymentsV1, TransactionRequestV1,
};
use uuid::Uuid;

use crate::client::{ConnectionStatus, EdgeClient};
use crate::error::ClientError;
use crate::CLIENT_VERSION;

/// First retry delay after the core goes away.
const MIN_BACKOFF: Duration = Duration::from_millis(250);
/// Ceiling for the retry delay.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// The production client: Edge Protocol over local IPC.
#[derive(Debug)]
pub struct IpcEdgeClient {
    transport: Arc<dyn EdgeTransport>,
    backoff: Mutex<Backoff>,
}

/// Connection attempts are throttled so a UI that refreshes on a timer cannot
/// hammer a dead socket. State changes only on connection outcomes, never on
/// business errors.
#[derive(Debug)]
struct Backoff {
    delay: Duration,
    retry_after: Option<Instant>,
}

impl IpcEdgeClient {
    pub fn new(transport: Arc<dyn EdgeTransport>) -> Self {
        Self {
            transport,
            backoff: Mutex::new(Backoff {
                delay: MIN_BACKOFF,
                retry_after: None,
            }),
        }
    }

    /// Build the desktop client for an installation, resolving the endpoint and
    /// the local credential from the database path. No credential is ever
    /// passed as an argument.
    pub fn for_database(
        database: impl AsRef<Path>,
        client_name: impl Into<String>,
    ) -> Result<Self, ClientError> {
        let transport = DesktopLocalTransport::for_database(database, client_name, CLIENT_VERSION)?;
        Ok(Self::new(Arc::new(transport)))
    }

    pub fn transport(&self) -> &Arc<dyn EdgeTransport> {
        &self.transport
    }

    /// Open a dedicated event stream. Authenticates independently of the
    /// request channel.
    pub fn subscribe(&self) -> Result<Box<dyn EdgeEventStream>, ClientError> {
        self.gate()?;
        match self.transport.subscribe() {
            Ok(stream) => {
                self.record_success();
                Ok(stream)
            }
            Err(error) => Err(self.record_failure(error)),
        }
    }

    /// Fail fast while the backoff window is open, instead of touching the
    /// socket again.
    fn gate(&self) -> Result<(), ClientError> {
        let guard = self.backoff.lock().map_err(|_| ClientError::Unavailable {
            detail: "client backoff state is poisoned".to_owned(),
        })?;
        match guard.retry_after {
            Some(deadline) if Instant::now() < deadline => Err(ClientError::Unavailable {
                detail: format!(
                    "waiting {} ms before reconnecting to {}",
                    (deadline - Instant::now()).as_millis(),
                    self.transport.endpoint_label()
                ),
            }),
            _ => Ok(()),
        }
    }

    fn record_success(&self) {
        if let Ok(mut guard) = self.backoff.lock() {
            guard.delay = MIN_BACKOFF;
            guard.retry_after = None;
        }
    }

    fn record_failure(&self, error: IpcError) -> ClientError {
        if error.is_core_unavailable() {
            // Learning the core is gone — from any call, including the event
            // listener's — invalidates the request channel as well. A stale
            // channel must never be reported as a live connection.
            self.transport.close();
            if let Ok(mut guard) = self.backoff.lock() {
                guard.retry_after = Some(Instant::now() + guard.delay);
                guard.delay = (guard.delay * 2).min(MAX_BACKOFF);
                tracing::debug!(
                    endpoint = %self.transport.endpoint_label(),
                    next_delay_ms = guard.delay.as_millis(),
                    "core unavailable; backing off"
                );
            }
        }
        ClientError::from(error)
    }

    fn call(
        &self,
        operation: OperationV1,
        request_id: Option<Uuid>,
    ) -> Result<ResultV1, ClientError> {
        let is_command = operation.is_command();
        let request = RequestEnvelopeV1::new(request_id.unwrap_or_else(Uuid::new_v4), operation);
        self.gate()?;

        match self.transport.request(&request) {
            Ok(response) => {
                self.record_success();
                into_result(response)
            }
            Err(error) if error.is_core_unavailable() && !is_command => {
                // Queries are safe to retry once: the transport has already
                // dropped the dead channel, so this attempt reconnects.
                tracing::debug!(
                    operation = request.operation.name(),
                    "retrying query after reconnect"
                );
                match self.transport.request(&request) {
                    Ok(response) => {
                        self.record_success();
                        into_result(response)
                    }
                    Err(error) => Err(self.record_failure(error)),
                }
            }
            // A command is never retried automatically: a lost reply does not
            // tell us whether the sale was applied. The caller decides, and can
            // replay safely by reusing its `request_id`.
            Err(error) => Err(self.record_failure(error)),
        }
    }
}

fn into_result(response: ResponseEnvelopeV1) -> Result<ResultV1, ClientError> {
    if response.success {
        response.result.ok_or(ClientError::UnexpectedResult {
            expected: "a result payload",
            actual: "nothing".to_owned(),
        })
    } else {
        let error = response.error.ok_or(ClientError::UnexpectedResult {
            expected: "an error payload",
            actual: "nothing".to_owned(),
        })?;
        Err(ClientError::Refused {
            code: error.code,
            message: error.message,
        })
    }
}

macro_rules! expect_result {
    ($result:expr, $variant:path, $expected:literal) => {
        match $result {
            $variant(value) => Ok(value),
            other => Err(ClientError::UnexpectedResult {
                expected: $expected,
                actual: other.name().to_owned(),
            }),
        }
    };
}

impl EdgeClient for IpcEdgeClient {
    fn core_info(&self) -> Result<CoreInfoV1, ClientError> {
        expect_result!(
            self.call(OperationV1::GetCoreInfo, None)?,
            ResultV1::CoreInfo,
            "CORE_INFO"
        )
    }

    fn get_edge_state(&self) -> Result<EdgeStateV1, ClientError> {
        expect_result!(
            self.call(OperationV1::GetEdgeState, None)?,
            ResultV1::EdgeState,
            "EDGE_STATE"
        )
    }

    fn get_active_transaction(&self) -> Result<Option<RetailTransactionV1>, ClientError> {
        expect_result!(
            self.call(OperationV1::GetActiveTransaction, None)?,
            ResultV1::MaybeTransaction,
            "MAYBE_TRANSACTION"
        )
    }

    fn get_pending_outbox(&self, limit: i64) -> Result<PendingOutboxV1, ClientError> {
        expect_result!(
            self.call(
                OperationV1::GetPendingOutbox(LimitRequestV1 { limit }),
                None
            )?,
            ResultV1::PendingOutbox,
            "PENDING_OUTBOX"
        )
    }

    fn get_journal(&self, limit: i64) -> Result<Vec<JournalEventV1>, ClientError> {
        expect_result!(
            self.call(OperationV1::GetJournal(LimitRequestV1 { limit }), None)?,
            ResultV1::Journal,
            "JOURNAL"
        )
    }

    fn init_device(&self, request: InitDeviceRequestV1) -> Result<DeviceV1, ClientError> {
        expect_result!(
            self.call(OperationV1::InitDevice(request), None)?,
            ResultV1::Device,
            "DEVICE"
        )
    }

    fn open_business_day(&self, request_id: Option<Uuid>) -> Result<BusinessDayV1, ClientError> {
        expect_result!(
            self.call(OperationV1::OpenBusinessDay, request_id)?,
            ResultV1::BusinessDay,
            "BUSINESS_DAY"
        )
    }

    fn open_session(
        &self,
        request: OpenRegisterSessionRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RegisterSessionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::OpenRegisterSession(request), request_id)?,
            ResultV1::RegisterSession,
            "REGISTER_SESSION"
        )
    }

    fn start_transaction(
        &self,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::StartTransaction, request_id)?,
            ResultV1::Transaction,
            "TRANSACTION"
        )
    }

    fn add_item(
        &self,
        request: AddItemRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::AddItem(request), request_id)?,
            ResultV1::Transaction,
            "TRANSACTION"
        )
    }

    fn remove_item(
        &self,
        request: RemoveItemRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::RemoveItem(request), request_id)?,
            ResultV1::Transaction,
            "TRANSACTION"
        )
    }

    fn change_quantity(
        &self,
        request: ChangeQuantityRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::ChangeQuantity(request), request_id)?,
            ResultV1::Transaction,
            "TRANSACTION"
        )
    }

    fn totalize(&self, request_id: Option<Uuid>) -> Result<RetailTransactionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::Totalize, request_id)?,
            ResultV1::Transaction,
            "TRANSACTION"
        )
    }

    fn start_payment(
        &self,
        request: StartPaymentRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<PaymentResultV1, ClientError> {
        expect_result!(
            self.call(OperationV1::StartPayment(request), request_id)?,
            ResultV1::Payment,
            "PAYMENT"
        )
    }

    fn reconcile_payment(
        &self,
        request: PaymentRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<PaymentResultV1, ClientError> {
        expect_result!(
            self.call(OperationV1::ReconcilePayment(request), request_id)?,
            ResultV1::Payment,
            "PAYMENT"
        )
    }

    fn reverse_payment(
        &self,
        request: PaymentRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<PaymentResultV1, ClientError> {
        expect_result!(
            self.call(OperationV1::ReversePayment(request), request_id)?,
            ResultV1::Payment,
            "PAYMENT"
        )
    }

    fn get_payment(&self, request: PaymentRequestV1) -> Result<PaymentResultV1, ClientError> {
        expect_result!(
            self.call(OperationV1::GetPayment(request), None)?,
            ResultV1::Payment,
            "PAYMENT"
        )
    }

    fn get_transaction_payments(
        &self,
        request: TransactionRequestV1,
    ) -> Result<TransactionPaymentsV1, ClientError> {
        expect_result!(
            self.call(OperationV1::GetTransactionPayments(request), None)?,
            ResultV1::TransactionPayments,
            "TRANSACTION_PAYMENTS"
        )
    }

    fn finalize_transaction(
        &self,
        request_id: Option<Uuid>,
    ) -> Result<RetailTransactionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::FinalizeTransaction, request_id)?,
            ResultV1::Transaction,
            "TRANSACTION"
        )
    }

    fn close_session(&self, request_id: Option<Uuid>) -> Result<RegisterSessionV1, ClientError> {
        expect_result!(
            self.call(OperationV1::CloseRegisterSession, request_id)?,
            ResultV1::RegisterSession,
            "REGISTER_SESSION"
        )
    }

    fn close_business_day(&self, request_id: Option<Uuid>) -> Result<BusinessDayV1, ClientError> {
        expect_result!(
            self.call(OperationV1::CloseBusinessDay, request_id)?,
            ResultV1::BusinessDay,
            "BUSINESS_DAY"
        )
    }

    fn catalog_status(&self) -> Result<CatalogStatusV1, ClientError> {
        expect_result!(
            self.call(OperationV1::GetCatalogStatus, None)?,
            ResultV1::CatalogStatus,
            "CATALOG_STATUS"
        )
    }

    fn catalog_import_status(
        &self,
        snapshot_id: &str,
    ) -> Result<CatalogImportStatusV1, ClientError> {
        expect_result!(
            self.call(
                OperationV1::GetCatalogImportStatus(CatalogSnapshotRequestV1 {
                    snapshot_id: snapshot_id.to_owned(),
                }),
                None
            )?,
            ResultV1::CatalogImport,
            "CATALOG_IMPORT"
        )
    }

    fn lookup_product(&self, selector: ProductSelectorV1) -> Result<ProductLookupV1, ClientError> {
        expect_result!(
            self.call(OperationV1::LookupProduct(selector), None)?,
            ResultV1::ProductLookup,
            "PRODUCT_LOOKUP"
        )
    }

    fn begin_catalog_import(
        &self,
        request: BeginCatalogImportRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<CatalogImportStatusV1, ClientError> {
        expect_result!(
            self.call(OperationV1::BeginCatalogImport(request), request_id)?,
            ResultV1::CatalogImport,
            "CATALOG_IMPORT"
        )
    }

    fn put_catalog_chunk(
        &self,
        request: PutCatalogChunkRequestV1,
    ) -> Result<CatalogImportStatusV1, ClientError> {
        expect_result!(
            self.call(OperationV1::PutCatalogChunk(request), None)?,
            ResultV1::CatalogImport,
            "CATALOG_IMPORT"
        )
    }

    fn commit_catalog_import(
        &self,
        request: CommitCatalogImportRequestV1,
        request_id: Option<Uuid>,
    ) -> Result<CatalogSnapshotViewV1, ClientError> {
        expect_result!(
            self.call(OperationV1::CommitCatalogImport(request), request_id)?,
            ResultV1::CatalogSnapshot,
            "CATALOG_SNAPSHOT"
        )
    }

    fn abort_catalog_import(
        &self,
        snapshot_id: &str,
        request_id: Option<Uuid>,
    ) -> Result<(), ClientError> {
        let result = self.call(
            OperationV1::AbortCatalogImport(CatalogSnapshotRequestV1 {
                snapshot_id: snapshot_id.to_owned(),
            }),
            request_id,
        )?;
        match result {
            ResultV1::Acknowledged => Ok(()),
            other => Err(ClientError::UnexpectedResult {
                expected: "ACKNOWLEDGED",
                actual: other.name().to_owned(),
            }),
        }
    }

    fn activate_catalog_snapshot(
        &self,
        snapshot_id: &str,
        request_id: Option<Uuid>,
    ) -> Result<CatalogSnapshotViewV1, ClientError> {
        expect_result!(
            self.call(
                OperationV1::ActivateCatalogSnapshot(CatalogSnapshotRequestV1 {
                    snapshot_id: snapshot_id.to_owned(),
                }),
                request_id
            )?,
            ResultV1::CatalogSnapshot,
            "CATALOG_SNAPSHOT"
        )
    }

    fn connection(&self) -> ConnectionStatus {
        let reconnect_in_ms = self
            .backoff
            .lock()
            .ok()
            .and_then(|guard| guard.retry_after)
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
            .map(|remaining| u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX));
        ConnectionStatus {
            connected: self.transport.is_connected() && reconnect_in_ms.is_none(),
            endpoint: self.transport.endpoint_label(),
            mode: "IPC",
            reconnect_in_ms,
        }
    }
}
