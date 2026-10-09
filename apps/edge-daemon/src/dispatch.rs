//! Operation dispatch: Edge Protocol in, core out.
//!
//! This is the only place where a wire operation becomes a use case. Payloads
//! are validated into domain value objects here, so a malformed field fails at
//! the boundary and never reaches a use case.
//!
//! The envelope's `request_id` doubles as the core's idempotency key for
//! commands: a client that loses a reply can replay the identical request and
//! get the recorded result instead of a second sale.

use std::sync::Arc;

use edge_application::commands::{
    AbortCatalogImportCommand, ActivateCatalogSnapshotCommand, CloseBusinessDayCommand,
    CloseRegisterSessionCommand, FinalizeTransactionCommand, OpenBusinessDayCommand,
    StartTransactionCommand, TotalizeTransactionCommand,
};
use edge_application::{AppError, EdgeApplication, CORE_VERSION};
use edge_catalog::CatalogError;
use edge_domain::prelude::{DomainError, RetailTransaction};
use edge_payments::{PaymentError, SimulatorPaymentProvider};
use edge_protocol::{
    error_codes, BusinessDayV1, CatalogImportStatusV1, CatalogSnapshotViewV1, CatalogStatusV1,
    CoreInfoV1, DeviceV1, EdgeStateV1, ErrorV1, JournalEventV1, OperationV1, PaymentResultV1,
    PendingOutboxV1, ProductLookupV1, RegisterSessionV1, RequestEnvelopeV1, ResponseEnvelopeV1,
    ResultV1, RetailTransactionV1, TransactionPaymentsV1, PROTOCOL_VERSION,
};
use edge_storage::SqliteEdgeStore;
use uuid::Uuid;

use crate::state::DaemonState;

/// The concrete core the daemon owns.
pub type Core = EdgeApplication<
    SqliteEdgeStore,
    edge_application::adapters::SystemClock,
    edge_application::adapters::UuidFactory,
    SimulatorPaymentProvider,
>;

/// Translates protocol operations into core calls.
#[derive(Debug)]
pub struct Dispatcher {
    core: Arc<Core>,
    state: Arc<DaemonState>,
}

impl Dispatcher {
    pub fn new(core: Arc<Core>, state: Arc<DaemonState>) -> Self {
        Self { core, state }
    }

    pub fn state(&self) -> &Arc<DaemonState> {
        &self.state
    }

    /// Handle one request and produce its response. Never panics, never
    /// returns `Err`: every failure is a protocol error with a stable code.
    pub fn handle(&self, request: RequestEnvelopeV1) -> ResponseEnvelopeV1 {
        if request.protocol_version != PROTOCOL_VERSION {
            return ResponseEnvelopeV1::failed(
                request.request_id,
                ErrorV1::new(
                    error_codes::PROTOCOL_VERSION_MISMATCH,
                    format!(
                        "this core speaks protocol version {PROTOCOL_VERSION}, request used {}",
                        request.protocol_version
                    ),
                ),
            );
        }

        let request_id = request.request_id;
        let name = request.operation.name();
        let is_command = request.operation.is_command();
        let _guard = self.state.enter_request();

        match self.execute(request_id, request.operation) {
            Ok(result) => {
                self.state.mark_healthy();
                if is_command {
                    let revision = self.state.bump_revision();
                    self.state.broadcast_state_changed(name, revision);
                }
                tracing::debug!(%request_id, operation = name, "operation applied");
                ResponseEnvelopeV1::ok(request_id, result)
            }
            Err(error) => {
                tracing::info!(
                    %request_id,
                    operation = name,
                    code = %error.code,
                    "operation refused"
                );
                ResponseEnvelopeV1::failed(request_id, error)
            }
        }
    }

    fn execute(&self, request_id: Uuid, operation: OperationV1) -> Result<ResultV1, ErrorV1> {
        match operation {
            // ---------------------------------------------------------- queries
            OperationV1::GetCoreInfo => Ok(ResultV1::CoreInfo(self.core_info())),
            OperationV1::GetEdgeState => {
                let state = self.core.get_edge_state().map_err(|e| self.app_error(e))?;
                Ok(ResultV1::EdgeState(
                    EdgeStateV1::from_domain(&state).map_err(domain_error)?,
                ))
            }
            OperationV1::GetActiveTransaction => {
                let transaction = self
                    .core
                    .get_active_transaction()
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::MaybeTransaction(
                    transaction.as_ref().map(dto).transpose()?,
                ))
            }
            OperationV1::GetPendingOutbox(payload) => {
                let pending = self
                    .core
                    .get_pending_outbox(payload.limit)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::PendingOutbox(PendingOutboxV1::from(&pending)))
            }
            OperationV1::GetCatalogStatus => {
                let status = self
                    .core
                    .get_catalog_status()
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CatalogStatus(CatalogStatusV1::from(&status)))
            }
            OperationV1::GetCatalogImportStatus(payload) => {
                let snapshot_id = payload.snapshot_id().map_err(catalog_error)?;
                let status = self
                    .core
                    .get_catalog_import_status(snapshot_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CatalogImport(CatalogImportStatusV1::from(
                    &status,
                )))
            }
            OperationV1::LookupProduct(payload) => {
                let selector = payload.into_domain().map_err(catalog_error)?;
                let lookup = self
                    .core
                    .lookup_product(selector)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::ProductLookup(ProductLookupV1::from(&lookup)))
            }
            OperationV1::GetJournal(payload) => {
                let events = self
                    .core
                    .get_journal(payload.limit)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Journal(
                    events.iter().map(JournalEventV1::from).collect(),
                ))
            }
            // Handled by the connection: it turns the socket into an event
            // stream instead of answering further requests.
            OperationV1::Subscribe => Ok(ResultV1::Acknowledged),

            // --------------------------------------------------------- commands
            OperationV1::InitDevice(payload) => {
                let command = payload.into_command().map_err(domain_error)?;
                let profile = self
                    .core
                    .init_device(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Device(DeviceV1::from(&profile)))
            }
            OperationV1::OpenBusinessDay => {
                let day = self
                    .core
                    .open_business_day(OpenBusinessDayCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::BusinessDay(BusinessDayV1::from(&day)))
            }
            OperationV1::OpenRegisterSession(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(domain_error)?;
                let session = self
                    .core
                    .open_register_session(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::RegisterSession(RegisterSessionV1::from(&session)))
            }
            OperationV1::StartTransaction => {
                let transaction = self
                    .core
                    .start_transaction(StartTransactionCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::AddItem(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(catalog_error)?;
                let transaction = self.core.add_item(command).map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::RemoveItem(payload) => {
                let transaction = self
                    .core
                    .remove_item(payload.into_command(Some(request_id)))
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::ChangeQuantity(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(catalog_error)?;
                let transaction = self
                    .core
                    .change_quantity(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::Totalize => {
                let transaction = self
                    .core
                    .totalize_transaction(TotalizeTransactionCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::StartPayment(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(payment_error)?;
                let outcome = self
                    .core
                    .start_payment(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Payment(
                    PaymentResultV1::from_domain(&outcome).map_err(|e| self.app_error(e))?,
                ))
            }
            OperationV1::ReconcilePayment(payload) => {
                let command = payload
                    .into_reconcile(Some(request_id))
                    .map_err(payment_error)?;
                let outcome = self
                    .core
                    .reconcile_payment(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Payment(
                    PaymentResultV1::from_domain(&outcome).map_err(|e| self.app_error(e))?,
                ))
            }
            OperationV1::ReversePayment(payload) => {
                let command = payload
                    .into_reverse(Some(request_id))
                    .map_err(payment_error)?;
                let outcome = self
                    .core
                    .reverse_payment(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Payment(
                    PaymentResultV1::from_domain(&outcome).map_err(|e| self.app_error(e))?,
                ))
            }
            OperationV1::GetPayment(payload) => {
                let payment_id = payload.payment_id().map_err(payment_error)?;
                let outcome = self
                    .core
                    .get_payment(payment_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Payment(
                    PaymentResultV1::from_domain(&outcome).map_err(|e| self.app_error(e))?,
                ))
            }
            OperationV1::GetTransactionPayments(payload) => {
                let transaction_id = payload.transaction_id().map_err(payment_error)?;
                let (payments, settlement) = self
                    .core
                    .get_transaction_payments(transaction_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::TransactionPayments(
                    TransactionPaymentsV1::from_domain(transaction_id, &payments, &settlement)
                        .map_err(|e| self.app_error(e))?,
                ))
            }
            OperationV1::FinalizeTransaction => {
                let transaction = self
                    .core
                    .finalize_transaction(FinalizeTransactionCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::CloseRegisterSession => {
                let session = self
                    .core
                    .close_register_session(CloseRegisterSessionCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::RegisterSession(RegisterSessionV1::from(&session)))
            }
            OperationV1::BeginCatalogImport(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(catalog_error)?;
                let status = self
                    .core
                    .begin_catalog_import(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CatalogImport(CatalogImportStatusV1::from(
                    &status,
                )))
            }
            OperationV1::PutCatalogChunk(payload) => {
                // A snapshot belongs to one store, so its prices are in the
                // device's currency; the import header already validated that.
                let device = self.core.get_device().map_err(|e| self.app_error(e))?;
                let command = payload
                    .into_command(device.currency)
                    .map_err(catalog_error)?;
                let status = self
                    .core
                    .put_catalog_chunk(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CatalogImport(CatalogImportStatusV1::from(
                    &status,
                )))
            }
            OperationV1::CommitCatalogImport(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(catalog_error)?;
                let view = self
                    .core
                    .commit_catalog_import(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CatalogSnapshot(CatalogSnapshotViewV1::from(
                    &view,
                )))
            }
            OperationV1::AbortCatalogImport(payload) => {
                let snapshot_id = payload.snapshot_id().map_err(catalog_error)?;
                self.core
                    .abort_catalog_import(AbortCatalogImportCommand {
                        request_id: Some(request_id),
                        snapshot_id,
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Acknowledged)
            }
            OperationV1::ActivateCatalogSnapshot(payload) => {
                let snapshot_id = payload.snapshot_id().map_err(catalog_error)?;
                let view = self
                    .core
                    .activate_catalog_snapshot(ActivateCatalogSnapshotCommand {
                        request_id: Some(request_id),
                        snapshot_id,
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CatalogSnapshot(CatalogSnapshotViewV1::from(
                    &view,
                )))
            }
            OperationV1::CloseBusinessDay => {
                let day = self
                    .core
                    .close_business_day(CloseBusinessDayCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::BusinessDay(BusinessDayV1::from(&day)))
            }
        }
    }

    pub fn core_info(&self) -> CoreInfoV1 {
        CoreInfoV1 {
            core_version: CORE_VERSION.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            status: self.state.status(),
            database_schema_version: self.state.schema_version(),
            uptime_seconds: self.state.uptime_seconds(),
            revision: self.state.revision(),
            pid: std::process::id(),
        }
    }

    /// Map a core error to its stable code, degrading the runtime when the
    /// database itself failed.
    fn app_error(&self, error: AppError) -> ErrorV1 {
        if matches!(error, AppError::Storage(_)) {
            self.state.mark_degraded();
            tracing::error!(%error, "storage failure; runtime is degraded");
        }
        ErrorV1::new(error.kind(), error.to_string())
    }
}

fn dto(transaction: &RetailTransaction) -> Result<RetailTransactionV1, ErrorV1> {
    RetailTransactionV1::from_domain(transaction).map_err(domain_error)
}

/// Catalog failures already carry their own stable code.
fn catalog_error(error: CatalogError) -> ErrorV1 {
    ErrorV1::new(error.code(), error.to_string())
}

fn payment_error(error: PaymentError) -> ErrorV1 {
    ErrorV1::new(error.code(), error.to_string())
}

fn domain_error(error: DomainError) -> ErrorV1 {
    // Validation failures at the boundary are the client's fault; everything
    // else is a refused business rule.
    let code = match &error {
        DomainError::Id(_) | DomainError::Money(_) | DomainError::Time(_) => {
            error_codes::INVALID_ARGUMENT
        }
        DomainError::InvalidTransition { .. } => error_codes::INVALID_TRANSACTION_STATE,
        _ => error_codes::DOMAIN_RULE_VIOLATION,
    };
    ErrorV1::new(code, error.to_string())
}
