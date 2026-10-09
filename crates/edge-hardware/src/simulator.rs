//! Simulators, so the whole POS can be tested and demonstrated with nothing
//! plugged in.
//!
//! One per port, scriptable to the four outcomes that actually change
//! behaviour: it works, it is not there, it times out, the transport breaks.
//! Not one per brand — a simulator of a model nobody owns teaches nothing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::drawer::{CashDrawerPort, DrawerOpenReason};
use crate::error::HardwareError;
use crate::printer::{PrintDocument, PrinterPort};

/// What a simulated device should answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SimulatedHardware {
    #[default]
    Works,
    Unavailable,
    Timeout,
    IoError,
}

impl SimulatedHardware {
    pub fn parse(value: &str) -> Result<Self, HardwareError> {
        match value {
            "works" | "ok" | "available" => Ok(SimulatedHardware::Works),
            "unavailable" | "down" => Ok(SimulatedHardware::Unavailable),
            "timeout" => Ok(SimulatedHardware::Timeout),
            "io" | "io_error" => Ok(SimulatedHardware::IoError),
            other => Err(HardwareError::NotConfigured {
                device: "simulator",
                detail: format!("`{other}` is not a simulated outcome"),
            }),
        }
    }

    fn outcome(self, device: &'static str) -> Result<(), HardwareError> {
        match self {
            SimulatedHardware::Works => Ok(()),
            SimulatedHardware::Unavailable => Err(HardwareError::Unavailable {
                device,
                detail: "simulated: nothing is listening".to_owned(),
            }),
            SimulatedHardware::Timeout => Err(HardwareError::Timeout {
                device,
                elapsed_ms: 2_000,
            }),
            SimulatedHardware::IoError => Err(HardwareError::Io {
                device,
                detail: "simulated: the transport broke mid-write".to_owned(),
            }),
        }
    }
}

/// A printer that records what it was asked to print.
#[derive(Debug, Default)]
pub struct SimulatorPrinter {
    outcome: SimulatedHardware,
    jobs: Mutex<Vec<PrintDocument>>,
    copies: AtomicUsize,
}

impl SimulatorPrinter {
    pub fn working() -> Self {
        Self::default()
    }

    pub fn answering(outcome: SimulatedHardware) -> Self {
        Self {
            outcome,
            ..Self::default()
        }
    }

    /// How many documents it accepted.
    pub fn job_count(&self) -> usize {
        self.jobs.lock().map(|jobs| jobs.len()).unwrap_or(0)
    }

    /// How many physical copies came out, which is what paper is spent on.
    pub fn copy_count(&self) -> usize {
        self.copies.load(Ordering::SeqCst)
    }

    /// The documents it accepted, for asserting on content.
    pub fn jobs(&self) -> Vec<PrintDocument> {
        self.jobs
            .lock()
            .map(|jobs| jobs.clone())
            .unwrap_or_default()
    }
}

impl PrinterPort for SimulatorPrinter {
    fn driver(&self) -> &'static str {
        "simulator"
    }

    fn print(&self, document: &PrintDocument) -> Result<(), HardwareError> {
        // A failing printer records nothing: no paper came out.
        self.outcome.outcome("printer")?;
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.push(document.clone());
        }
        self.copies
            .fetch_add(usize::from(document.copies.max(1)), Ordering::SeqCst);
        Ok(())
    }

    fn probe(&self) -> Result<(), HardwareError> {
        self.outcome.outcome("printer")
    }
}

/// A drawer that counts how many times it physically opened.
///
/// The count is the whole point: every rule about replays, reprints and card
/// sales is a statement about this number (ADR 037).
#[derive(Debug, Default)]
pub struct SimulatorDrawer {
    outcome: SimulatedHardware,
    opens: AtomicUsize,
    reasons: Mutex<Vec<DrawerOpenReason>>,
}

impl SimulatorDrawer {
    pub fn working() -> Self {
        Self::default()
    }

    pub fn answering(outcome: SimulatedHardware) -> Self {
        Self {
            outcome,
            ..Self::default()
        }
    }

    pub fn open_count(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }

    pub fn reasons(&self) -> Vec<DrawerOpenReason> {
        self.reasons
            .lock()
            .map(|reasons| reasons.clone())
            .unwrap_or_default()
    }
}

impl CashDrawerPort for SimulatorDrawer {
    fn driver(&self) -> &'static str {
        "simulator"
    }

    fn open(&self, reason: DrawerOpenReason) -> Result<(), HardwareError> {
        // A drawer that failed to open did not open: the count only ever
        // reflects physical openings.
        self.outcome.outcome("cash drawer")?;
        self.opens.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut reasons) = self.reasons.lock() {
            reasons.push(reason);
        }
        Ok(())
    }

    fn probe(&self) -> Result<(), HardwareError> {
        self.outcome.outcome("cash drawer")
    }
}

/// The iMin terminal's built-in drawer.
///
/// **Structural only.** The real path is
/// `CashDrawerPort → Android host bridge → Kotlin → IminSDKManager.opencashBox`,
/// and the bridge is the host's to supply: this adapter exists so the wiring,
/// the configuration and the error semantics are real and reviewable before
/// the device is in the room.
///
/// It answers `HARDWARE_NOT_CONFIGURED` whenever no bridge was installed, and
/// it **never falls back** to another driver: a configuration that asks for
/// iMin and silently gets an ESC/POS pulse is worse than one that refuses
/// (ADR 038).
/// What the Android host installs: a call that reaches Kotlin and
/// `IminSDKManager.opencashBox`. Named so the integration point has a name
/// rather than a type signature (ADR 038).
pub type IminBridge = dyn Fn(DrawerOpenReason) -> Result<(), HardwareError> + Send + Sync;

pub struct IminDrawer {
    /// Supplied by the Android host. `None` on every build that is not running
    /// inside that host — which is every build today.
    bridge: Option<Box<IminBridge>>,
}

impl std::fmt::Debug for IminDrawer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IminDrawer")
            .field("bridge_installed", &self.bridge.is_some())
            .finish()
    }
}

impl Default for IminDrawer {
    fn default() -> Self {
        Self::without_bridge()
    }
}

impl IminDrawer {
    /// What a desktop build gets: the driver is selected, the bridge is not
    /// there, and every call says so plainly.
    pub fn without_bridge() -> Self {
        Self { bridge: None }
    }

    /// What the Android host installs: a closure that reaches Kotlin and
    /// `IminSDKManager.opencashBox`. This is the integration point, and the
    /// only thing M10 leaves for the device.
    pub fn with_bridge(
        bridge: impl Fn(DrawerOpenReason) -> Result<(), HardwareError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            bridge: Some(Box::new(bridge)),
        }
    }

    pub fn bridge_installed(&self) -> bool {
        self.bridge.is_some()
    }

    fn require_bridge(&self) -> Result<&IminBridge, HardwareError> {
        self.bridge.as_deref().ok_or(HardwareError::NotConfigured {
            device: "cash drawer",
            detail: "the iMin driver is selected but this host installed no bridge; \
                     PI.Edge will not substitute another driver"
                .to_owned(),
        })
    }
}

impl CashDrawerPort for IminDrawer {
    fn driver(&self) -> &'static str {
        "imin"
    }

    fn open(&self, reason: DrawerOpenReason) -> Result<(), HardwareError> {
        self.require_bridge()?(reason)
    }

    fn probe(&self) -> Result<(), HardwareError> {
        // The terminal's drawer cannot be interrogated; all that is knowable
        // without opening it is whether the bridge is there at all.
        self.require_bridge().map(|_| ())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::{PrintDocument, PrintKind};
    use uuid::Uuid;

    fn document(copies: u8) -> PrintDocument {
        let mut doc = PrintDocument::new(Uuid::from_u128(1), PrintKind::TestPage, 48);
        doc.copies = copies;
        doc
    }

    #[test]
    fn a_working_printer_records_jobs_and_copies() {
        let printer = SimulatorPrinter::working();
        printer.print(&document(1)).unwrap();
        printer.print(&document(2)).unwrap();
        assert_eq!(printer.job_count(), 2);
        assert_eq!(printer.copy_count(), 3);
    }

    #[test]
    fn a_failing_printer_records_nothing() {
        for (outcome, code) in [
            (SimulatedHardware::Unavailable, "HARDWARE_UNAVAILABLE"),
            (SimulatedHardware::Timeout, "HARDWARE_TIMEOUT"),
            (SimulatedHardware::IoError, "HARDWARE_IO_ERROR"),
        ] {
            let printer = SimulatorPrinter::answering(outcome);
            assert_eq!(printer.print(&document(1)).unwrap_err().code(), code);
            assert_eq!(printer.job_count(), 0, "no paper, no job");
            assert_eq!(printer.copy_count(), 0);
        }
    }

    #[test]
    fn a_drawer_counts_only_physical_openings() {
        let drawer = SimulatorDrawer::working();
        drawer.open(DrawerOpenReason::CashSale).unwrap();
        drawer.open(DrawerOpenReason::Diagnostic).unwrap();
        assert_eq!(drawer.open_count(), 2);
        assert_eq!(
            drawer.reasons(),
            vec![DrawerOpenReason::CashSale, DrawerOpenReason::Diagnostic]
        );

        let broken = SimulatorDrawer::answering(SimulatedHardware::Unavailable);
        assert!(broken.open(DrawerOpenReason::CashSale).is_err());
        assert_eq!(broken.open_count(), 0, "a drawer that failed did not open");
    }

    #[test]
    fn a_timeout_is_unresolved_and_the_others_are_not() {
        let drawer = SimulatorDrawer::answering(SimulatedHardware::Timeout);
        let error = drawer.open(DrawerOpenReason::Diagnostic).unwrap_err();
        assert!(error.is_unresolved());
        assert_eq!(error.code(), "HARDWARE_TIMEOUT");

        let down = SimulatorDrawer::answering(SimulatedHardware::Unavailable);
        assert!(!down
            .open(DrawerOpenReason::Diagnostic)
            .unwrap_err()
            .is_unresolved());
    }

    #[test]
    fn the_imin_driver_without_a_bridge_refuses_and_substitutes_nothing() {
        let drawer = IminDrawer::without_bridge();
        assert_eq!(drawer.driver(), "imin");
        assert!(!drawer.bridge_installed());
        for outcome in [drawer.open(DrawerOpenReason::CashSale), drawer.probe()] {
            let error = outcome.unwrap_err();
            assert_eq!(error.code(), "HARDWARE_NOT_CONFIGURED");
            assert!(
                error.to_string().contains("will not substitute"),
                "the refusal says why rather than falling back"
            );
        }
    }

    #[test]
    fn the_imin_driver_with_a_bridge_calls_it_once_per_open() {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&calls);
        let drawer = IminDrawer::with_bridge(move |_reason| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(drawer.bridge_installed());
        drawer.open(DrawerOpenReason::CashSale).unwrap();
        drawer.open(DrawerOpenReason::CashRefund).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn simulated_outcomes_parse_from_configuration() {
        assert_eq!(
            SimulatedHardware::parse("works").unwrap(),
            SimulatedHardware::Works
        );
        assert_eq!(
            SimulatedHardware::parse("timeout").unwrap(),
            SimulatedHardware::Timeout
        );
        assert!(SimulatedHardware::parse("explode").is_err());
    }
}
