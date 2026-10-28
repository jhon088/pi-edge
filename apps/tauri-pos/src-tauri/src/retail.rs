//! The retail lane's door to the core.
//!
//! Retail is the same core with a different shell in front of it: the same
//! catalog, the same sale, the same payment journey, the same receipt. What
//! differs is that a supermarket lane **opens on the first scan** — nobody
//! presses "start" at a self-checkout — and the core never opens a sale by
//! itself (ADR 036). So these commands orchestrate two ops that already exist,
//! `start_transaction` and `submit_scan`, and add no transaction logic of
//! their own.
//!
//! There is no retail pricing, no retail totalizer and no retail finalize
//! here, and there is nowhere one could be added: a retail line is an ordinary
//! line, and payment goes through `kiosk_pay` exactly as the restaurant's does.

use std::fs;
use std::path::{Path, PathBuf};

use edge_protocol::{
    AddItemRequestV1, ProductSelectorV1, RetailTransactionV1, SubmitScanRequestV1,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{BridgeError, EdgeBridge};

/// Which lane this installation opens in.
///
/// A presentation choice, not core state: it selects a shell and a channel to
/// read the menu for, and changes nothing the core computes. Kept in the data
/// directory rather than in the webview's storage so it survives a reinstall
/// of the frontend and can be read by whoever is supporting the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Vertical {
    Restaurant,
    Retail,
}

impl Vertical {
    /// The catalog channel this lane sells from.
    ///
    /// Both lanes read one active snapshot — a store may have exactly one —
    /// and availability per channel decides which aisles each one shows
    /// (ADR 044).
    pub fn channel(self) -> &'static str {
        match self {
            Self::Restaurant => "KIOSK",
            Self::Retail => "KIOSK-RETAIL",
        }
    }
}

/// What this box is set up as. Local, and only local.
///
/// Everything here chooses how the screen behaves, never what the core
/// computes — so it is a file beside the database rather than a settings table
/// and a protocol operation. Kept out of the webview's storage so it survives
/// a reinstall of the frontend and can be read by whoever is supporting the
/// machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KioskPreferencesV1 {
    pub vertical: Vertical,
    /// So the shell never has to know the mapping; the host owns it.
    pub channel_ref: String,
    /// Whether the retail lane shows the customer their own camera.
    ///
    /// Off unless somebody turned it on: a kiosk must not reach for a camera
    /// because it happened to be installed somewhere with one.
    #[serde(default)]
    pub camera_preview: bool,
}

impl KioskPreferencesV1 {
    fn new(vertical: Vertical, camera_preview: bool) -> Self {
        Self {
            channel_ref: vertical.channel().to_owned(),
            vertical,
            camera_preview,
        }
    }
}

impl Default for KioskPreferencesV1 {
    fn default() -> Self {
        Self::new(Vertical::Restaurant, false)
    }
}

fn setting_path(dir: &Path) -> PathBuf {
    dir.join("preferences.json")
}

/// Read what is stored, defaulting to a restaurant with no camera.
///
/// Anything unreadable or unrecognised is the default rather than an error: a
/// corrupt preferences file must not stop a kiosk from opening, and the
/// default is a working screen. The default is also the *quiet* one — nothing
/// here can turn a camera on by failing to parse.
fn stored(dir: &Path) -> KioskPreferencesV1 {
    fs::read_to_string(setting_path(dir))
        .ok()
        .and_then(|text| serde_json::from_str::<KioskPreferencesV1>(&text).ok())
        // The channel is the host's to decide, so it is recomputed rather
        // than trusted: an edited file cannot point a lane at another's goods.
        .map(|stored| KioskPreferencesV1::new(stored.vertical, stored.camera_preview))
        .unwrap_or_default()
}

fn write(dir: &Path, preferences: &KioskPreferencesV1) -> Result<(), BridgeError> {
    let body = serde_json::to_string_pretty(preferences).map_err(|error| BridgeError {
        code: "PREFERENCES_ENCODE_FAILED".to_owned(),
        message: error.to_string(),
    })?;
    fs::write(setting_path(dir), body).map_err(|error| BridgeError {
        code: "PREFERENCES_WRITE_FAILED".to_owned(),
        message: error.to_string(),
    })
}

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, BridgeError> {
    crate::bootstrap::data_dir(app).map_err(|error| BridgeError {
        code: "DATA_DIRECTORY_UNAVAILABLE".to_owned(),
        message: error.to_string(),
    })
}

#[tauri::command]
pub fn kiosk_preferences(app: tauri::AppHandle) -> Result<KioskPreferencesV1, BridgeError> {
    Ok(stored(&data_dir(&app)?))
}

/// Turn the customer-facing camera preview on or off.
///
/// Nothing is started here: the host stores a choice and the retail shell acts
/// on it. Switching it off is what makes the shell stop the stream, which is
/// why this answers the whole preference rather than an acknowledgement.
#[tauri::command]
pub fn set_camera_preview(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<KioskPreferencesV1, BridgeError> {
    let dir = data_dir(&app)?;
    let preferences = KioskPreferencesV1::new(stored(&dir).vertical, enabled);
    write(&dir, &preferences)?;
    Ok(preferences)
}

/// Switch lanes, abandoning whatever is open first.
///
/// A basket half-built in one lane must not reappear in the other, so the
/// open sale is cancelled here *deliberately* rather than left for the new
/// shell to find. Everything else — the catalog, the photographs, the printer
/// settings, the business day, the till session — is untouched: switching
/// verticals is a change of channel, not a reinstallation.
#[tauri::command]
pub fn set_vertical_setting(
    app: tauri::AppHandle,
    bridge: tauri::State<'_, EdgeBridge>,
    vertical: Vertical,
) -> Result<KioskPreferencesV1, BridgeError> {
    let dir = data_dir(&app)?;
    // The camera choice belongs to the box, not to the lane: changing shops
    // does not turn a camera on, and does not turn one off either. The shell
    // stops the stream on its way out because the restaurant has no preview.
    let preferences = KioskPreferencesV1::new(vertical, stored(&dir).camera_preview);
    write(&dir, &preferences)?;

    abandon_open_sale(&bridge, "VERTICAL_SWITCH");
    Ok(preferences)
}

/// One keystroke from a keyboard-wedge scanner, in a lane that starts on the
/// first scan.
///
/// The parser in `edge-hardware` still decides when a scan completed, and the
/// core still decides what a barcode is worth. The only thing added here is
/// opening the sale the scan needs — and undoing that if the scan turns out
/// not to be sellable, so a refused barcode never leaves an empty basket on
/// the screen.
#[tauri::command]
pub fn retail_scan_key(
    bridge: tauri::State<'_, EdgeBridge>,
    key: String,
) -> Result<Option<RetailTransactionV1>, BridgeError> {
    let now = edge_domain::prelude::Timestamp::from_datetime(chrono::Utc::now());
    let Some(barcode) = bridge.scanner.push(&key, now) else {
        return Ok(None);
    };
    scan(&bridge, barcode).map(Some)
}

/// A complete barcode, as typed or as sent by a scanner that delivers whole
/// codes rather than keystrokes.
#[tauri::command]
pub fn retail_scan(
    bridge: tauri::State<'_, EdgeBridge>,
    barcode: String,
) -> Result<RetailTransactionV1, BridgeError> {
    scan(&bridge, barcode)
}

/// Add a product the customer found by looking for it instead of scanning it.
///
/// The same lane-opening rule: searching for the first item is as good a start
/// as scanning it. Ids and quantities only — there is no field here through
/// which a screen could state a price.
#[tauri::command]
pub fn retail_add_item(
    bridge: tauri::State<'_, EdgeBridge>,
    product_ref: String,
    quantity: i64,
) -> Result<RetailTransactionV1, BridgeError> {
    let opened = open_lane(&bridge)?;
    let outcome = bridge
        .client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef(product_ref),
                quantity_thousandths: quantity.max(1) * 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .map_err(BridgeError::from);
    undo_on_failure(&bridge, opened, outcome)
}

// --------------------------------------------------------------- internals

use edge_client::EdgeClient;

/// Make sure a sale is open. Answers whether *this call* opened it.
fn open_lane(bridge: &EdgeBridge) -> Result<bool, BridgeError> {
    match bridge
        .client
        .get_active_transaction()
        .map_err(BridgeError::from)?
    {
        Some(_) => Ok(false),
        None => {
            bridge
                .client
                .start_transaction(None)
                .map_err(BridgeError::from)?;
            Ok(true)
        }
    }
}

fn scan(bridge: &EdgeBridge, barcode: String) -> Result<RetailTransactionV1, BridgeError> {
    let opened = open_lane(bridge)?;
    let outcome = bridge
        .client
        .submit_scan(SubmitScanRequestV1 {
            // A fresh id per physical scan: two scans of one product are two
            // units, and a resend of *this* scan is one.
            event_id: Uuid::new_v4().to_string(),
            barcode,
            device_id: None,
        })
        .map_err(BridgeError::from);
    undo_on_failure(bridge, opened, outcome)
}

/// Leave nothing behind when the thing the lane was opened for failed.
fn undo_on_failure(
    bridge: &EdgeBridge,
    opened: bool,
    outcome: Result<RetailTransactionV1, BridgeError>,
) -> Result<RetailTransactionV1, BridgeError> {
    if outcome.is_err() && opened {
        abandon_open_sale(bridge, "SCAN_REJECTED");
    }
    outcome
}

/// Abandon the open sale if there is one, and say nothing if there is not.
///
/// Refused once money has been applied, which is the core's call and the right
/// one: a paid sale is not something a screen may discard.
fn abandon_open_sale(bridge: &EdgeBridge, reason: &str) {
    if let Err(error) = bridge
        .client
        .cancel_transaction(Some(reason.to_owned()), None)
    {
        let message = format!("{error}");
        if !message.contains("NO_ACTIVE_TRANSACTION") {
            tracing::warn!(%error, reason, "the open sale could not be abandoned");
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_lane_sells_from_its_own_channel() {
        assert_eq!(Vertical::Restaurant.channel(), "KIOSK");
        assert_eq!(Vertical::Retail.channel(), "KIOSK-RETAIL");
    }

    #[test]
    fn an_installation_with_no_preference_opens_the_restaurant_with_no_camera() {
        let dir = tempfile::tempdir().unwrap();
        let preferences = stored(dir.path());
        assert_eq!(preferences.vertical, Vertical::Restaurant);
        assert!(
            !preferences.camera_preview,
            "a fresh installation reached for a camera on its own"
        );
    }

    #[test]
    fn a_stored_preference_comes_back_and_a_broken_one_does_not_stop_the_kiosk() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &KioskPreferencesV1::new(Vertical::Retail, true)).unwrap();

        let back = stored(dir.path());
        assert_eq!(back.vertical, Vertical::Retail);
        assert!(back.camera_preview, "the camera choice did not survive");

        fs::write(setting_path(dir.path()), "{ not json").unwrap();
        let broken = stored(dir.path());
        assert_eq!(
            broken.vertical,
            Vertical::Restaurant,
            "a corrupt preference must not stop a kiosk from opening"
        );
        assert!(
            !broken.camera_preview,
            "an unreadable file turned a camera on"
        );
    }

    /// An edited file cannot point a lane at the other lane's goods.
    #[test]
    fn the_channel_is_the_hosts_to_decide_and_is_recomputed_on_every_read() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            setting_path(dir.path()),
            r#"{"vertical":"RETAIL","channel_ref":"KIOSK","camera_preview":false}"#,
        )
        .unwrap();
        assert_eq!(stored(dir.path()).channel_ref, "KIOSK-RETAIL");
    }

    #[test]
    fn the_setting_names_the_channel_so_the_shell_never_has_to() {
        let json =
            serde_json::to_string(&KioskPreferencesV1::new(Vertical::Retail, false)).unwrap();
        assert!(json.contains("\"vertical\":\"RETAIL\""), "{json}");
        assert!(json.contains("\"channel_ref\":\"KIOSK-RETAIL\""), "{json}");
        assert!(json.contains("\"camera_preview\":false"), "{json}");
    }

    /// Changing shops is not a reason to change what the camera does.
    #[test]
    fn switching_vertical_keeps_the_camera_choice() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &KioskPreferencesV1::new(Vertical::Retail, true)).unwrap();
        let kept = KioskPreferencesV1::new(Vertical::Restaurant, stored(dir.path()).camera_preview);
        write(dir.path(), &kept).unwrap();

        let back = stored(dir.path());
        assert_eq!(back.vertical, Vertical::Restaurant);
        assert!(back.camera_preview);
    }
}
