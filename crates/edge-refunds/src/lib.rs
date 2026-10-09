//! Commercial refunds.
//!
//! A refund is the commercial return of a sale that already closed. It is not
//! a payment reversal: a reversal corrects a financial operation that should
//! never have stayed applied, inside the lifecycle of the original payment,
//! while a refund is a new document that gives goods and money back after the
//! sale is done (ADR 034).
//!
//! Three consequences are modelled separately, because they settle at
//! different moments and can disagree: the **financial** refund, the
//! **inventory** return, and the **fiscal** consequence. A refund can be
//! commercially accepted with its card refund still unresolved (ADR 033).
//!
//! Everything here is pure: no database, no clock, no transport, no provider
//! and no CFDI. Time arrives as a parameter, and the refundable arithmetic is
//! a function of the original sale's materialised economics (ADR 031).

pub mod allocation;
pub mod error;
pub mod fiscal;
pub mod refund;

pub use allocation::{allocate_over_tenders, LineRefundable, RefundAmounts, TenderRefundable};
pub use error::RefundError;
pub use fiscal::{FiscalState, RefundFiscalStatus, TransactionFiscalMetadata};
pub use refund::{
    NewRefund, Refund, RefundAttempt, RefundAttemptStatus, RefundAuthority,
    RefundAuthorizationContext, RefundLine, RefundLineRequest, RefundPayment, RefundPaymentStatus,
    RefundPaymentView, RefundStatus,
};
