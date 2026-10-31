//! The retail lane's door to the core, and which lane the screen wears.
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
//! line, and payment goes through `kiosk_pay` exactly as every other lane's
//! does. What this box *is* — brand, name, open sign, day — lives in
//! `showroom`.

use edge_client::EdgeClient;
use edge_protocol::{
    AddItemRequestV1, ProductSelectorV1, RetailTransactionV1, SubmitScanRequestV1,
};
use uuid::Uuid;

use crate::showroom::{self, KioskPreferencesV1, Vertical};
use crate::{BridgeError, EdgeBridge};

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
    let dir = showroom::data_dir(&app)?;
    let mut preferences = showroom::stored(&dir);
    preferences.camera_preview = enabled;
    showroom::write(&dir, &preferences)?;
    Ok(preferences)
}

/// Switch lanes, abandoning whatever is open first.
///
/// A basket half-built in one brand must not reappear in another, so the open
/// sale is cancelled here *deliberately* rather than left for the new shell to
/// find. Everything else — the catalog, the photographs, the printer settings,
/// the identity, the day's tally, the business day — is untouched: switching
/// brands is a change of channel, not a reinstallation (ADR 047).
#[tauri::command]
pub fn set_vertical_setting(
    app: tauri::AppHandle,
    bridge: tauri::State<'_, EdgeBridge>,
    vertical: Vertical,
) -> Result<KioskPreferencesV1, BridgeError> {
    let dir = showroom::data_dir(&app)?;
    let mut preferences = showroom::stored(&dir);
    preferences.vertical = vertical;
    preferences.channel_ref = vertical.channel().to_owned();
    showroom::write(&dir, &preferences)?;

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
