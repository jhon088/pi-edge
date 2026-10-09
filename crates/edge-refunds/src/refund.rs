//! The refund document: its lifecycle, its lines, and its financial side.

use edge_domain::prelude::{
    BusinessDayId, Currency, Money, OperatorId, Quantity, RefundId, RegisterId, SessionId, StoreId,
    Timestamp, TransactionId,
};
use edge_payments::TenderType;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::allocation::RefundAmounts;
use crate::error::RefundError;
use crate::fiscal::{FiscalState, RefundFiscalStatus};

/// Where a refund is.
///
/// The split that matters: **commercial acceptance and financial settlement
/// are different facts**, because the goods can be back on the shelf while the
/// card refund is still unresolved (ADR 033).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundStatus {
    /// Authorised, priced and durable; the refundable quantities it names are
    /// reserved against the original sale. Nothing physical or financial has
    /// happened yet, so this is the only state that can still fail.
    Created,
    /// The goods came back and the commercial refund is irrevocable. This is
    /// where the inventory return is written, exactly once.
    Accepted,
    /// A financial refund is in flight or its outcome is unknown. The way out
    /// is reconciling that same attempt — never a second refund (ADR 018).
    FinancialPending,
    /// Commercially accepted and financially settled.
    Completed,
    /// Terminal, and only reachable from `Created`: a refund that already took
    /// the goods back never "fails", it waits for reconciliation.
    Failed,
}

impl RefundStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            RefundStatus::Created => "CREATED",
            RefundStatus::Accepted => "ACCEPTED",
            RefundStatus::FinancialPending => "FINANCIAL_PENDING",
            RefundStatus::Completed => "COMPLETED",
            RefundStatus::Failed => "FAILED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RefundError> {
        match value {
            "CREATED" => Ok(RefundStatus::Created),
            "ACCEPTED" => Ok(RefundStatus::Accepted),
            "FINANCIAL_PENDING" => Ok(RefundStatus::FinancialPending),
            "COMPLETED" => Ok(RefundStatus::Completed),
            "FAILED" => Ok(RefundStatus::Failed),
            other => Err(RefundError::UnknownEnumValue {
                kind: "RefundStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// Whether the refund is finished, either way.
    ///
    /// Only a terminal refund frees the refundable quantity it was holding, and
    /// only a non-terminal one blocks a second concurrent refund of the same
    /// sale (ADR 030).
    pub const fn is_terminal(self) -> bool {
        matches!(self, RefundStatus::Completed | RefundStatus::Failed)
    }

    /// Whether the goods have come back. True from `ACCEPTED` onwards, and
    /// never again false.
    pub const fn is_commercially_accepted(self) -> bool {
        matches!(
            self,
            RefundStatus::Accepted | RefundStatus::FinancialPending | RefundStatus::Completed
        )
    }
}

/// Evidence that the original sale may be refunded at all.
///
/// The domain is handed an **already resolved** authorisation; it never goes
/// looking for one. In production PI.COMMERCE Server validates the original
/// sale and produces this; until that exists, a development provider in the
/// composition root does, and it says so (ADR 030).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundAuthorizationContext {
    pub original_transaction_id: TransactionId,
    pub authority: RefundAuthority,
    /// Whatever the authority used to identify its decision. Opaque here.
    pub authorization_ref: Option<String>,
}

/// Who says the original sale is refundable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundAuthority {
    /// The central operation accepted the original sale. The only authority
    /// that exists: a locally finalized sale is **not** one (ADR 030).
    CentralAccepted,
}

impl RefundAuthority {
    pub const fn as_str(self) -> &'static str {
        match self {
            RefundAuthority::CentralAccepted => "CENTRAL_ACCEPTED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RefundError> {
        match value {
            "CENTRAL_ACCEPTED" => Ok(RefundAuthority::CentralAccepted),
            other => Err(RefundError::UnknownEnumValue {
                kind: "RefundAuthority",
                value: other.to_owned(),
            }),
        }
    }
}

/// One line of a refund, priced from the original line it points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundLine {
    pub refund_line_id: Uuid,
    /// The line of the original sale this gives back. Line numbers are never
    /// reused, so this always points at the same goods.
    pub original_line_number: i64,
    pub product_ref: String,
    pub description: String,
    pub quantity: Quantity,
    /// The original line's quoted unit price, carried for audit. **Not** used
    /// to compute the refund: the materialised amounts are (ADR 031).
    pub original_unit_price: Money,
    pub amounts: RefundAmounts,
}

/// What a refund asks for, before it is priced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundLineRequest {
    pub original_line_number: i64,
    pub quantity: Quantity,
}

/// A commercial refund of a closed sale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refund {
    pub refund_id: RefundId,
    pub original_transaction_id: TransactionId,
    pub store_id: StoreId,
    /// The day and session the *refund* happened in, not the sale's.
    pub business_day_id: BusinessDayId,
    pub session_id: SessionId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub status: RefundStatus,
    pub authority: RefundAuthority,
    pub authorization_ref: Option<String>,
    pub lines: Vec<RefundLine>,
    pub currency: Currency,
    /// The sum of the lines: what this refund owes the customer.
    pub total: RefundAmounts,
    pub fiscal_status: RefundFiscalStatus,
    /// The original sale's fiscal state when the refund was taken, kept so a
    /// later invoice cannot rewrite the history of this decision.
    pub original_fiscal_state: FiscalState,
    pub reason_code: String,
    /// V1 returns everything to sellable stock, always (ADR 033).
    pub restock: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub accepted_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub version: i64,
}

/// Everything needed to open a refund, already validated by the caller.
#[derive(Debug, Clone)]
pub struct NewRefund {
    pub refund_id: RefundId,
    pub original_transaction_id: TransactionId,
    pub store_id: StoreId,
    pub business_day_id: BusinessDayId,
    pub session_id: SessionId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub authority: RefundAuthority,
    pub authorization_ref: Option<String>,
    pub lines: Vec<RefundLine>,
    pub currency: Currency,
    pub fiscal_status: RefundFiscalStatus,
    pub original_fiscal_state: FiscalState,
    pub reason_code: String,
    pub now: Timestamp,
}

impl Refund {
    /// Open a refund. It exists durably before anything is given back.
    pub fn open(new: NewRefund) -> Result<Self, RefundError> {
        if new.reason_code.trim().is_empty() {
            return Err(RefundError::ReasonRequired);
        }
        if new.lines.is_empty() {
            return Err(RefundError::EmptyRefund);
        }
        let mut total = RefundAmounts::zero(new.currency);
        for line in &new.lines {
            total = total.checked_add(&line.amounts)?;
        }
        Ok(Self {
            refund_id: new.refund_id,
            original_transaction_id: new.original_transaction_id,
            store_id: new.store_id,
            business_day_id: new.business_day_id,
            session_id: new.session_id,
            register_id: new.register_id,
            operator_id: new.operator_id,
            status: RefundStatus::Created,
            authority: new.authority,
            authorization_ref: new.authorization_ref,
            lines: new.lines,
            currency: new.currency,
            total,
            fiscal_status: new.fiscal_status,
            original_fiscal_state: new.original_fiscal_state,
            reason_code: new.reason_code.trim().to_owned(),
            restock: true,
            created_at: new.now,
            updated_at: new.now,
            accepted_at: None,
            completed_at: None,
            version: 1,
        })
    }

    /// The goods came back: the commercial refund is now irrevocable.
    ///
    /// This is the moment the inventory return belongs to, and the application
    /// layer writes it in the same database transaction (ADR 033).
    pub fn accept(&mut self, now: Timestamp) -> Result<(), RefundError> {
        if self.status != RefundStatus::Created {
            return Err(RefundError::InvalidState {
                status: self.status.as_str(),
                attempted: "ACCEPT",
            });
        }
        self.status = RefundStatus::Accepted;
        self.accepted_at = Some(now);
        self.updated_at = now;
        self.version += 1;
        Ok(())
    }

    /// A financial refund went out and its outcome is not settled yet.
    pub fn mark_financial_pending(&mut self, now: Timestamp) -> Result<(), RefundError> {
        match self.status {
            RefundStatus::Accepted | RefundStatus::FinancialPending => {
                self.status = RefundStatus::FinancialPending;
                self.updated_at = now;
                self.version += 1;
                Ok(())
            }
            status => Err(RefundError::InvalidState {
                status: status.as_str(),
                attempted: "FINANCIAL_PENDING",
            }),
        }
    }

    /// The financial obligation is settled. Only then is the refund complete.
    pub fn complete(&mut self, now: Timestamp) -> Result<(), RefundError> {
        if !self.status.is_commercially_accepted() || self.status == RefundStatus::Completed {
            return Err(RefundError::InvalidState {
                status: self.status.as_str(),
                attempted: "COMPLETE",
            });
        }
        self.status = RefundStatus::Completed;
        self.completed_at = Some(now);
        self.updated_at = now;
        self.version += 1;
        Ok(())
    }

    /// Abandon a refund that never took the goods back.
    pub fn fail(&mut self, now: Timestamp) -> Result<(), RefundError> {
        if self.status != RefundStatus::Created {
            return Err(RefundError::InvalidState {
                status: self.status.as_str(),
                attempted: "FAIL",
            });
        }
        self.status = RefundStatus::Failed;
        self.completed_at = Some(now);
        self.updated_at = now;
        self.version += 1;
        Ok(())
    }

    /// What this refund owes the customer.
    pub fn amount_due(&self) -> Money {
        self.total.gross
    }

    /// The quantity this refund takes from one original line.
    pub fn quantity_for_line(&self, original_line_number: i64) -> i64 {
        self.lines
            .iter()
            .filter(|line| line.original_line_number == original_line_number)
            .map(|line| line.quantity.thousandths())
            .sum()
    }
}

/// One tender's share of a refund: the financial side of the document.
///
/// Mirrors `Payment` deliberately — requested, settled, status — because the
/// mechanics of giving money back through a provider are the mechanics of
/// taking it, and nothing is gained by inventing new vocabulary. What it is
/// *not* is a `Payment`: it belongs to a refund, not to a sale (ADR 034).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundPayment {
    pub refund_payment_id: Uuid,
    pub refund_id: RefundId,
    /// The original payment this gives back through, so a tender never
    /// returns more than it took.
    pub original_payment_id: edge_domain::prelude::PaymentId,
    pub tender_type: TenderType,
    pub requested_amount: Money,
    /// What actually went back. Zero until the refund settles.
    pub refunded_amount: Money,
    pub status: RefundPaymentStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundPaymentStatus {
    /// Durably recorded, nothing sent. Exists before any provider call
    /// (ADR 020, applied to refunds).
    Created,
    /// Sent and unresolved, or answered `UNKNOWN`. Reconcilable, never
    /// promoted to `FAILED` on its own (ADR 018).
    Pending,
    /// The money went back.
    Refunded,
    /// Terminal: the provider refused and nothing went back.
    Failed,
}

impl RefundPaymentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            RefundPaymentStatus::Created => "CREATED",
            RefundPaymentStatus::Pending => "PENDING",
            RefundPaymentStatus::Refunded => "REFUNDED",
            RefundPaymentStatus::Failed => "FAILED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RefundError> {
        match value {
            "CREATED" => Ok(RefundPaymentStatus::Created),
            "PENDING" => Ok(RefundPaymentStatus::Pending),
            "REFUNDED" => Ok(RefundPaymentStatus::Refunded),
            "FAILED" => Ok(RefundPaymentStatus::Failed),
            other => Err(RefundError::UnknownEnumValue {
                kind: "RefundPaymentStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// Whether the only way forward is asking the provider about this attempt.
    pub const fn requires_reconciliation(self) -> bool {
        matches!(self, RefundPaymentStatus::Pending)
    }
}

/// One concrete interaction with a provider to give money back.
///
/// Append-only, like a payment attempt: a reconciliation adds knowledge, it
/// never rewrites what was sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundAttempt {
    pub attempt_id: Uuid,
    pub refund_payment_id: Uuid,
    /// 1-based, never reused within a refund payment.
    pub attempt_number: i64,
    pub status: RefundAttemptStatus,
    pub requested_amount: Money,
    pub refunded_amount: Option<Money>,
    /// Minted and committed **before** the provider is called, so the provider
    /// can be asked about it later and a provider-side retry is idempotent.
    pub correlation_id: Uuid,
    pub provider_reference: Option<String>,
    pub failure_code: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundAttemptStatus {
    /// Recorded, not yet sent.
    Created,
    /// Sent, no result yet. A process that died here cannot know the outcome.
    Processing,
    /// Asked, and the answer is genuinely unknown.
    Unknown,
    /// The provider confirmed the money went back.
    Refunded,
    /// Terminal: refused, or a query proved nothing went back.
    Failed,
}

impl RefundAttemptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            RefundAttemptStatus::Created => "CREATED",
            RefundAttemptStatus::Processing => "PROCESSING",
            RefundAttemptStatus::Unknown => "UNKNOWN",
            RefundAttemptStatus::Refunded => "REFUNDED",
            RefundAttemptStatus::Failed => "FAILED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RefundError> {
        match value {
            "CREATED" => Ok(RefundAttemptStatus::Created),
            "PROCESSING" => Ok(RefundAttemptStatus::Processing),
            "UNKNOWN" => Ok(RefundAttemptStatus::Unknown),
            "REFUNDED" => Ok(RefundAttemptStatus::Refunded),
            "FAILED" => Ok(RefundAttemptStatus::Failed),
            other => Err(RefundError::UnknownEnumValue {
                kind: "RefundAttemptStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// `PROCESSING` and `UNKNOWN` both mean "we asked and we do not know".
    pub const fn is_unresolved(self) -> bool {
        matches!(
            self,
            RefundAttemptStatus::Processing | RefundAttemptStatus::Unknown
        )
    }
}

/// A refund payment with its attempt history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundPaymentView {
    pub payment: RefundPayment,
    pub attempts: Vec<RefundAttempt>,
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::PaymentId;

    fn now() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-09T15:00:00Z").unwrap()
    }

    fn mxn(minor: i64) -> Money {
        Money::from_minor(minor, Currency::Mxn)
    }

    fn line(quantity: i64, gross: i64) -> RefundLine {
        RefundLine {
            refund_line_id: Uuid::from_u128(1),
            original_line_number: 1,
            product_ref: "SKU-1".to_owned(),
            description: "Producto".to_owned(),
            quantity: Quantity::from_units(quantity).unwrap(),
            original_unit_price: mxn(10_000),
            amounts: RefundAmounts {
                extended: mxn(gross),
                discount: mxn(0),
                net: mxn(gross),
                tax: mxn(0),
                gross: mxn(gross),
            },
        }
    }

    fn new_refund(lines: Vec<RefundLine>) -> NewRefund {
        NewRefund {
            refund_id: RefundId::from_uuid(Uuid::from_u128(9)),
            original_transaction_id: TransactionId::from_uuid(Uuid::from_u128(8)),
            store_id: StoreId::new("STORE-DEMO").unwrap(),
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(7)),
            session_id: SessionId::from_uuid(Uuid::from_u128(6)),
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            authority: RefundAuthority::CentralAccepted,
            authorization_ref: Some("AUTH-1".to_owned()),
            lines,
            currency: Currency::Mxn,
            fiscal_status: RefundFiscalStatus::None,
            original_fiscal_state: FiscalState::NotInvoiced,
            reason_code: "CUSTOMER_RETURN".to_owned(),
            now: now(),
        }
    }

    fn opened() -> Refund {
        Refund::open(new_refund(vec![line(2, 20_000)])).unwrap()
    }

    #[test]
    fn a_refund_totals_its_lines_when_it_opens() {
        let refund = Refund::open(new_refund(vec![line(2, 20_000), line(1, 5_000)])).unwrap();
        assert_eq!(refund.status, RefundStatus::Created);
        assert_eq!(refund.amount_due().minor(), 25_000);
        assert_eq!(refund.quantity_for_line(1), 3_000);
        // V1 returns everything to sellable stock, and says so (ADR 033).
        assert!(refund.restock);
    }

    #[test]
    fn a_refund_needs_a_reason_and_at_least_one_line() {
        let mut blank = new_refund(vec![line(1, 1_000)]);
        blank.reason_code = "   ".to_owned();
        assert_eq!(
            Refund::open(blank).unwrap_err().code(),
            "REFUND_REASON_REQUIRED"
        );
        assert_eq!(
            Refund::open(new_refund(Vec::new())).unwrap_err().code(),
            "REFUND_INVALID_LINES"
        );
    }

    #[test]
    fn a_created_refund_can_still_fail_and_an_accepted_one_cannot() {
        let mut refund = opened();
        assert!(!refund.status.is_commercially_accepted());
        refund.fail(now()).unwrap();
        assert_eq!(refund.status, RefundStatus::Failed);
        assert!(refund.status.is_terminal());

        // Once the goods are back, the commercial refund does not go away.
        let mut accepted = opened();
        accepted.accept(now()).unwrap();
        assert_eq!(
            accepted.fail(now()).unwrap_err().code(),
            "REFUND_INVALID_STATE"
        );
    }

    #[test]
    fn acceptance_happens_once() {
        let mut refund = opened();
        refund.accept(now()).unwrap();
        assert_eq!(refund.status, RefundStatus::Accepted);
        assert_eq!(refund.accepted_at, Some(now()));
        // The second call is refused, which is what keeps the inventory return
        // to exactly one movement (ADR 033).
        assert_eq!(
            refund.accept(now()).unwrap_err().code(),
            "REFUND_INVALID_STATE"
        );
    }

    #[test]
    fn an_unresolved_financial_refund_keeps_the_goods_returned() {
        let mut refund = opened();
        refund.accept(now()).unwrap();
        refund.mark_financial_pending(now()).unwrap();

        assert_eq!(refund.status, RefundStatus::FinancialPending);
        // Still accepted: the merchandise is in the store whatever the card
        // says, and no state takes that back.
        assert!(refund.status.is_commercially_accepted());
        assert!(!refund.status.is_terminal());
        assert_eq!(refund.accepted_at, Some(now()));
    }

    #[test]
    fn a_refund_completes_only_after_it_was_accepted() {
        let mut never_accepted = opened();
        assert_eq!(
            never_accepted.complete(now()).unwrap_err().code(),
            "REFUND_INVALID_STATE"
        );

        let mut refund = opened();
        refund.accept(now()).unwrap();
        refund.complete(now()).unwrap();
        assert_eq!(refund.status, RefundStatus::Completed);
        assert!(refund.status.is_terminal());
        // And it does not complete twice.
        assert_eq!(
            refund.complete(now()).unwrap_err().code(),
            "REFUND_INVALID_STATE"
        );
    }

    #[test]
    fn a_pending_financial_refund_completes_after_reconciliation() {
        let mut refund = opened();
        refund.accept(now()).unwrap();
        refund.mark_financial_pending(now()).unwrap();
        refund.complete(now()).unwrap();
        assert_eq!(refund.status, RefundStatus::Completed);
    }

    #[test]
    fn statuses_round_trip_through_their_wire_form() {
        for status in [
            RefundStatus::Created,
            RefundStatus::Accepted,
            RefundStatus::FinancialPending,
            RefundStatus::Completed,
            RefundStatus::Failed,
        ] {
            assert_eq!(RefundStatus::parse(status.as_str()).unwrap(), status);
        }
        for status in [
            RefundPaymentStatus::Created,
            RefundPaymentStatus::Pending,
            RefundPaymentStatus::Refunded,
            RefundPaymentStatus::Failed,
        ] {
            assert_eq!(RefundPaymentStatus::parse(status.as_str()).unwrap(), status);
        }
        for status in [
            RefundAttemptStatus::Created,
            RefundAttemptStatus::Processing,
            RefundAttemptStatus::Unknown,
            RefundAttemptStatus::Refunded,
            RefundAttemptStatus::Failed,
        ] {
            assert_eq!(RefundAttemptStatus::parse(status.as_str()).unwrap(), status);
        }
        assert_eq!(
            RefundAuthority::parse("CENTRAL_ACCEPTED").unwrap(),
            RefundAuthority::CentralAccepted
        );
        assert!(RefundStatus::parse("FINANCIAL_COMPLETED").is_err());
    }

    #[test]
    fn processing_and_unknown_are_both_unresolved() {
        assert!(RefundAttemptStatus::Processing.is_unresolved());
        assert!(RefundAttemptStatus::Unknown.is_unresolved());
        assert!(!RefundAttemptStatus::Refunded.is_unresolved());
        assert!(!RefundAttemptStatus::Failed.is_unresolved());
        assert!(RefundPaymentStatus::Pending.requires_reconciliation());
        assert!(!RefundPaymentStatus::Refunded.requires_reconciliation());
    }

    #[test]
    fn a_refund_payment_points_at_the_tender_it_gives_back_through() {
        let payment = RefundPayment {
            refund_payment_id: Uuid::from_u128(5),
            refund_id: RefundId::from_uuid(Uuid::from_u128(9)),
            original_payment_id: PaymentId::from_uuid(Uuid::from_u128(4)),
            tender_type: TenderType::Cash,
            requested_amount: mxn(20_000),
            refunded_amount: mxn(0),
            status: RefundPaymentStatus::Created,
            created_at: now(),
            updated_at: now(),
        };
        // The link to the original payment is what caps a tender at what it
        // actually took (ADR 031).
        assert_eq!(
            payment.original_payment_id,
            PaymentId::from_uuid(Uuid::from_u128(4))
        );
        assert_eq!(payment.tender_type, TenderType::Cash);
    }
}
