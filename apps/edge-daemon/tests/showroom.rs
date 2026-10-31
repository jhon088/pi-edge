//! The showroom's four lanes, on one installation.
//!
//! Tim Hortons and Sbarro are two more channels of the same snapshot, beside
//! the restaurant demo and the retail lane. What is checked here is the thing
//! that makes that safe: four shells, four menus, and no lane ever showing
//! another's goods (ADR 047).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

use common::{client, DaemonProcess};
use edge_client::EdgeClient;
use edge_protocol::{
    AddItemRequestV1, ProductSelectorV1, RequestedSelectionV1, StartPaymentRequestV1,
};

/// Every lane, and the prefix its *categories* carry.
///
/// Categories are prefixed per brand because that is what channel
/// availability is written against. Product refs are not: the original
/// restaurant names its dishes `SMASH-CLASSIC` and `FRIES`, from before there
/// was a second brand to tell it apart from.
const LANES: [(&str, &str); 4] = [
    ("KIOSK", "C-"),
    ("KIOSK-TIM", "T-"),
    ("KIOSK-SBARRO", "S-"),
    ("KIOSK-RETAIL", "R-"),
];

fn demo_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("demo")
}

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
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Whatever a product's required questions need, answered with the first
/// option of each — which is what a customer tapping straight through gets.
fn answers_for(
    client: &impl EdgeClient,
    channel: &str,
    product_ref: &str,
) -> Vec<RequestedSelectionV1> {
    client
        .product_configuration(product_ref, Some(channel.to_owned()))
        .unwrap()
        .groups
        .iter()
        .filter(|group| group.min_quantity > 0 && group.owner_option_id.is_none())
        .map(|group| RequestedSelectionV1 {
            option_id: group.options[0].option_id.clone(),
            quantity: 1,
        })
        .collect()
}

/// Four lanes, one snapshot, and nothing shared between their menus.
#[test]
fn every_lane_shows_its_own_brand_and_nobody_elses() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let mut snapshot = None;
    for (channel, prefix) in LANES {
        let menu = client.menu(Some(channel.to_owned())).unwrap();
        assert!(!menu.items.is_empty(), "`{channel}` has an empty menu");

        // One catalog behind all of them.
        let seen = snapshot.get_or_insert(menu.snapshot_id.clone());
        assert_eq!(*seen, menu.snapshot_id, "`{channel}` reads another catalog");

        for item in &menu.items {
            // Every item belongs to an aisle this lane owns, which is the
            // check that actually matters: availability resolves product and
            // category together, so a stray dish would arrive with a stray
            // category or not at all.
            let aisle = item
                .category_ref
                .as_deref()
                .unwrap_or_else(|| panic!("{} has no aisle", item.product_ref));
            assert!(
                aisle.starts_with(prefix),
                "`{}` reached `{channel}` from `{aisle}`",
                item.product_ref
            );
            // And no other brand's goods, by the prefixes that do carry one.
            for (_, other) in LANES.iter().filter(|(_, other)| *other != prefix) {
                assert!(
                    !item.product_ref.starts_with(other),
                    "`{}` is a `{other}` product but reached `{channel}`",
                    item.product_ref
                );
            }
            assert!(item.image.is_some(), "{} has no picture", item.product_ref);
            assert!(item.from_price.minor > 0, "{} is free", item.product_ref);
        }
        for category in &menu.categories {
            assert!(
                category.category_ref.starts_with(prefix),
                "`{}` is an aisle of another brand",
                category.category_ref
            );
        }
    }
}

/// The two new brands have the shelves the screens expect.
#[test]
fn tim_hortons_and_sbarro_arrive_with_their_menus() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let report = seed(&database);
    assert_eq!(report["activated"], true);
    assert_eq!(report["photographs_wanted"].as_array().unwrap().len(), 0);

    let client = client(&database, "tauri-pos");

    let tim = client.menu(Some("KIOSK-TIM".to_owned())).unwrap();
    assert_eq!(tim.items.len(), 13, "Tim Hortons' shelf");
    assert_eq!(tim.categories.len(), 4);

    let sbarro = client.menu(Some("KIOSK-SBARRO".to_owned())).unwrap();
    assert_eq!(sbarro.items.len(), 14, "Sbarro's shelf, drinks included");
    assert_eq!(sbarro.categories.len(), 4);

    for menu in [&tim, &sbarro] {
        // Both brands ask questions now — a coffee has a size and a pizza has
        // a crust — and both still offer something alongside everything.
        assert!(
            menu.items.iter().any(|item| item.configurable),
            "nothing on this menu asks a question"
        );
        for item in &menu.items {
            assert!(
                !item.recommended_product_refs.is_empty(),
                "{} offers nothing alongside it",
                item.product_ref
            );
            for recommended in &item.recommended_product_refs {
                assert!(
                    menu.items
                        .iter()
                        .any(|entry| &entry.product_ref == recommended),
                    "{} recommends `{recommended}`, which is not on this menu",
                    item.product_ref
                );
            }
        }
    }
}

/// Each new brand sells, pays and closes through the one journey.
#[test]
fn a_sale_closes_on_every_brand_through_the_same_payment() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    for (number, (channel, _)) in [("KIOSK-TIM", ""), ("KIOSK-SBARRO", "")].iter().enumerate() {
        let menu = client.menu(Some(channel.to_string())).unwrap();
        client.start_transaction(None).unwrap();
        for item in menu.items.iter().take(2) {
            client
                .add_item(
                    AddItemRequestV1 {
                        product: ProductSelectorV1::ProductRef(item.product_ref.clone()),
                        quantity_thousandths: 1_000,
                        selections: answers_for(&client, channel, &item.product_ref),
                        preparation_note: None,
                    },
                    None,
                )
                .unwrap();
        }
        let totalized = client.totalize(None).unwrap();
        assert!(totalized.totals.grand_total.minor > 0);
        client
            .start_payment(
                StartPaymentRequestV1 {
                    tender_type: if number == 0 { "CASH" } else { "CARD" }.to_owned(),
                    amount_minor: totalized.totals.grand_total.minor,
                },
                None,
            )
            .unwrap();
        let finalized = client.finalize_transaction(None).unwrap();
        assert_eq!(finalized.status, "FINALIZED");
        assert_eq!(finalized.balance_due.minor, 0);
        assert_eq!(finalized.lines.len(), 2);
        assert!(client.get_active_transaction().unwrap().is_none());
    }
}

/// Switching brands costs the installation nothing.
#[test]
fn moving_between_brands_re_imports_and_re_activates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let before = client.catalog_status().unwrap();
    let snapshot = before.active.clone().unwrap().snapshot;

    for (channel, _) in LANES.iter().cycle().take(12) {
        let menu = client.menu(Some(channel.to_string())).unwrap();
        assert!(!menu.items.is_empty(), "`{channel}` went blank");
    }

    let after = client.catalog_status().unwrap();
    assert_eq!(
        after.active.map(|active| active.snapshot.snapshot_id),
        Some(snapshot.snapshot_id),
        "a brand switch re-activated the catalog"
    );
    assert_eq!(after.highest_version, before.highest_version);
}

/// The showroom's datasets reference nothing outside the installation.
#[test]
fn the_brand_datasets_carry_no_path_and_no_url() {
    for slug in ["tim", "sbarro"] {
        let dataset = demo_dir().join(slug).join("menu.json");
        let text = std::fs::read_to_string(&dataset).unwrap();
        for forbidden in ["examples/", "http://", "https://", "file://", "/Users/"] {
            assert!(
                !text.contains(forbidden),
                "{} mentions `{forbidden}`",
                dataset.display()
            );
        }
        let menu: serde_json::Value = serde_json::from_str(&text).unwrap();
        for product in menu["presentation"].as_array().unwrap() {
            let reference = product["image"]["content_hash"].as_str().unwrap();
            assert!(reference.starts_with("file:"), "{reference}");
            assert!(!reference.contains('/'), "{reference} carries a path");
        }
    }
}

/// Every brand's photograph is on disk, and nothing on disk is dead weight.
#[test]
fn every_brand_product_has_a_photograph_that_ships() {
    for (slug, expected) in [("tim", 13), ("sbarro", 14)] {
        let folder = demo_dir().join(slug);
        let menu: serde_json::Value =
            serde_json::from_slice(&std::fs::read(folder.join("menu.json")).unwrap()).unwrap();

        let mut referenced = std::collections::BTreeSet::new();
        for product in menu["presentation"].as_array().unwrap() {
            let stem = product["image"]["content_hash"]
                .as_str()
                .unwrap()
                .strip_prefix("file:")
                .unwrap();
            referenced.insert(stem.to_owned());
        }
        assert_eq!(referenced.len(), expected, "{slug} products");

        let mut on_disk = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(folder.join("assets")).unwrap() {
            let path = entry.unwrap().path();
            let extension = path
                .extension()
                .map(|value| value.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") {
                assert!(
                    path.metadata().unwrap().len() > 4_000,
                    "{} is too small to be a photograph",
                    path.display()
                );
                on_disk.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
            }
        }

        let missing: Vec<&String> = referenced.difference(&on_disk).collect();
        assert!(missing.is_empty(), "{slug} would ship without {missing:?}");
        let spare: Vec<&String> = on_disk.difference(&referenced).collect();
        assert!(spare.is_empty(), "{slug} carries unused {spare:?}");
    }
}

/// Both brands ask the questions their menu implies, and price the answers.
///
/// A modifier is only real if the core charges for it, so this walks one
/// configured line end to end: read the questions, answer them, and check the
/// line costs the base plus exactly what was chosen (ADR 046).
#[test]
fn the_new_brands_configure_and_price_their_choices() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    for (channel, product) in [
        ("KIOSK-TIM", "T-LATTE"),
        ("KIOSK-SBARRO", "S-PIZZA-PEPPERONI"),
    ] {
        let configuration = client
            .product_configuration(product, Some(channel.to_owned()))
            .unwrap();
        assert!(
            configuration.groups.len() >= 2,
            "{product} asks only {} question(s)",
            configuration.groups.len()
        );
        // A required question with a default, so a customer can take one tap
        // and go; and at least one that costs money.
        assert!(
            configuration
                .groups
                .iter()
                .any(|group| group.min_quantity > 0
                    && group.options.iter().any(|o| o.default_quantity > 0)),
            "{product} has a required question nobody answered for them"
        );
        let paid = configuration
            .groups
            .iter()
            .flat_map(|group| group.options.iter())
            .find(|option| option.unit_price.minor > 0)
            .unwrap_or_else(|| panic!("{product} has no choice that costs anything"));
        let extra = paid.unit_price.minor;

        // Answer every required group, then take the paid choice. If it
        // belongs to a group that accepts one answer, it *replaces* that
        // answer rather than joining it — which is what the screen's
        // single-select does, and what the core insists on.
        let owning = configuration
            .groups
            .iter()
            .find(|group| {
                group
                    .options
                    .iter()
                    .any(|option| option.option_id == paid.option_id)
            })
            .expect("the paid choice belongs to a group");
        let mut selections: Vec<RequestedSelectionV1> = configuration
            .groups
            .iter()
            .filter(|group| group.min_quantity > 0 && group.owner_option_id.is_none())
            .filter(|group| group.group_id != owning.group_id)
            .map(|group| RequestedSelectionV1 {
                option_id: group.options[0].option_id.clone(),
                quantity: 1,
            })
            .collect();
        selections.push(RequestedSelectionV1 {
            option_id: paid.option_id.clone(),
            quantity: 1,
        });

        client.start_transaction(None).unwrap();
        let sale = client
            .add_item(
                AddItemRequestV1 {
                    product: ProductSelectorV1::ProductRef(product.to_owned()),
                    quantity_thousandths: 1_000,
                    selections,
                    preparation_note: None,
                },
                None,
            )
            .unwrap_or_else(|error| panic!("{product} refused its own answers: {error:?}"));

        let line = &sale.lines[0];
        assert_eq!(
            line.configured_unit_price.minor,
            line.base_unit_price.minor + extra,
            "{product}: the chosen extra was not charged"
        );
        assert!(
            line.selections
                .iter()
                .any(|s| s.option_id == paid.option_id),
            "{product}: the choice did not reach the line"
        );
        client.cancel_transaction(None, None).unwrap();
    }
}

/// Every brand offers something alongside everything it sells.
///
/// This is what the offer screen draws, and a lane where nothing is
/// configurable would never reach that screen without it.
#[test]
fn every_brand_cross_sells_from_the_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    for (channel, _) in LANES
        .iter()
        .filter(|(channel, _)| *channel != "KIOSK-RETAIL")
    {
        let menu = client.menu(Some(channel.to_string())).unwrap();
        for item in &menu.items {
            assert!(
                !item.recommended_product_refs.is_empty(),
                "`{channel}`: {} offers nothing alongside it",
                item.product_ref
            );
            assert!(
                !item.recommended_product_refs.contains(&item.product_ref),
                "{} recommends itself",
                item.product_ref
            );
        }
    }

    // Sbarro's drinks are on the shelf and are offered with the food.
    let sbarro = client.menu(Some("KIOSK-SBARRO".to_owned())).unwrap();
    let drinks: Vec<&str> = sbarro
        .items
        .iter()
        .filter(|item| item.category_ref.as_deref() == Some("S-BEBIDAS"))
        .map(|item| item.product_ref.as_str())
        .collect();
    assert_eq!(drinks.len(), 4, "Sbarro's drinks: {drinks:?}");
    for drink in &drinks {
        assert!(
            sbarro.items.iter().any(|item| item.product_ref == *drink
                && item.image.is_some()
                && item.from_price.minor > 0),
            "{drink} is not sellable"
        );
    }
}
