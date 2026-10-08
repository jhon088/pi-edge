//! Hardware ports.
//!
//! The core only ever talks to these traits. Concrete adapters (iMin printer,
//! Android host, Windows OPOS, test doubles) implement them outside the core
//! and are injected at the composition root, so a missing or broken device can
//! never reach into transaction state.
//!
//! No adapter is implemented in this milestone on purpose.

use edge_domain::prelude::{Money, TransactionId};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HardwareError {
    #[error("{device} is not connected")]
    NotConnected { device: &'static str },
    #[error("{device} is busy")]
    Busy { device: &'static str },
    #[error("{device} reported a failure: {detail}")]
    DeviceFailure {
        device: &'static str,
        detail: String,
    },
    #[error("{device} does not support this operation")]
    Unsupported { device: &'static str },
    #[error("operation on {device} timed out after {millis} ms")]
    Timeout { device: &'static str, millis: u64 },
}

pub type HardwareResult<T> = Result<T, HardwareError>;

/// Rendered receipt handed to a printer. The core renders; the adapter prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptDocument {
    pub transaction_id: TransactionId,
    /// Plain-text lines, already laid out for the target paper width.
    pub lines: Vec<String>,
    pub open_drawer_after_print: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterStatus {
    Ready,
    PaperLow,
    PaperOut,
    CoverOpen,
    Offline,
}

pub trait PrinterPort {
    fn status(&self) -> HardwareResult<PrinterStatus>;
    fn print_receipt(&self, document: &ReceiptDocument) -> HardwareResult<()>;
    fn cut(&self) -> HardwareResult<()>;
}

/// A scanned code. Scanners are event sources, so the port is a pull-based
/// queue the core drains; the adapter owns any OS callback plumbing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedCode {
    pub symbology: String,
    pub data: String,
}

pub trait ScannerPort {
    /// Next scan, if one is queued. Never blocks the sale loop.
    fn try_next_scan(&self) -> HardwareResult<Option<ScannedCode>>;
}

pub trait CashDrawerPort {
    fn open(&self) -> HardwareResult<()>;
    /// `None` when the device cannot report drawer position.
    fn is_open(&self) -> HardwareResult<Option<bool>>;
}

/// What the customer-facing display should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomerDisplayContent {
    Idle,
    Line { description: String, amount: Money },
    Total { amount: Money },
    ChangeDue { amount: Money },
    Message { text: String },
}

pub trait CustomerDisplayPort {
    fn show(&self, content: &CustomerDisplayContent) -> HardwareResult<()>;
    fn clear(&self) -> HardwareResult<()>;
}

/// Request for an external payment terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPaymentRequest {
    pub transaction_id: TransactionId,
    pub amount: Money,
    /// Idempotency key: the adapter must not create two charges for it.
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalPaymentOutcome {
    Approved {
        authorization_code: String,
        /// Provider-specific id, stored for reconciliation.
        provider_reference: String,
    },
    Declined {
        reason: String,
    },
    /// Outcome unknown (cable pulled, timeout): the core must reconcile before
    /// deciding the payment state. Never treated as approved.
    Unknown {
        detail: String,
    },
}

pub trait PaymentTerminalPort {
    fn charge(&self, request: &TerminalPaymentRequest) -> HardwareResult<TerminalPaymentOutcome>;
    fn cancel(&self, reference: &str) -> HardwareResult<()>;
    /// Re-read the outcome of a previous request by its idempotency key.
    fn lookup(&self, reference: &str) -> HardwareResult<Option<TerminalPaymentOutcome>>;
}
