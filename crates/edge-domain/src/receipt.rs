//! What a finalized sale owes the customer in paper.
//!
//! One row per sale, created in the **same transaction** that finalizes it, so
//! there is no state in which a sale is commercially closed and nobody knows a
//! receipt is owed (ADR 043).
//!
//! This is deliberately *not* the hardware action log. That log records what
//! was attempted on a device; one sale can produce a failed attempt, a retry
//! and any number of reprints, and still owe exactly one receipt. Mixing the
//! two would make "did this customer get a ticket?" a question about sockets.
//!
//! The states are about **certainty**, which is the only thing that makes the
//! difference actionable:
//!
//! ```text
//! PENDING ──printed──> PRINTED   terminal
//!    │   \
//!    │    `──not sent──> FAILED  retryable: we know no paper came out
//!    │                     │
//!    │                     `──(retry)
//!    `────uncertain────> UNKNOWN terminal: paper may have come out, and no
//!                                amount of retrying will tell us
//! ```

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{BusinessDayId, SessionId, TransactionId};
use crate::time::Timestamp;

const AGGREGATE: &str = "TransactionReceipt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReceiptStatus {
    /// The sale owes a receipt and nothing has resolved yet.
    Pending,
    /// The device confirmed it.
    Printed,
    /// The device certainly did not print: nothing reached it, or it refused
    /// before anything could. Retrying the first receipt is safe.
    Failed,
    /// Paper may have come out. **No automatic retry, ever**: only an operator
    /// looking at the printer can tell, and the answer to "print it again?" is
    /// theirs to give as an explicit reprint (ADR 043).
    Unknown,
}

impl ReceiptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ReceiptStatus::Pending => "PENDING",
            ReceiptStatus::Printed => "PRINTED",
            ReceiptStatus::Failed => "FAILED",
            ReceiptStatus::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "PENDING" => Ok(ReceiptStatus::Pending),
            "PRINTED" => Ok(ReceiptStatus::Printed),
            "FAILED" => Ok(ReceiptStatus::Failed),
            "UNKNOWN" => Ok(ReceiptStatus::Unknown),
            other => Err(DomainError::UnknownEnumValue {
                kind: "ReceiptStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// Whether a further *first* receipt may be attempted.
    ///
    /// Never after `PRINTED` (it came out) and never after `UNKNOWN` (it may
    /// have come out). A reprint is a different act and is always available.
    pub const fn allows_first_attempt(self) -> bool {
        matches!(self, ReceiptStatus::Pending | ReceiptStatus::Failed)
    }
}

/// What one attempt at the first receipt established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptOutcome {
    /// The device finished the job.
    Printed,
    /// Nothing printable reached the device.
    NotSent { error_code: String, detail: String },
    /// Bytes went out, or the process died mid-call. Unresolvable.
    Uncertain { error_code: String, detail: String },
}

/// The durable obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionReceipt {
    pub transaction_id: TransactionId,
    pub status: ReceiptStatus,
    pub attempt_count: i64,
    /// Set once, and only on a confirmed success.
    pub first_printed_at: Option<Timestamp>,
    pub last_attempt_at: Option<Timestamp>,
    pub last_error_code: Option<String>,
    pub last_error_detail: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl TransactionReceipt {
    /// The obligation a sale takes on when it finalizes.
    pub fn owed(transaction_id: TransactionId, now: Timestamp) -> Self {
        Self {
            transaction_id,
            status: ReceiptStatus::Pending,
            attempt_count: 0,
            first_printed_at: None,
            last_attempt_at: None,
            last_error_code: None,
            last_error_detail: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Record what an attempt at the first receipt established.
    ///
    /// Refused once the receipt is `PRINTED` or `UNKNOWN`: both are settled
    /// history, and overwriting either would be claiming knowledge nobody has.
    pub fn record_outcome(
        &mut self,
        outcome: ReceiptOutcome,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        if !self.status.allows_first_attempt() {
            return Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: self.status.as_str(),
                attempted: "RECEIPT_OUTCOME",
            });
        }
        self.attempt_count += 1;
        self.last_attempt_at = Some(now);
        match outcome {
            ReceiptOutcome::Printed => {
                self.status = ReceiptStatus::Printed;
                self.first_printed_at = Some(now);
                self.last_error_code = None;
                self.last_error_detail = None;
            }
            ReceiptOutcome::NotSent { error_code, detail } => {
                self.status = ReceiptStatus::Failed;
                self.last_error_code = Some(error_code);
                self.last_error_detail = Some(detail);
            }
            ReceiptOutcome::Uncertain { error_code, detail } => {
                self.status = ReceiptStatus::Unknown;
                self.last_error_code = Some(error_code);
                self.last_error_detail = Some(detail);
            }
        }
        self.updated_at = now;
        Ok(())
    }
}

/// Which sales a receipt count is about.
///
/// Three, because there are three real consumers and each answers a different
/// question: *what do I still owe on this till I am closing*, *what does this
/// day still owe*, and *what does this installation still owe*. A close must
/// never report another till's or another day's leftovers (ADR 043).
///
/// The scope is always the sale's own identity — `session_id` or
/// `business_day_id`, both captured on `RetailTransaction` — never a window of
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptAttentionScope {
    /// Everything this installation still owes.
    Global,
    /// The sales of one business day.
    BusinessDay(BusinessDayId),
    /// The sales of one register session.
    RegisterSession(SessionId),
}

/// How many sales still need somebody to look at their receipt.
///
/// Reported at close and in the operational history. It never blocks a close:
/// a receipt is an operational obligation, not a financial one (ADR 043).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReceiptAttention {
    pub pending: i64,
    pub failed: i64,
    pub unknown: i64,
}

impl ReceiptAttention {
    pub const fn total(self) -> i64 {
        self.pending + self.failed + self.unknown
    }

    pub const fn is_clear(self) -> bool {
        self.total() == 0
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_datetime(
            chrono::DateTime::from_timestamp(1_760_000_000 + seconds, 0).expect("a valid instant"),
        )
    }

    fn owed() -> TransactionReceipt {
        TransactionReceipt::owed(TransactionId::from_uuid(Uuid::from_u128(1)), ts(0))
    }

    fn failure(code: &str) -> ReceiptOutcome {
        ReceiptOutcome::NotSent {
            error_code: code.to_owned(),
            detail: "nothing is listening".to_owned(),
        }
    }

    fn uncertain(code: &str) -> ReceiptOutcome {
        ReceiptOutcome::Uncertain {
            error_code: code.to_owned(),
            detail: "the socket died mid-write".to_owned(),
        }
    }

    #[test]
    fn a_new_obligation_is_pending_and_has_never_been_attempted() {
        let receipt = owed();
        assert_eq!(receipt.status, ReceiptStatus::Pending);
        assert_eq!(receipt.attempt_count, 0);
        assert!(receipt.first_printed_at.is_none());
        assert!(receipt.status.allows_first_attempt());
    }

    #[test]
    fn a_printed_receipt_is_settled_and_never_attempted_again() {
        let mut receipt = owed();
        receipt
            .record_outcome(ReceiptOutcome::Printed, ts(1))
            .unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Printed);
        assert_eq!(receipt.first_printed_at, Some(ts(1)));
        assert_eq!(receipt.attempt_count, 1);
        assert!(!receipt.status.allows_first_attempt());
        // Even an attempt that would have succeeded is refused: it already
        // came out once.
        assert!(matches!(
            receipt.record_outcome(ReceiptOutcome::Printed, ts(2)),
            Err(DomainError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn a_failure_is_retryable_because_we_know_no_paper_came_out() {
        let mut receipt = owed();
        receipt
            .record_outcome(failure("HARDWARE_UNAVAILABLE"), ts(1))
            .unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Failed);
        assert!(receipt.first_printed_at.is_none());
        assert!(receipt.status.allows_first_attempt());

        // A retry that works clears the error and stamps the time.
        receipt
            .record_outcome(ReceiptOutcome::Printed, ts(2))
            .unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Printed);
        assert_eq!(receipt.attempt_count, 2);
        assert_eq!(receipt.first_printed_at, Some(ts(2)));
        assert!(receipt.last_error_code.is_none());
    }

    #[test]
    fn uncertainty_is_terminal_and_cannot_be_overwritten() {
        let mut receipt = owed();
        receipt
            .record_outcome(uncertain("HARDWARE_IO_ERROR"), ts(1))
            .unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Unknown);
        assert!(
            receipt.first_printed_at.is_none(),
            "nobody knows that it printed"
        );
        assert!(!receipt.status.allows_first_attempt());

        // Not even a later success may claim the first receipt came out: that
        // question is closed, unanswered, on purpose.
        for outcome in [
            ReceiptOutcome::Printed,
            failure("HARDWARE_UNAVAILABLE"),
            uncertain("HARDWARE_TIMEOUT"),
        ] {
            assert!(receipt.record_outcome(outcome, ts(3)).is_err());
        }
        assert_eq!(receipt.status, ReceiptStatus::Unknown);
        assert_eq!(receipt.attempt_count, 1);
    }

    #[test]
    fn attention_counts_everything_but_a_printed_receipt() {
        assert!(ReceiptAttention::default().is_clear());
        let attention = ReceiptAttention {
            pending: 1,
            failed: 2,
            unknown: 3,
        };
        assert_eq!(attention.total(), 6);
        assert!(!attention.is_clear());
        // `PRINTED` is excluded by the query that builds these counts, so
        // there is no field for it to land in.
        assert_eq!(attention.pending + attention.failed + attention.unknown, 6);
    }
}
