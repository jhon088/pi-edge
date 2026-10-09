//! The cash drawer port, and why it is opened.

use serde::{Deserialize, Serialize};

use crate::error::HardwareError;

/// Why the drawer is being opened.
///
/// Every value is a *commercial* cause or an explicit diagnostic — there is no
/// generic "open it". That is what lets the action log tie a physical opening
/// to the thing that justified it (ADR 037).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DrawerOpenReason {
    /// A cash payment was committed: money is going in.
    CashSale,
    /// A cash refund settled: money is coming out.
    CashRefund,
    /// An operator asked, from the diagnostic surface.
    Diagnostic,
}

impl DrawerOpenReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            DrawerOpenReason::CashSale => "CASH_SALE",
            DrawerOpenReason::CashRefund => "CASH_REFUND",
            DrawerOpenReason::Diagnostic => "DIAGNOSTIC",
        }
    }

    pub fn parse(value: &str) -> Result<Self, HardwareError> {
        match value {
            "CASH_SALE" => Ok(DrawerOpenReason::CashSale),
            "CASH_REFUND" => Ok(DrawerOpenReason::CashRefund),
            "DIAGNOSTIC" => Ok(DrawerOpenReason::Diagnostic),
            other => Err(HardwareError::NotConfigured {
                device: "cash drawer",
                detail: format!("`{other}` is not a drawer open reason"),
            }),
        }
    }

    /// Whether this cause comes from a commercial event rather than from an
    /// operator asking.
    ///
    /// It decides idempotency, not behaviour: a commercial cause is keyed by
    /// the thing that caused it and must not repeat, while a diagnostic
    /// request may open the drawer as often as it is asked (ADR 037).
    pub const fn is_commercial(self) -> bool {
        matches!(
            self,
            DrawerOpenReason::CashSale | DrawerOpenReason::CashRefund
        )
    }
}

/// A cash drawer, as the application sees it.
pub trait CashDrawerPort: Send + Sync {
    /// Which driver this is, for the diagnostic view.
    fn driver(&self) -> &'static str;

    /// Open it. The reason travels so an adapter can log it; no adapter may
    /// decide *whether* to open based on it — that decision is the
    /// application's and is already made by the time this is called.
    fn open(&self, reason: DrawerOpenReason) -> Result<(), HardwareError>;

    /// Can we reach it? Must not open it.
    ///
    /// Many drawers cannot be queried at all — they are a solenoid on the end
    /// of a wire. An adapter that cannot answer without opening the drawer
    /// must report what it can verify (that the *printer* is reachable, say)
    /// and nothing more.
    fn probe(&self) -> Result<(), HardwareError>;
}
