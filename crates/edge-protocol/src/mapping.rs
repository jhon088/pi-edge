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
    AddItemCommand, CommitCashPaymentCommand, InitDeviceCommand, OpenRegisterSessionCommand,
};
use edge_application::ports::DeviceProfile;
use edge_application::queries::{EdgeState, PendingOutbox};
use edge_domain::prelude::*;
use uuid::Uuid;

use crate::dto::*;
use crate::PROTOCOL_VERSION;

// ------------------------------------------------------------------- outbound

impl From<&Money> for MoneyV1 {
    fn from(value: &Money) -> Self {
        Self {
            minor: value.minor(),
            currency: value.currency().code().to_owned(),
            formatted: value.format_amount(),
        }
    }
}

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
            gross_amount: MoneyV1::from(&line.gross_amount),
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
            tax_total: MoneyV1::from(&totals.tax_total),
            grand_total: MoneyV1::from(&totals.grand_total),
        }
    }
}

impl From<&Payment> for PaymentV1 {
    fn from(payment: &Payment) -> Self {
        Self {
            payment_id: payment.payment_id.to_string(),
            tender_type: payment.tender_type.as_str().to_owned(),
            amount: MoneyV1::from(&payment.amount),
            status: payment.status.as_str().to_owned(),
            created_at: payment.created_at.to_rfc3339(),
            committed_at: payment.committed_at.map(|ts| ts.to_rfc3339()),
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
            lines: state.lines.iter().map(TransactionLineV1::from).collect(),
            totals: TotalsV1::from(&state.totals),
            payments: state.payments.iter().map(PaymentV1::from).collect(),
            paid_total: MoneyV1::from(&transaction.paid_total()?),
            balance_due: MoneyV1::from(&transaction.balance_due()?),
            change_due: MoneyV1::from(&transaction.change_due()?),
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
    pub fn into_command(self, request_id: Option<Uuid>) -> Result<AddItemCommand, DomainError> {
        Ok(AddItemCommand {
            request_id,
            product_ref: ProductRef::new(self.product_ref)?,
            sku: self.sku,
            barcode: self.barcode,
            description: self.description,
            quantity: Quantity::from_thousandths(self.quantity_thousandths)?,
            unit_price_minor: self.unit_price_minor,
            tax_rate: TaxRate::from_bps(self.tax_rate_bps)?,
            discount_minor: self.discount_minor,
        })
    }
}

impl CommitCashPaymentRequestV1 {
    pub fn into_command(self, request_id: Option<Uuid>) -> CommitCashPaymentCommand {
        CommitCashPaymentCommand {
            request_id,
            amount_minor: self.amount_minor,
        }
    }
}
