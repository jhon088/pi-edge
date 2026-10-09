//! What a sale knows about its payments.
//!
//! The payment domain itself — tenders, the payment and attempt state
//! machines, providers, reversals — lives in `edge-payments` (ADR 016). A
//! `RetailTransaction` must not know any of that: it needs five numbers and
//! two flags to answer "what is still owed", "what comes back", "may the
//! basket change" and "may this sale finalize" (ADR 020).
//!
//! This summary is **derived**, never authoritative: it is recomputed from the
//! stored payments every time a sale is loaded, so the payment tables remain
//! the single source of truth.

use serde::{Deserialize, Serialize};

use crate::money::{Currency, Money};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentSummary {
    /// Sum of what is applied against the sale's debt.
    pub committed_total: Money,
    /// What customers actually handed over, cash overpayment included.
    pub tendered_total: Money,
    /// Cash owed back to the customer.
    pub change_due: Money,
    /// Any payment is in a financially active state, so the basket is frozen
    /// (ADR 019).
    pub blocks_basket: bool,
    /// Any payment has an open financial outcome, so the sale cannot finalize
    /// until it is reconciled (ADR 018).
    pub requires_resolution: bool,
}

impl PaymentSummary {
    /// The state of a sale nobody has paid anything towards.
    pub fn unpaid(currency: Currency) -> Self {
        Self {
            committed_total: Money::zero(currency),
            tendered_total: Money::zero(currency),
            change_due: Money::zero(currency),
            blocks_basket: false,
            requires_resolution: false,
        }
    }

    pub fn is_unpaid(&self) -> bool {
        !self.blocks_basket && !self.committed_total.is_positive()
    }
}
