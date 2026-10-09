//! Payment use cases.
//!
//! This is the only place in the core where a use case is **not** a single
//! database transaction. A provider call cannot happen inside one: if we
//! called first and persisted afterwards, a process death in between would
//! leave a customer charged with no local record that the charge was ever
//! attempted. So every provider interaction is bracketed —
//! *persist, call, persist* — and the provider is called with no transaction
//! open (ADR 020).
//!
//! Nothing here ever turns an unknown outcome into a failure, and nothing ever
//! retries a charge on its own (ADR 018).

use edge_domain::prelude::*;
use edge_payments::{
    settle, split_tender, AttemptStatus, NewPayment, NewPaymentAttempt, Payment, PaymentAttempt,
    PaymentError, PaymentProviderPort, PaymentStatus, PaymentView, ProviderOutcome, ProviderQuery,
    ProviderRequest, ProviderReversal, ReversalOutcome, Settlement, TenderType,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::commands::{ReconcilePaymentCommand, ReversePaymentCommand, StartPaymentCommand};
use crate::context::{
    idempotent, journal_event, require_active_transaction, require_device,
    require_open_business_day, require_single_open_session, EventContext,
};
use crate::error::AppError;
use crate::ports::{Clock, DeviceProfile, EdgeStore, IdFactory, UnitOfWork};
use crate::EdgeApplication;

/// What a payment command answers with: the payment, its attempts, and what
/// the sale looks like afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentOutcome {
    pub payment: PaymentView,
    pub transaction: RetailTransaction,
    pub settlement: Settlement,
}

/// The durable handle recorded against a `request_id`, so a replayed command
/// resolves the same payment instead of starting a new one (ADR 020).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaymentHandle {
    payment_id: PaymentId,
}

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: PaymentProviderPort,
{
    /// Start a payment against the active sale.
    ///
    /// Cash settles here and now. A provider-backed tender is recorded
    /// durably, then sent, then recorded again.
    pub fn start_payment(&self, cmd: StartPaymentCommand) -> Result<PaymentOutcome, AppError> {
        let now = self.clock.now();
        let payment_id = PaymentId::from_uuid(self.ids.new_uuid());
        let attempt_id = self.ids.new_uuid();
        let correlation_id = self.ids.new_uuid();
        let cash_movement_id = self.ids.new_uuid();
        let event_id = EventId::from_uuid(self.ids.new_uuid());
        let tender = cmd.tender_type;
        let provider_kind = tender.uses_provider().then(|| self.payments.kind());

        // ---- phase 1: everything financially relevant becomes durable -----
        let handle = self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "StartPayment", now, |uow| {
                let context = PaymentContext::load(uow)?;
                // Nothing can be tendered against a sale whose total is not
                // official yet. Checked before the amounts so the refusal says
                // "totalize first" rather than "there is nothing to pay".
                context.require_payable()?;
                // Availability goes here: the last moment at which nothing has
                // been charged and refusing costs the customer a re-scan
                // instead of a reversal. It is never checked after the money
                // moves (ADR 029).
                crate::inventory_service::require_available_stock(
                    uow,
                    &context.device,
                    &context.transaction,
                )?;
                let settlement = context.settlement(uow)?;

                // An unresolved payment makes the remaining balance unknowable
                // and could already be covering this very amount (ADR 018).
                if settlement.requires_resolution {
                    return Err(AppError::Payment(PaymentError::RequiresReconciliation {
                        payment_id: context.unresolved_payment(uow)?,
                    }));
                }
                let requested = Money::from_minor(cmd.amount_minor, context.device.currency);
                let split = split_tender(tender, requested, settlement.remaining_balance)
                    .map_err(AppError::Payment)?;

                let mut payment = Payment::open(NewPayment {
                    payment_id,
                    transaction_id: context.transaction.id(),
                    tender_type: tender,
                    provider_kind,
                    requested_amount: requested,
                    now,
                })
                .map_err(AppError::Payment)?;

                let attempt = if tender.uses_provider() {
                    // Committed as PROCESSING *before* the provider is called.
                    let mut attempt = PaymentAttempt::open(NewPaymentAttempt {
                        attempt_id,
                        payment_id,
                        attempt_number: uow.next_attempt_number(payment_id)?,
                        requested_amount: requested,
                        provider_kind: self.payments.kind(),
                        correlation_id,
                        now,
                    })
                    .map_err(AppError::Payment)?;
                    attempt.mark_processing(now).map_err(AppError::Payment)?;
                    payment.mark_in_progress(now).map_err(AppError::Payment)?;
                    Some(attempt)
                } else {
                    // Cash has no provider to interact with: the drawer
                    // movement is physical and immediate (ADR 016).
                    payment
                        .commit(split.committed, now)
                        .map_err(AppError::Payment)?;
                    None
                };

                uow.insert_payment(&payment)?;
                if let Some(attempt) = &attempt {
                    uow.insert_payment_attempt(attempt)?;
                }
                if payment.status == PaymentStatus::Committed
                    && payment.tender_type == TenderType::Cash
                {
                    // The drawer moves in the same transaction as the payment
                    // that moved it, keyed by the payment id, so a replay
                    // cannot move it twice (ADR 021).
                    crate::cash_service::record_cash_sale(
                        uow,
                        cash_movement_id,
                        context.day.id(),
                        &context.session,
                        crate::cash_service::CashSale {
                            payment_id: payment.payment_id,
                            tendered: payment.requested_amount,
                            committed: payment.committed_amount,
                            change: split.change,
                        },
                        now,
                    )?;
                }
                let mut context = context;
                context.apply_settlement(uow, now)?;
                context.journal(
                    uow,
                    event_id,
                    JournalEventType::PaymentStarted,
                    now,
                    json!({
                        "payment_id": payment.payment_id.to_string(),
                        "tender_type": payment.tender_type.as_str(),
                        "provider_kind": payment.provider_kind.map(|kind| kind.as_str()),
                        "requested_minor": payment.requested_amount.minor(),
                        "attempt_number": attempt.as_ref().map(|a| a.attempt_number),
                        "correlation_id": attempt.as_ref().map(|a| a.correlation_id.to_string()),
                        "payment_status": payment.status.as_str(),
                    }),
                )?;
                Ok(PaymentHandle { payment_id })
            })
        })?;

        // ---- the provider call, with no transaction open ------------------
        let view = self.load_payment(handle.payment_id)?;
        if let Some(attempt) = pending_attempt(&view) {
            let request = ProviderRequest {
                correlation_id: attempt.correlation_id,
                tender_type: view.payment.tender_type,
                amount: attempt.requested_amount,
                requested_at: now,
            };
            let outcome = self.payments.start_payment(&request);
            // ---- phase 2: the result becomes durable ----------------------
            return self.record_provider_outcome(handle.payment_id, attempt.attempt_id, outcome);
        }

        self.load_outcome(handle.payment_id)
    }

    /// Ask the provider what happened to an unresolved attempt.
    ///
    /// Read-only against the provider: it resolves an attempt, and it can
    /// never create a payment, an attempt or a charge (ADR 018).
    pub fn reconcile_payment(
        &self,
        cmd: ReconcilePaymentCommand,
    ) -> Result<PaymentOutcome, AppError> {
        let view = self.load_payment(cmd.payment_id)?;
        let Some(attempt) = unresolved_attempt(&view) else {
            // Nothing to reconcile: answer with the current state rather than
            // inventing work, so a retried reconcile is harmless.
            return self.load_outcome(cmd.payment_id);
        };
        let query = ProviderQuery {
            correlation_id: attempt.correlation_id,
            provider_reference: attempt.provider_reference.clone(),
            amount: attempt.requested_amount,
        };
        if attempt.status == AttemptStatus::ReversalPending {
            let outcome = self.payments.reverse_payment(&ProviderReversal {
                correlation_id: attempt.correlation_id,
                provider_reference: attempt.provider_reference.clone(),
                amount: attempt.requested_amount,
            });
            return self.record_reversal_outcome(cmd.payment_id, attempt.attempt_id, outcome);
        }
        let outcome = self.payments.query_payment(&query);
        self.record_provider_outcome(cmd.payment_id, attempt.attempt_id, outcome)
    }

    /// Undo a payment that must not remain applied.
    ///
    /// Durable before the call, like a charge: a reversal that reached the
    /// provider is always recorded locally (ADR 019, ADR 020).
    pub fn reverse_payment(&self, cmd: ReversePaymentCommand) -> Result<PaymentOutcome, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        let handle = self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "ReversePayment", now, |uow| {
                let view = uow.find_payment(cmd.payment_id)?.ok_or(AppError::Payment(
                    PaymentError::PaymentNotFound {
                        payment_id: cmd.payment_id.to_string(),
                    },
                ))?;
                let mut context = PaymentContext::load(uow)?;
                if context.transaction.id() != view.payment.transaction_id {
                    return Err(AppError::Payment(PaymentError::PaymentNotFound {
                        payment_id: cmd.payment_id.to_string(),
                    }));
                }
                let Some(attempt) = reversible_attempt(&view) else {
                    return Err(AppError::Payment(PaymentError::ReversalNotAllowed {
                        payment_id: cmd.payment_id.to_string(),
                        status: view.payment.status.as_str(),
                    }));
                };
                let mut attempt = attempt.clone();
                attempt
                    .mark_reversal_pending(now)
                    .map_err(AppError::Payment)?;
                uow.update_payment_attempt(&attempt)?;
                context.apply_settlement(uow, now)?;
                context.journal(
                    uow,
                    event_id,
                    JournalEventType::PaymentReversalRequested,
                    now,
                    json!({
                        "payment_id": cmd.payment_id.to_string(),
                        "attempt_number": attempt.attempt_number,
                        "correlation_id": attempt.correlation_id.to_string(),
                        "amount_minor": attempt.requested_amount.minor(),
                    }),
                )?;
                Ok(PaymentHandle {
                    payment_id: cmd.payment_id,
                })
            })
        })?;

        let view = self.load_payment(handle.payment_id)?;
        let Some(attempt) = unresolved_attempt(&view) else {
            return self.load_outcome(handle.payment_id);
        };
        let outcome = self.payments.reverse_payment(&ProviderReversal {
            correlation_id: attempt.correlation_id,
            provider_reference: attempt.provider_reference.clone(),
            amount: attempt.requested_amount,
        });
        self.record_reversal_outcome(handle.payment_id, attempt.attempt_id, outcome)
    }

    pub fn get_payment(&self, payment_id: PaymentId) -> Result<PaymentOutcome, AppError> {
        self.load_outcome(payment_id)
    }

    /// Every payment of a sale, with attempts, plus the sale's settlement.
    pub fn get_transaction_payments(
        &self,
        transaction_id: TransactionId,
    ) -> Result<(Vec<PaymentView>, Settlement), AppError> {
        self.store().transact(move |uow| {
            let transaction = uow
                .find_transaction(transaction_id)?
                .ok_or(AppError::Payment(PaymentError::PaymentNotFound {
                    payment_id: transaction_id.to_string(),
                }))?;
            let views = uow.list_transaction_payments(transaction_id)?;
            let settlement = settle(&views, transaction.state().totals.grand_total)
                .map_err(AppError::Payment)?;
            Ok((views, settlement))
        })
    }

    // ----------------------------------------------------------- internals

    fn load_payment(&self, payment_id: PaymentId) -> Result<PaymentView, AppError> {
        self.store().transact(move |uow| {
            uow.find_payment(payment_id)?
                .ok_or(AppError::Payment(PaymentError::PaymentNotFound {
                    payment_id: payment_id.to_string(),
                }))
        })
    }

    fn load_outcome(&self, payment_id: PaymentId) -> Result<PaymentOutcome, AppError> {
        self.store().transact(move |uow| {
            let view = uow.find_payment(payment_id)?.ok_or(AppError::Payment(
                PaymentError::PaymentNotFound {
                    payment_id: payment_id.to_string(),
                },
            ))?;
            let transaction_id = view.payment.transaction_id;
            let transaction = uow
                .find_transaction(transaction_id)?
                .ok_or(AppError::Payment(PaymentError::PaymentNotFound {
                    payment_id: payment_id.to_string(),
                }))?;
            let views = uow.list_transaction_payments(transaction_id)?;
            let settlement = settle(&views, transaction.state().totals.grand_total)
                .map_err(AppError::Payment)?;
            Ok(PaymentOutcome {
                payment: view,
                transaction,
                settlement,
            })
        })
    }

    /// Phase 2 of a charge or a query: persist what the provider said.
    ///
    /// Re-applying a result the attempt already carries is a no-op, which is
    /// what makes a replayed command and a retried reconcile safe.
    fn record_provider_outcome(
        &self,
        payment_id: PaymentId,
        attempt_id: Uuid,
        outcome: Result<ProviderOutcome, PaymentError>,
    ) -> Result<PaymentOutcome, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            let mut attempt = uow
                .find_payment_attempt(attempt_id)?
                .ok_or(AppError::Payment(PaymentError::AttemptNotFound {
                    attempt_id: attempt_id.to_string(),
                }))?;
            let mut payment = uow
                .find_payment(payment_id)?
                .ok_or(AppError::Payment(PaymentError::PaymentNotFound {
                    payment_id: payment_id.to_string(),
                }))?
                .payment;
            let mut context = PaymentContext::load(uow)?;

            // A provider that could not be reached is not a failure: it is an
            // unknown, and it stays that way until someone asks (ADR 018).
            let resolved = match outcome {
                Ok(outcome) => outcome,
                Err(PaymentError::ProviderUnavailable { .. }) => ProviderOutcome::Unknown,
                Err(other) => return Err(AppError::Payment(other)),
            };

            let (event, payload) = match resolved {
                ProviderOutcome::Authorized {
                    authorized,
                    provider_reference,
                } => {
                    attempt
                        .mark_authorized(authorized, Some(provider_reference.clone()), now)
                        .map_err(AppError::Payment)?;
                    payment
                        .mark_authorized(authorized, now)
                        .map_err(AppError::Payment)?;
                    // An authorization that covers the balance is committed in
                    // the same transaction: there is no state in between worth
                    // exposing, and leaving it uncommitted would block the
                    // sale for no reason.
                    let settlement = context.settlement(uow)?;
                    let remaining = settlement
                        .remaining_balance
                        .checked_add(payment.committed_amount)
                        .map_err(|e| AppError::Payment(PaymentError::Money(e)))?;
                    let split = split_tender(payment.tender_type, authorized, remaining)
                        .map_err(AppError::Payment)?;
                    payment
                        .commit(split.committed, now)
                        .map_err(AppError::Payment)?;
                    (
                        JournalEventType::PaymentCommitted,
                        json!({
                            "payment_id": payment.payment_id.to_string(),
                            "attempt_number": attempt.attempt_number,
                            "authorized_minor": authorized.minor(),
                            "committed_minor": payment.committed_amount.minor(),
                            "provider_reference": provider_reference,
                        }),
                    )
                }
                ProviderOutcome::Declined { failure_code } => {
                    attempt
                        .mark_failed(Some(failure_code.clone()), now)
                        .map_err(AppError::Payment)?;
                    payment.mark_failed(now).map_err(AppError::Payment)?;
                    (
                        JournalEventType::PaymentFailed,
                        json!({
                            "payment_id": payment.payment_id.to_string(),
                            "attempt_number": attempt.attempt_number,
                            "failure_code": failure_code,
                        }),
                    )
                }
                ProviderOutcome::Unknown => {
                    attempt.mark_unknown(now).map_err(AppError::Payment)?;
                    (
                        JournalEventType::PaymentUnknown,
                        json!({
                            "payment_id": payment.payment_id.to_string(),
                            "attempt_number": attempt.attempt_number,
                            "correlation_id": attempt.correlation_id.to_string(),
                            "requested_minor": attempt.requested_amount.minor(),
                        }),
                    )
                }
            };

            uow.update_payment_attempt(&attempt)?;
            uow.update_payment(&payment)?;
            context.apply_settlement(uow, now)?;
            context.journal(uow, event_id, event, now, payload)?;

            let views = uow.list_transaction_payments(context.transaction.id())?;
            let settlement = settle(&views, context.transaction.state().totals.grand_total)
                .map_err(AppError::Payment)?;
            let view = uow.find_payment(payment_id)?.ok_or(AppError::Payment(
                PaymentError::PaymentNotFound {
                    payment_id: payment_id.to_string(),
                },
            ))?;
            Ok(PaymentOutcome {
                payment: view,
                transaction: context.transaction,
                settlement,
            })
        })
    }

    /// Phase 2 of a reversal.
    fn record_reversal_outcome(
        &self,
        payment_id: PaymentId,
        attempt_id: Uuid,
        outcome: Result<ReversalOutcome, PaymentError>,
    ) -> Result<PaymentOutcome, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            let mut attempt = uow
                .find_payment_attempt(attempt_id)?
                .ok_or(AppError::Payment(PaymentError::AttemptNotFound {
                    attempt_id: attempt_id.to_string(),
                }))?;
            let mut payment = uow
                .find_payment(payment_id)?
                .ok_or(AppError::Payment(PaymentError::PaymentNotFound {
                    payment_id: payment_id.to_string(),
                }))?
                .payment;
            let mut context = PaymentContext::load(uow)?;

            let confirmed = match outcome {
                Ok(ReversalOutcome::Reversed) => true,
                // An unconfirmed reversal stays REVERSAL_PENDING: unfinished
                // financial business, so the sale still cannot finalize.
                Ok(ReversalOutcome::Unknown) => false,
                Err(PaymentError::ProviderUnavailable { .. }) => false,
                Err(other) => return Err(AppError::Payment(other)),
            };
            if confirmed {
                attempt.mark_reversed(now).map_err(AppError::Payment)?;
                payment.mark_reversed(now).map_err(AppError::Payment)?;
                uow.update_payment_attempt(&attempt)?;
                uow.update_payment(&payment)?;
                context.apply_settlement(uow, now)?;
                context.journal(
                    uow,
                    event_id,
                    JournalEventType::PaymentReversed,
                    now,
                    json!({
                        "payment_id": payment.payment_id.to_string(),
                        "attempt_number": attempt.attempt_number,
                        "amount_minor": attempt.requested_amount.minor(),
                    }),
                )?;
            }

            let views = uow.list_transaction_payments(context.transaction.id())?;
            let settlement = settle(&views, context.transaction.state().totals.grand_total)
                .map_err(AppError::Payment)?;
            let view = uow.find_payment(payment_id)?.ok_or(AppError::Payment(
                PaymentError::PaymentNotFound {
                    payment_id: payment_id.to_string(),
                },
            ))?;
            Ok(PaymentOutcome {
                payment: view,
                transaction: context.transaction,
                settlement,
            })
        })
    }
}

/// The sale a payment command acts on, plus its surrounding context.
struct PaymentContext {
    device: DeviceProfile,
    day: BusinessDay,
    session: RegisterSession,
    transaction: RetailTransaction,
}

impl PaymentContext {
    fn load(uow: &mut dyn UnitOfWork) -> Result<Self, AppError> {
        let device = require_device(uow)?;
        let day = require_open_business_day(uow, &device.store_id)?;
        let session = require_single_open_session(uow, day.id())?;
        let transaction = require_active_transaction(uow, session.id())?;
        Ok(Self {
            device,
            day,
            session,
            transaction,
        })
    }

    /// A payment needs official totals: `TOTALIZED`, or a sale already part
    /// way through paying.
    fn require_payable(&self) -> Result<(), AppError> {
        match self.transaction.status() {
            TransactionStatus::Totalized | TransactionStatus::PaymentInProgress => Ok(()),
            other => Err(AppError::Domain(DomainError::InvalidTransition {
                aggregate: "RetailTransaction",
                from: other.as_str(),
                attempted: "PAYMENT_STARTED",
            })),
        }
    }

    fn settlement(&self, uow: &mut dyn UnitOfWork) -> Result<Settlement, AppError> {
        let views = uow.list_transaction_payments(self.transaction.id())?;
        settle(&views, self.transaction.state().totals.grand_total).map_err(AppError::Payment)
    }

    /// Which payment is holding the sale up, for the error message.
    fn unresolved_payment(&self, uow: &mut dyn UnitOfWork) -> Result<String, AppError> {
        let views = uow.list_transaction_payments(self.transaction.id())?;
        Ok(views
            .iter()
            .find(|view| view.is_unresolved())
            .map(|view| view.payment.payment_id.to_string())
            .unwrap_or_default())
    }

    /// Recompute the sale's payment summary and persist the sale.
    fn apply_settlement(
        &mut self,
        uow: &mut dyn UnitOfWork,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let settlement = self.settlement(uow)?;
        self.transaction
            .apply_payment_summary(settlement.summary(), now)?;
        uow.update_transaction(&self.transaction, now)
    }

    fn journal(
        &self,
        uow: &mut dyn UnitOfWork,
        event_id: EventId,
        event_type: JournalEventType,
        now: Timestamp,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let ctx = EventContext {
            device: &self.device,
            register_id: Some(&self.transaction.state().register_id),
            business_day_id: Some(self.day.id()),
            session_id: Some(self.session.id()),
            transaction_id: Some(self.transaction.id()),
            operator_id: Some(&self.transaction.state().operator_id),
        };
        uow.append_journal(&journal_event(
            event_id,
            event_type,
            self.transaction.id().to_string(),
            &ctx,
            now,
            payload,
        ))
    }
}

/// The attempt a charge is waiting on: the newest one still `PROCESSING`.
fn pending_attempt(view: &PaymentView) -> Option<&PaymentAttempt> {
    view.attempts
        .iter()
        .filter(|attempt| attempt.status == AttemptStatus::Processing)
        .max_by_key(|attempt| attempt.attempt_number)
}

/// The attempt a reconciliation has to ask about.
fn unresolved_attempt(view: &PaymentView) -> Option<&PaymentAttempt> {
    view.attempts
        .iter()
        .filter(|attempt| attempt.status.requires_reconciliation())
        .max_by_key(|attempt| attempt.attempt_number)
}

/// The attempt a reversal applies to: an authorization that is still
/// standing, or one whose reversal was started and never confirmed.
///
/// Including `REVERSAL_PENDING` is what makes retrying an unconfirmed reversal
/// the documented recovery path: the provider is idempotent on the attempt's
/// correlation id, so asking again cannot reverse twice (ADR 019).
fn reversible_attempt(view: &PaymentView) -> Option<&PaymentAttempt> {
    if view.payment.status == PaymentStatus::Reversed {
        return None;
    }
    view.attempts
        .iter()
        .filter(|attempt| {
            matches!(
                attempt.status,
                AttemptStatus::Authorized | AttemptStatus::ReversalPending
            )
        })
        .max_by_key(|attempt| attempt.attempt_number)
}
