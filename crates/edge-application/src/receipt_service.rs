//! The receipt: an obligation the sale takes on, and what became of it.
//!
//! The rule the whole file exists to hold: **a paid sale does not depend on a
//! printer to be valid** (ADR 043). So the commercial close and the paper are
//! two different truths, kept in two different places:
//!
//! ```text
//! retail_transactions   FINALIZED   the sale is commercially closed
//! transaction_receipts  PENDING     …and owes a receipt
//! hardware_actions      PENDING     …and we touched a printer about it
//! ```
//!
//! The obligation is written **in the same transaction** as `FINALIZED`, so a
//! closed sale always owes a receipt. The attempt happens afterwards, with no
//! transaction open, in the claim → call → record shape payments and the
//! drawer already use (ADR 020, ADR 037).
//!
//! What a failed attempt means depends on one question only: did the printer
//! get the job? `NotSent` is retryable, `MayHaveBeenSent` is not — and nothing
//! in this file ever retries on its own. A receipt that may already have come
//! out is a decision for the person standing at the printer.

use edge_domain::prelude::*;
use edge_hardware::{DeliveryCertainty, HardwareError, PrintKind};
use edge_payments::PaymentProviderPort;

use crate::context::require_device;
use crate::error::AppError;
use crate::ports::{
    Clock, EdgeStore, HardwareActionKind, HardwareActionStatus, IdFactory, UnitOfWork,
};
use crate::queries::{ReceiptRecovery, RecentSalesView};
use crate::service::EdgeApplication;

/// How many recent sales a client may ask for at once.
const MAX_RECENT_SALES: i64 = 100;

/// How many stuck obligations one recovery pass looks at.
///
/// A till that finalized more than this many sales without resolving a single
/// receipt has a different problem, and the next pass takes the rest.
const RECOVERY_BATCH: i64 = 200;

/// Written on the obligation when recovery cannot tell what happened.
const INTERRUPTED: &str = "RECEIPT_INTERRUPTED";

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: PaymentProviderPort,
{
    /// What this sale owes in paper, and what became of it.
    pub fn get_receipt(
        &self,
        transaction_id: TransactionId,
    ) -> Result<TransactionReceipt, AppError> {
        self.store()
            .transact(move |uow| require_receipt(uow, transaction_id))
    }

    /// How many sales still need somebody to look at their receipt, across
    /// the whole installation.
    ///
    /// Never a reason to refuse a close: a receipt is an operational
    /// obligation, not a financial one (ADR 043).
    pub fn get_receipt_attention(&self) -> Result<ReceiptAttention, AppError> {
        self.store()
            .transact(|uow| uow.receipt_attention(ReceiptAttentionScope::Global))
    }

    /// The sales an operator might still want to do something about.
    ///
    /// Operational history, not reporting: newest first, bounded, with each
    /// sale's own materialised total and its receipt state. Enough to find the
    /// sale just made and reprint it (ADR 043).
    pub fn list_recent_sales(&self, limit: i64, offset: i64) -> Result<RecentSalesView, AppError> {
        let limit = limit.clamp(1, MAX_RECENT_SALES);
        let offset = offset.max(0);
        self.store().transact(move |uow| {
            Ok(RecentSalesView {
                attention: uow.receipt_attention(ReceiptAttentionScope::Global)?,
                sales: uow.list_recent_sales(limit, offset)?,
            })
        })
    }

    /// Attempt the first receipt. Called right after the sale closed, and by
    /// an operator retrying one that certainly did not print.
    ///
    /// Answers the hardware failure when there is one, so an explicit retry
    /// can be told what went wrong.
    pub(crate) fn attempt_first_receipt(
        &self,
        transaction_id: TransactionId,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let job_id = self.ids.new_uuid();
        let (device, transaction) = self.store().transact(move |uow| {
            let receipt = require_receipt(uow, transaction_id)?;
            // `PRINTED` came out and `UNKNOWN` may have. Neither is a first
            // receipt to attempt, and saying so is the point.
            if !receipt.status.allows_first_attempt() {
                return Err(refuse(&receipt));
            }
            let device = require_device(uow)?;
            let transaction = require_finalized_sale(uow, transaction_id)?;
            Ok((device, transaction))
        })?;

        let document = self.receipt_document(job_id, PrintKind::Receipt, &device, &transaction);
        let attempt = self.perform(
            HardwareActionKind::Print,
            edge_hardware::HardwareDeviceKind::Printer,
            PrintKind::Receipt.as_str(),
            // Keyed by the sale, so the first receipt is attempted once per
            // claim and a replayed command cannot print a second one.
            Some(transaction_id.to_string()),
            now,
            || self.hardware.printer.print(&document),
        );

        let outcome = match &attempt {
            Ok(()) => ReceiptOutcome::Printed,
            // The printer was already claimed for this sale. Nothing physical
            // happened now, and the obligation must not be overwritten with a
            // failure that belongs to no attempt.
            Err(AppError::Hardware(HardwareError::ActionAlreadyCompleted { .. })) => {
                return attempt;
            }
            Err(AppError::Hardware(error)) => failure_outcome(error),
            // A storage or serialization fault is a bug, not a print outcome.
            Err(other) => return Err(other.clone()),
        };

        self.store().transact(move |uow| {
            let mut receipt = require_receipt(uow, transaction_id)?;
            receipt.record_outcome(outcome, now)?;
            uow.update_receipt(&receipt)
        })?;
        attempt
    }

    /// The first receipt as a side effect of closing the sale.
    ///
    /// Swallows every failure, exactly like the drawer: the sale is closed, the
    /// money has moved and no printer can un-move it. What happened is on the
    /// obligation for an operator to see (ADR 037, ADR 043).
    pub(crate) fn print_first_receipt(&self, transaction_id: TransactionId, now: Timestamp) {
        match self.attempt_first_receipt(transaction_id, now) {
            Ok(()) => tracing::info!(%transaction_id, "receipt printed"),
            Err(error) => tracing::warn!(
                %transaction_id,
                code = error.kind(),
                %error,
                "the receipt did not print; the sale is unaffected"
            ),
        }
    }

    /// Settle every receipt the previous runtime left in the air.
    ///
    /// Two cases, and confusing them is how a till prints a second ticket or
    /// loses a first one (ADR 043):
    ///
    /// * **No device evidence at all** — the process died between the
    ///   commercial commit and the claim. Nothing was attempted, so attempting
    ///   it now is safe and the obligation stays `PENDING` until it resolves.
    /// * **A claim with no resolution** — the printer may already have the
    ///   job. The obligation becomes `UNKNOWN` and **nothing is printed**.
    ///
    /// A claim that *did* resolve is simply applied: the crash happened after
    /// the device answered and before the obligation was updated, and the
    /// answer is durable evidence.
    pub fn recover_receipts(&self) -> Result<ReceiptRecovery, AppError> {
        let now = self.clock.now();
        let mut report = ReceiptRecovery::default();

        let resumable = self.store().transact(move |uow| {
            let mut resumable = Vec::new();
            let mut applied = 0;
            let mut uncertain = 0;
            for mut receipt in
                uow.list_receipts_by_status(ReceiptStatus::Pending, RECOVERY_BATCH)?
            {
                let transaction_id = receipt.transaction_id;
                let evidence = uow.find_hardware_action_for(
                    HardwareActionKind::Print,
                    &transaction_id.to_string(),
                )?;
                let Some(action) = evidence else {
                    resumable.push(transaction_id);
                    continue;
                };
                match action.status {
                    HardwareActionStatus::Pending => {
                        receipt.record_outcome(
                            ReceiptOutcome::Uncertain {
                                error_code: INTERRUPTED.to_owned(),
                                detail: "the runtime stopped while the printer had the job; \
                                         whether paper came out is unknown"
                                    .to_owned(),
                            },
                            now,
                        )?;
                        uncertain += 1;
                    }
                    HardwareActionStatus::Completed => {
                        // The device confirmed before the crash. Stamp the
                        // time it actually printed, not the time we noticed.
                        receipt.record_outcome(
                            ReceiptOutcome::Printed,
                            action.completed_at.unwrap_or(now),
                        )?;
                        applied += 1;
                    }
                    HardwareActionStatus::Failed => {
                        receipt.record_outcome(
                            ReceiptOutcome::NotSent {
                                error_code: action
                                    .error_code
                                    .clone()
                                    .unwrap_or_else(|| INTERRUPTED.to_owned()),
                                detail: action
                                    .error_message
                                    .clone()
                                    .unwrap_or_else(|| "the attempt failed".to_owned()),
                            },
                            action.completed_at.unwrap_or(now),
                        )?;
                        applied += 1;
                    }
                }
                uow.update_receipt(&receipt)?;
            }
            Ok((resumable, applied, uncertain))
        })?;

        let (resumable, applied, uncertain) = resumable;
        report.resolved_from_evidence = applied;
        report.marked_unknown = uncertain;
        report.resumed = resumable.len() as i64;
        if report.marked_unknown > 0 {
            tracing::warn!(
                receipts = report.marked_unknown,
                "receipts were in flight when the runtime stopped; they are UNKNOWN and will \
                 not be printed automatically"
            );
        }
        // Attempted with no transaction open, one pass, no watcher.
        for transaction_id in resumable {
            self.print_first_receipt(transaction_id, now);
        }
        Ok(report)
    }
}

// ----------------------------------------------------------------- internals

fn require_receipt(
    uow: &mut dyn UnitOfWork,
    transaction_id: TransactionId,
) -> Result<TransactionReceipt, AppError> {
    uow.find_receipt(transaction_id)?
        .ok_or_else(|| AppError::ReceiptNotFound {
            transaction_id: transaction_id.to_string(),
        })
}

/// The sale a receipt is for, which must be closed.
pub(crate) fn require_finalized_sale(
    uow: &mut dyn UnitOfWork,
    transaction_id: TransactionId,
) -> Result<RetailTransaction, AppError> {
    let transaction =
        uow.find_transaction(transaction_id)?
            .ok_or_else(|| AppError::TransactionNotFound {
                transaction_id: transaction_id.to_string(),
            })?;
    if transaction.status() != TransactionStatus::Finalized {
        return Err(AppError::Domain(DomainError::InvalidTransition {
            aggregate: "RetailTransaction",
            from: transaction.status().as_str(),
            attempted: "PRINT",
        }));
    }
    Ok(transaction)
}

/// Why a first receipt cannot be attempted again.
fn refuse(receipt: &TransactionReceipt) -> AppError {
    let transaction_id = receipt.transaction_id.to_string();
    match receipt.status {
        ReceiptStatus::Unknown => AppError::ReceiptOutcomeUnknown { transaction_id },
        _ => AppError::ReceiptAlreadyPrinted { transaction_id },
    }
}

/// What a hardware failure says about the paper.
fn failure_outcome(error: &HardwareError) -> ReceiptOutcome {
    let error_code = error.code().to_owned();
    let detail = error.to_string();
    match error.certainty() {
        DeliveryCertainty::NotSent => ReceiptOutcome::NotSent { error_code, detail },
        DeliveryCertainty::MayHaveBeenSent => ReceiptOutcome::Uncertain { error_code, detail },
    }
}
