//! The kiosk journey over real IPC, against the real demo dataset.
//!
//! This is the flow the Tauri screens drive, call for call: seed the
//! restaurant offline, read the menu, configure a burger, add it, read its
//! photograph out of the local asset store, pay by card and get an order
//! number. If this passes, the screens have a core to stand on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

use common::{client, DaemonProcess};
use edge_client::EdgeClient;
use edge_protocol::{
    AddItemRequestV1, PrintTransactionRequestV1, ProductSelectorV1, RequestedSelectionV1,
    StartPaymentRequestV1, TransactionRequestV1, MAX_ASSET_CHUNK_BYTES,
};

const CHANNEL: &str = "KIOSK";

/// Simulated hardware, asked for explicitly: a test that uses fake devices
/// says so out loud (ADR 038).
const SIMULATED: [(&str, &str); 2] = [
    ("PI_EDGE_PRINTER", "simulator"),
    ("PI_EDGE_DRAWER", "simulator"),
];

/// Run the one-command offline bootstrap: assets, catalog, activation, till.
fn seed(database: &std::path::Path) -> serde_json::Value {
    let output = Command::new(common::cli_binary())
        .arg("--db")
        .arg(database)
        .arg("--format")
        .arg("json")
        .arg("demo-seed")
        .arg("--path")
        .arg(demo_dir())
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "demo-seed failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("the CLI prints JSON")
}

fn demo_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("demo")
}

fn chose(option_id: &str, quantity: i64) -> RequestedSelectionV1 {
    RequestedSelectionV1 {
        option_id: option_id.to_owned(),
        quantity,
    }
}

#[test]
fn the_demo_restaurant_seeds_itself_offline_and_serves_a_configured_sale() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    // --- bootstrap, with no network anywhere in it ------------------------
    let report = seed(&database);
    assert_eq!(report["activated"], true);
    assert_eq!(report["session_opened"], true);
    assert_eq!(report["channel"], CHANNEL);
    // One file per stem: twelve dishes plus three option photographs, and
    // the twenty-two retail packshots, because one installation seeds both
    // verticals into one snapshot and tells them apart by channel (ADR 044).
    // Category tiles and the attract banner reuse product stems, so they add
    // none.
    assert_eq!(report["assets_imported"].as_i64().unwrap(), 15 + 22);

    let client = client(&database, "tauri-pos");

    // --- the menu a screen draws -----------------------------------------
    let menu = client.menu(Some(CHANNEL.to_owned())).unwrap();
    assert_eq!(menu.currency, "MXN");
    assert_eq!(menu.categories.len(), 4);
    assert_eq!(
        menu.categories[0].name, "Hamburguesas",
        "categories come back in display order"
    );
    assert_eq!(
        menu.items.len(),
        12,
        "only products with presentation are on the menu; components are not"
    );
    let burger = menu
        .items
        .iter()
        .find(|item| item.product_ref == "SMASH-CLASSIC")
        .expect("the hero burger is on the menu");
    assert_eq!(burger.from_price.minor, 12_900);
    assert_eq!(burger.from_price.formatted, "129.00");
    assert!(burger.configurable);
    assert!(burger.image.is_some());
    assert_eq!(menu.promotions.len(), 2, "both demo promotions are live");

    // Every dish on the menu carries a picture, and the runtime holds more
    // than one kind: two of them are animated.
    for item in &menu.items {
        let image = item
            .image
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no picture", item.product_ref));
        assert!(
            matches!(image.media_type.as_str(), "image/jpeg" | "image/gif"),
            "{} carries `{}`, which no screen can draw",
            item.product_ref,
            image.media_type
        );
    }

    // --- the photograph, from local storage, with no network --------------
    let asset = burger.image.clone().unwrap();
    let chunk = client
        .read_asset(asset.clone(), 0, MAX_ASSET_CHUNK_BYTES as i64)
        .unwrap();
    let bytes = chunk.data().unwrap();
    assert_eq!(bytes.len() as i64, asset.byte_size);
    assert_eq!(
        &bytes[..3],
        &[0xff, 0xd8, 0xff],
        "the bytes really are the JPEG photograph the catalog declared"
    );

    // --- the questions the burger asks -----------------------------------
    let configuration = client
        .product_configuration("SMASH-CLASSIC", Some(CHANNEL.to_owned()))
        .unwrap();
    assert_eq!(configuration.base_price.minor, 12_900);
    assert_eq!(configuration.groups.len(), 3);
    let size = &configuration.groups[0];
    assert_eq!(size.name, "¿Cómo la quieres?");
    assert_eq!((size.min_quantity, size.max_quantity), (1, 1));
    let double = size
        .options
        .iter()
        .find(|option| option.option_id == "O-CL-DOUBLE")
        .unwrap();
    assert_eq!(double.unit_price.minor, 4_500);

    // --- a configured burger ---------------------------------------------
    client.start_transaction(None).unwrap();
    let sale = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
                quantity_thousandths: 1_000,
                selections: vec![
                    chose("O-CL-DOUBLE", 1),
                    chose("O-CL-CHEESE", 1),
                    chose("O-CL-BACON", 2),
                    chose("O-CL-NOONION", 1),
                ],
                preparation_note: Some("Bien cocida".to_owned()),
            },
            None,
        )
        .unwrap();

    let line = &sale.lines[0];
    assert_eq!(line.base_unit_price.minor, 12_900);
    // 129.00 + 45.00 + 15.00 + 2 × 22.00 = 233.00
    assert_eq!(line.configured_unit_price.minor, 23_300);
    assert_eq!(line.selections.len(), 4);
    assert_eq!(line.preparation_note.as_deref(), Some("Bien cocida"));
    assert!(line
        .selections
        .iter()
        .any(|s| s.name == "Sin cebolla" && !s.chargeable));
    // The 25% promotion found the configured line on its own.
    assert_eq!(line.discount_amount.minor, 5_825);
    assert_eq!(sale.applied_promotions[0].code, "SMASH25");

    // --- a nested question, through the combo -----------------------------
    let combo = client
        .product_configuration("COMBO-SMASH", Some(CHANNEL.to_owned()))
        .unwrap();
    let nested = combo
        .groups
        .iter()
        .find(|group| group.owner_option_id.as_deref() == Some("O-CO-SODA"))
        .expect("the drink size only appears under the soft drink");
    assert_eq!(nested.name, "Tamaño de la bebida");
    let sale = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("COMBO-SMASH".to_owned()),
                quantity_thousandths: 1_000,
                selections: vec![
                    chose("O-CO-FRIES", 1),
                    chose("O-CO-SODA", 1),
                    chose("O-CO-LARGE", 1),
                ],
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(sale.lines[1].configured_unit_price.minor, 18_900 + 1_500);

    // --- the cart's running total, computed by the core -------------------
    assert!(sale.running_totals.grand_total.minor > 0);
    assert_eq!(
        sale.totals.grand_total.minor, 0,
        "the official totals wait for TOTALIZE"
    );
    // The basket threshold promotion fires too, because the basket is over
    // $250: two promotions, both stackable, both the engine's decision.
    assert_eq!(sale.applied_promotions.len(), 2);

    // --- pay and close -----------------------------------------------------
    let totalized = client.totalize(None).unwrap();
    let due = totalized.totals.grand_total.minor;
    assert_eq!(due, totalized.running_totals.grand_total.minor);
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CARD".to_owned(),
                amount_minor: due,
            },
            None,
        )
        .unwrap();
    let finalized = client.finalize_transaction(None).unwrap();
    assert_eq!(finalized.status, "FINALIZED");
    assert_eq!(finalized.order_code, "K01-0001");
    assert_eq!(finalized.balance_due.minor, 0);

    // --- the receipt a kiosk prints ---------------------------------------
    let receipt = client.get_receipt(&finalized.transaction_id).unwrap();
    assert_eq!(receipt.transaction_id, finalized.transaction_id);

    // --- and the next order gets the next number --------------------------
    client.start_transaction(None).unwrap();
    let next = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("FRIES".to_owned()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(next.order_code, "K01-0002");
}

#[test]
fn the_kiosk_cannot_sell_a_burger_without_answering_its_questions() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    client.start_transaction(None).unwrap();
    let refused = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .expect_err("a required question is a required question");
    assert!(
        format!("{refused}").contains("CATALOG_SELECTION_REQUIRED"),
        "got {refused}"
    );

    // And a cancelled order leaves nothing behind, which is what the idle
    // timeout does when a customer walks away.
    client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
                quantity_thousandths: 1_000,
                selections: vec![chose("O-CL-SINGLE", 1)],
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    let cancelled = client
        .cancel_transaction(Some("KIOSK_ABANDONED".to_owned()), None)
        .unwrap();
    assert_eq!(cancelled.status, "CANCELLED");
    assert!(client.get_active_transaction().unwrap().is_none());

    // A fresh order starts clean.
    let fresh = client.start_transaction(None).unwrap();
    assert!(fresh.lines.is_empty());
}

/// The bug that blacked out the cart, pinned where it happened.
///
/// A product that asks no questions used to arrive with `selections` missing
/// entirely, because the field was skipped when empty. The cart screen read
/// `.length` on it, threw while rendering, and React unmounted the whole
/// tree — a black screen with no message. The contract now always sends the
/// list (ADR 046).
#[test]
fn every_line_the_cart_renders_carries_the_fields_the_contract_promises() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    client.start_transaction(None).unwrap();
    // An ordinary product: no groups, so nothing is ever chosen for it.
    client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("FRIES".to_owned()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    // And a configured one, so both shapes are in the same cart.
    let sale = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
                quantity_thousandths: 1_000,
                selections: vec![chose("O-CL-SINGLE", 1), chose("O-CL-NOONION", 1)],
                preparation_note: None,
            },
            None,
        )
        .unwrap();

    // Exactly what the webview receives.
    let wire = serde_json::to_value(&sale).unwrap();
    let lines = wire["lines"].as_array().expect("lines is a list");
    assert_eq!(lines.len(), 2);
    for line in lines {
        for field in [
            "selections",
            "preparation_note",
            "base_unit_price",
            "configured_unit_price",
            "line_total",
            "extended_amount",
            "discount_amount",
            "quantity_thousandths",
        ] {
            assert!(
                line.get(field).is_some(),
                "`{field}` must be on every line the cart renders: {line}"
            );
        }
        assert!(
            line["selections"].is_array(),
            "`selections` is always a list, empty or not: {line}"
        );
    }
    assert_eq!(lines[0]["selections"].as_array().unwrap().len(), 0);
    assert_eq!(lines[1]["selections"].as_array().unwrap().len(), 2);

    // And the numbers the cart puts in its totals block.
    for field in ["subtotal", "discount_total", "tax_total", "grand_total"] {
        assert!(wire["running_totals"].get(field).is_some(), "{field}");
    }
    assert!(wire.get("order_code").is_some());
}

/// Cash and card are the same sale, taken through the same payment engine.
///
/// The screen asks which; the core records which. Nothing about a second
/// tender needed a second lifecycle (ADR 046).
#[test]
fn a_kiosk_order_can_be_paid_in_cash_or_by_card_and_the_sale_says_which() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    for (tender, product) in [("CASH", "FRIES"), ("CARD", "NUGGETS")] {
        client.start_transaction(None).unwrap();
        client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef(product.to_owned()),
                    quantity_thousandths: 1_000,
                    selections: Vec::new(),
                    preparation_note: None,
                },
                None,
            )
            .unwrap();
        let totalized = client.totalize(None).unwrap();
        client
            .start_payment(
                StartPaymentRequestV1 {
                    tender_type: tender.to_owned(),
                    amount_minor: totalized.totals.grand_total.minor,
                },
                None,
            )
            .unwrap();
        let finalized = client.finalize_transaction(None).unwrap();
        assert_eq!(finalized.status, "FINALIZED");
        assert_eq!(finalized.balance_due.minor, 0);

        // The tender is on the sale's payments, not on a screen's memory.
        let payments = client
            .get_transaction_payments(TransactionRequestV1 {
                transaction_id: finalized.transaction_id.clone(),
            })
            .unwrap();
        assert_eq!(payments.payments.len(), 1);
        assert_eq!(
            payments.payments[0].tender_type, tender,
            "the sale records how it was paid"
        );
    }
}

/// What "Completa tu pedido" has to work from.
///
/// The suggestions are the catalog's own: each product carries the refs it
/// recommends, in the order to offer them, and the core narrows that list to
/// products the same menu actually shows. No product reference and no
/// category rule is written into a screen.
#[test]
fn the_menu_carries_what_the_upsell_rail_needs() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let menu = client.menu(Some(CHANNEL.to_owned())).unwrap();
    let shown: std::collections::BTreeSet<&str> = menu
        .items
        .iter()
        .map(|item| item.product_ref.as_str())
        .collect();

    // Every dish sells something alongside it, and says so itself.
    for item in &menu.items {
        assert!(
            !item.recommended_product_refs.is_empty(),
            "`{}` recommends nothing, so it has no upsell to offer",
            item.product_ref
        );
        assert!(
            !item.recommended_product_refs.contains(&item.product_ref),
            "`{}` recommends itself",
            item.product_ref
        );
        for recommended in &item.recommended_product_refs {
            // The core drops anything this menu cannot render, so a screen
            // never has to check whether a suggestion is showable.
            assert!(
                shown.contains(recommended.as_str()),
                "`{}` recommends `{recommended}`, which is not on this menu",
                item.product_ref
            );
        }
    }

    // The burger's list is the catalog's, in the catalog's order: fries
    // first, and every one of them has a picture and a price to show.
    let burger = menu
        .items
        .iter()
        .find(|item| item.product_ref == "SMASH-CLASSIC")
        .expect("the hero burger is on the menu");
    assert_eq!(
        burger.recommended_product_refs,
        vec![
            "FRIES".to_owned(),
            "SODA".to_owned(),
            "SHAKE-CHOCO".to_owned(),
            "NUGGETS".to_owned(),
            "MOZZA-STICKS".to_owned(),
            "CERVEZA".to_owned()
        ],
        "declared order is merchandising, not an implementation detail"
    );
    for recommended in &burger.recommended_product_refs {
        let item = menu
            .items
            .iter()
            .find(|candidate| &candidate.product_ref == recommended)
            .unwrap();
        assert!(
            item.image.is_some(),
            "`{recommended}` needs a photograph to be worth offering"
        );
        assert!(item.from_price.minor > 0);
    }
    // At least one suggestion adds in a single tap, so the rail is not all
    // detours through a configuration screen.
    assert!(menu
        .items
        .iter()
        .filter(|item| burger.recommended_product_refs.contains(&item.product_ref))
        .any(|item| !item.configurable));

    // A configurable product's questions carry pictures where the option is
    // itself a dish: the combo's side and drink are the menu's own photos.
    let combo = client
        .product_configuration("COMBO-SMASH", Some(CHANNEL.to_owned()))
        .unwrap();
    let pictured: usize = combo
        .groups
        .iter()
        .flat_map(|group| group.options.iter())
        .filter(|option| option.image.is_some())
        .count();
    assert_eq!(
        pictured, 6,
        "the combo's side, drink and extras are shown, not described"
    );
}

/// The journey, exactly as a customer walks it.
///
/// Attract → comer aquí → menu → configurable product → cart → one-tap upsell
/// → checkout → cash → confirmation. Then again with a simple product, and
/// again with several in the basket. What is being checked is that nothing in
/// the chain refuses, nothing is lost between screens, and the money is the
/// core's at every step (ADR 046).
#[test]
fn the_whole_kiosk_journey_holds_for_configured_simple_and_multi_item_orders() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");
    let menu = client.menu(Some(CHANNEL.to_owned())).unwrap();

    let add = |product: &str, selections: Vec<RequestedSelectionV1>| {
        client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef(product.to_owned()),
                    quantity_thousandths: 1_000,
                    selections,
                    preparation_note: None,
                },
                None,
            )
            .unwrap()
    };

    // --- orders 1 and 2: the same journey, once in cash and once by card ---
    //
    // Configure a burger, take a suggestion off the upsell rail in one tap,
    // pay, read the order number back. The tender is the only difference:
    // the screens ask for it on their own screen, and the core sells it as
    // one sale either way.
    for (number, tender) in [(1, "CASH"), (2, "CARD")] {
        client.start_transaction(None).unwrap();
        add(
            "SMASH-CLASSIC",
            vec![chose("O-CL-SINGLE", 1), chose("O-CL-BACON", 1)],
        );
        // The upsell rail offers what the cart does not already cover;
        // `FRIES` is in another category and needs no configuring, so one tap
        // adds it without leaving the cart.
        let fries = menu
            .items
            .iter()
            .find(|item| item.product_ref == "FRIES")
            .expect("a one-tap suggestion exists");
        assert!(!fries.configurable);
        let cart = add("FRIES", Vec::new());
        assert_eq!(cart.lines.len(), 2);
        let expected = 12_900 + 2_200 + 5_900;
        assert_eq!(cart.running_totals.subtotal.minor, expected);

        let totalized = client.totalize(None).unwrap();
        client
            .start_payment(
                StartPaymentRequestV1 {
                    tender_type: tender.to_owned(),
                    amount_minor: totalized.totals.grand_total.minor,
                },
                None,
            )
            .unwrap();
        let finalized = client.finalize_transaction(None).unwrap();
        assert_eq!(finalized.order_code, format!("K01-{number:04}"));
        assert_eq!(finalized.balance_due.minor, 0);

        // What the confirmation screen prints under "Pago" comes from here,
        // not from what the screen remembers having tapped.
        let payments = client
            .get_transaction_payments(TransactionRequestV1 {
                transaction_id: finalized.transaction_id.clone(),
            })
            .unwrap();
        assert_eq!(payments.payments[0].tender_type, tender);

        // The confirmation counts four seconds down and resets; by then the
        // core has nothing open, so the next customer cannot walk into it.
        assert!(client.get_active_transaction().unwrap().is_none());
    }

    // --- order 3: a simple product, by card -------------------------------
    client.start_transaction(None).unwrap();
    let simple = add("NUGGETS", Vec::new());
    assert!(simple.lines[0].selections.is_empty());
    let totalized = client.totalize(None).unwrap();
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CARD".to_owned(),
                amount_minor: totalized.totals.grand_total.minor,
            },
            None,
        )
        .unwrap();
    let third = client.finalize_transaction(None).unwrap();
    assert_eq!(
        third.order_code, "K01-0003",
        "the next customer, the next number"
    );

    // --- order 4: several products, one of them a nested combo ------------
    client.start_transaction(None).unwrap();
    add("SMASH-DOUBLE", vec![chose("O-DB-CHEESE", 2)]);
    add(
        "COMBO-SMASH",
        vec![
            chose("O-CO-FRIES", 1),
            chose("O-CO-SODA", 1),
            chose("O-CO-LARGE", 1),
        ],
    );
    add("SODA", vec![chose("O-SO-REG", 1)]);
    let basket = add("SHAKE-CHOCO", Vec::new());
    assert_eq!(basket.lines.len(), 4);
    // Over $250, so the basket promotion fires on top of the line one.
    assert!(basket.running_totals.discount_total.minor > 0);
    assert_eq!(
        basket.running_totals.grand_total.minor,
        client.totalize(None).unwrap().totals.grand_total.minor,
        "what the cart showed is what the till charges"
    );
    let totalized = client.totalize(None).unwrap();
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CARD".to_owned(),
                amount_minor: totalized.totals.grand_total.minor,
            },
            None,
        )
        .unwrap();
    let fourth = client.finalize_transaction(None).unwrap();
    assert_eq!(fourth.order_code, "K01-0004");

    // --- and nothing from one customer reaches the next --------------------
    client.start_transaction(None).unwrap();
    let fresh = client.get_active_transaction().unwrap().unwrap();
    assert!(fresh.lines.is_empty());
    assert_eq!(fresh.running_totals.grand_total.minor, 0);
    assert!(fresh.applied_promotions.is_empty());
}

/// The three journeys the demo is shown on, call for call.
///
/// Each one walks the screens in order and checks the thing that screen is
/// responsible for: that the configurator refuses an unanswered question,
/// that the offer after a dish comes from the catalog, that the cart's
/// running total is what the till charges, that the tender is recorded, and
/// that the next customer starts on an empty core.
///
/// The four-second confirmation is a screen timer and is pinned in
/// `KioskApp.tsx`; what the core owes it is checked here — once a sale is
/// finalized there is nothing open for the reset to come back to.
#[test]
fn the_three_demo_journeys_each_end_clean() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");
    let menu = client.menu(Some(CHANNEL.to_owned())).unwrap();

    let add = |product: &str, selections: Vec<RequestedSelectionV1>| {
        client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef(product.to_owned()),
                    quantity_thousandths: 1_000,
                    selections,
                    preparation_note: None,
                },
                None,
            )
            .unwrap()
    };
    let pay = |tender: &str| {
        let totalized = client.totalize(None).unwrap();
        client
            .start_payment(
                StartPaymentRequestV1 {
                    tender_type: tender.to_owned(),
                    amount_minor: totalized.totals.grand_total.minor,
                },
                None,
            )
            .unwrap();
        let finalized = client.finalize_transaction(None).unwrap();
        assert_eq!(finalized.status, "FINALIZED");
        assert_eq!(finalized.balance_due.minor, 0);
        let payments = client
            .get_transaction_payments(TransactionRequestV1 {
                transaction_id: finalized.transaction_id.clone(),
            })
            .unwrap();
        assert_eq!(payments.payments[0].tender_type, tender);
        // Nothing open: the confirmation's reset has no previous customer to
        // walk back into.
        assert!(client.get_active_transaction().unwrap().is_none());
        finalized
    };

    // --- FLOW A: configure, take the offer, add from the cart, pay by card -
    client.start_transaction(None).unwrap();
    // Required group unanswered: the configurator's CTA is not a suggestion.
    let refused = client.add_item(
        AddItemRequestV1 {
            product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
            quantity_thousandths: 1_000,
            selections: vec![chose("O-CL-BACON", 1)],
            preparation_note: None,
        },
        None,
    );
    assert!(refused.is_err(), "a required question cannot be skipped");
    // Answered: required size, a paid extra, and a removal that costs nothing.
    let cart = add(
        "SMASH-CLASSIC",
        vec![
            chose("O-CL-SINGLE", 1),
            chose("O-CL-BACON", 1),
            chose("O-CL-NOONION", 1),
        ],
    );
    let line = &cart.lines[0];
    assert!(line.base_unit_price.minor < line.configured_unit_price.minor);
    assert!(
        line.selections.iter().any(|s| !s.chargeable),
        "the removal is on the line and costs nothing"
    );

    // "Completa tu pedido" offers what the catalog says, minus the basket.
    let burger = menu
        .items
        .iter()
        .find(|item| item.product_ref == "SMASH-CLASSIC")
        .unwrap();
    let offered: Vec<&str> = burger
        .recommended_product_refs
        .iter()
        .map(String::as_str)
        .filter(|candidate| !cart.lines.iter().any(|line| line.product_ref == *candidate))
        .collect();
    assert_eq!(offered.first().copied(), Some("FRIES"));
    add("FRIES", Vec::new());

    // And the cart's own rail, which excludes everything now in the basket.
    let basket = add("SHAKE-CHOCO", Vec::new());
    assert_eq!(basket.lines.len(), 3);
    assert_eq!(
        basket.running_totals.grand_total.minor,
        client.totalize(None).unwrap().totals.grand_total.minor,
        "what the cart showed is what the till charges"
    );
    let first = pay("CARD");
    assert_eq!(first.order_code, "K01-0001");

    // --- FLOW B: a simple dish, no offer taken, paid in cash ---------------
    client.start_transaction(None).unwrap();
    let simple = add("NUGGETS", Vec::new());
    assert!(simple.lines[0].selections.is_empty());
    assert_eq!(pay("CASH").order_code, "K01-0002");

    // --- FLOW C: the nested combo, priced right, paid by card --------------
    client.start_transaction(None).unwrap();
    let combo = add(
        "COMBO-SMASH",
        vec![
            chose("O-CO-FRIES", 1),
            chose("O-CO-SODA", 1),
            chose("O-CO-LARGE", 1),
        ],
    );
    assert!(
        combo.lines[0]
            .selections
            .iter()
            .any(|s| s.option_id == "O-CO-LARGE"),
        "the nested drink size is materialised on the line"
    );
    add("SMASH-DOUBLE", vec![chose("O-DB-CHEESE", 2)]);
    let over = add("SODA", vec![chose("O-SO-REG", 1)]);
    assert!(
        over.running_totals.discount_total.minor > 0,
        "over $250 the basket promotion fires"
    );
    assert_eq!(pay("CARD").order_code, "K01-0003");

    // --- and the next customer starts on nothing ---------------------------
    client.start_transaction(None).unwrap();
    let fresh = client.get_active_transaction().unwrap().unwrap();
    assert!(fresh.lines.is_empty());
    assert_eq!(fresh.running_totals.grand_total.minor, 0);
    assert!(fresh.applied_promotions.is_empty());
}

/// Every choice worth seeing is photographed, and the rest are not.
///
/// A group renders as photo cards when its options carry images and as plain
/// tiles when they do not, so a group has to be *all* one or the other —
/// half a grid of photographs and half empty wells reads as a bug. Removals
/// are deliberately textual: there is no photograph of an absence.
///
/// The seed also reports any stem the dataset names but nobody supplied. It
/// is empty here, and that is the assertion: no product and no pictured
/// choice is standing on a missing file.
#[test]
fn every_choice_worth_seeing_is_photographed_and_removals_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let report = seed(&database);
    assert_eq!(
        report["photographs_wanted"]
            .as_array()
            .expect("the seed reports unresolved stems")
            .len(),
        0,
        "the dataset names no photograph that is missing: {}",
        report["photographs_wanted"]
    );

    let client = client(&database, "tauri-pos");
    let menu = client.menu(Some(CHANNEL.to_owned())).unwrap();

    for item in &menu.items {
        let configuration = client
            .product_configuration(&item.product_ref, Some(CHANNEL.to_owned()))
            .unwrap();
        for group in &configuration.groups {
            let pictured = group
                .options
                .iter()
                .filter(|option| option.image.is_some())
                .count();
            assert!(
                pictured == 0 || pictured == group.options.len(),
                "`{}` on `{}` mixes {pictured} photographed options with {} bare ones",
                group.name,
                item.product_ref,
                group.options.len() - pictured
            );
        }
    }

    // A removal never gets one, however pictured its neighbours are.
    let burger = client
        .product_configuration("SMASH-CLASSIC", Some(CHANNEL.to_owned()))
        .unwrap();
    let removals = burger
        .groups
        .iter()
        .flat_map(|group| group.options.iter())
        .filter(|option| option.option_id.contains("-NO"));
    for removal in removals {
        assert!(removal.image.is_none(), "{} is textual", removal.option_id);
    }

    // And an option that *is* a dish shows that dish, from one stored file.
    let combo = client
        .product_configuration("COMBO-SMASH", Some(CHANNEL.to_owned()))
        .unwrap();
    let fries_option = combo
        .groups
        .iter()
        .flat_map(|group| group.options.iter())
        .find(|option| option.option_id == "O-CO-FRIES")
        .unwrap();
    let fries_dish = menu
        .items
        .iter()
        .find(|item| item.product_ref == "FRIES")
        .unwrap();
    assert_eq!(
        fries_option.image.as_ref().map(|asset| &asset.content_hash),
        fries_dish.image.as_ref().map(|asset| &asset.content_hash),
        "the option and the dish are the same photograph, stored once"
    );
}

/// An animated dish survives the pipeline with its animation.
///
/// A GIF is only a GIF while its bytes are untouched: re-encode it, keep the
/// first frame, or hand the screen a different media type, and what arrives
/// is a still. So this walks the whole path a card actually uses — seed,
/// snapshot, menu, `READ_ASSET` — and checks the bytes that come back are the
/// bytes that went in, frame markers and all.
#[test]
fn an_animated_dish_arrives_animated() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");
    let menu = client.menu(Some(CHANNEL.to_owned())).unwrap();

    for (product_ref, file) in [
        ("CERVEZA", "cerveza.gif"),
        ("MOZZA-STICKS", "mozzarella.gif"),
    ] {
        let item = menu
            .items
            .iter()
            .find(|item| item.product_ref == product_ref)
            .unwrap_or_else(|| panic!("{product_ref} is on the menu"));
        let asset = item
            .image
            .clone()
            .unwrap_or_else(|| panic!("{product_ref} has a picture"));

        // The media type the catalog declares is the one the screen needs to
        // render an animation rather than a photograph.
        assert_eq!(asset.media_type, "image/gif", "{product_ref}");

        // Read it back the way the bridge does: chunked, over IPC.
        let mut bytes = Vec::new();
        while (bytes.len() as i64) < asset.byte_size {
            let chunk = client
                .read_asset(
                    asset.clone(),
                    bytes.len() as i64,
                    MAX_ASSET_CHUNK_BYTES as i64,
                )
                .unwrap();
            let slice = chunk.data().unwrap();
            assert!(!slice.is_empty(), "{product_ref} stopped short");
            bytes.extend_from_slice(&slice);
        }

        let source = std::fs::read(demo_dir().join("assets").join(file)).unwrap();
        assert_eq!(
            bytes, source,
            "{product_ref} came back re-encoded; an animation would not survive that"
        );
        assert_eq!(&bytes[..6], b"GIF89a", "{product_ref} is still a GIF");

        // A single-frame GIF is a still. Count the Graphic Control Extension
        // blocks: more than one means it actually moves.
        let frames = bytes
            .windows(4)
            .filter(|window| window == b"\x00\x21\xf9\x04")
            .count();
        assert!(
            frames > 1,
            "{product_ref} has {frames} frame marker(s); that is a still image"
        );
    }
}

/// What an option's quantity is worth, all the way to the receipt.
///
/// The screen's stepper only ever sends `{option_id, quantity}` — so the thing
/// that has to be right on this side is that the *quantity* is honoured
/// everywhere a number appears: the configured unit price, the line, the
/// running total, the official total, the finalized sale, and the selections
/// the printed receipt renders from.
///
/// Generic on purpose: the option is whichever one in this product's groups
/// accepts more than one of itself, found by reading the contract rather than
/// by naming a product.
#[test]
fn an_options_quantity_is_honoured_from_the_screen_to_the_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED);
    seed(&database);
    let client = client(&database, "tauri-pos");

    // --- find a countable option by asking the catalog, not by knowing one -
    let configuration = client
        .product_configuration("SMASH-CLASSIC", Some(CHANNEL.to_owned()))
        .unwrap();
    let (group, option) = configuration
        .groups
        .iter()
        .filter(|group| group.max_quantity > 1)
        .flat_map(|group| group.options.iter().map(move |option| (group, option)))
        .find(|(_, option)| option.max_quantity > 1)
        .expect("this product has an option you can take several of");
    assert!(
        option.max_quantity >= 2 && group.max_quantity >= option.max_quantity,
        "`{}` allows {} of itself in a group that accepts {}",
        option.option_id,
        option.max_quantity,
        group.max_quantity
    );
    let unit = option.unit_price.minor;
    assert!(unit > 0, "a free option would prove nothing about pricing");

    // --- one of it ---------------------------------------------------------
    client.start_transaction(None).unwrap();
    let one = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
                quantity_thousandths: 1_000,
                selections: vec![chose("O-CL-SINGLE", 1), chose(&option.option_id, 1)],
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    let line = &one.lines[0];
    assert_eq!(
        line.configured_unit_price.minor,
        line.base_unit_price.minor + unit
    );
    let chosen = line
        .selections
        .iter()
        .find(|selection| selection.option_id == option.option_id)
        .expect("the choice came back on the line");
    assert_eq!(chosen.quantity, 1);
    assert_eq!(chosen.amount.minor, unit);
    let with_one = one.running_totals.grand_total.minor;

    // --- two of it, on an otherwise identical line -------------------------
    client.cancel_transaction(None, None).unwrap();
    client.start_transaction(None).unwrap();
    let two = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
                quantity_thousandths: 1_000,
                selections: vec![chose("O-CL-SINGLE", 1), chose(&option.option_id, 2)],
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    let line = &two.lines[0];
    assert_eq!(
        line.configured_unit_price.minor,
        line.base_unit_price.minor + unit * 2,
        "the second one was not charged"
    );
    let chosen = line
        .selections
        .iter()
        .find(|selection| selection.option_id == option.option_id)
        .unwrap();
    assert_eq!(
        chosen.quantity, 2,
        "the quantity the screen sent was not kept"
    );
    assert_eq!(chosen.amount.minor, unit * 2);
    let with_two = two.running_totals.grand_total.minor;
    assert!(
        with_two > with_one,
        "one more of a paid option left the total alone"
    );

    // --- and the official total agrees with the basket ---------------------
    let totalized = client.totalize(None).unwrap();
    assert_eq!(totalized.totals.grand_total.minor, with_two);

    // --- through payment, with the quantity intact -------------------------
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CARD".to_owned(),
                amount_minor: totalized.totals.grand_total.minor,
            },
            None,
        )
        .unwrap();
    let finalized = client.finalize_transaction(None).unwrap();
    assert_eq!(finalized.status, "FINALIZED");
    assert_eq!(finalized.totals.grand_total.minor, with_two);
    let kept = finalized.lines[0]
        .selections
        .iter()
        .find(|selection| selection.option_id == option.option_id)
        .expect("the finalized sale forgot the choice");
    assert_eq!(
        kept.quantity, 2,
        "the quantity did not survive finalization"
    );
    assert_eq!(kept.amount.minor, unit * 2);

    // --- the receipt is built from exactly those selections ----------------
    //
    // The printed document renders `+ <name> x<quantity>` straight off the
    // line's materialised selections — the same rows asserted just above — and
    // nothing is read back out of a catalog that may be gone (ADR 046). With a
    // printer configured the obligation is discharged as the sale closes, so
    // its state is the evidence that the document was built and went out
    // (ADR 043).
    let receipt = client.get_receipt(&finalized.transaction_id).unwrap();
    assert_eq!(receipt.status, "PRINTED");
    assert_eq!(receipt.attempt_count, 1);
    assert!(receipt.first_printed_at.is_some());
    // Asking for it a second time is refused rather than quietly reprinting.
    let again = client.print_transaction(
        PrintTransactionRequestV1 {
            transaction_id: finalized.transaction_id.clone(),
            reprint: false,
        },
        None,
    );
    assert!(format!("{:?}", again.err()).contains("RECEIPT_ALREADY_PRINTED"));
}

/// The core refuses more of an option than the catalog allows.
///
/// The screen's stepper stops at the same ceiling, but the ceiling is the
/// core's: a client that asked for more — an old build, a replayed request —
/// is refused rather than priced.
#[test]
fn more_of_an_option_than_the_catalog_allows_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let configuration = client
        .product_configuration("SMASH-CLASSIC", Some(CHANNEL.to_owned()))
        .unwrap();
    let option = configuration
        .groups
        .iter()
        .flat_map(|group| group.options.iter())
        .find(|option| option.max_quantity > 1)
        .expect("an option you can take several of");

    client.start_transaction(None).unwrap();
    let refused = client.add_item(
        AddItemRequestV1 {
            product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
            quantity_thousandths: 1_000,
            selections: vec![
                chose("O-CL-SINGLE", 1),
                chose(&option.option_id, option.max_quantity + 1),
            ],
            preparation_note: None,
        },
        None,
    );
    let error = refused.expect_err("the core priced more than it offers");
    assert!(
        format!("{error}").contains("CATALOG_SELECTION_QUANTITY_REFUSED"),
        "{error}"
    );
    // And nothing was sold on the way to being refused.
    assert!(client
        .get_active_transaction()
        .unwrap()
        .unwrap()
        .lines
        .is_empty());
}

/// The journey the offer screen drives, through the core.
///
/// A dish, then one of the things it recommends — a one-tap one and a
/// configurable one, both found by reading the menu rather than by naming a
/// product — and then a sale that closes with every line on it. The screen's
/// own navigation is tested on its side of the wire; what is checked here is
/// that the calls it makes in that order do what it assumes.
#[test]
fn a_recommended_dish_can_be_added_right_after_the_one_that_recommended_it() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let menu = client.menu(Some(CHANNEL.to_owned())).unwrap();
    let by_ref = |wanted: &str| menu.items.iter().find(|item| item.product_ref == wanted);
    let kinds = |item: &edge_protocol::MenuItemV1| {
        let offered: Vec<&edge_protocol::MenuItemV1> = item
            .recommended_product_refs
            .iter()
            .filter_map(|reference| by_ref(reference))
            .collect();
        (
            offered.iter().find(|entry| !entry.configurable).copied(),
            offered.iter().find(|entry| entry.configurable).copied(),
        )
    };
    // The offer screen shows both kinds side by side, so the dish under test
    // is one that offers both — found by reading the menu, not by name.
    let (source, one_tap, asks) = menu
        .items
        .iter()
        .filter(|item| item.configurable)
        .find_map(|item| match kinds(item) {
            (Some(one_tap), Some(asks)) => Some((item, one_tap, asks)),
            _ => None,
        })
        .expect("a dish that recommends both a one-tap item and a configurable one");

    client.start_transaction(None).unwrap();
    client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef(source.product_ref.clone()),
                quantity_thousandths: 1_000,
                selections: answers_for(&client, &source.product_ref),
                preparation_note: None,
            },
            None,
        )
        .unwrap();

    // --- the one-tap suggestion: no configuration call needed -------------
    let configuration = client
        .product_configuration(&one_tap.product_ref, Some(CHANNEL.to_owned()))
        .unwrap();
    assert!(
        configuration.groups.is_empty(),
        "`{}` asks questions after all",
        one_tap.product_ref
    );
    let after = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef(one_tap.product_ref.clone()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(after.lines.len(), 2);
    assert_eq!(after.lines[1].product_ref, one_tap.product_ref);

    // --- the configurable one: its questions, then its line ---------------
    let configuration = client
        .product_configuration(&asks.product_ref, Some(CHANNEL.to_owned()))
        .unwrap();
    let required = configuration
        .groups
        .iter()
        .find(|group| group.min_quantity > 0)
        .expect("a question with a required answer");
    let answer = &required.options[0];
    let after = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef(asks.product_ref.clone()),
                quantity_thousandths: 1_000,
                selections: vec![chose(&answer.option_id, 1)],
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(after.lines.len(), 3);

    // --- adding the same suggestion again is a second line, as asked ------
    let twice = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef(one_tap.product_ref.clone()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(
        twice.lines.len(),
        4,
        "a repeat of a suggestion was silently merged"
    );

    // --- and it all closes ------------------------------------------------
    let totalized = client.totalize(None).unwrap();
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CARD".to_owned(),
                amount_minor: totalized.totals.grand_total.minor,
            },
            None,
        )
        .unwrap();
    let finalized = client.finalize_transaction(None).unwrap();
    assert_eq!(finalized.status, "FINALIZED");
    assert_eq!(finalized.lines.len(), 4);
}

/// Whatever a required group needs, answered with its first option.
///
/// So a test can add a configured dish without naming an option: the screen
/// reads the questions and sends ids, and so does this.
fn answers_for(client: &impl EdgeClient, product_ref: &str) -> Vec<RequestedSelectionV1> {
    client
        .product_configuration(product_ref, Some(CHANNEL.to_owned()))
        .unwrap()
        .groups
        .iter()
        .filter(|group| group.min_quantity > 0 && group.owner_option_id.is_none())
        .map(|group| chose(&group.options[0].option_id, 1))
        .collect()
}

/// A suggestion the core refuses leaves the sale exactly as it was.
///
/// This is what the offer screen's recovery path depends on: it puts the
/// customer back on the offer screen, and the basket behind it has to still be
/// the basket they had.
#[test]
fn a_refused_suggestion_changes_nothing_about_the_sale() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    client.start_transaction(None).unwrap();
    let before = client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("SMASH-CLASSIC".to_owned()),
                quantity_thousandths: 1_000,
                selections: vec![chose("O-CL-SINGLE", 1)],
                preparation_note: None,
            },
            None,
        )
        .unwrap();

    // A suggestion that is no longer in the catalog: what a kiosk holding a
    // menu from a snapshot that has since been replaced would ask for. It is
    // also the shape of every other refusal the offer screen can meet — the
    // screen cannot tell them apart and does not try.
    let refused = client.add_item(
        AddItemRequestV1 {
            product: ProductSelectorV1::ProductRef("NO-SUCH-DISH".to_owned()),
            quantity_thousandths: 1_000,
            selections: Vec::new(),
            preparation_note: None,
        },
        None,
    );
    let error = refused.expect_err("the core sold something it does not have");
    assert!(format!("{error}").contains("PRODUCT_NOT_FOUND"), "{error}");

    // And one that exists but was sent without the answers it requires, which
    // is the refusal a configured suggestion would meet.
    let incomplete = client.add_item(
        AddItemRequestV1 {
            product: ProductSelectorV1::ProductRef("SODA".to_owned()),
            quantity_thousandths: 1_000,
            selections: Vec::new(),
            preparation_note: None,
        },
        None,
    );
    assert!(incomplete.is_err(), "a required question went unanswered");

    let after = client.get_active_transaction().unwrap().unwrap();
    assert_eq!(after.lines.len(), before.lines.len());
    assert_eq!(
        after.running_totals.grand_total.minor, before.running_totals.grand_total.minor,
        "a refused suggestion moved the total"
    );
    // And the sale is still open, so the screen has something to go back to.
    assert_eq!(after.status, before.status);
}
