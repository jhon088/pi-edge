//! Refund use cases.
//!
//! The three consequences of a refund settle at different moments and the
//! order is deliberate (ADR 033):
//!
//! ```text
//! CreateRefund        prices the lines, reserves the quantities
//! AcceptRefund        the goods are back  -> REFUND_RETURN, exactly once
//! StartRefundPayment  the money goes back -> CASH_REFUND / provider refund
//! ReconcileRefund     resolves an unresolved provider refund
//! ```
//!
//! Nothing here re-prices anything. The refundable amounts come from the
//! original sale's materialised economics, so a refund is correct even after
//! the catalog that priced the sale is gone (ADR 031).

use edge_cash::{CashMovement, Direction, NewCashMovement};
use edge_domain::prelude::*;
use edge_inventory::{
    InventoryMovement, MovementSource, MovementType as InventoryMovementType, NewInventoryMovement,
};
use edge_payments::{
    PaymentProviderPort, PaymentStatus, ProviderQuery, RefundOutcome, RefundRequest, TenderType,
};
use edge_refunds::{
    allocate_over_tenders, FiscalState, LineRefundable, NewRefund, Refund, RefundAmounts,
    RefundAttempt, RefundAttemptStatus, RefundError, RefundFiscalStatus, RefundLine, RefundPayment,
    RefundPaymentStatus, RefundPaymentView, RefundStatus, TenderRefundable,
    TransactionFiscalMetadata,
};
use serde_json::json;
use uuid::Uuid;

#[cfg(feature = "testing")]
use crate::commands::MarkTransactionInvoicedCommand;
use crate::commands::{
    AcceptRefundCommand, CreateRefundCommand, ReconcileRefundCommand, StartRefundPaymentCommand,
};
use crate::context::{
    idempotent, journal_event, require_device, require_open_business_day,
    require_single_open_session, EventContext,
};
use crate::error::AppError;
use crate::ports::{Clock, DeviceProfile, EdgeStore, IdFactory, UnitOfWork};
use crate::queries::{RefundEligibilityView, RefundView};
use crate::service::EdgeApplication;

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: PaymentProviderPort,
{
    /// What a sale can still give back.
    ///
    /// The client gets quantities *and* amounts already worked out: it never
    /// computes a refundable figure of its own.
    pub fn get_refund_eligibility(
        &self,
        transaction_id: TransactionId,
    ) -> Result<RefundEligibilityView, AppError> {
        self.store()
            .transact(move |uow| eligibility(uow, transaction_id))
    }

    pub fn get_refund(&self, refund_id: RefundId) -> Result<RefundView, AppError> {
        self.store().transact(move |uow| {
            let refund = uow
                .find_refund(refund_id)?
                .ok_or_else(|| AppError::RefundNotFound {
                    refund_id: refund_id.to_string(),
                })?;
            refund_view(uow, refund)
        })
    }

    pub fn get_transaction_refunds(
        &self,
        transaction_id: TransactionId,
    ) -> Result<Vec<RefundView>, AppError> {
        self.store().transact(move |uow| {
            let refunds = uow.list_transaction_refunds(transaction_id)?;
            refunds
                .into_iter()
                .map(|refund| refund_view(uow, refund))
                .collect()
        })
    }

    /// Open a refund: validate, price and reserve, in one transaction.
    pub fn create_refund(&self, cmd: CreateRefundCommand) -> Result<RefundView, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());
        let refund_id = RefundId::from_uuid(self.ids.new_uuid());
        // One id per line, and a refund can name any number of them.
        let ids = &self.ids;

        self.store().transact(|uow| {
            idempotent(uow, cmd.request_id, "CreateRefund", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;

                let transaction_id = cmd.authorization.original_transaction_id;
                let view = eligibility(uow, transaction_id)?;
                require_refundable(&view)?;

                if cmd.lines.is_empty() {
                    return Err(AppError::Refund(RefundError::EmptyRefund));
                }
                let mut seen: Vec<i64> = Vec::new();
                let mut lines = Vec::with_capacity(cmd.lines.len());
                for request in &cmd.lines {
                    if seen.contains(&request.original_line_number) {
                        return Err(AppError::Refund(RefundError::DuplicateLine {
                            line_number: request.original_line_number,
                        }));
                    }
                    seen.push(request.original_line_number);
                    let refundable = view
                        .lines
                        .iter()
                        .find(|line| line.line_number == request.original_line_number)
                        .ok_or(RefundError::LineNotFound {
                            line_number: request.original_line_number,
                        })?;
                    let amounts = refundable.quote(request.quantity)?;
                    lines.push(RefundLine {
                        refund_line_id: ids.new_uuid(),
                        original_line_number: refundable.line_number,
                        product_ref: refundable.product_ref.clone(),
                        description: refundable.description.clone(),
                        quantity: request.quantity,
                        original_unit_price: refundable.unit_price,
                        amounts,
                    });
                }

                // The fiscal consequence is constrained by the original sale,
                // never declared freely by the operator (ADR 032).
                let fiscal_status =
                    RefundFiscalStatus::choose(view.fiscal_state, cmd.fiscal_action)?;

                let refund = Refund::open(NewRefund {
                    refund_id,
                    original_transaction_id: transaction_id,
                    store_id: device.store_id.clone(),
                    business_day_id: day.id(),
                    session_id: session.id(),
                    register_id: session.state().register_id.clone(),
                    operator_id: session.state().operator_id.clone(),
                    authority: cmd.authorization.authority,
                    authorization_ref: cmd.authorization.authorization_ref.clone(),
                    lines,
                    currency: view.currency,
                    fiscal_status,
                    original_fiscal_state: view.fiscal_state,
                    reason_code: cmd.reason_code.clone(),
                    now,
                })
                .map_err(AppError::Refund)?;
                // The unique index on a non-terminal refund per sale is what
                // actually stops two refunds racing (ADR 030).
                uow.insert_refund(&refund)?;

                journal(
                    uow,
                    event_id,
                    JournalEventType::RefundCreated,
                    &device,
                    &refund,
                    now,
                    json!({
                        "original_transaction_id": transaction_id.to_string(),
                        "authority": refund.authority.as_str(),
                        "lines": refund.lines.len(),
                        "gross_minor": refund.total.gross.minor(),
                        "net_minor": refund.total.net.minor(),
                        "tax_minor": refund.total.tax.minor(),
                        "fiscal_status": refund.fiscal_status.as_str(),
                        "reason_code": refund.reason_code,
                    }),
                )?;
                refund_view(uow, refund)
            })
        })
    }

    /// The goods came back. Writes the inventory return, exactly once.
    pub fn accept_refund(&self, cmd: AcceptRefundCommand) -> Result<RefundView, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());
        let ids = &self.ids;

        self.store().transact(|uow| {
            idempotent(uow, cmd.request_id, "AcceptRefund", now, |uow| {
                let device = require_device(uow)?;
                let mut refund = require_refund(uow, cmd.refund_id)?;
                refund.accept(now).map_err(AppError::Refund)?;
                uow.update_refund(&refund)?;

                // One movement per refund line, in the same transaction as the
                // acceptance: there is no state in which a refund is accepted
                // and the goods are not back on the ledger (ADR 033).
                let mut returned = 0;
                for line in &refund.lines {
                    let source = MovementSource::RefundLine {
                        refund_id: refund.refund_id,
                        line_number: line.original_line_number,
                    };
                    let product_ref =
                        ProductRef::new(line.product_ref.clone()).map_err(DomainError::from)?;
                    if uow.inventory_movement_exists(
                        &device.store_id,
                        InventoryMovementType::RefundReturn,
                        &product_ref,
                        &source.as_ref_string(),
                    )? {
                        continue;
                    }
                    let movement = InventoryMovement::record(NewInventoryMovement {
                        movement_id: ids.new_uuid(),
                        store_id: device.store_id.clone(),
                        product_ref,
                        movement_type: InventoryMovementType::RefundReturn,
                        quantity: line.quantity,
                        source,
                        business_day_id: Some(refund.business_day_id),
                        session_id: Some(refund.session_id),
                        reason_code: None,
                        occurred_at: now,
                    })
                    .map_err(AppError::Inventory)?;
                    uow.insert_inventory_movement(&movement)?;
                    returned += 1;
                }

                journal(
                    uow,
                    event_id,
                    JournalEventType::RefundAccepted,
                    &device,
                    &refund,
                    now,
                    json!({
                        "original_transaction_id":
                            refund.original_transaction_id.to_string(),
                        "inventory_movements": returned,
                        "restock": refund.restock,
                    }),
                )?;
                // Same transaction as the acceptance and the returned goods:
                // there is no state in which the commercial return is durable
                // and the fact about it is missing (ADR 004, ADR 041).
                enqueue_refund_fact(
                    uow,
                    EventId::from_uuid(ids.new_uuid()),
                    OutboxEventType::RefundAccepted,
                    ACCEPTED_VERSION,
                    &device,
                    &refund,
                    now,
                )?;
                refund_view(uow, refund)
            })
        })
    }

    /// Give the money back, spread over the original tenders.
    ///
    /// Durable before the provider call, exactly like taking money: the refund
    /// payment and its attempt exist, with their correlation ids, before
    /// anything is sent (ADR 020).
    pub fn start_refund_payment(
        &self,
        cmd: StartRefundPaymentCommand,
    ) -> Result<RefundView, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());
        let ids = &self.ids;

        // ---- phase 1: everything durable, no provider contacted -----------
        self.store().transact(|uow| {
            idempotent(uow, cmd.request_id, "StartRefundPayment", now, |uow| {
                let device = require_device(uow)?;
                let refund = require_refund(uow, cmd.refund_id)?;
                if !refund.status.is_commercially_accepted() {
                    return Err(AppError::Refund(RefundError::InvalidState {
                        status: refund.status.as_str(),
                        attempted: "START_REFUND_PAYMENT",
                    }));
                }
                let existing = uow.list_refund_payments(refund.refund_id)?;
                if existing
                    .iter()
                    .any(|view| view.payment.status.requires_reconciliation())
                {
                    return Err(AppError::Refund(RefundError::RequiresReconciliation {
                        refund_id: refund.refund_id.to_string(),
                    }));
                }
                if !existing.is_empty() {
                    // Already paid out: answer with the current state rather
                    // than refunding a second time.
                    return refund_view(uow, refund);
                }

                let tenders = tender_capacity(uow, refund.original_transaction_id)?;
                let shares = allocate_over_tenders(refund.amount_due(), &tenders)?;
                let mut created = Vec::new();
                for (index, share) in shares {
                    let tender = &tenders[index];
                    let payment = RefundPayment {
                        refund_payment_id: ids.new_uuid(),
                        refund_id: refund.refund_id,
                        original_payment_id: PaymentId::from_uuid(
                            Uuid::parse_str(&tender.payment_id).map_err(|_| {
                                RefundError::InvalidRecord {
                                    field: "original_payment_id",
                                    reason: "is not a uuid".to_owned(),
                                }
                            })?,
                        ),
                        tender_type: TenderType::parse(&tender.tender_type)
                            .map_err(AppError::Payment)?,
                        requested_amount: share,
                        refunded_amount: Money::zero(refund.currency),
                        status: RefundPaymentStatus::Created,
                        created_at: now,
                        updated_at: now,
                    };
                    uow.insert_refund_payment(&payment)?;
                    created.push(payment);
                }

                let mut refund = refund;
                let mut day_session = None;
                for payment in &created {
                    match payment.tender_type {
                        // Cash has no provider: the drawer moves here and now,
                        // in the same transaction, keyed by the refund payment
                        // so a replay cannot empty it twice (ADR 021).
                        TenderType::Cash => {
                            let (day_id, session) = match &day_session {
                                Some(pair) => pair,
                                None => {
                                    let day = require_open_business_day(uow, &device.store_id)?;
                                    let session = require_single_open_session(uow, day.id())?;
                                    day_session = Some((day.id(), session));
                                    day_session.as_ref().unwrap_or_else(|| unreachable!())
                                }
                            };
                            record_cash_refund(
                                uow,
                                ids.new_uuid(),
                                *day_id,
                                session,
                                payment,
                                now,
                            )?;
                            let mut settled = payment.clone();
                            settled.refunded_amount = payment.requested_amount;
                            settled.status = RefundPaymentStatus::Refunded;
                            settled.updated_at = now;
                            uow.update_refund_payment(&settled)?;
                        }
                        TenderType::Card => {
                            // Committed as PROCESSING before the provider is
                            // called, with its correlation id.
                            let attempt = RefundAttempt {
                                attempt_id: ids.new_uuid(),
                                refund_payment_id: payment.refund_payment_id,
                                attempt_number: uow
                                    .next_refund_attempt_number(payment.refund_payment_id)?,
                                status: RefundAttemptStatus::Processing,
                                requested_amount: payment.requested_amount,
                                refunded_amount: None,
                                correlation_id: ids.new_uuid(),
                                provider_reference: None,
                                failure_code: None,
                                created_at: now,
                                updated_at: now,
                            };
                            uow.insert_refund_attempt(&attempt)?;
                            let mut pending = payment.clone();
                            pending.status = RefundPaymentStatus::Pending;
                            pending.updated_at = now;
                            uow.update_refund_payment(&pending)?;
                            refund
                                .mark_financial_pending(now)
                                .map_err(AppError::Refund)?;
                        }
                    }
                }
                if settle_if_done(uow, &mut refund, now)? {
                    enqueue_refund_fact(
                        uow,
                        EventId::from_uuid(ids.new_uuid()),
                        OutboxEventType::RefundCompleted,
                        COMPLETED_VERSION,
                        &device,
                        &refund,
                        now,
                    )?;
                }
                uow.update_refund(&refund)?;

                journal(
                    uow,
                    event_id,
                    JournalEventType::RefundFinancialStarted,
                    &device,
                    &refund,
                    now,
                    json!({
                        "tenders": created.iter().map(|payment| json!({
                            "tender_type": payment.tender_type.as_str(),
                            "requested_minor": payment.requested_amount.minor(),
                        })).collect::<Vec<_>>(),
                        "refund_status": refund.status.as_str(),
                    }),
                )?;
                refund_view(uow, refund)
            })
        })?;

        // ---- the physical side effects, with no transaction open ---------
        // Cash left the drawer, so the drawer opens: keyed by the refund
        // payment, so a replay cannot open it twice, and a failure leaves the
        // refund exactly as settled as it was (ADR 037).
        let cash_payments = self.store().transact(move |uow| {
            Ok(uow
                .list_refund_payments(cmd.refund_id)?
                .into_iter()
                .filter(|view| {
                    view.payment.tender_type == TenderType::Cash
                        && view.payment.status == RefundPaymentStatus::Refunded
                })
                .map(|view| view.payment.refund_payment_id)
                .collect::<Vec<_>>())
        })?;
        for refund_payment_id in cash_payments {
            self.open_drawer_for(
                edge_hardware::DrawerOpenReason::CashRefund,
                refund_payment_id.to_string(),
                now,
            );
        }

        self.push_pending_refunds(cmd.refund_id, now)
    }

    /// Ask the provider what happened to an unresolved refund.
    ///
    /// Read-only against the provider: it resolves an attempt and can never
    /// send a second refund (ADR 018).
    pub fn reconcile_refund(&self, cmd: ReconcileRefundCommand) -> Result<RefundView, AppError> {
        let now = self.clock.now();
        let pending = self.store().transact(move |uow| {
            require_refund(uow, cmd.refund_id)?;
            Ok(unresolved_attempts(
                &uow.list_refund_payments(cmd.refund_id)?,
            ))
        })?;

        for (attempt, amount, reference) in pending {
            let query = ProviderQuery {
                correlation_id: attempt,
                provider_reference: reference,
                amount,
            };
            let outcome = self.payments.query_refund(&query);
            self.record_refund_outcome(cmd.refund_id, attempt, outcome, now)?;
        }
        self.get_refund(cmd.refund_id)
    }

    /// Record that a sale was invoiced upstream.
    ///
    /// **Test and development only.** PI.Edge consumes fiscal metadata; it is
    /// not the authority for declaring a sale invoiced, and there is no
    /// production operation, protocol message or CLI command that writes it.
    /// A fixture uses this to arrange the invoiced case until a sync brings
    /// the metadata in (ADR 032).
    #[cfg(feature = "testing")]
    pub fn mark_transaction_invoiced(
        &self,
        cmd: MarkTransactionInvoicedCommand,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        self.store().transact(|uow| {
            idempotent(uow, cmd.request_id, "MarkTransactionInvoiced", now, |uow| {
                // The same projection the inbound sync applies, so the test
                // fixture and the production path cannot drift apart.
                crate::sync_service::apply_fiscal_metadata(
                    uow,
                    &crate::sync_service::InboundFiscalMetadata {
                        transaction_id: cmd.transaction_id,
                        fiscal_state: FiscalState::Invoiced.as_str().to_owned(),
                        invoice_ref: Some(cmd.invoice_ref.clone()),
                    },
                    now,
                )
            })
        })
    }

    /// Send every refund attempt that is sitting `PROCESSING`, then persist
    /// what the provider said. Called with no transaction open.
    fn push_pending_refunds(
        &self,
        refund_id: RefundId,
        now: Timestamp,
    ) -> Result<RefundView, AppError> {
        let pending = self
            .store()
            .transact(move |uow| Ok(processing_attempts(&uow.list_refund_payments(refund_id)?)))?;
        for (correlation_id, amount) in pending {
            let request = RefundRequest {
                correlation_id,
                tender_type: TenderType::Card,
                amount,
                original_provider_reference: None,
                requested_at: now,
            };
            let outcome = self.payments.refund_payment(&request);
            self.record_refund_outcome(refund_id, correlation_id, outcome, now)?;
        }
        self.get_refund(refund_id)
    }

    /// Phase 2: what the provider said becomes durable.
    fn record_refund_outcome(
        &self,
        refund_id: RefundId,
        correlation_id: Uuid,
        outcome: Result<RefundOutcome, edge_payments::PaymentError>,
        now: Timestamp,
    ) -> Result<RefundView, AppError> {
        let event_id = EventId::from_uuid(self.ids.new_uuid());
        let ids = &self.ids;
        self.store().transact(|uow| {
            let device = require_device(uow)?;
            let mut refund = require_refund(uow, refund_id)?;
            let views = uow.list_refund_payments(refund_id)?;
            let Some((mut payment, mut attempt)) = find_attempt(&views, correlation_id) else {
                return refund_view(uow, refund);
            };

            match &outcome {
                Ok(RefundOutcome::Refunded {
                    refunded,
                    provider_reference,
                }) => {
                    attempt.status = RefundAttemptStatus::Refunded;
                    attempt.refunded_amount = Some(*refunded);
                    attempt.provider_reference = Some(provider_reference.clone());
                    payment.refunded_amount = *refunded;
                    payment.status = RefundPaymentStatus::Refunded;
                }
                Ok(RefundOutcome::Declined { failure_code }) => {
                    attempt.status = RefundAttemptStatus::Failed;
                    attempt.failure_code = Some(failure_code.clone());
                    payment.status = RefundPaymentStatus::Failed;
                }
                // Unknown, or the provider itself failed: the money may or may
                // not have gone back. Never `FAILED`, never retried on its
                // own — the only way out is asking about this attempt
                // (ADR 018).
                Ok(RefundOutcome::Unknown) | Err(_) => {
                    attempt.status = RefundAttemptStatus::Unknown;
                    payment.status = RefundPaymentStatus::Pending;
                }
            }
            attempt.updated_at = now;
            payment.updated_at = now;
            uow.update_refund_attempt(&attempt)?;
            uow.update_refund_payment(&payment)?;

            // A settled card refund of a cash-and-card sale can be the last
            // thing missing, so completion is evaluated after every outcome.
            let event_type = match attempt.status {
                RefundAttemptStatus::Refunded => JournalEventType::RefundFinancialCompleted,
                RefundAttemptStatus::Unknown => JournalEventType::RefundFinancialUnknown,
                _ => JournalEventType::RefundFinancialFailed,
            };
            if settle_if_done(uow, &mut refund, now)? {
                enqueue_refund_fact(
                    uow,
                    EventId::from_uuid(ids.new_uuid()),
                    OutboxEventType::RefundCompleted,
                    COMPLETED_VERSION,
                    &device,
                    &refund,
                    now,
                )?;
            }
            uow.update_refund(&refund)?;
            journal(
                uow,
                event_id,
                event_type,
                &device,
                &refund,
                now,
                json!({
                    "correlation_id": correlation_id.to_string(),
                    "attempt_status": attempt.status.as_str(),
                    "refunded_minor": attempt.refunded_amount.map(|a| a.minor()),
                    "failure_code": attempt.failure_code,
                    "refund_status": refund.status.as_str(),
                }),
            )?;
            refund_view(uow, refund)
        })
    }
}

// ----------------------------------------------------------------- internals

/// The fiscal metadata row every finalized sale gets.
///
/// Written at finalization so a missing row for a finalized sale is a bug
/// rather than a third fiscal state nobody can act on (ADR 032). PI.Edge
/// issues no invoice, so the value it writes is always `NOT_INVOICED`.
pub(crate) fn record_sale_fiscal_metadata(
    uow: &mut dyn UnitOfWork,
    transaction_id: TransactionId,
    now: Timestamp,
) -> Result<(), AppError> {
    uow.put_transaction_fiscal_metadata(
        transaction_id,
        &TransactionFiscalMetadata {
            fiscal_state: FiscalState::NotInvoiced,
            invoice_ref: None,
            updated_at: now,
        },
    )
}

fn require_refund(uow: &mut dyn UnitOfWork, refund_id: RefundId) -> Result<Refund, AppError> {
    uow.find_refund(refund_id)?
        .ok_or_else(|| AppError::RefundNotFound {
            refund_id: refund_id.to_string(),
        })
}

pub(crate) fn require_finalized(
    uow: &mut dyn UnitOfWork,
    transaction_id: TransactionId,
) -> Result<RetailTransaction, AppError> {
    let transaction =
        uow.find_transaction(transaction_id)?
            .ok_or_else(|| AppError::TransactionNotFound {
                transaction_id: transaction_id.to_string(),
            })?;
    if transaction.status() != TransactionStatus::Finalized {
        return Err(AppError::Refund(RefundError::OriginalNotRefundable {
            transaction_id: transaction_id.to_string(),
            status: transaction.status().as_str(),
        }));
    }
    Ok(transaction)
}

/// Everything a client — or `CreateRefund` — needs to know about what a sale
/// can still give back.
fn eligibility(
    uow: &mut dyn UnitOfWork,
    transaction_id: TransactionId,
) -> Result<RefundEligibilityView, AppError> {
    let transaction =
        uow.find_transaction(transaction_id)?
            .ok_or_else(|| AppError::TransactionNotFound {
                transaction_id: transaction_id.to_string(),
            })?;
    let state = transaction.state();
    let currency = state.currency;

    let already = uow.refunded_by_line(transaction_id)?;
    let mut lines = Vec::with_capacity(state.lines.len());
    let mut refundable_total = Money::zero(currency);
    for line in &state.lines {
        let refunded = already
            .iter()
            .find(|row| row.original_line_number == line.line_number);
        let view = LineRefundable::new(
            line,
            refunded.map(|row| row.quantity_thousandths).unwrap_or(0),
            refunded
                .map(|row| row.amounts)
                .unwrap_or_else(|| RefundAmounts::zero(currency)),
        )
        .map_err(AppError::Refund)?;
        refundable_total = refundable_total
            .checked_add(view.remaining.gross)
            .map_err(DomainError::from)?;
        lines.push(view);
    }

    // The fiscal state of a sale that never finalized is not a thing yet; the
    // refundability check below is what actually refuses it.
    let fiscal = uow.find_transaction_fiscal_metadata(transaction_id)?;
    let refunds = uow.list_transaction_refunds(transaction_id)?;

    Ok(RefundEligibilityView {
        transaction_id,
        original_status: state.status.as_str().to_owned(),
        register_sequence: state.register_sequence,
        currency,
        original_total: state.totals.grand_total,
        fiscal_state: fiscal
            .as_ref()
            .map(|row| row.fiscal_state)
            .unwrap_or(FiscalState::NotInvoiced),
        invoice_ref: fiscal.and_then(|row| row.invoice_ref),
        lines,
        tenders: tender_capacity(uow, transaction_id)?,
        refundable_total,
        refund_in_progress: uow
            .find_refund_in_progress(transaction_id)?
            .map(|refund| refund.refund_id),
        refund_ids: refunds.iter().map(|refund| refund.refund_id).collect(),
    })
}

/// What each original tender took, and what it has left to give back.
fn tender_capacity(
    uow: &mut dyn UnitOfWork,
    transaction_id: TransactionId,
) -> Result<Vec<TenderRefundable>, AppError> {
    let reserved = uow.refunded_by_payment(transaction_id)?;
    let mut tenders = Vec::new();
    for view in uow.list_transaction_payments(transaction_id)? {
        if view.payment.status != PaymentStatus::Committed {
            continue;
        }
        let taken = reserved
            .iter()
            .find(|row| row.original_payment_id == view.payment.payment_id)
            .map(|row| row.reserved)
            .unwrap_or_else(|| Money::zero(view.payment.committed_amount.currency()));
        tenders.push(TenderRefundable {
            payment_id: view.payment.payment_id.to_string(),
            tender_type: view.payment.tender_type.as_str().to_owned(),
            committed: view.payment.committed_amount,
            remaining: view
                .payment
                .committed_amount
                .checked_sub(taken)
                .map_err(DomainError::from)?
                .clamp_non_negative(),
        });
    }
    Ok(tenders)
}

/// The checks a refund has to pass before it exists at all.
///
/// Authority is **not** among them: the command carries an already-resolved
/// context whose only variant is `CENTRAL_ACCEPTED`, so the refusal happens
/// where authority is resolved — the composition root, which answers
/// `REFUND_AUTHORITY_REQUIRED` when it has none (ADR 030). Re-checking a
/// single-variant enum here would only look like a guard.
fn require_refundable(view: &RefundEligibilityView) -> Result<(), AppError> {
    if view.original_status != TransactionStatus::Finalized.as_str() {
        return Err(AppError::Refund(RefundError::OriginalNotRefundable {
            transaction_id: view.transaction_id.to_string(),
            // The status came from the aggregate, so it is one of its values.
            status: TransactionStatus::parse(&view.original_status)
                .map(|status| status.as_str())
                .unwrap_or("UNKNOWN"),
        }));
    }
    if let Some(refund_id) = view.refund_in_progress {
        return Err(AppError::Refund(RefundError::RefundInProgress {
            transaction_id: view.transaction_id.to_string(),
            refund_id: refund_id.to_string(),
        }));
    }
    Ok(())
}

/// Cash out of the drawer for a refund, exactly once per refund payment.
fn record_cash_refund(
    uow: &mut dyn UnitOfWork,
    movement_id: Uuid,
    day_id: BusinessDayId,
    session: &RegisterSession,
    payment: &RefundPayment,
    now: Timestamp,
) -> Result<(), AppError> {
    let source_ref = payment.refund_payment_id.to_string();
    if uow
        .find_cash_movement_by_source(
            session.id(),
            edge_cash::MovementType::CashRefund,
            &source_ref,
        )?
        .is_some()
    {
        return Ok(());
    }
    let movement = CashMovement::record(NewCashMovement {
        movement_id,
        business_day_id: day_id,
        session_id: session.id(),
        register_id: session.state().register_id.clone(),
        operator_id: session.state().operator_id.clone(),
        movement_type: edge_cash::MovementType::CashRefund,
        direction: Direction::Out,
        amount: payment.requested_amount,
        tendered: None,
        change: None,
        source_ref: Some(source_ref),
        reason_code: None,
        approval_id: None,
        now,
    })
    .map_err(AppError::Cash)?;
    uow.insert_cash_movement(&movement)
}

/// Complete the refund when nothing financial is outstanding.
///
/// Answers whether *this* call is the one that completed it, which is what
/// makes the outbox event publishable exactly once: a refund is a fact about
/// the business only when the goods and the money have both settled (ADR 041).
fn settle_if_done(
    uow: &mut dyn UnitOfWork,
    refund: &mut Refund,
    now: Timestamp,
) -> Result<bool, AppError> {
    if refund.status == RefundStatus::Completed || !refund.status.is_commercially_accepted() {
        return Ok(false);
    }
    let views = uow.list_refund_payments(refund.refund_id)?;
    if views.is_empty() {
        return Ok(false);
    }
    let settled: i64 = views
        .iter()
        .map(|view| view.payment.refunded_amount.minor())
        .sum();
    let unresolved = views
        .iter()
        .any(|view| view.payment.status.requires_reconciliation());
    if unresolved {
        refund
            .mark_financial_pending(now)
            .map_err(AppError::Refund)?;
        return Ok(false);
    }
    if settled >= refund.amount_due().minor() {
        refund.complete(now).map_err(AppError::Refund)?;
        return Ok(true);
    }
    Ok(false)
}

/// The two outbound facts a refund produces, and their order.
///
/// A refund is published **twice**, as two versions of one aggregate:
///
/// | Fact | Version | Means |
/// | --- | --- | --- |
/// | `REFUND_ACCEPTED` | 1 | the commercial return happened and is irrevocable |
/// | `REFUND_COMPLETED` | 2 | the financial obligation is settled |
///
/// Acceptance is published on its own because it is already a fact about the
/// business: the goods are back, the refundable quantities are consumed and
/// `REFUND_RETURN` is written, none of which is undone because a provider
/// answered `UNKNOWN`. A refund can sit in `FINANCIAL_PENDING` indefinitely,
/// and the consumer must not hear about the return only when the money
/// resolves (ADR 041).
///
/// These are the versions of the published refund stream, not the aggregate's
/// internal row version — the consumer sees two facts about one refund, in
/// order, and `seq` plus the one-fact-per-aggregate-per-batch rule guarantees
/// it never gets the second before the first (ADR 041).
const ACCEPTED_VERSION: i64 = 1;
const COMPLETED_VERSION: i64 = 2;

/// Publish one of a refund's two facts.
///
/// The whole refund travels, not a delta: the consumer gets the lines, the
/// amounts, the authority and the authorization reference, and needs none of
/// PI.Edge's intermediate states to make sense of it. The stock that came back
/// is *not* published separately — it is derivable from the refund, and
/// publishing both would hand the consumer the same physical return twice
/// (ADR 028).
fn enqueue_refund_fact(
    uow: &mut dyn UnitOfWork,
    event_id: EventId,
    event_type: OutboxEventType,
    aggregate_version: i64,
    device: &DeviceProfile,
    refund: &Refund,
    now: Timestamp,
) -> Result<(), AppError> {
    let payload =
        serde_json::to_value(refund).map_err(|e| AppError::Serialization(e.to_string()))?;
    uow.enqueue_outbox(&OutboxEvent::pending(
        event_id,
        event_type,
        refund.refund_id.to_string(),
        aggregate_version,
        device.tenant_id.clone(),
        device.store_id.clone(),
        Some(refund.register_id.clone()),
        Some(refund.business_day_id),
        now,
        payload,
    ))
}

fn refund_view(uow: &mut dyn UnitOfWork, refund: Refund) -> Result<RefundView, AppError> {
    let payments = uow.list_refund_payments(refund.refund_id)?;
    let settled: i64 = payments
        .iter()
        .map(|view| view.payment.refunded_amount.minor())
        .sum();
    let remaining = Money::from_minor(
        (refund.amount_due().minor() - settled).max(0),
        refund.currency,
    );
    Ok(RefundView {
        inventory_returned: refund.status.is_commercially_accepted(),
        requires_reconciliation: payments
            .iter()
            .any(|view| view.payment.status.requires_reconciliation()),
        remaining_to_refund: remaining,
        payments,
        refund,
    })
}

/// Attempts that were sent and never answered.
fn processing_attempts(views: &[RefundPaymentView]) -> Vec<(Uuid, Money)> {
    views
        .iter()
        .flat_map(|view| view.attempts.iter())
        .filter(|attempt| attempt.status == RefundAttemptStatus::Processing)
        .map(|attempt| (attempt.correlation_id, attempt.requested_amount))
        .collect()
}

/// Attempts whose outcome is open, for a reconciliation.
fn unresolved_attempts(views: &[RefundPaymentView]) -> Vec<(Uuid, Money, Option<String>)> {
    views
        .iter()
        .flat_map(|view| view.attempts.iter())
        .filter(|attempt| attempt.status.is_unresolved())
        .map(|attempt| {
            (
                attempt.correlation_id,
                attempt.requested_amount,
                attempt.provider_reference.clone(),
            )
        })
        .collect()
}

fn find_attempt(
    views: &[RefundPaymentView],
    correlation_id: Uuid,
) -> Option<(RefundPayment, RefundAttempt)> {
    views.iter().find_map(|view| {
        view.attempts
            .iter()
            .find(|attempt| attempt.correlation_id == correlation_id)
            .map(|attempt| (view.payment.clone(), attempt.clone()))
    })
}

#[allow(clippy::too_many_arguments)]
fn journal(
    uow: &mut dyn UnitOfWork,
    event_id: EventId,
    event_type: JournalEventType,
    device: &DeviceProfile,
    refund: &Refund,
    now: Timestamp,
    payload: serde_json::Value,
) -> Result<(), AppError> {
    let ctx = EventContext {
        device,
        register_id: Some(&refund.register_id),
        business_day_id: Some(refund.business_day_id),
        session_id: Some(refund.session_id),
        transaction_id: Some(refund.original_transaction_id),
        operator_id: Some(&refund.operator_id),
    };
    uow.append_journal(&journal_event(
        event_id,
        event_type,
        refund.refund_id.to_string(),
        &ctx,
        now,
        payload,
    ))
}
