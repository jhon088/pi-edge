//! A CashGenic note acceptor, for cash on a showroom kiosk (ADR 049, ADR 051).
//!
//! Built against *CashGenic API Specification v1* and split by what changes
//! for different reasons:
//!
//! * [`protocol`] — the document's routes, bodies, response codes and event
//!   shapes;
//! * [`http`] — one HTTPS request and exactly its response, with the
//!   connect, handshake and response budgets apart;
//! * [`runtime`] — the one link to the device, the one reader of `/status`,
//!   the session it follows and closes, and the health everything reads.
//!
//! Here: what the screens and the floor monitor receive, and the Tauri
//! commands. Each command that touches the network runs off the main thread,
//! and none of them reads `/status` — the snapshot is the authority's.
//!
//! **No cardholder data passes through here, and none can**: the device
//! accepts banknotes, and what comes back is an amount in minor units.

mod http;
mod protocol;
mod runtime;

#[cfg(test)]
pub(crate) mod bench;

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::showroom::CashGenicSettingsV1;
use crate::BridgeError;
use runtime::Runtime;

/// What the acceptor is doing, as the screens need it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashGenicStatusV1 {
    /// Ready, or in a session: it can be talked to and can take money.
    pub online: bool,
    /// What the device calls itself, from `/system`.
    pub device: Option<String>,
    /// The device's own last event (`Idle`, `Waiting`, `PaymentComplete` …)
    /// while it is talking; `OFFLINE`, `DISABLED` or `UNKNOWN` otherwise.
    pub state: String,
    /// Where the payment this kiosk started stands: `NONE`, `PAYING`,
    /// `CANCELLING`, `STOPPING`, `COMPLETE`, `CANCELLED` or `STOPPED`. A sale
    /// is finalized on `COMPLETE` — `PaymentComplete` from the device — and on
    /// nothing else.
    pub phase: String,
    /// Paid in so far for that payment (`cash_in`), in minor units.
    pub deposited_minor: i64,
    /// What is left of the core's total. Zero once covered.
    pub outstanding_minor: i64,
    /// What is owed back against the core's total. Zero until covered.
    pub change_minor: i64,
    /// `cashStatus.totalSystemCash`, when the device has said.
    pub cash_total_minor: Option<i64>,
    /// Why it is not usable, or what needs a person, in words a stand can
    /// act on.
    pub detail: Option<String>,
}

/// The acceptor as the floor monitor publishes it, and when it was last
/// heard from.
#[derive(Debug, Clone)]
pub struct CachedHealth {
    pub status: CashGenicStatusV1,
    /// RFC 3339. `None` before the first exchange.
    pub last_checked_at: Option<String>,
}

impl Default for CachedHealth {
    fn default() -> Self {
        Self {
            status: CashGenicStatusV1 {
                online: false,
                device: None,
                state: "UNKNOWN".to_owned(),
                phase: "NONE".to_owned(),
                deposited_minor: 0,
                outstanding_minor: 0,
                change_minor: 0,
                cash_total_minor: None,
                detail: Some("not checked yet".to_owned()),
            },
            last_checked_at: None,
        }
    }
}

static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();

/// Start this process's CashGenic runtime. The preference is read every
/// round, so enabling the device in Admin needs no restart.
pub fn start(app: tauri::AppHandle) {
    let runtime = RUNTIME.get_or_init(|| {
        Arc::new(Runtime::new(
            move || {
                crate::showroom::data_dir(&app)
                    .map(|dir| crate::showroom::stored(&dir).cashgenic)
                    .unwrap_or_default()
            },
            http::TIMEOUTS,
            runtime::CADENCE,
        ))
    });
    runtime.start();
}

fn runtime() -> Result<Arc<Runtime>, BridgeError> {
    RUNTIME.get().cloned().ok_or_else(|| BridgeError {
        code: "CASHGENIC_NOT_STARTED".to_owned(),
        message: "the CashGenic runtime has not started".to_owned(),
    })
}

/// What the acceptor last said. Never dials; never blocks on the network.
pub fn cached_health() -> CachedHealth {
    match RUNTIME.get() {
        Some(runtime) => CachedHealth {
            status: runtime.snapshot(0),
            last_checked_at: runtime.last_checked_at(),
        },
        None => CachedHealth::default(),
    }
}

/// Run `work` on the blocking pool: a TLS exchange must never hold the
/// window's thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, BridgeError> + Send + 'static,
) -> Result<T, BridgeError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| BridgeError {
            code: "CASHGENIC_INTERNAL".to_owned(),
            message: error.to_string(),
        })?
}

// ----------------------------------------------------------------- commands

#[tauri::command]
pub fn cashgenic_save(
    app: tauri::AppHandle,
    settings: CashGenicSettingsV1,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let saved = save(&crate::showroom::data_dir(&app)?, settings)?;
    if let Ok(runtime) = runtime() {
        runtime.wake();
    }
    Ok(saved)
}

fn save(
    dir: &std::path::Path,
    settings: CashGenicSettingsV1,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let mut preferences = crate::showroom::stored(dir);
    preferences.cashgenic = settings;
    crate::showroom::write(dir, &preferences)?;
    Ok(preferences)
}

/// The "Habilitar CashGenic" switch, saved the moment it is flipped.
///
/// Only the flag: the address and the credentials are whatever is stored, so a
/// switch never carries half-typed fields into the file, and Admin can never
/// show a device on that the runtime reads as off.
#[tauri::command]
pub fn cashgenic_set_enabled(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let saved = set_enabled(&crate::showroom::data_dir(&app)?, enabled)?;
    if let Ok(runtime) = runtime() {
        runtime.wake();
    }
    Ok(saved)
}

fn set_enabled(
    dir: &std::path::Path,
    enabled: bool,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let mut preferences = crate::showroom::stored(dir);
    preferences.cashgenic.enabled = enabled;
    crate::showroom::write(dir, &preferences)?;
    Ok(preferences)
}

/// "Probar conexión": through the same link as everything else, a grant if
/// none is held and `GET /system`. Never `/status`.
#[tauri::command]
pub async fn cashgenic_probe() -> Result<CashGenicStatusV1, BridgeError> {
    let runtime = runtime()?;
    blocking(move || Ok(runtime.probe())).await
}

/// The payment screen's view of the acceptor. No network: the authority's
/// snapshot, with the arithmetic done against the core's own total.
#[tauri::command]
pub fn cashgenic_status(due_minor: i64) -> Result<CashGenicStatusV1, BridgeError> {
    Ok(runtime()?.snapshot(due_minor))
}

/// Start the acceptor taking notes towards an amount the core computed.
///
/// Gated on the printer for the same reason `kiosk_pay` is, and separately:
/// this is the point at which a customer physically puts a banknote into a
/// machine, and getting it back out is a conversation with a member of staff.
#[tauri::command]
pub async fn cashgenic_pay(
    app: tauri::AppHandle,
    bridge: tauri::State<'_, crate::EdgeBridge>,
    amount_minor: i64,
) -> Result<CashGenicStatusV1, BridgeError> {
    crate::kiosk::require_a_printer_that_can_follow_through(&bridge)?;
    crate::kiosk::require_an_authorization_if_the_basket_needs_one(
        &crate::showroom::data_dir(&app)?,
        &bridge,
    )?;
    let runtime = runtime()?;
    blocking(move || {
        runtime
            .pay(amount_minor)
            .map_err(runtime::Failure::into_bridge)
    })
    .await
}

/// Give the notes back. The refund and the close follow from the events.
#[tauri::command]
pub async fn cashgenic_cancel() -> Result<(), BridgeError> {
    let runtime = runtime()?;
    blocking(move || runtime.cancel().map_err(runtime::Failure::into_bridge)).await
}

/// Make sure a finished session is closed. Idempotent; a session still
/// running is refused rather than closed.
#[tauri::command]
pub async fn cashgenic_close() -> Result<(), BridgeError> {
    let runtime = runtime()?;
    blocking(move || runtime.close().map_err(runtime::Failure::into_bridge)).await
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::showroom::{stored, write, KioskPreferencesV1, Vertical};

    fn configured(enabled: bool) -> CashGenicSettingsV1 {
        CashGenicSettingsV1 {
            enabled,
            host: "192.168.88.240".to_owned(),
            port: 44_333,
            username: "ApiUserOne".to_owned(),
            password: "s3cr3t-pw".to_owned(),
        }
    }

    /// A stored kiosk with an identity and a PIN, so a save that dropped any
    /// other field would show.
    fn a_kiosk_with(dir: &std::path::Path, cashgenic: CashGenicSettingsV1) {
        let preferences = KioskPreferencesV1 {
            vertical: Vertical::Retail,
            branch_id: "001".to_owned(),
            kiosk_id: "K01".to_owned(),
            admin_pin: "4321".to_owned(),
            remote_control: true,
            cashgenic,
            ..KioskPreferencesV1::default()
        };
        write(dir, &preferences).unwrap();
    }

    #[test]
    fn switching_the_acceptor_on_is_saved_at_once_and_keeps_its_configuration() {
        let dir = tempfile::tempdir().unwrap();
        a_kiosk_with(dir.path(), configured(false));
        let before = stored(dir.path());

        let answered = set_enabled(dir.path(), true).unwrap();

        let after = stored(dir.path());
        assert!(
            after.cashgenic.enabled,
            "ON is in the file, not only on screen"
        );
        assert_eq!(after.cashgenic, configured(true));
        assert_eq!(answered, after, "Admin is handed what was written");
        assert_eq!(
            KioskPreferencesV1 {
                cashgenic: before.cashgenic.clone(),
                ..after
            },
            before
        );
    }

    #[test]
    fn switching_the_acceptor_off_is_saved_at_once_and_forgets_nothing() {
        let dir = tempfile::tempdir().unwrap();
        a_kiosk_with(dir.path(), configured(true));
        let before = stored(dir.path());

        set_enabled(dir.path(), false).unwrap();

        let after = stored(dir.path());
        assert!(!after.cashgenic.enabled);
        assert_eq!(
            after.cashgenic,
            configured(false),
            "address and credentials kept"
        );
        assert_eq!(after.admin_pin, before.admin_pin);
        assert_eq!(after.branch_id, before.branch_id);
        assert_eq!(after.vertical, before.vertical);
        assert!(after.remote_control);
    }

    #[test]
    fn a_saved_configuration_is_the_one_read_back() {
        let dir = tempfile::tempdir().unwrap();
        a_kiosk_with(dir.path(), CashGenicSettingsV1::default());

        save(dir.path(), configured(true)).unwrap();

        let back = stored(dir.path()).cashgenic;
        assert!(back.enabled);
        assert_eq!(back.host, "192.168.88.240");
        assert_eq!(back.port, 44_333);
        assert_eq!(back.username, "ApiUserOne");
        assert_eq!(back.password, "s3cr3t-pw");
        assert_eq!(stored(dir.path()).branch_id, "001");
    }

    #[test]
    fn before_the_runtime_starts_the_monitor_reads_unknown_and_nothing_dials() {
        let health = CachedHealth::default();
        assert!(!health.status.online);
        assert_eq!(health.status.state, "UNKNOWN");
        assert_eq!(health.last_checked_at, None);
    }
}
