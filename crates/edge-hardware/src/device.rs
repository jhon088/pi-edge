//! What a device is, and what we know about it.

use edge_domain::prelude::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::HardwareError;

/// The three kinds M10 actually drives. Each arrives with its port and its
/// adapter; none is here in advance (ADR 005, amended).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HardwareDeviceKind {
    Scanner,
    Printer,
    CashDrawer,
}

impl HardwareDeviceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            HardwareDeviceKind::Scanner => "SCANNER",
            HardwareDeviceKind::Printer => "PRINTER",
            HardwareDeviceKind::CashDrawer => "CASH_DRAWER",
        }
    }

    pub fn parse(value: &str) -> Result<Self, HardwareError> {
        match value {
            "SCANNER" => Ok(HardwareDeviceKind::Scanner),
            "PRINTER" => Ok(HardwareDeviceKind::Printer),
            "CASH_DRAWER" => Ok(HardwareDeviceKind::CashDrawer),
            other => Err(HardwareError::NotConfigured {
                device: "device",
                detail: format!("`{other}` is not a device kind"),
            }),
        }
    }
}

/// What we know about a device *right now*.
///
/// `UNKNOWN` is the honest default and the state every device returns to after
/// a restart: a configured device is not an available one, and an `AVAILABLE`
/// written before the process died says nothing about now (ADR 035).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareDeviceState {
    /// Reached, and it worked.
    Available,
    /// Configured and could not be reached, or it refused.
    Unavailable,
    /// Reached, but the last action did not resolve — a timeout, so the effect
    /// may or may not have happened.
    Degraded,
    /// Never probed in this process lifetime. Not a failure.
    Unknown,
}

impl HardwareDeviceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            HardwareDeviceState::Available => "AVAILABLE",
            HardwareDeviceState::Unavailable => "UNAVAILABLE",
            HardwareDeviceState::Degraded => "DEGRADED",
            HardwareDeviceState::Unknown => "UNKNOWN",
        }
    }

    /// The state an outcome implies.
    pub fn from_outcome(outcome: Result<(), &HardwareError>) -> Self {
        match outcome {
            Ok(()) => HardwareDeviceState::Available,
            Err(error) if error.is_unresolved() => HardwareDeviceState::Degraded,
            Err(_) => HardwareDeviceState::Unavailable,
        }
    }
}

/// One device's health, as a diagnostic screen shows it.
///
/// Carries no host, port, serial number, driver internals or stack trace: a
/// diagnostic view must not become a way to read the installation's
/// configuration (ADR 035).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareDeviceStatus {
    pub kind: HardwareDeviceKind,
    /// Opaque, so a second scanner or printer later is a value rather than a
    /// schema change. No device registry in V1.
    pub device_id: String,
    /// Which driver is configured, as a name. `None` when nothing is.
    pub driver: Option<String>,
    pub state: HardwareDeviceState,
    /// When this process last tried.
    pub last_checked_at: Option<Timestamp>,
    /// When it last worked — rebuilt from the action log after a restart.
    pub last_success_at: Option<Timestamp>,
    pub last_error_code: Option<String>,
    /// Human detail. Never a stack trace, never a payload.
    pub last_error_message: Option<String>,
}

impl HardwareDeviceStatus {
    /// A configured device nobody has tried yet.
    pub fn unknown(kind: HardwareDeviceKind, device_id: &str, driver: Option<String>) -> Self {
        Self {
            kind,
            device_id: device_id.to_owned(),
            driver,
            state: HardwareDeviceState::Unknown,
            last_checked_at: None,
            last_success_at: None,
            last_error_code: None,
            last_error_message: None,
        }
    }

    /// Fold one outcome into the status.
    pub fn record(&mut self, outcome: Result<(), &HardwareError>, now: Timestamp) {
        self.state = HardwareDeviceState::from_outcome(outcome);
        self.last_checked_at = Some(now);
        match outcome {
            Ok(()) => {
                self.last_success_at = Some(now);
                self.last_error_code = None;
                self.last_error_message = None;
            }
            Err(error) => {
                self.last_error_code = Some(error.code().to_owned());
                self.last_error_message = Some(error.to_string());
            }
        }
    }
}
