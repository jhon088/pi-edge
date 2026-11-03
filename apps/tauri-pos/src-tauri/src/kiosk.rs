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

use serde::{Deserialize, Serialize};

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

/// Which products in the open sale cannot be paid for without permission.
///
/// Retail only. The counter brands have somebody standing behind them, the
/// flag is a retail concept in the brief, and a restaurant lane that started
/// refusing to take money because a stored flag mentioned one of its dishes
/// would be a surprise nobody asked for.
///
/// Answers the sale's id alongside, because an approval belongs to one sale
/// and the gate has nothing else to key it on.
pub(crate) fn restricted_in_the_open_sale(
    dir: &std::path::Path,
    bridge: &EdgeBridge,
) -> Option<(String, Vec<crate::authorization::AuthorizationProductV1>)> {
    if crate::showroom::stored(dir).vertical != crate::showroom::Vertical::Retail {
        return None;
    }
    let overrides = crate::overrides::stored(dir);
    let sale = bridge.client.get_active_transaction().ok()??;
    let mut restricted: Vec<crate::authorization::AuthorizationProductV1> = Vec::new();
    for line in &sale.lines {
        if !crate::overrides::needs_authorization(&overrides, &line.product_ref) {
            continue;
        }
        // One entry per product, not per line: two tins of the same thing is
        // one thing to come over about.
        if restricted
            .iter()
            .any(|seen| seen.product_ref == line.product_ref)
        {
            continue;
        }
        restricted.push(crate::authorization::AuthorizationProductV1 {
            product_ref: line.product_ref.clone(),
            // The line's own description, which is what the customer saw
            // when they scanned it.
            name: line.description.clone(),
        });
    }
    Some((sale.transaction_id, restricted))
}

/// Refuse to start a payment the lane is still waiting on permission for.
///
/// The second hard gate, beside the printer's. Raised *here* rather than when
/// the product is scanned, deliberately: a customer fills their basket
/// normally and finds out at the till, which is how a real self-checkout
/// behaves and is the behaviour the showroom is demonstrating.
pub(crate) fn require_an_authorization_if_the_basket_needs_one(
    dir: &std::path::Path,
    bridge: &EdgeBridge,
) -> Result<(), BridgeError> {
    let Some((sale_id, restricted)) = restricted_in_the_open_sale(dir, bridge) else {
        return Ok(());
    };
    if restricted.is_empty() {
        // The customer took it back out. The request goes with it — an
        // approval left pending for a basket that no longer needs one is one
        // a floor app would keep showing.
        crate::authorization::clear_unless(&sale_id);
        return Ok(());
    }
    let refs = restricted
        .iter()
        .map(|product| product.product_ref.clone())
        .collect();
    if crate::authorization::allows(&sale_id, &refs) {
        return Ok(());
    }
    let pending = crate::authorization::require(&sale_id, restricted);
    tracing::info!(
        request_id = %pending.request_id,
        sale_id = %pending.sale_id,
        products = pending.products.len(),
        "a payment is waiting for an authorization"
    );
    Err(BridgeError {
        code: "AUTHORIZATION_REQUIRED".to_owned(),
        message: crate::authorization::WAITING_MESSAGE.to_owned(),
    })
}

/// Why a payment cannot start, when it cannot.
///
/// Answered as a value rather than raised as an error, because the screen is
/// not handling a failure — it is deciding which screen to draw. The hard
/// refusals stay inside `kiosk_pay` and `cashgenic_pay`, which is what makes
/// this a courtesy rather than the rule.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentGateV1 {
    pub allowed: bool,
    /// `""`, `"PRINTER"` or `"AUTHORIZATION"`.
    pub blocked_by: String,
    /// What to put in front of the customer. `""` when allowed.
    pub message: String,
    /// The authorization the lane is waiting on, when that is the reason.
    pub authorization: crate::authorization::AuthorizationV1,
}

impl PaymentGateV1 {
    fn allowed() -> Self {
        Self {
            allowed: true,
            blocked_by: String::new(),
            message: String::new(),
            authorization: crate::authorization::AuthorizationV1::none(),
        }
    }
}

/// May a payment be started on this kiosk right now?
///
/// The same two questions `kiosk_pay` asks itself, exposed so the screen can
/// ask them *before* it puts a payment screen in front of a customer rather
/// than after. The printer comes first: a lane with no paper cannot serve a
/// customer whether or not somebody authorises the basket, and asking for a
/// PIN to then refuse over the printer would waste a member of staff's walk.
#[tauri::command]
pub fn kiosk_payment_allowed(
    app: tauri::AppHandle,
    bridge: tauri::State<'_, EdgeBridge>,
) -> PaymentGateV1 {
    if let Err(error) = require_a_printer_that_can_follow_through(&bridge) {
        return PaymentGateV1 {
            allowed: false,
            blocked_by: "PRINTER".to_owned(),
            message: error.message,
            authorization: crate::authorization::published(),
        };
    }
    let dir = match crate::showroom::data_dir(&app) {
        Ok(dir) => dir,
        // No data directory is no stored flags, so nothing is restricted.
        // The printer gate above already ran, which is the one that matters
        // on a kiosk whose own directory has gone.
        Err(_) => return PaymentGateV1::allowed(),
    };
    if let Err(error) = require_an_authorization_if_the_basket_needs_one(&dir, &bridge) {
        return PaymentGateV1 {
            allowed: false,
            blocked_by: "AUTHORIZATION".to_owned(),
            message: error.message,
            authorization: crate::authorization::published(),
        };
    }
    PaymentGateV1::allowed()
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
    app: tauri::AppHandle,
    bridge: tauri::State<'_, EdgeBridge>,
    tender_type: String,
    amount_minor: i64,
) -> Result<RetailTransactionV1, BridgeError> {
    require_a_printer_that_can_follow_through(&bridge)?;
    require_an_authorization_if_the_basket_needs_one(&crate::showroom::data_dir(&app)?, &bridge)?;
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
    let finalized = bridge
        .client
        .finalize_transaction(None)
        .map_err(BridgeError::from)?;
    // The sale is over, so whatever released it is too. An approval left
    // behind is one the next customer could inherit.
    crate::authorization::clear();
    Ok(finalized)
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

// ======================================================= products and prices
//
// Admin's product list is the **core's own menu** for the chosen channel, with
// this installation's overrides laid over it. There is deliberately no second
// catalog and no second list of products: the one thing a showroom must not
// grow is a place where a price is right and the till disagrees.

/// One row of the Admin product list.
#[derive(Debug, Clone, Serialize)]
pub struct AdminProductV1 {
    /// What every command names. The identity.
    pub product_ref: String,
    /// What a person reads off a shelf label. Empty when the catalog has
    /// none — the counter brands use one string for both.
    pub sku: String,
    pub name: String,
    pub category_ref: String,
    /// The aisle's own name, so the list reads like a menu rather than a
    /// table of keys.
    pub category_name: String,
    /// What the catalog in force charges, in minor units. **The core's
    /// number**, not the override — so a row that shows the new price is a
    /// row the till agrees with.
    pub price_minor: i64,
    /// The same amount, formatted by the core.
    pub price_formatted: String,
    /// Whether this experience offers it right now.
    ///
    /// Every row is `true`, and that is not a stub: this list *is* what the
    /// channel offers, because it is the channel's menu. A withheld product
    /// is not on it at all. The field exists because the floor app's contract
    /// asks for it and because a later build may withhold one.
    pub available: bool,
    /// Whether it cannot be paid for without somebody's permission. Retail
    /// only; the counter lanes ignore it.
    pub authorization_required: bool,
    /// Whether the price shown came from an override rather than the dataset.
    pub price_overridden: bool,
    pub image: Option<AssetRefV1>,
}

/// The real products of one experience, with this kiosk's edits laid over.
#[tauri::command]
pub fn admin_products(
    app: tauri::AppHandle,
    bridge: tauri::State<'_, EdgeBridge>,
    channel_ref: Option<String>,
) -> Result<Vec<AdminProductV1>, BridgeError> {
    let overrides = crate::overrides::stored(&crate::showroom::data_dir(&app)?);
    products_of(&bridge, channel_ref, &overrides)
}

/// Shared by the admin list and `GET /demo/products`, so the two can never
/// disagree about what this experience sells.
pub fn products_of(
    bridge: &EdgeBridge,
    channel_ref: Option<String>,
    overrides: &crate::overrides::Overrides,
) -> Result<Vec<AdminProductV1>, BridgeError> {
    let menu = bridge.client.menu(channel_ref).map_err(BridgeError::from)?;
    let aisles: HashMap<&str, &str> = menu
        .categories
        .iter()
        .map(|category| (category.category_ref.as_str(), category.name.as_str()))
        .collect();

    Ok(menu
        .items
        .iter()
        .map(|item| {
            let entry = overrides.get(&item.product_ref);
            AdminProductV1 {
                product_ref: item.product_ref.clone(),
                sku: item.sku.clone().unwrap_or_default(),
                name: item.name.clone(),
                category_ref: item.category_ref.clone().unwrap_or_default(),
                category_name: item
                    .category_ref
                    .as_deref()
                    .and_then(|reference| aisles.get(reference).copied())
                    .unwrap_or_default()
                    .to_owned(),
                price_minor: item.from_price.minor,
                price_formatted: item.from_price.formatted.clone(),
                available: true,
                authorization_required: entry.is_some_and(|entry| entry.authorization_required),
                // Whether an override exists, not whether it differs: an
                // operator who typed the dataset's own price still set it.
                price_overridden: entry.is_some_and(|entry| entry.price_minor.is_some()),
                image: item.image.clone(),
            }
        })
        .collect())
}

/// What Admin sends when it saves a batch of product edits.
#[derive(Debug, Clone, Deserialize)]
pub struct ProductEditV1 {
    pub product_ref: String,
    /// The new price in minor units, or `None` to go back to the dataset's.
    #[serde(default)]
    pub price_minor: Option<i64>,
    #[serde(default)]
    pub authorization_required: bool,
}

/// Store a batch of product edits and make the prices real.
///
/// **Batched on purpose.** Each price has to reach the catalog in force, and
/// the only honest way there is a new active snapshot built by the same
/// pipeline a release uses — which costs a second or two and a catalog
/// version. Eight prices adjusted in one sitting is one import, not eight.
///
/// The order matters and is the whole correctness of this command: the file is
/// written **first**, then the catalog is rebuilt from it. A reseed that ran
/// before the write would import the old prices, and a write that happened
/// without a reseed would leave a number on the screen that no sale charges.
/// If the reseed fails, the file is still right and the next attempt — or the
/// next launch — applies it.
///
/// `authorization_required` needs no reseed: it never reaches the core.
#[tauri::command]
pub fn admin_save_products(
    app: tauri::AppHandle,
    bridge: tauri::State<'_, EdgeBridge>,
    edits: Vec<ProductEditV1>,
) -> Result<Vec<AdminProductV1>, BridgeError> {
    let dir = crate::showroom::data_dir(&app)?;
    let mut overrides = crate::overrides::stored(&dir);

    let mut prices_changed = false;
    for edit in &edits {
        let before = overrides
            .get(&edit.product_ref)
            .cloned()
            .unwrap_or_default();
        if before.price_minor != edit.price_minor {
            prices_changed = true;
        }
        if let Some(minor) = edit.price_minor {
            if minor < 0 {
                return Err(BridgeError {
                    code: "PRICE_INVALID".to_owned(),
                    message: format!("{} cannot cost less than nothing", edit.product_ref),
                });
            }
        }
        overrides.insert(
            edit.product_ref.clone(),
            crate::overrides::ProductOverrideV1 {
                price_minor: edit.price_minor,
                authorization_required: edit.authorization_required,
            },
        );
    }
    crate::overrides::write(&dir, &overrides)?;

    if prices_changed {
        // The pipeline a release uses, against the dataset that shipped. The
        // new catalog is active by the time this returns, so the list the
        // screen re-reads below is already the one the till charges.
        crate::bootstrap::reseed(
            bridge.database(),
            &crate::bootstrap::bundled_demo(&app),
            Some(&crate::overrides::path(&dir)),
        )
        .map_err(|error| BridgeError {
            code: "CATALOG_REIMPORT_FAILED".to_owned(),
            message: error.to_string(),
        })?;
    }

    // Read back from the core, never from the edits: what the screen shows
    // next is what the catalog in force says, which is the only number worth
    // showing an operator.
    let channel = crate::showroom::stored(&dir).channel_ref;
    products_of(&bridge, Some(channel), &crate::overrides::stored(&dir))
}

// ============================================================= authorization

/// Set, or clear, the PIN that approves an authorization and drives the
/// control endpoints.
///
/// Clearing it is meaningful: with no PIN set nothing can be authorized and
/// the control endpoints answer that they are not configured. That is the
/// safe default and what a kiosk ships as.
#[tauri::command]
pub fn admin_set_pin(
    app: tauri::AppHandle,
    pin: String,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let pin = pin.trim().to_owned();
    if !pin.is_empty() && (pin.len() < 4 || !pin.chars().all(|c| c.is_ascii_digit())) {
        return Err(BridgeError {
            code: "PIN_INVALID".to_owned(),
            message: "a PIN is at least four digits, or empty to remove it".to_owned(),
        });
    }
    let dir = crate::showroom::data_dir(&app)?;
    let mut preferences = crate::showroom::stored(&dir);
    preferences.admin_pin = pin;
    crate::showroom::write(&dir, &preferences)?;
    Ok(preferences)
}

/// Let the control endpoints answer, or stop them answering.
#[tauri::command]
pub fn admin_set_remote_control(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let dir = crate::showroom::data_dir(&app)?;
    let mut preferences = crate::showroom::stored(&dir);
    if enabled && preferences.admin_pin.trim().is_empty() {
        return Err(BridgeError {
            code: "PIN_REQUIRED".to_owned(),
            message: "set a PIN before letting this kiosk be controlled remotely".to_owned(),
        });
    }
    preferences.remote_control = enabled;
    crate::showroom::write(&dir, &preferences)?;
    Ok(preferences)
}

/// Approve the pending authorization with a PIN typed on this screen.
#[tauri::command]
pub fn authorization_approve(
    app: tauri::AppHandle,
    request_id: String,
    pin: String,
) -> Result<crate::authorization::AuthorizationV1, BridgeError> {
    let preferences = crate::showroom::stored(&crate::showroom::data_dir(&app)?);
    crate::authorization::approve(&request_id, preferences.pin_matches(&pin)).map_err(|refusal| {
        BridgeError {
            code: refusal.code().to_owned(),
            message: refusal.detail().to_owned(),
        }
    })
}

/// What, if anything, the lane is waiting on.
#[tauri::command]
pub fn authorization_pending() -> crate::authorization::AuthorizationV1 {
    crate::authorization::published()
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod showroom_tests {
    use super::*;
    use crate::overrides::ProductOverrideV1;
    use crate::showroom::{KioskPreferencesV1, Vertical};
    use edge_protocol::{AddItemRequestV1, SubmitScanRequestV1};

    /// A daemon with the demo seeded, and a data directory beside it.
    ///
    /// The real thing: the catalog these tests read is the one `demo-seed`
    /// imports, so a product list that passes here is the product list an
    /// operator sees.
    struct Kiosk {
        bridge: EdgeBridge,
        dir: tempfile::TempDir,
        _core: crate::bootstrap::CoreProcess,
    }

    impl Kiosk {
        fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let database = dir.path().join("pi-edge.db");
            let core = crate::bootstrap::CoreProcess::default();
            crate::bootstrap::ensure_core(&database, &core).expect("a daemon");
            let demo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../demo");
            crate::bootstrap::ensure_demo(&database, &demo, None).expect("the demo");
            Self {
                bridge: EdgeBridge::connect(database).expect("a bridge"),
                dir,
                _core: core,
            }
        }

        /// Put this kiosk on one experience, as Admin would.
        fn as_lane(&self, vertical: Vertical) {
            let preferences = KioskPreferencesV1 {
                vertical,
                channel_ref: vertical.channel().to_owned(),
                ..KioskPreferencesV1::default()
            };
            crate::showroom::write(self.dir.path(), &preferences).unwrap();
        }

        fn restrict(&self, product_ref: &str) {
            let mut overrides = crate::overrides::stored(self.dir.path());
            overrides.insert(
                product_ref.to_owned(),
                ProductOverrideV1 {
                    price_minor: None,
                    authorization_required: true,
                },
            );
            crate::overrides::write(self.dir.path(), &overrides).unwrap();
        }

        fn products(&self, vertical: Vertical) -> Vec<AdminProductV1> {
            products_of(
                &self.bridge,
                Some(vertical.channel().to_owned()),
                &crate::overrides::stored(self.dir.path()),
            )
            .expect("a product list")
        }
    }

    impl Drop for Kiosk {
        fn drop(&mut self) {
            self._core.shutdown();
        }
    }

    /// Every experience lists its own real products, and nobody else's.
    ///
    /// The counts are the acceptance numbers, so this fails if a dataset
    /// changes shape without anybody saying so.
    #[test]
    fn each_experience_lists_its_own_products_with_a_price_and_an_aisle() {
        let kiosk = Kiosk::start();
        for (vertical, expected, prefix) in [
            (Vertical::Restaurant, 12, "C-"),
            (Vertical::TimHortons, 13, "T-"),
            (Vertical::Sbarro, 14, "S-"),
            (Vertical::Retail, 22, "R-"),
        ] {
            let products = kiosk.products(vertical);
            assert_eq!(
                products.len(),
                expected,
                "{:?} listed {} products",
                vertical,
                products.len()
            );
            for product in &products {
                assert!(
                    !product.name.is_empty(),
                    "{} has no name",
                    product.product_ref
                );
                assert!(
                    product.category_ref.starts_with(prefix),
                    "`{}` is in `{}`, which belongs to another brand",
                    product.product_ref,
                    product.category_ref
                );
                // The aisle's own name, not just its key.
                assert!(
                    !product.category_name.is_empty(),
                    "{} shows no category name",
                    product.product_ref
                );
                assert!(product.price_minor > 0, "{} is free", product.product_ref);
                assert!(!product.price_formatted.is_empty());
                assert!(product.available, "a listed product was unavailable");
                // Nothing is edited on a fresh installation.
                assert!(!product.price_overridden);
                assert!(!product.authorization_required);
            }
        }
    }

    /// Retail carries its own SKUs; the counter brands name products once.
    #[test]
    fn a_retail_product_shows_the_shops_own_code() {
        let kiosk = Kiosk::start();
        for product in kiosk.products(Vertical::Retail) {
            assert!(
                product.sku.starts_with("SKU-RT-"),
                "`{}` shows `{}` as its code",
                product.product_ref,
                product.sku
            );
            assert_ne!(product.sku, product.product_ref);
        }
        // Tim names a product once, so the code is the ref and the list still
        // has something to show.
        for product in kiosk.products(Vertical::TimHortons) {
            assert_eq!(product.sku, product.product_ref);
        }
    }

    /// An edit shows up in the list as an edit, and the price shown is the
    /// core's rather than the file's.
    #[test]
    fn the_list_marks_an_edited_price_and_shows_what_the_core_charges() {
        let kiosk = Kiosk::start();
        let before = kiosk
            .products(Vertical::TimHortons)
            .into_iter()
            .find(|product| product.product_ref == "T-LATTE")
            .expect("a latte");
        assert!(!before.price_overridden);

        // An override the catalog has not been rebuilt for yet. The list must
        // show what the *core* charges, not what the file hopes.
        let mut overrides = crate::overrides::stored(kiosk.dir.path());
        overrides.insert(
            "T-LATTE".to_owned(),
            ProductOverrideV1 {
                price_minor: Some(9_900),
                authorization_required: false,
            },
        );
        crate::overrides::write(kiosk.dir.path(), &overrides).unwrap();

        let after = kiosk
            .products(Vertical::TimHortons)
            .into_iter()
            .find(|product| product.product_ref == "T-LATTE")
            .expect("a latte");
        assert!(after.price_overridden, "the edit is not marked");
        assert_eq!(
            after.price_minor, before.price_minor,
            "the list showed a price the catalog does not charge yet"
        );

        // Rebuild, and now the two agree.
        crate::bootstrap::reseed(
            kiosk.bridge.database(),
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../demo"),
            Some(&crate::overrides::path(kiosk.dir.path())),
        )
        .expect("the reimport");
        let rebuilt = kiosk
            .products(Vertical::TimHortons)
            .into_iter()
            .find(|product| product.product_ref == "T-LATTE")
            .expect("a latte");
        assert_eq!(rebuilt.price_minor, 9_900);
        assert!(rebuilt.price_overridden);
    }

    /// The flag shows up on the retail list, and is a retail concept.
    #[test]
    fn the_authorization_flag_is_shown_on_the_retail_shelf() {
        let kiosk = Kiosk::start();
        kiosk.restrict("R-ARRACHERA");
        let flagged: Vec<String> = kiosk
            .products(Vertical::Retail)
            .into_iter()
            .filter(|product| product.authorization_required)
            .map(|product| product.product_ref)
            .collect();
        assert_eq!(flagged, ["R-ARRACHERA"]);
    }

    // ------------------------------------------------------------ the gate

    /// Scanning is never blocked; paying is.
    #[test]
    fn a_restricted_product_scans_freely_and_stops_the_payment() {
        let kiosk = Kiosk::start();
        kiosk.as_lane(Vertical::Retail);
        kiosk.restrict("R-ARRACHERA");
        let _turn = crate::authorization::turn();
        crate::authorization::clear();

        // The customer fills the basket. Nothing refuses them.
        kiosk.bridge.client.start_transaction(None).unwrap();
        kiosk
            .bridge
            .client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef("R-COCA-3L".to_owned()),
                    quantity_thousandths: 1_000,
                    selections: Vec::new(),
                    preparation_note: None,
                },
                None,
            )
            .expect("an ordinary product");
        let sale = kiosk
            .bridge
            .client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef("R-ARRACHERA".to_owned()),
                    quantity_thousandths: 1_000,
                    selections: Vec::new(),
                    preparation_note: None,
                },
                None,
            )
            .expect("a restricted product still scans");
        assert_eq!(sale.lines.len(), 2);

        // Paying does not.
        let refused =
            require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
                .expect_err("the payment was allowed to start");
        assert_eq!(refused.code, "AUTHORIZATION_REQUIRED");
        assert!(refused.message.contains("requiere autorización"));

        // And the lane publishes what it is waiting on.
        let waiting = crate::authorization::published();
        assert!(waiting.required);
        assert!(!waiting.approved);
        assert_eq!(waiting.sale_id, sale.transaction_id);
        assert_eq!(waiting.products.len(), 1, "{:?}", waiting.products);
        assert_eq!(waiting.products[0].product_ref, "R-ARRACHERA");
        assert!(!waiting.products[0].name.is_empty());

        // A PIN releases it, and only this sale.
        crate::authorization::approve(&waiting.request_id, true).unwrap();
        require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
            .expect("the approval did not release the payment");
        // The turnstile is dropped with the test; nothing else clears here.
        // Taking it a second time would deadlock — it is an ordinary
        // `Mutex`, and an ordinary `Mutex` is not reentrant.
    }

    /// Put a restricted product in an open basket and stop at the till.
    ///
    /// The state every test below starts from: a sale that cannot be paid for
    /// until somebody says so, and the request the lane published about it.
    fn a_basket_waiting_for_permission(
        kiosk: &Kiosk,
    ) -> (String, crate::authorization::AuthorizationV1) {
        kiosk.as_lane(Vertical::Retail);
        kiosk.restrict("R-ARRACHERA");
        crate::authorization::clear();
        kiosk.bridge.client.start_transaction(None).unwrap();
        let sale = kiosk
            .bridge
            .client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef("R-ARRACHERA".to_owned()),
                    quantity_thousandths: 1_000,
                    selections: Vec::new(),
                    preparation_note: None,
                },
                None,
            )
            .expect("a restricted product still scans");
        require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
            .expect_err("the payment was not held");
        (sale.transaction_id, crate::authorization::published())
    }

    /// Whether the lane would let a payment start right now.
    ///
    /// The same question `kiosk_payment_allowed` answers for the screen, and
    /// the same one `kiosk_pay` refuses on. Asked without a window, which is
    /// the only reason it is spelled out here.
    fn payment_is_allowed(kiosk: &Kiosk) -> bool {
        require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge).is_ok()
    }

    /// A PIN typed on the glass.
    #[test]
    fn a_local_approval_releases_the_open_sale() {
        let kiosk = Kiosk::start();
        let _turn = crate::authorization::turn();
        let (_sale_id, waiting) = a_basket_waiting_for_permission(&kiosk);

        crate::authorization::approve(&waiting.request_id, true).unwrap();

        assert!(payment_is_allowed(&kiosk), "a PIN did not release the lane");
    }

    /// The bug this exists for: `POST /demo/control/authorization` released
    /// the basket and the lane stayed held, because nothing re-asked.
    ///
    /// The host half, proven here: the remote route releases **this** sale
    /// through the same state the screen's gate reads. What was missing was
    /// only the screen asking again — see `afterAuthorizationRecheck`.
    #[test]
    fn a_remote_approval_releases_that_same_open_sale() {
        let kiosk = Kiosk::start();
        let _turn = crate::authorization::turn();
        let (sale_id, waiting) = a_basket_waiting_for_permission(&kiosk);
        assert!(!payment_is_allowed(&kiosk));

        // Exactly what the control route does with a correct PIN, and nothing
        // else: no second command, nobody touching the kiosk.
        let approved = crate::authorization::approve(&waiting.request_id, true)
            .expect("the remote approval was refused");

        assert_eq!(approved.sale_id, sale_id);
        assert_eq!(approved.request_id, waiting.request_id);
        assert!(approved.approved);
        assert!(
            payment_is_allowed(&kiosk),
            "the lane stayed held after a remote approval"
        );
    }

    /// A floor app approving a request the basket has already replaced.
    #[test]
    fn a_remote_approval_with_a_stale_request_id_releases_nothing() {
        let kiosk = Kiosk::start();
        let _turn = crate::authorization::turn();
        let (_sale_id, first) = a_basket_waiting_for_permission(&kiosk);

        // The customer adds a second restricted product, so the lane mints a
        // new request and the one the floor is holding is no longer the one
        // waiting.
        kiosk.restrict("R-COCA-3L");
        kiosk
            .bridge
            .client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef("R-COCA-3L".to_owned()),
                    quantity_thousandths: 1_000,
                    selections: Vec::new(),
                    preparation_note: None,
                },
                None,
            )
            .expect("a second restricted product");
        require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
            .expect_err("the bigger basket was not held");
        let second = crate::authorization::published();
        assert_ne!(second.request_id, first.request_id);

        let refused = crate::authorization::approve(&first.request_id, true)
            .expect_err("a stale request id was honoured");
        assert_eq!(refused, crate::authorization::RefusedBecause::WrongRequest);
        assert!(
            !payment_is_allowed(&kiosk),
            "a stale approval released the lane"
        );
    }

    /// An approval that belongs to a different sale.
    #[test]
    fn a_remote_approval_for_another_sale_releases_nothing() {
        let kiosk = Kiosk::start();
        let _turn = crate::authorization::turn();
        let (sale_id, _waiting) = a_basket_waiting_for_permission(&kiosk);

        // Another lane's request, raised over this one and approved. The id is
        // right for *that* request, and the sale is not this sale.
        let elsewhere = crate::authorization::require(
            "sale-from-another-lane",
            vec![crate::authorization::AuthorizationProductV1 {
                product_ref: "R-ARRACHERA".to_owned(),
                name: "Arrachera".to_owned(),
            }],
        );
        assert_ne!(elsewhere.sale_id, sale_id);
        crate::authorization::approve(&elsewhere.request_id, true)
            .expect("that request was the one waiting");

        assert!(
            !payment_is_allowed(&kiosk),
            "an approval for another sale released this one"
        );
    }

    /// The rule that must survive the fix: a basket that grows past what was
    /// approved asks again, however it was approved.
    #[test]
    fn another_restricted_product_after_a_remote_approval_holds_the_lane_again() {
        let kiosk = Kiosk::start();
        let _turn = crate::authorization::turn();
        let (_sale_id, waiting) = a_basket_waiting_for_permission(&kiosk);
        crate::authorization::approve(&waiting.request_id, true).unwrap();
        assert!(payment_is_allowed(&kiosk));

        kiosk.restrict("R-COCA-3L");
        kiosk
            .bridge
            .client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef("R-COCA-3L".to_owned()),
                    quantity_thousandths: 1_000,
                    selections: Vec::new(),
                    preparation_note: None,
                },
                None,
            )
            .expect("a second restricted product");

        assert!(
            !payment_is_allowed(&kiosk),
            "a basket that grew past its approval was still payable"
        );
        let again = crate::authorization::published();
        assert_ne!(again.request_id, waiting.request_id);
        assert!(!again.approved);
    }

    /// One authority, read by both.
    ///
    /// The screen's gate and `GET /demo/status` are the same two fields of the
    /// same in-memory state. If these ever disagree, a floor app and the lane
    /// in front of a customer are telling two different stories.
    #[test]
    fn the_screen_and_the_floor_endpoint_read_one_authorization_state() {
        let kiosk = Kiosk::start();
        let _turn = crate::authorization::turn();
        let (sale_id, _waiting) = a_basket_waiting_for_permission(&kiosk);

        // Held: the floor is told somebody has to come over.
        let on_the_wire = crate::authorization::status();
        assert_eq!(on_the_wire["required"], true);
        assert_eq!(on_the_wire["approved"], false);
        assert_eq!(on_the_wire["sale_id"], sale_id);
        assert!(!payment_is_allowed(&kiosk));

        let request_id = on_the_wire["request_id"].as_str().unwrap().to_owned();
        crate::authorization::approve(&request_id, true).unwrap();

        // Released: the same state, read again by both.
        let after = crate::authorization::status();
        assert_eq!(after["required"], true);
        assert_eq!(after["approved"], true);
        assert_eq!(after["request_id"], request_id);
        assert!(payment_is_allowed(&kiosk));
    }

    /// A basket with nothing restricted in it is never held up.
    #[test]
    fn an_ordinary_basket_is_never_held_up() {
        let kiosk = Kiosk::start();
        kiosk.as_lane(Vertical::Retail);
        kiosk.restrict("R-ARRACHERA");
        let _turn = crate::authorization::turn();
        crate::authorization::clear();

        kiosk.bridge.client.start_transaction(None).unwrap();
        kiosk
            .bridge
            .client
            .submit_scan(SubmitScanRequestV1 {
                event_id: uuid::Uuid::new_v4().to_string(),
                barcode: "7500000000015".to_owned(),
                device_id: None,
            })
            .expect("a coca scans");
        require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
            .expect("an ordinary basket was held up");
        assert!(!crate::authorization::published().required);
    }

    /// The flag is a retail concept. A counter lane with the same flag stored
    /// against one of its dishes sells it without asking anybody.
    #[test]
    fn a_counter_lane_ignores_the_flag_entirely() {
        let kiosk = Kiosk::start();
        kiosk.as_lane(Vertical::TimHortons);
        kiosk.restrict("T-LATTE");
        let _turn = crate::authorization::turn();
        crate::authorization::clear();

        kiosk.bridge.client.start_transaction(None).unwrap();
        assert!(
            restricted_in_the_open_sale(kiosk.dir.path(), &kiosk.bridge).is_none(),
            "a counter lane consulted the retail flag"
        );
        require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
            .expect("a counter lane was held up by a retail flag");
    }

    /// Taking the restricted product back out clears the hold.
    #[test]
    fn removing_the_restricted_product_clears_what_the_lane_was_waiting_on() {
        let kiosk = Kiosk::start();
        kiosk.as_lane(Vertical::Retail);
        kiosk.restrict("R-ARRACHERA");
        let _turn = crate::authorization::turn();
        crate::authorization::clear();

        kiosk.bridge.client.start_transaction(None).unwrap();
        let sale = kiosk
            .bridge
            .client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef("R-ARRACHERA".to_owned()),
                    quantity_thousandths: 1_000,
                    selections: Vec::new(),
                    preparation_note: None,
                },
                None,
            )
            .unwrap();
        assert!(
            require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
                .is_err()
        );
        assert!(crate::authorization::published().required);

        kiosk
            .bridge
            .client
            .remove_item(
                edge_protocol::RemoveItemRequestV1 {
                    line_number: sale.lines[0].line_number,
                },
                None,
            )
            .unwrap();
        require_an_authorization_if_the_basket_needs_one(kiosk.dir.path(), &kiosk.bridge)
            .expect("the hold outlived the product");
        assert!(
            !crate::authorization::published().required,
            "a floor app would still be showing a hold nobody is waiting on"
        );
    }
}
