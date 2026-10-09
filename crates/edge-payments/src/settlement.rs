//! Settlement: what a set of payments means for a sale.
//!
//! The four operational questions — is the money applied, is the basket
//! locked, can the sale finalize, does something need reconciling — are pure
//! functions over the stored payment and attempt states. Nothing here infers
//! anything from a clock or from the absence of a record.

use edge_domain::prelude::{Money, PaymentSummary};

use crate::attempt::PaymentAttempt;
use crate::error::PaymentError;
use crate::payment::Payment;
use crate::tender::TenderType;

/// One payment together with its attempt history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentView {
    pub payment: Payment,
    /// Ascending by `attempt_number`. Empty for cash (ADR 016).
    pub attempts: Vec<PaymentAttempt>,
}

impl PaymentView {
    pub fn cash(payment: Payment) -> Self {
        Self {
            payment,
            attempts: Vec::new(),
        }
    }

    /// The attempt that currently speaks for this payment: the highest
    /// numbered one.
    pub fn latest_attempt(&self) -> Option<&PaymentAttempt> {
        self.attempts
            .iter()
            .max_by_key(|attempt| attempt.attempt_number)
    }

    /// Whether any attempt of this payment has an open financial outcome.
    pub fn has_unresolved_attempt(&self) -> bool {
        self.attempts
            .iter()
            .any(|attempt| attempt.status.is_unresolved())
    }

    pub fn requires_reconciliation(&self) -> bool {
        self.attempts
            .iter()
            .any(|attempt| attempt.status.requires_reconciliation())
    }

    /// `CREATED` and `IN_PROGRESS` payments are unresolved by themselves: a
    /// record written before a provider call may already have reached it.
    pub fn is_unresolved(&self) -> bool {
        use crate::payment::PaymentStatus;
        match self.payment.status {
            PaymentStatus::Created => true,
            PaymentStatus::InProgress => true,
            PaymentStatus::Committed | PaymentStatus::Failed | PaymentStatus::Reversed => {
                self.has_unresolved_attempt()
            }
        }
    }

    pub fn blocks_basket(&self) -> bool {
        self.payment.status.blocks_basket()
            || self
                .attempts
                .iter()
                .any(|attempt| attempt.status.blocks_basket())
    }
}

/// What the whole payment set means for one sale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// The sale's official grand total.
    pub amount_due: Money,
    /// Sum of what is applied against the debt.
    pub committed_total: Money,
    /// `amount_due − committed_total`, never negative.
    pub remaining_balance: Money,
    /// What customers actually handed over, cash overpayment included.
    pub tendered_total: Money,
    /// Cash owed back.
    pub change_due: Money,
    /// Any payment or attempt in a financially active state (ADR 019).
    pub blocks_basket: bool,
    /// Any payment with an open financial outcome (ADR 018).
    pub requires_resolution: bool,
    /// Any state whose way out is an explicit provider query.
    pub requires_reconciliation: bool,
}

impl Settlement {
    /// A sale may finalize only when the balance is covered and no payment is
    /// unresolved (ADR 018, §22 of the milestone brief).
    pub fn can_finalize(&self) -> bool {
        !self.remaining_balance.is_positive() && !self.requires_resolution
    }

    /// The projection the transaction aggregate keeps.
    pub fn summary(&self) -> PaymentSummary {
        PaymentSummary {
            committed_total: self.committed_total,
            tendered_total: self.tendered_total,
            change_due: self.change_due,
            blocks_basket: self.blocks_basket,
            requires_resolution: self.requires_resolution,
        }
    }
}

/// Settle a sale's payments against its amount due.
pub fn settle(payments: &[PaymentView], amount_due: Money) -> Result<Settlement, PaymentError> {
    let currency = amount_due.currency();
    let mut committed_total = Money::zero(currency);
    let mut tendered_total = Money::zero(currency);
    let mut change_due = Money::zero(currency);
    let mut blocks_basket = false;
    let mut requires_resolution = false;
    let mut requires_reconciliation = false;

    for view in payments {
        if view.payment.status.is_applied() {
            committed_total = committed_total.checked_add(view.payment.committed_amount)?;
            // Only what was actually handed over counts as tendered, which is
            // why a failed or reversed payment contributes nothing.
            tendered_total = tendered_total.checked_add(view.payment.requested_amount)?;
            change_due = change_due.checked_add(view.payment.change_due()?)?;
        }
        blocks_basket |= view.blocks_basket();
        requires_resolution |= view.is_unresolved();
        requires_reconciliation |= view.requires_reconciliation();
    }

    let remaining_balance = amount_due
        .checked_sub(committed_total)?
        .clamp_non_negative();

    Ok(Settlement {
        amount_due,
        committed_total,
        remaining_balance,
        tendered_total,
        change_due,
        blocks_basket,
        requires_resolution,
        requires_reconciliation,
    })
}

/// How much of a cash tender covers the debt, and how much comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CashSplit {
    pub committed: Money,
    pub change: Money,
}

/// Decide what a tender may cover.
///
/// Cash may exceed the remaining balance; the excess is change. A card may
/// not: a provider authorization larger than the debt would have to be
/// partially reversed to settle the sale, which is a worse outcome than
/// refusing the request (`PAYMENT_EXCEEDS_BALANCE`).
pub fn split_tender(
    tender: TenderType,
    requested: Money,
    remaining_balance: Money,
) -> Result<CashSplit, PaymentError> {
    if !requested.is_positive() {
        return Err(PaymentError::NonPositiveAmount {
            minor: requested.minor(),
        });
    }
    if !remaining_balance.is_positive() {
        return Err(PaymentError::NothingDue);
    }
    if requested.minor() > remaining_balance.minor() {
        if !tender.may_overpay() {
            return Err(PaymentError::ExceedsBalance {
                tender: tender.as_str(),
                requested_minor: requested.minor(),
                remaining_minor: remaining_balance.minor(),
            });
        }
        return Ok(CashSplit {
            committed: remaining_balance,
            change: requested.checked_sub(remaining_balance)?,
        });
    }
    Ok(CashSplit {
        committed: requested,
        change: Money::zero(requested.currency()),
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::{Currency, PaymentId, Timestamp, TransactionId};
    use uuid::Uuid;

    use crate::attempt::{AttemptStatus, NewPaymentAttempt};
    use crate::payment::{NewPayment, PaymentStatus};
    use crate::tender::ProviderKind;

    const MXN: Currency = Currency::Mxn;

    fn money(minor: i64) -> Money {
        Money::from_minor(minor, MXN)
    }

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T15:00:00Z").unwrap()
    }

    fn payment(tender: TenderType, requested: i64, seed: u128) -> Payment {
        Payment::open(NewPayment {
            payment_id: PaymentId::from_uuid(Uuid::from_u128(seed)),
            transaction_id: TransactionId::from_uuid(Uuid::from_u128(1)),
            tender_type: tender,
            provider_kind: tender.uses_provider().then_some(ProviderKind::Simulator),
            requested_amount: money(requested),
            now: ts(),
        })
        .unwrap()
    }

    fn attempt(payment: &Payment, number: i64) -> PaymentAttempt {
        PaymentAttempt::open(NewPaymentAttempt {
            attempt_id: Uuid::from_u128(900 + number as u128),
            payment_id: payment.payment_id,
            attempt_number: number,
            requested_amount: payment.requested_amount,
            provider_kind: ProviderKind::Simulator,
            correlation_id: Uuid::from_u128(800 + number as u128),
            now: ts(),
        })
        .unwrap()
    }

    #[test]
    fn an_unpaid_sale_owes_everything() {
        let settlement = settle(&[], money(11_600)).unwrap();
        assert_eq!(settlement.committed_total, money(0));
        assert_eq!(settlement.remaining_balance, money(11_600));
        assert!(!settlement.blocks_basket);
        assert!(!settlement.can_finalize());
    }

    #[test]
    fn exact_cash_covers_the_sale_with_no_change() {
        let mut cash = payment(TenderType::Cash, 11_600, 2);
        cash.commit(money(11_600), ts()).unwrap();
        let settlement = settle(&[PaymentView::cash(cash)], money(11_600)).unwrap();
        assert_eq!(settlement.committed_total, money(11_600));
        assert_eq!(settlement.remaining_balance, money(0));
        assert_eq!(settlement.change_due, money(0));
        assert!(settlement.can_finalize());
        // A committed payment still freezes the basket.
        assert!(settlement.blocks_basket);
    }

    #[test]
    fn cash_overpayment_commits_the_debt_and_returns_the_rest() {
        let split = split_tender(TenderType::Cash, money(20_000), money(11_600)).unwrap();
        assert_eq!(split.committed, money(11_600));
        assert_eq!(split.change, money(8_400));

        let mut cash = payment(TenderType::Cash, 20_000, 2);
        cash.commit(split.committed, ts()).unwrap();
        let settlement = settle(&[PaymentView::cash(cash)], money(11_600)).unwrap();
        // The committed amount is the debt, not the note the customer handed
        // over: overpayment must not distort what the sale collected.
        assert_eq!(settlement.committed_total, money(11_600));
        assert_eq!(settlement.tendered_total, money(20_000));
        assert_eq!(settlement.change_due, money(8_400));
        assert_eq!(settlement.remaining_balance, money(0));
        assert!(settlement.can_finalize());
    }

    #[test]
    fn a_card_may_not_exceed_the_remaining_balance() {
        let error = split_tender(TenderType::Card, money(20_000), money(11_600)).unwrap_err();
        assert_eq!(error.code(), "PAYMENT_EXCEEDS_BALANCE");
        // Exactly the balance is fine.
        assert_eq!(
            split_tender(TenderType::Card, money(11_600), money(11_600))
                .unwrap()
                .committed,
            money(11_600)
        );
    }

    #[test]
    fn nothing_can_be_tendered_against_a_covered_balance() {
        assert_eq!(
            split_tender(TenderType::Cash, money(100), money(0))
                .unwrap_err()
                .code(),
            "PAYMENT_INVALID_AMOUNT"
        );
    }

    #[test]
    fn mixed_tender_accumulates_to_the_total() {
        let mut cash = payment(TenderType::Cash, 30_000, 2);
        cash.commit(money(30_000), ts()).unwrap();
        let mut card = payment(TenderType::Card, 70_000, 3);
        let mut card_attempt = attempt(&card, 1);
        card_attempt.mark_processing(ts()).unwrap();
        card_attempt
            .mark_authorized(money(70_000), Some("SIM-1".to_owned()), ts())
            .unwrap();
        card.mark_authorized(money(70_000), ts()).unwrap();
        card.commit(money(70_000), ts()).unwrap();

        let settlement = settle(
            &[
                PaymentView::cash(cash),
                PaymentView {
                    payment: card,
                    attempts: vec![card_attempt],
                },
            ],
            money(100_000),
        )
        .unwrap();
        assert_eq!(settlement.committed_total, money(100_000));
        assert_eq!(settlement.remaining_balance, money(0));
        assert_eq!(settlement.change_due, money(0));
        assert!(settlement.can_finalize());
    }

    #[test]
    fn a_partial_payment_leaves_the_balance_and_blocks_finalization() {
        let mut cash = payment(TenderType::Cash, 30_000, 2);
        cash.commit(money(30_000), ts()).unwrap();
        let settlement = settle(&[PaymentView::cash(cash)], money(100_000)).unwrap();
        assert_eq!(settlement.remaining_balance, money(70_000));
        assert!(!settlement.can_finalize());
        assert!(settlement.blocks_basket);
        assert!(!settlement.requires_resolution);
    }

    #[test]
    fn an_unknown_attempt_blocks_everything_until_it_is_resolved() {
        let card = payment(TenderType::Card, 70_000, 3);
        let mut unknown = attempt(&card, 1);
        unknown.mark_processing(ts()).unwrap();
        unknown.mark_unknown(ts()).unwrap();
        let view = PaymentView {
            payment: card,
            attempts: vec![unknown],
        };
        let settlement = settle(&[view], money(70_000)).unwrap();

        // Nothing is applied, so the balance stands...
        assert_eq!(settlement.committed_total, money(0));
        assert_eq!(settlement.remaining_balance, money(70_000));
        // ...but the sale may not finalize and the basket stays frozen even if
        // another tender covered the balance, because the money state of this
        // payment is genuinely unknown.
        assert!(!settlement.can_finalize());
        assert!(settlement.requires_resolution);
        assert!(settlement.requires_reconciliation);
        assert!(settlement.blocks_basket);
    }

    #[test]
    fn a_processing_attempt_is_as_unresolved_as_an_unknown_one() {
        let card = payment(TenderType::Card, 70_000, 3);
        let mut processing = attempt(&card, 1);
        processing.mark_processing(ts()).unwrap();
        let settlement = settle(
            &[PaymentView {
                payment: card,
                attempts: vec![processing],
            }],
            money(70_000),
        )
        .unwrap();
        assert!(settlement.requires_resolution);
        assert!(settlement.requires_reconciliation);
        assert!(!settlement.can_finalize());
    }

    #[test]
    fn a_covered_sale_with_an_unknown_second_payment_still_cannot_finalize() {
        let mut cash = payment(TenderType::Cash, 70_000, 2);
        cash.commit(money(70_000), ts()).unwrap();
        let card = payment(TenderType::Card, 70_000, 3);
        let mut unknown = attempt(&card, 1);
        unknown.mark_processing(ts()).unwrap();
        unknown.mark_unknown(ts()).unwrap();

        let settlement = settle(
            &[
                PaymentView::cash(cash),
                PaymentView {
                    payment: card,
                    attempts: vec![unknown],
                },
            ],
            money(70_000),
        )
        .unwrap();
        assert_eq!(settlement.remaining_balance, money(0));
        assert!(!settlement.can_finalize(), "the unknown charge may exist");
    }

    #[test]
    fn failed_and_reversed_payments_free_the_basket_again() {
        let mut declined = payment(TenderType::Card, 70_000, 3);
        let mut attempt_one = attempt(&declined, 1);
        attempt_one.mark_processing(ts()).unwrap();
        attempt_one
            .mark_failed(Some("DECLINED".to_owned()), ts())
            .unwrap();
        declined.mark_failed(ts()).unwrap();

        let mut reversed = payment(TenderType::Card, 70_000, 4);
        let mut attempt_two = attempt(&reversed, 1);
        attempt_two.mark_processing(ts()).unwrap();
        attempt_two
            .mark_authorized(money(70_000), None, ts())
            .unwrap();
        attempt_two.mark_reversal_pending(ts()).unwrap();
        attempt_two.mark_reversed(ts()).unwrap();
        reversed.mark_authorized(money(70_000), ts()).unwrap();
        reversed.mark_reversed(ts()).unwrap();

        let settlement = settle(
            &[
                PaymentView {
                    payment: declined,
                    attempts: vec![attempt_one],
                },
                PaymentView {
                    payment: reversed,
                    attempts: vec![attempt_two],
                },
            ],
            money(70_000),
        )
        .unwrap();
        assert_eq!(settlement.committed_total, money(0));
        assert_eq!(settlement.tendered_total, money(0));
        assert_eq!(settlement.remaining_balance, money(70_000));
        assert!(!settlement.blocks_basket, "nothing is active any more");
        assert!(!settlement.requires_resolution);
    }

    #[test]
    fn a_reversal_in_flight_blocks_finalization() {
        let mut card = payment(TenderType::Card, 70_000, 3);
        let mut pending = attempt(&card, 1);
        pending.mark_processing(ts()).unwrap();
        pending.mark_authorized(money(70_000), None, ts()).unwrap();
        pending.mark_reversal_pending(ts()).unwrap();
        card.mark_authorized(money(70_000), ts()).unwrap();
        card.commit(money(70_000), ts()).unwrap();

        let settlement = settle(
            &[PaymentView {
                payment: card,
                attempts: vec![pending],
            }],
            money(70_000),
        )
        .unwrap();
        assert_eq!(settlement.remaining_balance, money(0));
        assert!(!settlement.can_finalize());
        assert!(settlement.requires_reconciliation);
    }

    #[test]
    fn attempt_history_is_kept_across_a_retry() {
        let card = payment(TenderType::Card, 70_000, 3);
        let mut first = attempt(&card, 1);
        first.mark_processing(ts()).unwrap();
        first
            .mark_failed(Some("DECLINED".to_owned()), ts())
            .unwrap();
        let mut second = attempt(&card, 2);
        second.mark_processing(ts()).unwrap();
        second
            .mark_authorized(money(70_000), Some("SIM-2".to_owned()), ts())
            .unwrap();

        let view = PaymentView {
            payment: card,
            attempts: vec![first, second],
        };
        // The declined attempt is still there, and the latest one speaks.
        assert_eq!(view.attempts.len(), 2);
        assert_eq!(view.attempts[0].status, AttemptStatus::Failed);
        assert_eq!(
            view.latest_attempt().map(|attempt| attempt.status),
            Some(AttemptStatus::Authorized)
        );
    }

    #[test]
    fn a_cash_payment_has_no_attempts() {
        let mut cash = payment(TenderType::Cash, 11_600, 2);
        assert_eq!(cash.status, PaymentStatus::Created);
        assert!(cash.provider_kind.is_none());
        cash.commit(money(11_600), ts()).unwrap();
        let view = PaymentView::cash(cash);
        assert!(view.attempts.is_empty());
        assert!(!view.is_unresolved());
    }
}
