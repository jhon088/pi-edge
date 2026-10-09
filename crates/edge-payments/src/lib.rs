//! The payment domain.
//!
//! A [`Payment`] is the commercial intention to cover part of a sale with one
//! tender. A [`PaymentAttempt`] is one concrete interaction with a provider.
//! Keeping them apart is what preserves history across retries, timeouts and
//! reconciliations (ADR 016).
//!
//! Everything here is pure: no database, no clock, no transport, no provider
//! SDK. Time always arrives as a parameter. The application layer owns
//! persistence and the order of operations around a provider call (ADR 020).

pub mod attempt;
pub mod error;
pub mod payment;
pub mod provider;
pub mod settlement;
pub mod simulator;
pub mod tender;

pub use attempt::{AttemptStatus, NewPaymentAttempt, PaymentAttempt};
pub use error::PaymentError;
pub use payment::{NewPayment, Payment, PaymentStatus};
pub use provider::{
    PaymentProviderPort, ProviderOutcome, ProviderQuery, ProviderRequest, ProviderReversal,
    RefundOutcome, RefundRequest, ReversalOutcome,
};
pub use settlement::{settle, split_tender, CashSplit, PaymentView, Settlement};
pub use simulator::{SimulatedOutcome, SimulatorPaymentProvider, SimulatorScript};
pub use tender::{ProviderKind, TenderType};
