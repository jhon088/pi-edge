//! The retail lane: one core, one catalog, a different shell in front of it.
//!
//! Retail and restaurant are two *channels* of one snapshot rather than two
//! catalogs, because a store may have exactly one active catalog and swapping
//! it under a running till to change verticals would be a worse answer than
//! filtering (ADR 044). So what these tests are mostly checking is that the
//! filtering really is a wall: neither lane may ever show the other's goods.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

use common::{client, DaemonProcess};
use edge_client::EdgeClient;
use edge_protocol::{
    AddItemRequestV1, ProductSelectorV1, StartPaymentRequestV1, SubmitScanRequestV1,
    MAX_ASSET_CHUNK_BYTES,
};

const RESTAURANT: &str = "KIOSK";
const RETAIL: &str = "KIOSK-RETAIL";

/// The first barcode in the retail dataset, deterministic so a scanner can be
/// tested by hand: 750 + index + EAN-13 check digit.
const FIRST_BARCODE: &str = "7500000000015";

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

/// Neither lane can see the other's goods.
///
/// The wall is channel availability, and this is the test that it holds. A
/// restaurant product appearing in a supermarket aisle would be the whole
/// vertical separation failing at once.
#[test]
fn the_two_lanes_show_disjoint_catalogs_from_one_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let restaurant = client.menu(Some(RESTAURANT.to_owned())).unwrap();
    let retail = client.menu(Some(RETAIL.to_owned())).unwrap();

    assert_eq!(restaurant.snapshot_id, retail.snapshot_id, "one catalog");

    assert!(!restaurant.items.is_empty() && !retail.items.is_empty());
    for item in &retail.items {
        assert!(
            item.product_ref.starts_with("R-"),
            "`{}` is not a retail product but reached the retail lane",
            item.product_ref
        );
    }
    for item in &restaurant.items {
        assert!(
            !item.product_ref.starts_with("R-"),
            "`{}` is a retail product but reached the restaurant lane",
            item.product_ref
        );
    }

    // Categories too, not only the items inside them.
    for category in &retail.categories {
        assert!(category.category_ref.starts_with("R-"), "{category:?}");
    }
    for category in &restaurant.categories {
        assert!(!category.category_ref.starts_with("R-"), "{category:?}");
    }
}

/// Everything the retail shell needs in order to render.
#[test]
fn the_retail_catalog_seeds_offline_with_prices_pictures_and_barcodes() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let report = seed(&database);
    assert_eq!(report["activated"], true);
    assert_eq!(report["photographs_wanted"].as_array().unwrap().len(), 0);

    let client = client(&database, "tauri-pos");
    let menu = client.menu(Some(RETAIL.to_owned())).unwrap();

    assert!(
        menu.items.len() >= 18,
        "a supermarket needs a shelf: {} items",
        menu.items.len()
    );
    assert_eq!(menu.categories.len(), 8, "every aisle the shelf declares");
    assert_eq!(menu.currency, "MXN");

    for item in &menu.items {
        assert!(item.image.is_some(), "{} has no picture", item.product_ref);
        assert!(item.from_price.minor > 0, "{} is free", item.product_ref);
        assert!(
            item.category_ref.is_some(),
            "{} has no aisle",
            item.product_ref
        );
        // Nothing in retail asks questions; a scan must be able to add it.
        assert!(
            !item.configurable,
            "{} would need a configuration screen",
            item.product_ref
        );
    }

    // Every aisle has something in it.
    for category in &menu.categories {
        assert!(
            menu.items
                .iter()
                .any(|item| item.category_ref.as_deref() == Some(&category.category_ref)),
            "`{}` is an empty aisle",
            category.category_ref
        );
    }
}

/// A scan starts the sale, and a second scan of the same product adds to it.
///
/// The core never opens a sale by itself (ADR 036), so the lane's first scan
/// is the client opening one and then submitting. What is checked here is the
/// outcome the shell depends on: one line, then two units.
#[test]
fn scanning_builds_the_basket_and_an_unknown_code_leaves_it_alone() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    // Nothing open: a scan has nothing to join, and the core says so rather
    // than inventing a sale.
    let orphan = client.submit_scan(SubmitScanRequestV1 {
        event_id: uuid::Uuid::new_v4().to_string(),
        barcode: FIRST_BARCODE.to_owned(),
        device_id: None,
    });
    assert!(orphan.is_err(), "a scan must not open a sale on its own");

    client.start_transaction(None).unwrap();
    let first = client
        .submit_scan(SubmitScanRequestV1 {
            event_id: uuid::Uuid::new_v4().to_string(),
            barcode: FIRST_BARCODE.to_owned(),
            device_id: None,
        })
        .unwrap();
    assert_eq!(first.lines.len(), 1);
    let scanned = first.lines[0].product_ref.clone();
    assert!(scanned.starts_with("R-"));
    assert!(first.running_totals.grand_total.minor > 0);

    // The same product again. A scan is an ordinary `add_item` of one unit,
    // so it appends a line rather than incrementing the first one — which is
    // what a till does, and what the shell renders verbatim.
    let second = client
        .submit_scan(SubmitScanRequestV1 {
            event_id: uuid::Uuid::new_v4().to_string(),
            barcode: FIRST_BARCODE.to_owned(),
            device_id: None,
        })
        .unwrap();
    assert_eq!(second.lines.len(), 2, "a repeat scan went somewhere else");
    assert_eq!(second.lines[1].product_ref, scanned);
    assert_eq!(
        second.running_totals.grand_total.minor,
        first.running_totals.grand_total.minor * 2,
        "two of the same thing cost twice as much"
    );

    // An unknown code is refused, and the basket is untouched by it.
    let unknown = client.submit_scan(SubmitScanRequestV1 {
        event_id: uuid::Uuid::new_v4().to_string(),
        barcode: "7500000999999".to_owned(),
        device_id: None,
    });
    assert!(
        unknown.is_err(),
        "an unknown barcode must not sell anything"
    );
    let after = client.get_active_transaction().unwrap().unwrap();
    assert_eq!(after.lines.len(), 2);
    assert_eq!(
        after.running_totals.grand_total.minor, second.running_totals.grand_total.minor,
        "a refused scan changed the total"
    );
}

/// The first scan opens the lane, and a refused one leaves it closed.
///
/// This is the sequence the retail shell's host performs on every scan — ask
/// the core whether a sale is open, open one if it is not, then submit — and
/// the reason it exists is that the core deliberately will not open a sale for
/// a scan (ADR 036). It is asserted here rather than only in the shell because
/// the second half is the part that is easy to get wrong: a barcode nobody
/// recognises must not leave an empty basket sitting on the lane.
#[test]
fn the_first_scan_opens_the_lane_and_a_refused_one_leaves_it_closed() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    /// What the host does for one scan. Answers the sale, or the refusal.
    fn scan(client: &impl EdgeClient, barcode: &str) -> Result<usize, String> {
        let opened = match client.get_active_transaction().map_err(|e| e.to_string())? {
            Some(_) => false,
            None => {
                client.start_transaction(None).map_err(|e| e.to_string())?;
                true
            }
        };
        match client.submit_scan(SubmitScanRequestV1 {
            event_id: uuid::Uuid::new_v4().to_string(),
            barcode: barcode.to_owned(),
            device_id: None,
        }) {
            Ok(transaction) => Ok(transaction.lines.len()),
            Err(error) => {
                if opened {
                    let _ = client.cancel_transaction(Some("SCAN_REJECTED".to_owned()), None);
                }
                Err(error.to_string())
            }
        }
    }

    // Nothing recognised, nothing opened.
    assert!(client.get_active_transaction().unwrap().is_none());
    assert!(scan(&client, "7500000999999").is_err());
    assert!(
        client.get_active_transaction().unwrap().is_none(),
        "a refused first scan left an empty basket on the lane"
    );

    // A real product opens it.
    assert_eq!(scan(&client, FIRST_BARCODE).unwrap(), 1);

    // And a refusal *after* the lane is open leaves the basket alone: the
    // sale was not this scan's to abandon.
    assert!(scan(&client, "7500000999999").is_err());
    let basket = client.get_active_transaction().unwrap().unwrap();
    assert_eq!(basket.lines.len(), 1, "a refused scan emptied the basket");
}

/// The retail lane pays through the same engine the restaurant does.
#[test]
fn a_retail_basket_finalizes_in_cash_and_by_card() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");
    let menu = client.menu(Some(RETAIL.to_owned())).unwrap();

    for (number, tender) in [(1, "CASH"), (2, "CARD")] {
        client.start_transaction(None).unwrap();
        // Three items off the shelf, added the way the visual catalog does.
        for item in menu.items.iter().take(3) {
            client
                .add_item(
                    AddItemRequestV1 {
                        product: ProductSelectorV1::ProductRef(item.product_ref.clone()),
                        quantity_thousandths: 1_000,
                        selections: Vec::new(),
                        preparation_note: None,
                    },
                    None,
                )
                .unwrap();
        }
        let cart = client.get_active_transaction().unwrap().unwrap();
        assert_eq!(cart.lines.len(), 3);

        let totalized = client.totalize(None).unwrap();
        assert_eq!(
            cart.running_totals.grand_total.minor, totalized.totals.grand_total.minor,
            "what the basket showed is what the till charges"
        );

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
        assert_eq!(finalized.order_code, format!("K01-{number:04}"));

        // The receipt the shell asks to print exists and knows this sale.
        let receipt = client.get_receipt(&finalized.transaction_id).unwrap();
        assert_eq!(receipt.transaction_id, finalized.transaction_id);

        // And the lane is clear for the next customer.
        assert!(client.get_active_transaction().unwrap().is_none());
    }
}

/// Switching verticals is a change of channel, not of catalog.
///
/// Nothing is re-imported, nothing is re-activated, and a sale open in one
/// lane is still the sale the other lane sees — which is why the shell
/// abandons it deliberately rather than leaving it to chance.
#[test]
fn switching_between_the_lanes_costs_the_installation_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let before = client.catalog_status().unwrap();
    let snapshot = before.active.expect("a catalog is active").snapshot;

    // Restaurant, then retail, then back: four menu reads either way.
    for channel in [RESTAURANT, RETAIL, RESTAURANT, RETAIL] {
        let menu = client.menu(Some(channel.to_owned())).unwrap();
        assert!(!menu.items.is_empty(), "`{channel}` went blank");
        assert_eq!(menu.snapshot_id, snapshot.snapshot_id);
    }

    let after = client.catalog_status().unwrap();
    assert_eq!(
        after.active.map(|active| active.snapshot.snapshot_id),
        Some(snapshot.snapshot_id.clone()),
        "switching lanes re-activated the catalog"
    );

    // A basket abandoned on the way out of one lane leaves nothing behind.
    let retail = client.menu(Some(RETAIL.to_owned())).unwrap();
    client.start_transaction(None).unwrap();
    client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef(
                    // Whatever is first on the shelf; the test is about the
                    // basket being abandoned, not about what was in it.
                    retail.items[0].product_ref.clone(),
                ),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    client
        .cancel_transaction(Some("VERTICAL_SWITCH".to_owned()), None)
        .unwrap();
    assert!(client.get_active_transaction().unwrap().is_none());
}

// ================================================== the dataset on its own

fn retail_menu() -> serde_json::Value {
    let path = demo_dir().join("retail").join("menu.json");
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

/// The check digit of an EAN-13, computed the way a scanner does.
fn check_digit(body: &str) -> u32 {
    let sum: u32 = body
        .chars()
        .enumerate()
        .map(|(index, digit)| {
            let value = digit.to_digit(10).expect("a barcode is digits");
            if index % 2 == 0 {
                value
            } else {
                value * 3
            }
        })
        .sum();
    (10 - sum % 10) % 10
}

/// Every barcode in the dataset is one a real scanner would accept.
///
/// They are generated, not transcribed, so the thing that can go wrong is the
/// check digit — and a wrong one fails silently at the glass: the reader
/// simply never emits it, and the demo looks broken with nothing in the log.
#[test]
fn every_demo_barcode_is_a_valid_ean13_and_belongs_to_one_product() {
    let menu = retail_menu();
    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    for product in menu["products"].as_array().unwrap() {
        let product_ref = product["product_ref"].as_str().unwrap().to_owned();
        let barcodes = product["barcodes"].as_array().expect("a shelf needs codes");
        assert_eq!(
            barcodes.len(),
            1,
            "{product_ref} has {} barcodes; a lane expects one",
            barcodes.len()
        );
        let barcode = barcodes[0].as_str().unwrap().to_owned();

        assert_eq!(barcode.len(), 13, "{barcode} is not an EAN-13");
        assert!(
            barcode.chars().all(|digit| digit.is_ascii_digit()),
            "{barcode} is not all digits"
        );
        // 750 is Mexico's GS1 prefix, which is what makes these look like
        // what a shopper would actually hand over.
        assert!(
            barcode.starts_with("750"),
            "{barcode} is not a Mexican code"
        );
        let (body, last) = barcode.split_at(12);
        assert_eq!(
            check_digit(body),
            last.parse::<u32>().unwrap(),
            "{barcode} ({product_ref}) would be refused by a scanner"
        );

        if let Some(owner) = seen.insert(barcode.clone(), product_ref.clone()) {
            panic!("{barcode} is on both {owner} and {product_ref}");
        }
    }
    assert!(
        seen.len() >= 18,
        "only {} scannable products in the lane",
        seen.len()
    );
}

/// Each barcode resolves, through the core, to the product it was written for.
#[test]
fn scanning_a_demo_barcode_resolves_the_product_it_was_printed_for() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");

    let menu = retail_menu();
    for product in menu["products"].as_array().unwrap() {
        let product_ref = product["product_ref"].as_str().unwrap();
        let barcode = product["barcodes"][0].as_str().unwrap();
        let found = client
            .lookup_product(ProductSelectorV1::Barcode(barcode.to_owned()))
            .unwrap_or_else(|error| panic!("{barcode} ({product_ref}) does not scan: {error:?}"));
        assert_eq!(found.product.product_ref, product_ref);
        assert!(
            found.product.sellable,
            "{product_ref} scans but is not for sale"
        );
        let price = found.price.unwrap_or_else(|| {
            panic!("{product_ref} scans with no price: {:?}", found.price_issue)
        });
        assert!(
            price.unit_price.minor > 0,
            "{product_ref} scans but is free"
        );
    }
}

/// The retail dataset is as complete as the restaurant's, and as lean.
#[test]
fn the_retail_dataset_names_a_photograph_for_everything_and_nothing_spare() {
    let retail = demo_dir().join("retail");
    let menu = retail_menu();

    let mut referenced = std::collections::BTreeSet::new();
    fn stems(node: &serde_json::Value, found: &mut std::collections::BTreeSet<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(stem) = map
                    .get("content_hash")
                    .and_then(|value| value.as_str())
                    .and_then(|hash| hash.strip_prefix("file:"))
                {
                    found.insert(stem.to_owned());
                }
                for value in map.values() {
                    stems(value, found);
                }
            }
            serde_json::Value::Array(items) => {
                for value in items {
                    stems(value, found);
                }
            }
            _ => {}
        }
    }
    stems(&menu, &mut referenced);

    let mut on_disk = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(retail.join("assets")).unwrap() {
        let path = entry.unwrap().path();
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") {
            on_disk.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }

    let missing: Vec<&String> = referenced.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "the installer would ship a lane without {missing:?}"
    );
    let spare: Vec<&String> = on_disk.difference(&referenced).collect();
    assert!(spare.is_empty(), "nothing references {spare:?}");
}

/// Every product on the shelf has a picture the kiosk can actually draw.
///
/// Not "the catalog names an image" — that is checked elsewhere — but that the
/// bytes behind the name are here, decode as a real image, and are big enough
/// not to look soft on a kiosk panel. The retail lane is a grid of
/// photographs; one empty well reads as a broken machine.
///
/// Everything is read back **through the core**, over IPC, from the
/// content-addressed store. There is no filesystem path in this test and no
/// URL anywhere in the catalog, which is the point: an installed lane works
/// with its network cable pulled out (ADR 045).
#[test]
fn every_retail_product_has_real_asset() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    seed(&database);
    let client = client(&database, "tauri-pos");
    let menu = client.menu(Some(RETAIL.to_owned())).unwrap();

    let mut seen = std::collections::BTreeSet::new();
    for item in &menu.items {
        let asset = item
            .image
            .clone()
            .unwrap_or_else(|| panic!("{} has no picture", item.product_ref));
        assert!(
            asset.content_hash.starts_with("sha256:"),
            "{} points at `{}` rather than at stored content",
            item.product_ref,
            asset.content_hash
        );
        assert!(
            asset.byte_size > 4_000,
            "{} carries {} bytes, which is not a photograph",
            item.product_ref,
            asset.byte_size
        );
        assert!(
            asset.byte_size < 4 * 1024 * 1024,
            "{} carries {} bytes; an installer would be carrying it",
            item.product_ref,
            asset.byte_size
        );

        // The bytes themselves, in chunks, exactly as the screen reads them.
        let mut bytes = Vec::with_capacity(asset.byte_size as usize);
        let mut offset = 0_i64;
        while offset < asset.byte_size {
            let chunk = client
                .read_asset(asset.clone(), offset, MAX_ASSET_CHUNK_BYTES as i64)
                .unwrap_or_else(|error| {
                    panic!("{}: the bytes are not here: {error:?}", item.product_ref)
                });
            let slice = chunk.data().unwrap();
            assert!(!slice.is_empty(), "{} stopped short", item.product_ref);
            offset += slice.len() as i64;
            bytes.extend_from_slice(&slice);
        }
        assert_eq!(bytes.len() as i64, asset.byte_size);

        let (format, width, height) = decode_header(&bytes)
            .unwrap_or_else(|| panic!("{} is not a decodable image", item.product_ref));
        assert_eq!(
            format, asset.media_type,
            "{} is stored as {format} but declared {}",
            item.product_ref, asset.media_type
        );
        assert!(
            width >= 600 && height >= 600,
            "{} is {width}x{height}, which will look soft on a kiosk",
            item.product_ref
        );
        // Square, so a grid of them does not letterbox.
        let ratio = f64::from(width) / f64::from(height);
        assert!(
            (0.9..=1.1).contains(&ratio),
            "{} is {width}x{height}; the grid would letterbox it",
            item.product_ref
        );
        seen.insert(asset.content_hash.clone());
    }

    assert!(
        seen.len() >= 18,
        "only {} distinct pictures across the shelf",
        seen.len()
    );
    assert_eq!(
        seen.len(),
        menu.items.len(),
        "two products share one picture, so the shelf repeats itself"
    );
}

/// Width, height and media type, straight out of the file header.
///
/// Enough of a decoder to prove the bytes are an image and how big it is —
/// which is all this needs to know, and keeps an image library out of the
/// build for one assertion.
fn decode_header(bytes: &[u8]) -> Option<(String, u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        // IHDR is always the first chunk: length, type, then width/height.
        let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return Some(("image/png".to_owned(), width, height));
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        // Walk the segments to the frame header, which carries the size.
        let mut at = 2usize;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xff {
                return None;
            }
            let marker = bytes[at + 1];
            let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
            // SOF0..SOF3 and SOF5..SOF15, skipping the four that are not frames.
            if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                let height = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]);
                let width = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]);
                return Some(("image/jpeg".to_owned(), u32::from(width), u32::from(height)));
            }
            at += 2 + length;
        }
        return None;
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let width = u16::from_le_bytes([*bytes.get(6)?, *bytes.get(7)?]);
        let height = u16::from_le_bytes([*bytes.get(8)?, *bytes.get(9)?]);
        return Some(("image/gif".to_owned(), u32::from(width), u32::from(height)));
    }
    None
}

/// Nothing in the shipped dataset points outside the installation.
///
/// The photographs are prepared from source material in `examples/`, which is
/// a working folder on somebody's laptop and is **not** part of the product.
/// A dataset that referenced it — or any absolute path, or any URL — would
/// work on the machine it was built on and nowhere else, which is the failure
/// that does not show up until the stand.
#[test]
fn the_dataset_references_nothing_outside_the_installation() {
    for dataset in [
        demo_dir().join("menu.json"),
        demo_dir().join("retail").join("menu.json"),
    ] {
        let text = std::fs::read_to_string(&dataset).unwrap();
        for forbidden in [
            "examples/",
            "http://",
            "https://",
            "file://",
            "/Users/",
            "C:\\",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} mentions `{forbidden}`",
                dataset.display()
            );
        }
        // Images are named by stem and nothing else; the seed turns each one
        // into the `sha256:` the core actually stores.
        let menu: serde_json::Value = serde_json::from_str(&text).unwrap();
        fn refs(node: &serde_json::Value, found: &mut Vec<String>) {
            match node {
                serde_json::Value::Object(map) => {
                    if let Some(hash) = map.get("content_hash").and_then(|v| v.as_str()) {
                        found.push(hash.to_owned());
                    }
                    for value in map.values() {
                        refs(value, found);
                    }
                }
                serde_json::Value::Array(items) => {
                    for value in items {
                        refs(value, found);
                    }
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        refs(&menu, &mut found);
        assert!(
            !found.is_empty(),
            "{} references no images",
            dataset.display()
        );
        for reference in found {
            assert!(
                reference.starts_with("file:") || reference.starts_with("sha256:"),
                "`{reference}` is neither a stem nor stored content"
            );
            assert!(
                !reference.contains('/') && !reference.contains('\\'),
                "`{reference}` carries a path"
            );
        }
    }
}

/// The installer carries the second lane.
///
/// `demo-seed` reads `retail/menu.json` and `retail/assets` out of whatever
/// directory it is pointed at, and in an installed app that directory is the
/// bundle's resources. A dataset that is not listed there is simply absent on
/// the machine, the retail channel comes back empty, and nothing fails until
/// someone switches the lane on a stand.
#[test]
fn the_bundle_ships_both_verticals() {
    let config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../apps/tauri-pos/src-tauri/tauri.conf.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let resources = config["bundle"]["resources"]
        .as_object()
        .expect("the bundle declares its resources as a map");
    let shipped: Vec<&str> = resources
        .values()
        .filter_map(|value| value.as_str())
        .collect();

    for needed in [
        "demo/menu.json",
        "demo/assets/",
        "demo/retail/menu.json",
        "demo/retail/assets/",
    ] {
        assert!(
            shipped.contains(&needed),
            "the installer would not carry {needed}; it ships {shipped:?}"
        );
    }
}
