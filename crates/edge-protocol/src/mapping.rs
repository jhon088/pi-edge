//! Conversions between the wire contract and the core.
//!
//! Enabled by the `core-mapping` feature, which only the daemon and the
//! in-process client turn on. A UI client links the DTOs without ever pulling
//! in the application layer.
//!
//! Inbound direction is also where request payloads are **validated** into
//! domain value objects, so a malformed field fails at the boundary with a
//! typed error instead of reaching a use case.

use edge_application::commands::{
    AddItemCommand, ApproveSafeDropCommand, BeginCatalogImportCommand, CashInCommand,
    CashOutCommand, ChangeQuantityCommand, CommitCatalogImportCommand, DeclareClosingCashCommand,
    ForceCloseBusinessDayCommand, ImportInventoryBootstrapCommand, InitDeviceCommand,
    InventoryInCommand, InventoryOutCommand, OpenRegisterSessionCommand, PutCatalogChunkCommand,
    ReconcilePaymentCommand, RemoveItemCommand, ReopenSessionCommand, RequestSafeDropCommand,
    ReversePaymentCommand, StartPaymentCommand,
};
use edge_application::ports::{DeviceProfile, InventoryBootstrapState};
use edge_application::queries::{
    CashPositionView, CatalogImportStatus, CatalogSnapshotView, CatalogStatus, EdgeState,
    InventoryPositionView, PendingOutbox, ProductLookup, SessionCloseResult,
};
use edge_application::{AppError, PaymentOutcome};
use edge_cash::{CashError, CashMovement, SafeDrop, SessionCloseRequirements};
use edge_catalog::CatalogError;
use edge_domain::prelude::*;
use edge_inventory::{BootstrapItem, InventoryMovement};
use edge_payments::{PaymentAttempt, PaymentError, PaymentView, Settlement, TenderType};
use uuid::Uuid;

use crate::content_mapping::{parse_snapshot_id, parse_timestamp};

use crate::dto::*;
use crate::PROTOCOL_VERSION;

// ------------------------------------------------------------------- outbound

impl From<&DeviceProfile> for DeviceV1 {
    fn from(value: &DeviceProfile) -> Self {
        Self {
            tenant_id: value.tenant_id.to_string(),
            store_id: value.store_id.to_string(),
            currency: value.currency.code().to_owned(),
        }
    }
}

impl From<&BusinessDay> for BusinessDayV1 {
    fn from(value: &BusinessDay) -> Self {
        let state = value.state();
        Self {
            business_day_id: state.business_day_id.to_string(),
            tenant_id: state.tenant_id.to_string(),
            store_id: state.store_id.to_string(),
            status: state.status.as_str().to_owned(),
            opened_at: state.opened_at.to_rfc3339(),
            closed_at: state.closed_at.map(|ts| ts.to_rfc3339()),
            version: state.version,
        }
    }
}

impl From<&RegisterSession> for RegisterSessionV1 {
    fn from(value: &RegisterSession) -> Self {
        let state = value.state();
        Self {
            session_id: state.session_id.to_string(),
            business_day_id: state.business_day_id.to_string(),
            register_id: state.register_id.to_string(),
            operator_id: state.operator_id.to_string(),
            status: state.status.as_str().to_owned(),
            opening_cash: MoneyV1::from(&state.opening_cash),
            opened_at: state.opened_at.to_rfc3339(),
            closed_at: state.closed_at.map(|ts| ts.to_rfc3339()),
            version: state.version,
        }
    }
}

impl From<&TransactionLine> for TransactionLineV1 {
    fn from(line: &TransactionLine) -> Self {
        Self {
            line_id: line.line_id.to_string(),
            line_number: line.line_number,
            product_ref: line.product_ref.to_string(),
            sku: line.sku.clone(),
            barcode: line.barcode.clone(),
            description: line.description.clone(),
            quantity: line.quantity.to_string(),
            quantity_thousandths: line.quantity.thousandths(),
            unit_price: MoneyV1::from(&line.unit_price),
            tax_rate_bps: line.tax_rate.bps(),
            price_mode: line.price_mode.as_str().to_owned(),
            price_book_id: line.price_book_id.as_ref().map(ToString::to_string),
            extended_amount: MoneyV1::from(&line.extended_amount),
            discount_amount: MoneyV1::from(&line.discount_amount),
            taxable_amount: MoneyV1::from(&line.taxable_amount),
            tax_amount: MoneyV1::from(&line.tax_amount),
            line_total: MoneyV1::from(&line.line_total),
        }
    }
}

impl From<&Totals> for TotalsV1 {
    fn from(totals: &Totals) -> Self {
        Self {
            subtotal: MoneyV1::from(&totals.subtotal),
            discount_total: MoneyV1::from(&totals.discount_total),
            net_total: MoneyV1::from(&totals.net_total),
            tax_total: MoneyV1::from(&totals.tax_total),
            grand_total: MoneyV1::from(&totals.grand_total),
        }
    }
}

impl PaymentV1 {
    pub fn from_domain(view: &PaymentView) -> Result<Self, PaymentError> {
        let payment = &view.payment;
        Ok(Self {
            payment_id: payment.payment_id.to_string(),
            transaction_id: payment.transaction_id.to_string(),
            tender_type: payment.tender_type.as_str().to_owned(),
            provider_kind: payment.provider_kind.map(|kind| kind.as_str().to_owned()),
            requested_amount: MoneyV1::from(&payment.requested_amount),
            authorized_amount: payment.authorized_amount.as_ref().map(MoneyV1::from),
            committed_amount: MoneyV1::from(&payment.committed_amount),
            change_due: MoneyV1::from(&payment.change_due()?),
            status: payment.status.as_str().to_owned(),
            attempts: view.attempts.iter().map(PaymentAttemptV1::from).collect(),
            created_at: payment.created_at.to_rfc3339(),
            updated_at: payment.updated_at.to_rfc3339(),
        })
    }
}

impl From<&PaymentAttempt> for PaymentAttemptV1 {
    fn from(attempt: &PaymentAttempt) -> Self {
        Self {
            attempt_id: attempt.attempt_id.to_string(),
            attempt_number: attempt.attempt_number,
            status: attempt.status.as_str().to_owned(),
            requested_amount: MoneyV1::from(&attempt.requested_amount),
            authorized_amount: attempt.authorized_amount.as_ref().map(MoneyV1::from),
            provider_kind: attempt.provider_kind.as_str().to_owned(),
            correlation_id: attempt.correlation_id.to_string(),
            provider_reference: attempt.provider_reference.clone(),
            failure_code: attempt.failure_code.clone(),
            unresolved: attempt.status.is_unresolved(),
            created_at: attempt.created_at.to_rfc3339(),
            updated_at: attempt.updated_at.to_rfc3339(),
        }
    }
}

impl From<&Settlement> for SettlementV1 {
    fn from(settlement: &Settlement) -> Self {
        Self {
            amount_due: MoneyV1::from(&settlement.amount_due),
            committed_total: MoneyV1::from(&settlement.committed_total),
            remaining_balance: MoneyV1::from(&settlement.remaining_balance),
            tendered_total: MoneyV1::from(&settlement.tendered_total),
            change_due: MoneyV1::from(&settlement.change_due),
            basket_locked: settlement.blocks_basket,
            requires_resolution: settlement.requires_resolution,
            requires_reconciliation: settlement.requires_reconciliation,
            can_finalize: settlement.can_finalize(),
        }
    }
}

impl RetailTransactionV1 {
    pub fn from_domain(transaction: &RetailTransaction) -> Result<Self, DomainError> {
        let state = transaction.state();
        Ok(Self {
            transaction_id: state.transaction_id.to_string(),
            business_day_id: state.business_day_id.to_string(),
            session_id: state.session_id.to_string(),
            store_id: state.store_id.to_string(),
            register_id: state.register_id.to_string(),
            operator_id: state.operator_id.to_string(),
            register_sequence: state.register_sequence,
            status: state.status.as_str().to_owned(),
            currency: state.currency.code().to_owned(),
            catalog_snapshot_id: state.pricing.map(|pricing| pricing.snapshot_id.to_string()),
            catalog_version: state.pricing.map(|pricing| pricing.catalog_version),
            pricing_effective_at: state
                .pricing
                .map(|pricing| pricing.effective_at.to_rfc3339()),
            applied_promotions: state
                .applied_promotions
                .iter()
                .map(AppliedPromotionV1::from)
                .collect(),
            lines: state.lines.iter().map(TransactionLineV1::from).collect(),
            totals: TotalsV1::from(&state.totals),
            paid_total: MoneyV1::from(&transaction.paid_total()?),
            tendered_total: MoneyV1::from(&state.payments.tendered_total),
            balance_due: MoneyV1::from(&transaction.balance_due()?),
            change_due: MoneyV1::from(&transaction.change_due()?),
            basket_locked: transaction.basket_is_locked(),
            requires_payment_resolution: state.payments.requires_resolution,
            created_at: state.created_at.to_rfc3339(),
            updated_at: state.updated_at.to_rfc3339(),
            finalized_at: state.finalized_at.map(|ts| ts.to_rfc3339()),
            version: state.version,
        })
    }
}

impl EdgeStateV1 {
    pub fn from_domain(state: &EdgeState) -> Result<Self, DomainError> {
        let active_transaction = match &state.active_transaction {
            Some(transaction) => Some(RetailTransactionV1::from_domain(transaction)?),
            None => None,
        };
        Ok(Self {
            protocol_version: PROTOCOL_VERSION,
            core_version: state.core_version.clone(),
            core_status: if state.device.is_some() {
                "READY".to_owned()
            } else {
                "NOT_INITIALIZED".to_owned()
            },
            device: state.device.as_ref().map(DeviceV1::from),
            business_day: state.business_day.as_ref().map(BusinessDayV1::from),
            session: state.session.as_ref().map(RegisterSessionV1::from),
            active_transaction,
            pending_outbox: state.pending_outbox,
        })
    }
}

impl From<&OutboxEvent> for OutboxEventV1 {
    fn from(event: &OutboxEvent) -> Self {
        Self {
            event_id: event.event_id.to_string(),
            event_type: event.event_type.as_str().to_owned(),
            aggregate_id: event.aggregate_id.clone(),
            aggregate_version: event.aggregate_version,
            status: event.status.as_str().to_owned(),
            attempt_count: event.attempt_count,
            occurred_at: event.occurred_at.to_rfc3339(),
            next_attempt_at: event.next_attempt_at.map(|ts| ts.to_rfc3339()),
            payload: event.payload_json.clone(),
        }
    }
}

impl From<&PendingOutbox> for PendingOutboxV1 {
    fn from(value: &PendingOutbox) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            pending_count: value.pending_count,
            events: value.events.iter().map(OutboxEventV1::from).collect(),
        }
    }
}

impl From<&JournalEvent> for JournalEventV1 {
    fn from(event: &JournalEvent) -> Self {
        Self {
            event_id: event.event_id.to_string(),
            event_type: event.event_type.as_str().to_owned(),
            aggregate_id: event.aggregate_id.clone(),
            business_day_id: event.business_day_id.map(|id| id.to_string()),
            session_id: event.session_id.map(|id| id.to_string()),
            transaction_id: event.transaction_id.map(|id| id.to_string()),
            operator_id: event.operator_id.as_ref().map(OperatorId::to_string),
            occurred_at: event.occurred_at.to_rfc3339(),
            payload: event.payload_json.clone(),
        }
    }
}

// -------------------------------------------------------------------- inbound

impl InitDeviceRequestV1 {
    pub fn into_command(self) -> Result<InitDeviceCommand, DomainError> {
        Ok(InitDeviceCommand {
            tenant_id: TenantId::new(self.tenant_id)?,
            store_id: StoreId::new(self.store_id)?,
            currency: Currency::from_code(&self.currency)?,
            allow_negative_inventory: self.allow_negative_inventory,
        })
    }
}

impl OpenRegisterSessionRequestV1 {
    pub fn into_command(
        self,
        request_id: Option<Uuid>,
    ) -> Result<OpenRegisterSessionCommand, DomainError> {
        Ok(OpenRegisterSessionCommand {
            request_id,
            register_id: RegisterId::new(self.register_id)?,
            operator_id: OperatorId::new(self.operator_id)?,
            opening_cash_minor: self.opening_cash_minor,
        })
    }
}

impl AddItemRequestV1 {
    pub fn into_command(self, request_id: Option<Uuid>) -> Result<AddItemCommand, CatalogError> {
        Ok(AddItemCommand {
            request_id,
            selector: self.product.into_domain()?,
            quantity: Quantity::from_thousandths(self.quantity_thousandths)?,
        })
    }
}

impl RemoveItemRequestV1 {
    pub fn into_command(self, request_id: Option<Uuid>) -> RemoveItemCommand {
        RemoveItemCommand {
            request_id,
            line_number: self.line_number,
        }
    }
}

impl ChangeQuantityRequestV1 {
    pub fn into_command(
        self,
        request_id: Option<Uuid>,
    ) -> Result<ChangeQuantityCommand, CatalogError> {
        Ok(ChangeQuantityCommand {
            request_id,
            line_number: self.line_number,
            quantity: Quantity::from_thousandths(self.quantity_thousandths)?,
        })
    }
}

impl StartPaymentRequestV1 {
    pub fn into_command(
        self,
        request_id: Option<Uuid>,
    ) -> Result<StartPaymentCommand, PaymentError> {
        Ok(StartPaymentCommand {
            request_id,
            tender_type: TenderType::parse(&self.tender_type)?,
            amount_minor: self.amount_minor,
        })
    }
}

impl PaymentRequestV1 {
    pub fn payment_id(&self) -> Result<PaymentId, PaymentError> {
        self.payment_id
            .parse()
            .map_err(|_| PaymentError::InvalidRecord {
                field: "payment_id",
                reason: format!("`{}` is not a UUID", self.payment_id),
            })
    }

    pub fn into_reconcile(
        self,
        request_id: Option<Uuid>,
    ) -> Result<ReconcilePaymentCommand, PaymentError> {
        Ok(ReconcilePaymentCommand {
            request_id,
            payment_id: self.payment_id()?,
        })
    }

    pub fn into_reverse(
        self,
        request_id: Option<Uuid>,
    ) -> Result<ReversePaymentCommand, PaymentError> {
        Ok(ReversePaymentCommand {
            request_id,
            payment_id: self.payment_id()?,
        })
    }
}

impl TransactionRequestV1 {
    pub fn transaction_id(&self) -> Result<TransactionId, PaymentError> {
        self.transaction_id
            .parse()
            .map_err(|_| PaymentError::InvalidRecord {
                field: "transaction_id",
                reason: format!("`{}` is not a UUID", self.transaction_id),
            })
    }
}

impl PaymentResultV1 {
    pub fn from_domain(outcome: &PaymentOutcome) -> Result<Self, AppError> {
        Ok(Self {
            payment: PaymentV1::from_domain(&outcome.payment).map_err(AppError::Payment)?,
            transaction: RetailTransactionV1::from_domain(&outcome.transaction)?,
            settlement: SettlementV1::from(&outcome.settlement),
        })
    }
}

impl TransactionPaymentsV1 {
    pub fn from_domain(
        transaction_id: TransactionId,
        payments: &[PaymentView],
        settlement: &Settlement,
    ) -> Result<Self, AppError> {
        Ok(Self {
            transaction_id: transaction_id.to_string(),
            payments: payments
                .iter()
                .map(PaymentV1::from_domain)
                .collect::<Result<Vec<_>, _>>()
                .map_err(AppError::Payment)?,
            settlement: SettlementV1::from(settlement),
        })
    }
}

// ----------------------------------------------------- catalog read models

impl From<&CatalogSnapshotView> for CatalogSnapshotViewV1 {
    fn from(view: &CatalogSnapshotView) -> Self {
        Self {
            snapshot: CatalogSnapshotV1::from(&view.snapshot),
            stored: SnapshotCountsV1::from(view.stored),
        }
    }
}

impl From<&CatalogStatus> for CatalogStatusV1 {
    fn from(status: &CatalogStatus) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            active: status.active.as_ref().map(CatalogSnapshotViewV1::from),
            pending: status
                .pending
                .iter()
                .map(CatalogSnapshotViewV1::from)
                .collect(),
            importing: status
                .importing
                .iter()
                .map(CatalogSnapshotViewV1::from)
                .collect(),
        }
    }
}

impl From<&CatalogImportStatus> for CatalogImportStatusV1 {
    fn from(status: &CatalogImportStatus) -> Self {
        Self {
            snapshot: CatalogSnapshotV1::from(&status.snapshot),
            progress: ImportProgressV1::from(&status.progress),
        }
    }
}

impl From<&ProductLookup> for ProductLookupV1 {
    fn from(lookup: &ProductLookup) -> Self {
        Self {
            snapshot_id: lookup.snapshot_id.to_string(),
            snapshot_version: lookup.snapshot_version,
            effective_at: lookup.effective_at.to_rfc3339(),
            product: CatalogProductV1::from(&lookup.product),
            tax_profile: TaxProfileV1::from(&lookup.tax_profile),
            price: lookup.price.as_ref().map(ResolvedPriceV1::from),
            price_issue: lookup.price_issue.clone(),
        }
    }
}

// ------------------------------------------------------ catalog import input

impl BeginCatalogImportRequestV1 {
    pub fn into_command(
        self,
        request_id: Option<Uuid>,
    ) -> Result<BeginCatalogImportCommand, CatalogError> {
        if self.version <= 0 {
            return Err(CatalogError::InvalidRecord {
                field: "version",
                reason: "must be positive".to_owned(),
            });
        }
        let generated_at = parse_timestamp(&self.generated_at, "generated_at")?;
        let effective_at = match self.effective_at.as_deref() {
            Some(value) => Some(parse_timestamp(value, "effective_at")?),
            None => None,
        };
        let tax_profiles = self
            .tax_profiles
            .into_iter()
            .map(TaxProfileV1::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let price_books = self
            .price_books
            .into_iter()
            .map(PriceBookV1::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BeginCatalogImportCommand {
            request_id,
            version: self.version,
            currency: Currency::from_code(&self.currency)?,
            declared_content_hash: self.content_hash,
            generated_at,
            effective_at,
            expected: edge_catalog::SnapshotCounts::from(self.expected),
            tax_profiles,
            price_books,
        })
    }
}

impl PutCatalogChunkRequestV1 {
    pub fn into_command(self, currency: Currency) -> Result<PutCatalogChunkCommand, CatalogError> {
        if self.chunk_index < 0 {
            return Err(CatalogError::InvalidRecord {
                field: "chunk_index",
                reason: "must not be negative".to_owned(),
            });
        }
        let snapshot_id = parse_snapshot_id(&self.snapshot_id)?;
        let mut products = Vec::with_capacity(self.products.len());
        let mut price_entries = Vec::new();
        let mut barcodes = Vec::new();
        for record in self.products {
            let mut record = record.into_domain(currency)?;
            products.push(record.product);
            price_entries.append(&mut record.prices);
            barcodes.append(&mut record.barcodes);
        }
        let promotions = self
            .promotions
            .into_iter()
            .map(|record| record.into_domain(currency))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PutCatalogChunkCommand {
            snapshot_id,
            chunk_index: self.chunk_index,
            chunk_hash: self.chunk_hash,
            products,
            price_entries,
            barcodes,
            promotions,
        })
    }
}

impl CommitCatalogImportRequestV1 {
    pub fn into_command(
        self,
        request_id: Option<Uuid>,
    ) -> Result<CommitCatalogImportCommand, CatalogError> {
        Ok(CommitCatalogImportCommand {
            request_id,
            snapshot_id: parse_snapshot_id(&self.snapshot_id)?,
            chunk_count: self.chunk_count,
        })
    }
}

impl From<&AppliedPromotion> for AppliedPromotionV1 {
    fn from(applied: &AppliedPromotion) -> Self {
        Self {
            adjustment_id: applied.adjustment_id.clone(),
            promotion_id: applied.promotion_id.clone(),
            code: applied.code.clone(),
            name: applied.name.clone(),
            priority: applied.priority,
            stackability: applied.stackability.clone(),
            target: applied.target.kind().to_owned(),
            line_number: applied.target.line_number(),
            amount: MoneyV1::from(&applied.amount),
            reason: applied.reason.clone(),
            detail: applied.detail.clone(),
        }
    }
}

// ------------------------------------------------------------ cash management

impl From<&CashMovement> for CashMovementV1 {
    fn from(movement: &CashMovement) -> Self {
        Self {
            movement_id: movement.movement_id.to_string(),
            movement_type: movement.movement_type.as_str().to_owned(),
            direction: movement.direction.as_str().to_owned(),
            amount: MoneyV1::from(&movement.amount),
            tendered: movement.tendered.as_ref().map(MoneyV1::from),
            change: movement.change.as_ref().map(MoneyV1::from),
            source_ref: movement.source_ref.clone(),
            reason_code: movement.reason_code.clone(),
            approval_id: movement.approval_id.map(|id| id.to_string()),
            operator_id: movement.operator_id.to_string(),
            created_at: movement.created_at.to_rfc3339(),
        }
    }
}

impl From<&SafeDrop> for SafeDropV1 {
    fn from(drop: &SafeDrop) -> Self {
        Self {
            safe_drop_id: drop.safe_drop_id.to_string(),
            approval_id: drop.approval_id.to_string(),
            amount: MoneyV1::from(&drop.amount),
            reason_code: drop.reason_code.clone(),
            status: drop.status.as_str().to_owned(),
            requested_by: drop.requested_by.to_string(),
            requested_at: drop.requested_at.to_rfc3339(),
            approved_by: drop.approved_by.as_ref().map(ToString::to_string),
            approved_at: drop.approved_at.map(|ts| ts.to_rfc3339()),
        }
    }
}

impl From<&CashPositionView> for CashPositionV1 {
    fn from(view: &CashPositionView) -> Self {
        let position = &view.position;
        Self {
            session_id: view.session_id.to_string(),
            register_id: view.register_id.to_string(),
            operator_id: view.operator_id.to_string(),
            session_status: view.session_status.as_str().to_owned(),
            opening_float: MoneyV1::from(&position.opening_float),
            cash_sales: MoneyV1::from(&position.cash_sales),
            cash_in: MoneyV1::from(&position.cash_in),
            cash_out: MoneyV1::from(&position.cash_out),
            safe_drops: MoneyV1::from(&position.safe_drops),
            expected_cash: MoneyV1::from(&position.expected_cash),
            tendered_total: MoneyV1::from(&position.tendered_total),
            change_total: MoneyV1::from(&position.change_total),
            movements: view.movements.iter().map(CashMovementV1::from).collect(),
            pending_safe_drops: view
                .pending_safe_drops
                .iter()
                .map(SafeDropV1::from)
                .collect(),
        }
    }
}

impl From<&SessionCloseRequirements> for SessionCloseRequirementsV1 {
    /// Note what is *not* copied: there is no expected figure, no movement
    /// list and no total in either type (ADR 022).
    fn from(requirements: &SessionCloseRequirements) -> Self {
        Self {
            session_id: requirements.session_id.to_string(),
            currency: requirements.currency.code().to_owned(),
            movement_count: requirements.movement_count,
            blockers: requirements.blockers.clone(),
            requires_declaration: requirements.requires_declaration,
        }
    }
}

impl From<&SessionCloseResult> for SessionCloseResultV1 {
    fn from(result: &SessionCloseResult) -> Self {
        Self {
            session_id: result.session_id.to_string(),
            closure_kind: result.closure_kind.as_str().to_owned(),
            declared_cash: result.declared_cash.as_ref().map(MoneyV1::from),
            expected_at_close: MoneyV1::from(&result.expected_at_close),
            variance: result.variance.as_ref().map(MoneyV1::from),
            variance_label: result.variance_label.clone(),
        }
    }
}

impl CashMovementRequestV1 {
    pub fn into_cash_in(self, request_id: Option<Uuid>) -> CashInCommand {
        CashInCommand {
            request_id,
            amount_minor: self.amount_minor,
            reason_code: self.reason_code,
        }
    }

    pub fn into_cash_out(self, request_id: Option<Uuid>) -> CashOutCommand {
        CashOutCommand {
            request_id,
            amount_minor: self.amount_minor,
            reason_code: self.reason_code,
        }
    }

    pub fn into_safe_drop(self, request_id: Option<Uuid>) -> RequestSafeDropCommand {
        RequestSafeDropCommand {
            request_id,
            amount_minor: self.amount_minor,
            reason_code: self.reason_code,
        }
    }
}

impl ApproveSafeDropRequestV1 {
    pub fn into_command(
        self,
        request_id: Option<Uuid>,
    ) -> Result<ApproveSafeDropCommand, CashError> {
        Ok(ApproveSafeDropCommand {
            request_id,
            safe_drop_id: self
                .safe_drop_id
                .parse()
                .map_err(|_| CashError::InvalidRecord {
                    field: "safe_drop_id",
                    reason: format!("`{}` is not a UUID", self.safe_drop_id),
                })?,
            approver: OperatorId::new(self.approver).map_err(|error| CashError::InvalidRecord {
                field: "approver",
                reason: error.to_string(),
            })?,
            approve: self.approve,
        })
    }
}

impl DeclareClosingCashRequestV1 {
    pub fn into_command(self, request_id: Option<Uuid>) -> DeclareClosingCashCommand {
        DeclareClosingCashCommand {
            request_id,
            counted_minor: self.counted_minor,
        }
    }
}

impl ReopenSessionRequestV1 {
    pub fn into_command(self, request_id: Option<Uuid>) -> Result<ReopenSessionCommand, CashError> {
        Ok(ReopenSessionCommand {
            request_id,
            session_id: self
                .session_id
                .parse()
                .map_err(|_| CashError::InvalidRecord {
                    field: "session_id",
                    reason: format!("`{}` is not a UUID", self.session_id),
                })?,
            reason: self.reason,
            operator_id: OperatorId::new(self.operator_id).map_err(|error| {
                CashError::InvalidRecord {
                    field: "operator_id",
                    reason: error.to_string(),
                }
            })?,
        })
    }
}

impl ForceCloseBusinessDayRequestV1 {
    pub fn into_command(self, request_id: Option<Uuid>) -> ForceCloseBusinessDayCommand {
        ForceCloseBusinessDayCommand {
            request_id,
            reason: self.reason,
            actor: self.actor,
        }
    }
}

// ------------------------------------------------------------------- inventory

impl From<&InventoryMovement> for InventoryMovementV1 {
    fn from(movement: &InventoryMovement) -> Self {
        Self {
            movement_id: movement.movement_id.to_string(),
            product_ref: movement.product_ref.to_string(),
            movement_type: movement.movement_type.as_str().to_owned(),
            direction: movement.direction.as_str().to_owned(),
            quantity: movement.quantity.to_string(),
            quantity_thousandths: movement.quantity.thousandths(),
            source_ref: movement.source_ref.clone(),
            transaction_id: movement.transaction_id.map(|id| id.to_string()),
            line_number: movement.line_number,
            reason_code: movement.reason_code.clone(),
            occurred_at: movement.occurred_at.to_rfc3339(),
        }
    }
}

impl From<&InventoryPositionView> for InventoryPositionV1 {
    fn from(view: &InventoryPositionView) -> Self {
        let position = &view.position;
        Self {
            store_id: position.store_id.to_string(),
            product_ref: position.product_ref.to_string(),
            on_hand: view.on_hand_display.clone(),
            on_hand_thousandths: position.on_hand_thousandths,
            movement_count: position.movement_count,
            last_movement_at: position.last_movement_at.map(|ts| ts.to_rfc3339()),
            movements: view
                .movements
                .iter()
                .map(InventoryMovementV1::from)
                .collect(),
        }
    }
}

impl From<&InventoryBootstrapState> for InventoryBootstrapV1 {
    fn from(state: &InventoryBootstrapState) -> Self {
        Self {
            store_id: state.store_id.to_string(),
            source_snapshot_id: state.source_snapshot_id.clone(),
            generated_at: state.generated_at.to_rfc3339(),
            item_count: state.item_count,
            bootstrapped_at: state.bootstrapped_at.to_rfc3339(),
        }
    }
}

impl ImportInventoryBootstrapRequestV1 {
    /// Fails with a `DomainError`, like every other inbound payload: a bad
    /// timestamp, product reference or quantity here is a malformed request,
    /// not a refused business rule, and `domain_error` in the daemon is what
    /// turns that into `INVALID_ARGUMENT`.
    pub fn into_command(
        self,
        request_id: Option<Uuid>,
    ) -> Result<ImportInventoryBootstrapCommand, DomainError> {
        let generated_at = Timestamp::parse_rfc3339(&self.generated_at)?;
        let items = self
            .items
            .into_iter()
            .map(|item| {
                Ok(BootstrapItem {
                    product_ref: ProductRef::new(item.product_ref)?,
                    quantity: Quantity::from_thousandths(item.quantity_thousandths)?,
                })
            })
            .collect::<Result<Vec<_>, DomainError>>()?;
        Ok(ImportInventoryBootstrapCommand {
            request_id,
            source_snapshot_id: self.source_snapshot_id,
            generated_at,
            items,
        })
    }
}

impl InventoryMovementRequestV1 {
    pub fn into_inventory_in(
        self,
        request_id: Option<Uuid>,
    ) -> Result<InventoryInCommand, DomainError> {
        Ok(InventoryInCommand {
            request_id,
            product_ref: ProductRef::new(self.product_ref)?,
            quantity: Quantity::from_thousandths(self.quantity_thousandths)?,
            reason_code: self.reason_code,
        })
    }

    pub fn into_inventory_out(
        self,
        request_id: Option<Uuid>,
    ) -> Result<InventoryOutCommand, DomainError> {
        Ok(InventoryOutCommand {
            request_id,
            product_ref: ProductRef::new(self.product_ref)?,
            quantity: Quantity::from_thousandths(self.quantity_thousandths)?,
            reason_code: self.reason_code,
        })
    }
}

impl InventoryQueryV1 {
    /// A product reference is validated at the boundary, like every other id.
    pub fn product(&self) -> Result<ProductRef, DomainError> {
        Ok(ProductRef::new(self.product_ref.clone())?)
    }
}
