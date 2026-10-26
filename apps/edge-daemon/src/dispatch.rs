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
    AbortCatalogImportCommand, AcceptRefundCommand, ActivateCatalogSnapshotCommand,
    CloseBusinessDayCommand, CommitAssetCommand, FinalizeTransactionCommand,
    OpenBusinessDayCommand, OpenDrawerCommand, ReconcileRefundCommand, RequestSessionCloseCommand,
    StartRefundPaymentCommand, StartTransactionCommand, TotalizeTransactionCommand,
};
use edge_application::{AppError, EdgeApplication, CORE_VERSION};
use edge_cash::CashError;
use edge_catalog::CatalogError;
use edge_domain::prelude::{DomainError, RetailTransaction};
use edge_payments::{PaymentError, SimulatorPaymentProvider};
use edge_protocol::{
    error_codes, AssetChunkRequestV1, AssetChunkV1, AssetRefV1, AssetStatusV1, BusinessDayCloseV1,
    BusinessDayV1, CashPositionV1, CatalogImportStatusV1, CatalogSnapshotViewV1, CatalogStatusV1,
    CoreInfoV1, DeviceV1, EdgeStateV1, ErrorV1, HardwareStatusV1, InventoryBootstrapV1,
    InventoryMovementV1, InventoryPositionV1, JournalEventV1, MenuV1, OperationV1, PaymentResultV1,
    PendingOutboxV1, PrinterConfigurationV1, PrinterModelV1, PrinterQueueV1, PrinterSettingsV1,
    ProductConfigurationV1, ProductLookupV1, ReceiptV1, RecentSalesV1, RefundEligibilityV1,
    RefundV1, RegisterSessionV1, RequestEnvelopeV1, ResponseEnvelopeV1, ResultV1,
    RetailTransactionV1, SafeDropV1, SessionCloseRequirementsV1, SessionCloseResultV1,
    SnapshotAssetsV1, SyncCycleV1, SyncStatusV1, TransactionPaymentsV1, MAX_ASSET_CHUNK_BYTES,
    PROTOCOL_VERSION,
};
use edge_refunds::RefundError;
use edge_storage::SqliteEdgeStore;

use crate::refund_authority::RefundAuthoritySource;
use uuid::Uuid;

use crate::state::DaemonState;

/// The concrete core the daemon owns.
pub type Core = EdgeApplication<
    SqliteEdgeStore,
    edge_application::SystemClock,
    edge_application::UuidFactory,
    SimulatorPaymentProvider,
>;

/// Translates protocol operations into core calls.
#[derive(Debug)]
pub struct Dispatcher {
    core: Arc<Core>,
    state: Arc<DaemonState>,
    /// How this runtime resolves authority over an original sale being
    /// refunded. Resolved here, in the composition root's dispatch, so the
    /// refund domain only ever sees an answer (ADR 030).
    refund_authority: RefundAuthoritySource,
    /// The composition root's printer handle, when this runtime has one.
    /// `None` in a harness that composed a core without devices.
    printer: Option<crate::printer_settings::PrinterControl>,
}

impl Dispatcher {
    pub fn new(core: Arc<Core>, state: Arc<DaemonState>) -> Self {
        Self::with_refund_authority(core, state, RefundAuthoritySource::Unavailable)
    }

    pub fn with_refund_authority(
        core: Arc<Core>,
        state: Arc<DaemonState>,
        refund_authority: RefundAuthoritySource,
    ) -> Self {
        Self {
            core,
            state,
            refund_authority,
            printer: None,
        }
    }

    /// Give dispatch the handle that reconfigures the receipt printer.
    pub fn with_printer_control(
        mut self,
        control: crate::printer_settings::PrinterControl,
    ) -> Self {
        self.printer = Some(control);
        self
    }

    /// What the admin screen draws: what is configured, what models this
    /// build offers, and what queues this host actually reports.
    fn printer_configuration(&self) -> PrinterConfigurationV1 {
        let models = crate::printer_settings::MODELS
            .iter()
            .map(|entry| PrinterModelV1 {
                id: entry.id.to_owned(),
                label: entry.label.to_owned(),
                paper_mm: i64::from(entry.paper_mm),
                columns: entry.columns as i64,
            })
            .collect();
        // Whatever the host says, and nothing when it says nothing.
        let available = edge_hardware::list_queues()
            .unwrap_or_default()
            .into_iter()
            .map(|queue| PrinterQueueV1 {
                name: queue.name,
                accepting: queue.accepting,
            })
            .collect();
        let settings = self
            .printer
            .as_ref()
            .map(|control| control.settings())
            .unwrap_or_default();
        PrinterConfigurationV1 {
            settings: PrinterSettingsV1 {
                enabled: settings.enabled,
                model: settings.model.clone(),
                queue: settings.queue.clone(),
                columns: settings.columns as i64,
                auto_print: settings.auto_print,
            },
            models,
            available,
            driver: self
                .printer
                .as_ref()
                .map(|control| control.driver().to_owned())
                .unwrap_or_else(|| edge_hardware::UNCONFIGURED.to_owned()),
            reachable: self.printer.as_ref().and_then(|c| c.reachable()),
        }
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
            OperationV1::GetReceipt(payload) => {
                let transaction_id = payload.transaction_id().map_err(payment_error)?;
                let receipt = self
                    .core
                    .get_receipt(transaction_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Receipt(ReceiptV1::from(&receipt)))
            }
            OperationV1::ListRecentSales(payload) => {
                let view = self
                    .core
                    .list_recent_sales(payload.limit, payload.offset)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::RecentSales(RecentSalesV1::from(&view)))
            }
            OperationV1::CancelTransaction(payload) => {
                let transaction = self
                    .core
                    .cancel_transaction(payload.into_command(Some(request_id)))
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::GetSyncStatus => {
                let view = self.core.get_sync_status().map_err(|e| self.app_error(e))?;
                Ok(ResultV1::SyncStatus(SyncStatusV1::from(&view)))
            }
            OperationV1::RunSyncCycle => {
                let report = self.core.run_sync_cycle().map_err(|e| self.app_error(e))?;
                Ok(ResultV1::SyncCycle(SyncCycleV1::from(&report)))
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
            OperationV1::GetHardwareStatus => {
                let view = self
                    .core
                    .get_hardware_status()
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::HardwareStatus(HardwareStatusV1::from(&view)))
            }
            OperationV1::SubmitScan(payload) => {
                // The host's parser already decided a scan completed; the core
                // resolves the barcode through the ordinary catalog path
                // (ADR 036).
                let command = payload
                    .into_command(self.core.clock_now())
                    .map_err(domain_error)?;
                let transaction = self
                    .core
                    .submit_scan(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Transaction(dto(&transaction)?))
            }
            OperationV1::PrintTestPage => {
                let view = self.core.print_test_page().map_err(|e| self.app_error(e))?;
                Ok(ResultV1::HardwareStatus(HardwareStatusV1::from(&view)))
            }
            OperationV1::GetPrinterSettings => {
                Ok(ResultV1::PrinterConfiguration(self.printer_configuration()))
            }
            OperationV1::SetPrinterSettings(payload) => {
                let control = self.printer.as_ref().ok_or_else(|| {
                    // No handle means this runtime was composed without one,
                    // which is a deployment fact, not a client mistake.
                    ErrorV1::new(
                        error_codes::HARDWARE_NOT_CONFIGURED,
                        "this runtime cannot be configured from a client",
                    )
                })?;
                let settings = crate::printer_settings::PrinterSettings {
                    enabled: payload.enabled,
                    model: payload.model,
                    queue: payload.queue,
                    columns: usize::try_from(payload.columns).unwrap_or(48),
                    auto_print: payload.auto_print,
                };
                control.apply(&settings).map_err(|detail| {
                    ErrorV1::new(error_codes::HARDWARE_INVALID_CONFIGURATION, detail)
                })?;
                tracing::info!(
                    enabled = settings.enabled,
                    model = %settings.model,
                    auto_print = settings.auto_print,
                    "receipt printer reconfigured"
                );
                Ok(ResultV1::PrinterConfiguration(self.printer_configuration()))
            }
            OperationV1::PrintTransaction(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(domain_error)?;
                let view = self
                    .core
                    .print_transaction(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::HardwareStatus(HardwareStatusV1::from(&view)))
            }
            OperationV1::OpenDrawerDiagnostic => {
                let view = self
                    .core
                    .open_drawer_diagnostic(OpenDrawerCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::HardwareStatus(HardwareStatusV1::from(&view)))
            }
            OperationV1::GetRefundEligibility(payload) => {
                let transaction_id = payload.transaction_id().map_err(payment_error)?;
                let view = self
                    .core
                    .get_refund_eligibility(transaction_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::RefundEligibility(RefundEligibilityV1::from(
                    &view,
                )))
            }
            OperationV1::CreateRefund(payload) => {
                // The runtime resolves the authority over the original sale.
                // A client never asserts it, and with no authority source
                // configured every refund is refused (ADR 030).
                let transaction_id = payload.transaction().map_err(domain_error)?;
                let device = self.core.get_device().map_err(|e| self.app_error(e))?;
                let authorization = self
                    .refund_authority
                    .authorize(
                        transaction_id,
                        &device.tenant_id,
                        &device.store_id,
                        request_id,
                    )
                    .map_err(refund_error)?;
                let command = payload
                    .into_command(authorization, Some(request_id))
                    .map_err(domain_error)?;
                let view = self
                    .core
                    .create_refund(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Refund(RefundV1::from(&view)))
            }
            OperationV1::AcceptRefund(payload) => {
                let refund_id = payload.refund().map_err(domain_error)?;
                let view = self
                    .core
                    .accept_refund(AcceptRefundCommand {
                        request_id: Some(request_id),
                        refund_id,
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Refund(RefundV1::from(&view)))
            }
            OperationV1::StartRefundPayment(payload) => {
                let refund_id = payload.refund().map_err(domain_error)?;
                let view = self
                    .core
                    .start_refund_payment(StartRefundPaymentCommand {
                        request_id: Some(request_id),
                        refund_id,
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Refund(RefundV1::from(&view)))
            }
            OperationV1::ReconcileRefund(payload) => {
                let refund_id = payload.refund().map_err(domain_error)?;
                let view = self
                    .core
                    .reconcile_refund(ReconcileRefundCommand {
                        request_id: Some(request_id),
                        refund_id,
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Refund(RefundV1::from(&view)))
            }
            OperationV1::GetRefund(payload) => {
                let refund_id = payload.refund().map_err(domain_error)?;
                let view = self
                    .core
                    .get_refund(refund_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Refund(RefundV1::from(&view)))
            }
            OperationV1::GetTransactionRefunds(payload) => {
                let transaction_id = payload.transaction_id().map_err(payment_error)?;
                let views = self
                    .core
                    .get_transaction_refunds(transaction_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Refunds(
                    views.iter().map(RefundV1::from).collect(),
                ))
            }
            OperationV1::ImportInventoryBootstrap(payload) => {
                let command = payload
                    .into_command(Some(request_id))
                    .map_err(domain_error)?;
                let state = self
                    .core
                    .import_inventory_bootstrap(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::InventoryBootstrap(InventoryBootstrapV1::from(
                    &state,
                )))
            }
            OperationV1::GetInventoryPosition(payload) => {
                let product = payload.product().map_err(domain_error)?;
                let view = self
                    .core
                    .get_inventory_position(product, payload.movement_limit)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::InventoryPosition(InventoryPositionV1::from(
                    &view,
                )))
            }
            OperationV1::GetInventoryMovements(payload) => {
                let product = payload.product().map_err(domain_error)?;
                let movements = self
                    .core
                    .get_inventory_movements(product, payload.movement_limit, payload.offset)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::InventoryMovements(
                    movements.iter().map(InventoryMovementV1::from).collect(),
                ))
            }
            OperationV1::InventoryIn(payload) => {
                let command = payload
                    .into_inventory_in(Some(request_id))
                    .map_err(domain_error)?;
                let view = self
                    .core
                    .inventory_in(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::InventoryPosition(InventoryPositionV1::from(
                    &view,
                )))
            }
            OperationV1::InventoryOut(payload) => {
                let command = payload
                    .into_inventory_out(Some(request_id))
                    .map_err(domain_error)?;
                let view = self
                    .core
                    .inventory_out(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::InventoryPosition(InventoryPositionV1::from(
                    &view,
                )))
            }
            OperationV1::GetCashPosition => {
                let view = self
                    .core
                    .get_cash_position()
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CashPosition(CashPositionV1::from(&view)))
            }
            OperationV1::CashIn(payload) => {
                let view = self
                    .core
                    .cash_in(payload.into_cash_in(Some(request_id)))
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CashPosition(CashPositionV1::from(&view)))
            }
            OperationV1::CashOut(payload) => {
                let view = self
                    .core
                    .cash_out(payload.into_cash_out(Some(request_id)))
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::CashPosition(CashPositionV1::from(&view)))
            }
            OperationV1::RequestSafeDrop(payload) => {
                let drop = self
                    .core
                    .request_safe_drop(payload.into_safe_drop(Some(request_id)))
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::SafeDrop(SafeDropV1::from(&drop)))
            }
            OperationV1::ApproveSafeDrop(payload) => {
                let command = payload.into_command(Some(request_id)).map_err(cash_error)?;
                let drop = self
                    .core
                    .approve_safe_drop(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::SafeDrop(SafeDropV1::from(&drop)))
            }
            OperationV1::RequestSessionClose => {
                // The answer deliberately carries no expected cash: the blind
                // close is a property of this contract (ADR 022).
                let requirements = self
                    .core
                    .request_session_close(RequestSessionCloseCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::SessionCloseRequirements(
                    SessionCloseRequirementsV1::from(&requirements),
                ))
            }
            OperationV1::DeclareClosingCash(payload) => {
                let result = self
                    .core
                    .declare_closing_cash(payload.into_command(Some(request_id)))
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::SessionCloseResult(SessionCloseResultV1::from(
                    &result,
                )))
            }
            OperationV1::ReopenSession(payload) => {
                let command = payload.into_command(Some(request_id)).map_err(cash_error)?;
                let session = self
                    .core
                    .reopen_session(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::RegisterSession(RegisterSessionV1::from(&session)))
            }
            OperationV1::ForceCloseBusinessDay(payload) => {
                let result = self
                    .core
                    .force_close_business_day(payload.into_command(Some(request_id)))
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::BusinessDayClose(BusinessDayCloseV1::from(
                    &result,
                )))
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
            OperationV1::GetMenu(payload) => {
                let channel = payload.channel().map_err(catalog_error)?;
                let menu = self.core.get_menu(channel).map_err(|e| self.app_error(e))?;
                Ok(ResultV1::Menu(MenuV1::from(&menu)))
            }
            OperationV1::GetProductConfiguration(payload) => {
                let product_ref = payload.product().map_err(catalog_error)?;
                let channel = payload.channel().map_err(catalog_error)?;
                let configuration = self
                    .core
                    .get_product_configuration(product_ref, channel)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::ProductConfiguration(
                    ProductConfigurationV1::from(&configuration),
                ))
            }
            OperationV1::ReadAsset(payload) => {
                let (asset, offset, length) = payload.into_domain().map_err(catalog_error)?;
                // Clamped here rather than trusted: a client asking for
                // everything at once must not be able to overrun the frame.
                let wanted = length.clamp(0, MAX_ASSET_CHUNK_BYTES as i64);
                let bytes = self
                    .core
                    .read_asset(&asset, offset, wanted)
                    .map_err(|e| self.app_error(e))?;
                let chunk = AssetChunkRequestV1::new(AssetRefV1::from(&asset), offset, &bytes);
                Ok(ResultV1::AssetChunk(AssetChunkV1 {
                    content_hash: asset.content_hash.clone(),
                    offset,
                    data_hex: chunk.data_hex,
                    byte_size: asset.byte_size,
                }))
            }
            OperationV1::GetSnapshotAssets(payload) => {
                let snapshot_id = payload.snapshot_id().map_err(catalog_error)?;
                let view = self
                    .core
                    .get_snapshot_assets(snapshot_id)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::SnapshotAssets(SnapshotAssetsV1::from(&view)))
            }
            OperationV1::GetAssetStatus(payload) => {
                let asset = payload.into_domain().map_err(catalog_error)?;
                let view = self
                    .core
                    .get_asset_status(asset)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::AssetStatus(AssetStatusV1::from(&view)))
            }
            OperationV1::PutAssetChunk(payload) => {
                let command = payload.into_command().map_err(catalog_error)?;
                let view = self
                    .core
                    .put_asset_chunk(command)
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::AssetStatus(AssetStatusV1::from(&view)))
            }
            OperationV1::CommitAsset(payload) => {
                let asset = payload.into_domain().map_err(catalog_error)?;
                let view = self
                    .core
                    .commit_asset(CommitAssetCommand { asset })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::AssetStatus(AssetStatusV1::from(&view)))
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
                let result = self
                    .core
                    .close_business_day(CloseBusinessDayCommand {
                        request_id: Some(request_id),
                    })
                    .map_err(|e| self.app_error(e))?;
                Ok(ResultV1::BusinessDayClose(BusinessDayCloseV1::from(
                    &result,
                )))
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

fn cash_error(error: CashError) -> ErrorV1 {
    ErrorV1::new(error.code(), error.to_string())
}

/// Refund failures already carry their own stable code — including the refusal
/// produced when no refund authority is configured.
fn refund_error(error: RefundError) -> ErrorV1 {
    ErrorV1::new(error.code(), error.to_string())
}

fn domain_error(error: DomainError) -> ErrorV1 {
    // Validation failures at the boundary are the client's fault; everything
    // else is a refused business rule.
    let code = match &error {
        // Everything a client can get structurally wrong in a payload: a bad
        // id, a bad amount or quantity, a bad timestamp, or a value the
        // contract does not define.
        DomainError::Id(_)
        | DomainError::Money(_)
        | DomainError::Time(_)
        | DomainError::UnknownEnumValue { .. } => error_codes::INVALID_ARGUMENT,
        DomainError::InvalidTransition { .. } => error_codes::INVALID_TRANSACTION_STATE,
        _ => error_codes::DOMAIN_RULE_VIOLATION,
    };
    ErrorV1::new(code, error.to_string())
}
