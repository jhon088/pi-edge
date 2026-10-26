//! Which devices this installation has, and how to reach them.
//!
//! Configuration lives **here**, in the composition root, and nowhere else.
//! Nothing downstream — not a use case, not the protocol, not a client — can
//! name a host, a port or a driver, which is what stops a till from being
//! able to print to an arbitrary machine on the network (ADR 039).
//!
//! The source is the environment, read once at startup, with an unrecognised
//! value refusing to start rather than falling back silently. A configuration
//! file is the obvious next step and changes nothing about the boundary.
//!
//! ```text
//! PI_EDGE_PRINTER=escpos_tcp:printer.local:9100 | simulator[:outcome] | none
//! PI_EDGE_DRAWER=escpos_kick[:host[:port]] | imin | simulator[:outcome] | none
//! PI_EDGE_PAPER_WIDTH=48
//! ```
//!
//! **A simulator is opt-in.** An absent variable, or `none`, selects the
//! unconfigured adapter — which answers `HARDWARE_NOT_CONFIGURED` to
//! everything — and never a simulator. A production runtime that was
//! misconfigured must say so rather than report healthy fake hardware
//! (ADR 038).

use std::sync::Arc;

use edge_application::hardware_service::Hardware;
use edge_hardware::{
    CashDrawerPort, EscPosConfig, EscPosKickDrawer, EscPosTcpPrinter, IminDrawer, PrinterPort,
    SimulatedHardware, SimulatorDrawer, SimulatorPrinter, SwappablePrinter, UnconfiguredDrawer,
    UnconfiguredPrinter,
};

use crate::printer_settings::PrinterControl;

pub const PRINTER_ENV: &str = "PI_EDGE_PRINTER";
pub const DRAWER_ENV: &str = "PI_EDGE_DRAWER";
pub const PAPER_WIDTH_ENV: &str = "PI_EDGE_PAPER_WIDTH";

/// The value, and the meaning of an absent variable: no device configured.
const NONE: &str = "none";

/// Build the hardware this runtime drives.
///
/// With nothing configured, the runtime still starts — a till must come up so
/// its diagnostics can be read — but every device answers
/// `HARDWARE_NOT_CONFIGURED`. It never substitutes a simulator for a missing
/// driver, and never one real driver for another (ADR 038).
pub fn from_env() -> Result<Hardware, String> {
    from_env_inner().map(|(hardware, _)| hardware)
}

/// The same, plus the handle that lets an operator change the receipt printer
/// without restarting the runtime holding the day open.
///
/// The stored settings win over the environment when they configure a
/// printer: a kiosk configured from its own admin screen must come back up
/// pointing at the same box. With nothing stored, this behaves exactly as it
/// always did.
pub fn from_env_with_control(
    runtime_dir: &std::path::Path,
) -> Result<(Hardware, PrinterControl), String> {
    let (hardware, _) = from_env_inner()?;
    // One indirection, installed once. Everything downstream keeps calling a
    // `PrinterPort` and never learns the adapter behind it can change.
    let port = Arc::new(SwappablePrinter::new(Arc::clone(&hardware.printer)));
    let hardware = Hardware::new(
        Arc::clone(&port) as Arc<dyn PrinterPort>,
        Arc::clone(&hardware.drawer),
        hardware.paper_width(),
    );
    let control = PrinterControl::new(
        runtime_dir.to_path_buf(),
        port,
        hardware.paper_width_handle(),
    );
    let stored = crate::printer_settings::load(runtime_dir);
    if stored.is_configured() {
        tracing::info!(
            model = %stored.model,
            "receipt printer configured from the admin screen"
        );
        control.install(&stored);
    }
    Ok((hardware, control))
}

fn from_env_inner() -> Result<(Hardware, ()), String> {
    let paper_width = match std::env::var(PAPER_WIDTH_ENV) {
        Ok(raw) => raw
            .trim()
            .parse::<usize>()
            .map_err(|_| format!("`{raw}` is not a paper width in characters"))?,
        Err(_) => EscPosConfig::DEFAULT_WIDTH,
    };

    // Absent means unconfigured, never simulated.
    let printer_spec = std::env::var(PRINTER_ENV).unwrap_or_else(|_| NONE.to_owned());
    let drawer_spec = std::env::var(DRAWER_ENV).unwrap_or_else(|_| NONE.to_owned());

    let printer = printer(&printer_spec, paper_width)?;
    let drawer = drawer(&drawer_spec, &printer_spec, paper_width)?;
    for (device, driver, env) in [
        ("printer", printer.driver(), PRINTER_ENV),
        ("cash drawer", drawer.driver(), DRAWER_ENV),
    ] {
        match driver {
            edge_hardware::UNCONFIGURED => tracing::warn!(
                device,
                "no {env} configured; this device will answer HARDWARE_NOT_CONFIGURED"
            ),
            "simulator" => {
                tracing::warn!(device, "{env} selects a simulator; not for production use")
            }
            driver => tracing::info!(device, driver, "hardware configured"),
        }
    }
    tracing::info!(paper_width, "paper width configured");
    Ok((Hardware::new(printer, drawer, paper_width), ()))
}

fn printer(spec: &str, paper_width: usize) -> Result<Arc<dyn PrinterPort>, String> {
    match driver_of(spec) {
        (NONE, _) => Ok(Arc::new(UnconfiguredPrinter)),
        ("simulator", rest) => Ok(Arc::new(SimulatorPrinter::answering(outcome(rest)?))),
        ("escpos_tcp", rest) => Ok(Arc::new(EscPosTcpPrinter::new(escpos(rest, paper_width)?))),
        (other, _) => Err(format!(
            "`{other}` is not a known {PRINTER_ENV} driver \
             (expected `escpos_tcp`, `simulator` or `none`)"
        )),
    }
}

fn drawer(
    spec: &str,
    printer_spec: &str,
    paper_width: usize,
) -> Result<Arc<dyn CashDrawerPort>, String> {
    match driver_of(spec) {
        (NONE, _) => Ok(Arc::new(UnconfiguredDrawer)),
        ("simulator", rest) => Ok(Arc::new(SimulatorDrawer::answering(outcome(rest)?))),
        ("escpos_kick", rest) => {
            // The pulse goes down the printer's wire, so this driver needs a
            // printer address: its own when given, otherwise the configured
            // printer's. It never borrows a *simulator* printer, because then
            // "the drawer opened" would be a lie.
            let config = if rest.is_empty() {
                match driver_of(printer_spec) {
                    ("escpos_tcp", printer_rest) => escpos(printer_rest, paper_width)?,
                    (other, _) => {
                        return Err(format!(
                            "{DRAWER_ENV}=escpos_kick needs an ESC/POS printer to pulse, but \
                             {PRINTER_ENV} is `{other}`; give the drawer its own address as \
                             `escpos_kick:<host>:<port>`"
                        ))
                    }
                }
            } else {
                escpos(rest, paper_width)?
            };
            Ok(Arc::new(EscPosKickDrawer::new(EscPosTcpPrinter::new(
                config,
            ))))
        }
        // Selected, and this host installed no bridge. Every call will answer
        // `HARDWARE_NOT_CONFIGURED`, and **nothing falls back**: a
        // configuration that asks for iMin and silently gets an ESC/POS pulse
        // is worse than one that refuses (ADR 038).
        ("imin", _) => {
            tracing::warn!(
                "the iMin drawer driver is selected but this build installs no Android bridge; \
                 drawer commands will answer HARDWARE_NOT_CONFIGURED"
            );
            Ok(Arc::new(IminDrawer::without_bridge()))
        }
        (other, _) => Err(format!(
            "`{other}` is not a known {DRAWER_ENV} driver \
             (expected `escpos_kick`, `imin`, `simulator` or `none`)"
        )),
    }
}

/// Split `driver[:rest]`. A blank spec means no device configured, which is
/// the same thing as an absent variable.
fn driver_of(spec: &str) -> (&str, &str) {
    let spec = spec.trim();
    if spec.is_empty() {
        return (NONE, "");
    }
    match spec.split_once(':') {
        Some((driver, rest)) => (driver.trim(), rest.trim()),
        None => (spec, ""),
    }
}

fn outcome(rest: &str) -> Result<SimulatedHardware, String> {
    if rest.is_empty() {
        return Ok(SimulatedHardware::Works);
    }
    SimulatedHardware::parse(rest).map_err(|error| error.to_string())
}

/// Parse `host[:port]`. There is no default host: an ESC/POS driver with no
/// address is a configuration error, not an attempt to guess.
fn escpos(rest: &str, paper_width: usize) -> Result<EscPosConfig, String> {
    if rest.is_empty() {
        return Err(format!(
            "an ESC/POS driver needs an address, as `escpos_tcp:<host>[:<port>]`; \
             {PRINTER_ENV} has none"
        ));
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.trim()
                .parse::<u16>()
                .map_err(|_| format!("`{port}` is not a TCP port"))?,
        ),
        None => (rest, EscPosConfig::DEFAULT_PORT),
    };
    if host.trim().is_empty() {
        return Err("an ESC/POS driver needs a host".to_owned());
    }
    Ok(EscPosConfig {
        width: paper_width,
        ..EscPosConfig::new(host.trim(), port)
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    /// The blocker this rule exists for: a runtime nobody configured must not
    /// report healthy fake hardware (ADR 038).
    #[test]
    fn nothing_configured_means_unconfigured_and_never_a_simulator() {
        for spec in [NONE, "", "   "] {
            let paper = printer(spec, 48).unwrap();
            assert_eq!(paper.driver(), edge_hardware::UNCONFIGURED, "`{spec}`");
            assert_ne!(paper.driver(), "simulator");
            assert_eq!(paper.probe().unwrap_err().code(), "HARDWARE_NOT_CONFIGURED");

            let till = drawer(spec, spec, 48).unwrap();
            assert_eq!(till.driver(), edge_hardware::UNCONFIGURED, "`{spec}`");
            assert_ne!(till.driver(), "simulator");
            assert_eq!(till.probe().unwrap_err().code(), "HARDWARE_NOT_CONFIGURED");
        }
    }

    /// And a simulator still works — when it is asked for.
    #[test]
    fn a_simulator_is_selected_only_when_it_is_asked_for() {
        let paper = printer("simulator", 48).unwrap();
        assert_eq!(paper.driver(), "simulator");
        assert!(paper.probe().is_ok());

        let till = drawer("simulator", "simulator", 48).unwrap();
        assert_eq!(till.driver(), "simulator");
        assert!(till.probe().is_ok());
        assert!(till
            .open(edge_hardware::DrawerOpenReason::Diagnostic)
            .is_ok());
    }

    /// An unconfigured drawer does not borrow a configured printer, and a
    /// configured printer does not imply a drawer.
    #[test]
    fn one_configured_device_never_configures_the_other() {
        let without = drawer(NONE, "escpos_tcp:printer.local:9100", 48).unwrap();
        assert_eq!(without.driver(), edge_hardware::UNCONFIGURED);

        let no_printer = printer(NONE, 48).unwrap();
        assert_eq!(no_printer.driver(), edge_hardware::UNCONFIGURED);
        let simulated = drawer("simulator", NONE, 48).unwrap();
        assert_eq!(simulated.driver(), "simulator");
    }

    #[test]
    fn an_escpos_printer_takes_a_host_and_an_optional_port() {
        assert_eq!(
            printer("escpos_tcp:printer.local:9100", 48)
                .unwrap()
                .driver(),
            "escpos_tcp"
        );
        assert_eq!(
            printer("escpos_tcp:printer.local", 48).unwrap().driver(),
            "escpos_tcp"
        );
        // No address is a configuration error rather than a guess.
        assert!(printer("escpos_tcp", 48).is_err());
        assert!(printer("escpos_tcp:printer.local:notaport", 48).is_err());
    }

    #[test]
    fn an_unknown_driver_refuses_to_start() {
        assert!(printer("laserjet", 48).is_err());
        assert!(drawer("serial", "simulator", 48).is_err());
    }

    #[test]
    fn the_kick_drawer_borrows_the_printer_address_only_when_it_is_real() {
        assert_eq!(
            drawer("escpos_kick", "escpos_tcp:printer.local:9100", 48)
                .unwrap()
                .driver(),
            "escpos_kick"
        );
        // A simulated printer cannot carry a real pulse, and pretending it can
        // would make "the drawer opened" a lie.
        assert!(drawer("escpos_kick", "simulator", 48).is_err());
        // Its own address always works.
        assert_eq!(
            drawer("escpos_kick:drawer.local:9100", "simulator", 48)
                .unwrap()
                .driver(),
            "escpos_kick"
        );
    }

    #[test]
    fn the_imin_driver_is_selected_without_a_bridge_and_never_falls_back() {
        let selected = drawer("imin", "escpos_tcp:printer.local:9100", 48).unwrap();
        // Selected as iMin, not quietly turned into an ESC/POS pulse.
        assert_eq!(selected.driver(), "imin");
        assert_eq!(
            selected
                .open(edge_hardware::DrawerOpenReason::CashSale)
                .unwrap_err()
                .code(),
            "HARDWARE_NOT_CONFIGURED"
        );
    }

    #[test]
    fn a_simulated_device_can_be_scripted_to_fail() {
        let down = printer("simulator:unavailable", 48).unwrap();
        assert_eq!(down.probe().unwrap_err().code(), "HARDWARE_UNAVAILABLE");
        let slow = drawer("simulator:timeout", "simulator", 48).unwrap();
        assert_eq!(slow.probe().unwrap_err().code(), "HARDWARE_TIMEOUT");
        assert!(printer("simulator:explode", 48).is_err());
    }
}
