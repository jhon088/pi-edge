//! What a device is when nothing was configured.
//!
//! **A simulator is never selected implicitly.** A runtime whose printer or
//! drawer was not configured must say so — it must not quietly succeed against
//! a fake device, because then a misconfigured till reports healthy hardware
//! and nobody finds out until a customer is waiting for a receipt (ADR 038).
//!
//! So the absence of configuration gets its own adapter rather than a default
//! one. Every call answers `HARDWARE_NOT_CONFIGURED`, the diagnostic view says
//! `unconfigured` where a driver name would be, and
//! `Hardware { printer: Arc<dyn PrinterPort>, drawer: Arc<dyn CashDrawerPort> }`
//! stays free of `Option`s.

use crate::drawer::{CashDrawerPort, DrawerOpenReason};
use crate::error::HardwareError;
use crate::printer::{PrintDocument, PrinterPort};

/// The driver name reported when nothing was configured. Deliberately not a
/// device: a panel showing `unconfigured` is unambiguous, and one showing
/// `simulator` nobody asked for is a lie.
pub const UNCONFIGURED: &str = "unconfigured";

fn not_configured(device: &'static str) -> HardwareError {
    HardwareError::NotConfigured {
        device,
        detail: "no driver is configured for this installation; \
                 PI.Edge does not fall back to a simulator"
            .to_owned(),
    }
}

/// No printer configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredPrinter;

impl PrinterPort for UnconfiguredPrinter {
    fn driver(&self) -> &'static str {
        UNCONFIGURED
    }

    fn print(&self, _document: &PrintDocument) -> Result<(), HardwareError> {
        Err(not_configured("printer"))
    }

    fn probe(&self) -> Result<(), HardwareError> {
        Err(not_configured("printer"))
    }
}

/// No cash drawer configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredDrawer;

impl CashDrawerPort for UnconfiguredDrawer {
    fn driver(&self) -> &'static str {
        UNCONFIGURED
    }

    fn open(&self, _reason: DrawerOpenReason) -> Result<(), HardwareError> {
        Err(not_configured("cash drawer"))
    }

    fn probe(&self) -> Result<(), HardwareError> {
        Err(not_configured("cash drawer"))
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::PrintKind;
    use uuid::Uuid;

    #[test]
    fn an_unconfigured_printer_never_claims_success() {
        let printer = UnconfiguredPrinter;
        assert_eq!(printer.driver(), "unconfigured");
        let document = PrintDocument::new(Uuid::from_u128(1), PrintKind::TestPage, 48);
        for outcome in [printer.print(&document), printer.probe()] {
            let error = outcome.unwrap_err();
            assert_eq!(error.code(), "HARDWARE_NOT_CONFIGURED");
            // And it says *why*, so nobody reads it as a transient failure.
            assert!(error.to_string().contains("does not fall back"));
        }
    }

    #[test]
    fn an_unconfigured_drawer_never_claims_success() {
        let drawer = UnconfiguredDrawer;
        assert_eq!(drawer.driver(), "unconfigured");
        for outcome in [drawer.open(DrawerOpenReason::CashSale), drawer.probe()] {
            assert_eq!(outcome.unwrap_err().code(), "HARDWARE_NOT_CONFIGURED");
        }
    }

    /// The point of the whole module: nothing here is a simulator, and the
    /// driver name a panel shows can never be mistaken for one.
    #[test]
    fn nothing_unconfigured_is_reported_as_a_simulator() {
        assert_ne!(UnconfiguredPrinter.driver(), "simulator");
        assert_ne!(UnconfiguredDrawer.driver(), "simulator");
    }
}
