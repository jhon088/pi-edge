//! The kiosk's door to the core.
//!
//! Every command here is a forward to one `EdgeClient` call. The UI sends
//! intent — which product, which options, pay now — and renders what the core
//! answers. It never prices anything, never discounts anything and never
//! decides that a sale is paid (ADR 002, ADR 046).
//!
//! `asset_data_url` is the one place that does any work of its own: it pulls
//! an asset's bytes over IPC in chunks and hands the webview a `data:` URL, so
//! a photograph reaches the screen with **no HTTP of any kind** and no path
//! ever leaves the core (ADR 045).

use std::collections::HashMap;
use std::sync::Mutex;

use edge_client::EdgeClient;
use edge_protocol::{
    AddItemRequestV1, AssetRefV1, ChangeQuantityRequestV1, HardwareStatusV1, MenuV1,
    PrintTransactionRequestV1, PrinterConfigurationV1, PrinterSettingsV1, PrinterStatusV1,
    ProductConfigurationV1, ProductSelectorV1, ReceiptV1, RemoveItemRequestV1,
    RequestedSelectionV1, RetailTransactionV1, StartPaymentRequestV1, MAX_ASSET_CHUNK_BYTES,
};
use uuid::Uuid;

use crate::{BridgeError, EdgeBridge};

/// Decoded assets, kept for the life of the window.
///
/// A kiosk draws the same twenty photographs all day; fetching each one once
/// is the difference between a screen that feels instant and one that does
/// not. Keyed by content hash, so the cache can never serve the wrong bytes.
#[derive(Debug, Default)]
pub struct AssetCache(Mutex<HashMap<String, String>>);

#[tauri::command]
pub fn kiosk_menu(
    bridge: tauri::State<'_, EdgeBridge>,
    channel_ref: Option<String>,
) -> Result<MenuV1, BridgeError> {
    bridge.client.menu(channel_ref).map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_product_configuration(
    bridge: tauri::State<'_, EdgeBridge>,
    product_ref: String,
    channel_ref: Option<String>,
) -> Result<ProductConfigurationV1, BridgeError> {
    bridge
        .client
        .product_configuration(&product_ref, channel_ref)
        .map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_active_transaction(
    bridge: tauri::State<'_, EdgeBridge>,
) -> Result<Option<RetailTransactionV1>, BridgeError> {
    bridge
        .client
        .get_active_transaction()
        .map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_start_transaction(
    bridge: tauri::State<'_, EdgeBridge>,
) -> Result<RetailTransactionV1, BridgeError> {
    bridge
        .client
        .start_transaction(None)
        .map_err(BridgeError::from)
}

/// Add a product with the choices the customer made.
///
/// Ids and quantities only: there is no field here through which a screen
/// could state a price, a delta or a discount.
#[tauri::command]
pub fn kiosk_add_item(
    bridge: tauri::State<'_, EdgeBridge>,
    product_ref: String,
    quantity: i64,
    selections: Vec<RequestedSelectionV1>,
    preparation_note: Option<String>,
) -> Result<RetailTransactionV1, BridgeError> {
    bridge
        .client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef(product_ref),
                quantity_thousandths: quantity.max(1) * 1_000,
                selections,
                preparation_note,
            },
            None,
        )
        .map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_remove_item(
    bridge: tauri::State<'_, EdgeBridge>,
    line_number: i64,
) -> Result<RetailTransactionV1, BridgeError> {
    bridge
        .client
        .remove_item(RemoveItemRequestV1 { line_number }, None)
        .map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_change_quantity(
    bridge: tauri::State<'_, EdgeBridge>,
    line_number: i64,
    quantity: i64,
) -> Result<RetailTransactionV1, BridgeError> {
    bridge
        .client
        .change_quantity(
            ChangeQuantityRequestV1 {
                line_number,
                quantity_thousandths: quantity.max(1) * 1_000,
            },
            None,
        )
        .map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_cancel_transaction(
    bridge: tauri::State<'_, EdgeBridge>,
    reason_code: Option<String>,
) -> Result<RetailTransactionV1, BridgeError> {
    bridge
        .client
        .cancel_transaction(reason_code, None)
        .map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_totalize(
    bridge: tauri::State<'_, EdgeBridge>,
) -> Result<RetailTransactionV1, BridgeError> {
    bridge.client.totalize(None).map_err(BridgeError::from)
}

/// Refuse to start a payment the printer could not follow with a receipt.
///
/// **The gate, and the only one.** It sits here because `kiosk_pay` is the
/// single door from any of the four lanes to `START_PAYMENT`, and because the
/// screen must not be the thing enforcing it — React can be reloaded, and a
/// rule a reload can remove is not a rule.
///
/// It is deliberately *not* in the core. A device failure never changes a
/// sale, a cent or a unit of stock (CLAUDE.md), and the core keeps that
/// promise: the receipt is a separate obligation that comes after the money
/// and can fail without touching it (ADR 043). What this adds is a showroom
/// policy one level out — do not let a customer put cash in for a ticket that
/// cannot come out — and the right place for a policy about *this screen's*
/// customers is this screen's host.
///
/// Three conditions stop it, and the core decides which: the cover is open,
/// the roll has run out, or the printer is not answering. Paper near its end
/// does not. Neither does a printer nobody can ask: an installation with no
/// status channel knows nothing, and a rule that refuses every sale wherever
/// the question cannot be put is a rule that closes the shop.
pub(crate) fn require_a_printer_that_can_follow_through(
    bridge: &EdgeBridge,
) -> Result<(), BridgeError> {
    // Asked *now* rather than read from the core's cache: between the last
    // fifteen-second poll and this moment is exactly when somebody lifts the
    // cover to change the roll.
    let status = match bridge.client.printer_status(true) {
        Ok(status) => status,
        Err(error) => {
            // A runtime that drives no printer, or a core that cannot be
            // asked, does not get to refuse a customer over a printer.
            tracing::warn!(%error, "the printer could not be asked before a payment");
            return Ok(());
        }
    };
    if !status.blocks_payment {
        if status.paper == "NEAR_END" {
            // Worth a line in the log for whoever restocks, and nothing more:
            // the sale goes ahead.
            tracing::warn!("the receipt roll is near its end");
        }
        return Ok(());
    }
    tracing::warn!(
        state = %status.state,
        paper = %status.paper,
        cover_open = status.cover_open,
        source = %status.status_source,
        detail = status.status_error.as_deref().unwrap_or("-"),
        "a payment was refused because the printer cannot print"
    );
    Err(BridgeError {
        code: "PRINTER_UNAVAILABLE".to_owned(),
        message: edge_hardware::PrinterStatus::BLOCKED_MESSAGE.to_owned(),
    })
}

/// May a payment be started on this kiosk right now?
///
/// The same question `kiosk_pay` asks itself, exposed so the screen can ask
/// it *before* it puts a payment screen in front of a customer rather than
/// after. The screen asking is a courtesy; the refusal inside `kiosk_pay` is
/// the rule, and it stands whether or not anybody asked first.
#[tauri::command]
pub fn kiosk_payment_allowed(bridge: tauri::State<'_, EdgeBridge>) -> Result<(), BridgeError> {
    require_a_printer_that_can_follow_through(&bridge)
}

/// Take the money through whatever provider the composition root configured.
///
/// For the demo that is the deterministic simulator, which is **opt-in and
/// named**: nothing here pretends a sale was paid, and the amount comes from
/// the totals the core computed (ADR 020, ADR 038).
///
/// Nothing is sent to a provider before the printer has been asked whether a
/// receipt could follow; see
/// [`require_a_printer_that_can_follow_through`].
#[tauri::command]
pub fn kiosk_pay(
    bridge: tauri::State<'_, EdgeBridge>,
    tender_type: String,
    amount_minor: i64,
) -> Result<RetailTransactionV1, BridgeError> {
    require_a_printer_that_can_follow_through(&bridge)?;
    bridge
        .client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type,
                amount_minor,
            },
            None,
        )
        .map_err(BridgeError::from)?;
    bridge
        .client
        .finalize_transaction(None)
        .map_err(BridgeError::from)
}

#[tauri::command]
pub fn kiosk_receipt(
    bridge: tauri::State<'_, EdgeBridge>,
    transaction_id: String,
) -> Result<ReceiptV1, BridgeError> {
    bridge
        .client
        .get_receipt(&transaction_id)
        .map_err(BridgeError::from)
}

/// An asset's bytes as a `data:` URL the webview can put in an `<img>`.
///
/// Pulled over IPC in chunks and decoded here. The webview is never told a
/// filesystem path, and there is no remote URL to fall back to: if the bytes
/// are not local the image is simply absent, which is the honest answer
/// (ADR 045).
#[tauri::command]
pub fn kiosk_asset(
    bridge: tauri::State<'_, EdgeBridge>,
    cache: tauri::State<'_, AssetCache>,
    asset: AssetRefV1,
) -> Result<String, BridgeError> {
    if let Ok(cached) = cache.0.lock() {
        if let Some(url) = cached.get(&asset.content_hash) {
            return Ok(url.clone());
        }
    }

    let mut bytes = Vec::with_capacity(asset.byte_size.max(0) as usize);
    let mut offset = 0_i64;
    while offset < asset.byte_size {
        let chunk = bridge
            .client
            .read_asset(asset.clone(), offset, MAX_ASSET_CHUNK_BYTES as i64)
            .map_err(BridgeError::from)?;
        let slice = chunk.data().map_err(|error| BridgeError {
            code: "ASSET_DECODE_FAILED".to_owned(),
            message: error.to_string(),
        })?;
        if slice.is_empty() {
            break;
        }
        offset += slice.len() as i64;
        bytes.extend_from_slice(&slice);
    }

    let url = format!("data:{};base64,{}", asset.media_type, base64(&bytes));
    if let Ok(mut cached) = cache.0.lock() {
        cached.insert(asset.content_hash.clone(), url.clone());
    }
    Ok(url)
}

/// Standard base64, so the webview can read the bytes back out of a data URL.
///
/// Written here rather than pulled in as a dependency: it is twenty lines and
/// the only place in the repository that needs it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let b0 = u32::from(group[0]);
        let b1 = group.get(1).copied().map(u32::from);
        let b2 = group.get(2).copied().map(u32::from);
        let packed = (b0 << 16) | (b1.unwrap_or(0) << 8) | b2.unwrap_or(0);
        out.push(ALPHABET[((packed >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((packed >> 12) & 0x3f) as usize] as char);
        out.push(if b1.is_some() {
            ALPHABET[((packed >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if b2.is_some() {
            ALPHABET[(packed & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // The PNG signature, which every demo asset starts with.
        assert_eq!(base64(&[0x89, b'P', b'N', b'G']), "iVBORw==");
    }
}

// ===================================================================== admin

/// What the admin screen draws. One call; the core composes it.
#[tauri::command]
pub fn admin_printer_settings(
    bridge: tauri::State<'_, EdgeBridge>,
) -> Result<PrinterConfigurationV1, BridgeError> {
    bridge.client.printer_settings().map_err(BridgeError::from)
}

/// Store a choice. The core validates it and refuses what it cannot drive;
/// this forwards the answer verbatim.
#[tauri::command]
pub fn admin_save_printer_settings(
    bridge: tauri::State<'_, EdgeBridge>,
    settings: PrinterSettingsV1,
) -> Result<PrinterConfigurationV1, BridgeError> {
    bridge
        .client
        .set_printer_settings(settings, None)
        .map_err(BridgeError::from)
}

/// How the printer is, for the admin screen.
///
/// `refresh` asks the device now. The screen asks fresh when an operator
/// presses the button and cached while the panel is only being drawn, which
/// is the same distinction the payment path makes for the same reason: a
/// status read waits on a wire a receipt may be using.
#[tauri::command]
pub fn admin_printer_status(
    bridge: tauri::State<'_, EdgeBridge>,
    refresh: bool,
) -> Result<PrinterStatusV1, BridgeError> {
    bridge
        .client
        .printer_status(refresh)
        .map_err(BridgeError::from)
}

/// Put a diagnostic page out, with no sale involved.
#[tauri::command]
pub fn admin_test_print(
    bridge: tauri::State<'_, EdgeBridge>,
) -> Result<HardwareStatusV1, BridgeError> {
    bridge
        .client
        .print_test_page(None)
        .map_err(BridgeError::from)
}

/// Put the receipt for a finalized sale out.
///
/// Called by the screen after a sale closes when auto-print is on. It is a
/// **second, separate** act: the sale is already finalized and durable before
/// this runs, so a printer that is off, jammed or unplugged changes nothing
/// about it. The receipt obligation's own PRINTED / FAILED / UNKNOWN state is
/// the core's and is recorded either way (ADR 043).
#[tauri::command]
pub fn admin_print_receipt(
    bridge: tauri::State<'_, EdgeBridge>,
    transaction_id: String,
) -> Result<bool, BridgeError> {
    // Parsed here so a malformed id is refused at the boundary rather than
    // travelling as a string the core has to reject.
    Uuid::parse_str(&transaction_id).map_err(|error| BridgeError {
        code: "INVALID_TRANSACTION_ID".to_owned(),
        message: error.to_string(),
    })?;
    match bridge.client.print_transaction(
        PrintTransactionRequestV1 {
            transaction_id,
            reprint: false,
        },
        None,
    ) {
        Ok(_) => Ok(true),
        // Reported, not raised: the customer's order is paid for and the
        // screen must not show them a failure that is the shop's problem.
        Err(error) => {
            tracing::warn!(%error, "the receipt did not print");
            Ok(false)
        }
    }
}

/// Put the kiosk back to a clean demo state between visitors.
///
/// Deliberately small: it abandons whatever order is open and nothing else.
/// The catalog, the photographs and the printer settings are all untouched,
/// because none of them is what gets dirty during a day on a stand — a
/// half-finished order is. Safe to press with nothing open.
#[tauri::command]
pub fn admin_reset_demo(bridge: tauri::State<'_, EdgeBridge>) -> Result<(), BridgeError> {
    match bridge
        .client
        .cancel_transaction(Some("DEMO_RESET".to_owned()), None)
    {
        Ok(_) => Ok(()),
        // Nothing open is the state we were asked for, not a failure.
        Err(error) if format!("{error}").contains("NO_ACTIVE_TRANSACTION") => Ok(()),
        Err(error) => Err(BridgeError::from(error)),
    }
}
