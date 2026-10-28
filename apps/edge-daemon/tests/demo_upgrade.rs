//! Re-seeding the demo over a demo that is already there.
//!
//! The dataset ships *with* the application, so it changes between releases —
//! adding the retail vertical changed it — and the installations it has to
//! land on are not empty. A catalog version is an identity: the core refuses a
//! version that already exists holding different content, and that refusal is
//! what makes "what did this sale cost" answerable. So the seed has to pick
//! its version against what is already there, and these tests are the four
//! shapes it has to get right: clean, unchanged, changed, and not-ours.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{client, DaemonProcess};
use edge_client::EdgeClient;
use edge_protocol::InitDeviceRequestV1;

const RESTAURANT: &str = "KIOSK";
const RETAIL: &str = "KIOSK-RETAIL";

fn demo_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("demo")
}

/// Run `demo-seed` against a dataset directory, expecting it to work.
fn seed_from(database: &Path, dataset: &Path) -> serde_json::Value {
    let output = run_seed(database, dataset);
    assert!(
        output.status.success(),
        "demo-seed failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn run_seed(database: &Path, dataset: &Path) -> std::process::Output {
    Command::new(common::cli_binary())
        .arg("--db")
        .arg(database)
        .arg("--format")
        .arg("json")
        .arg("demo-seed")
        .arg("--path")
        .arg(dataset)
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap()
}

/// A copy of the shipped dataset with the retail vertical removed — which is
/// exactly what an installation seeded before the second vertical existed has
/// in its database.
fn restaurant_only(into: &Path) -> PathBuf {
    let dataset = into.join("demo-old");
    let assets = dataset.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::copy(demo_dir().join("menu.json"), dataset.join("menu.json")).unwrap();
    for entry in std::fs::read_dir(demo_dir().join("assets")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, assets.join(path.file_name().unwrap())).unwrap();
        }
    }
    // No `retail/`, so `demo-seed` merges nothing and imports the restaurant
    // alone — the old content, byte for byte.
    dataset
}

/// A clean installation takes the version the dataset names.
#[test]
fn a_clean_installation_seeds_at_the_version_the_dataset_declares() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let report = seed_from(&database, &demo_dir());
    assert_eq!(report["activated"], true);
    assert_eq!(report["catalog_version"], 1, "a clean store starts at 1");
    assert_eq!(report["catalog_upgraded"], false);

    let catalog = client(&database, "px-kiosk");
    let status = catalog.catalog_status().unwrap();
    assert_eq!(status.highest_version, 1);
    assert_eq!(status.active.unwrap().snapshot.version, 1);
    assert!(!catalog
        .menu(Some(RESTAURANT.to_owned()))
        .unwrap()
        .items
        .is_empty());
    assert!(!catalog
        .menu(Some(RETAIL.to_owned()))
        .unwrap()
        .items
        .is_empty());
}

/// Seeding the same dataset twice costs nothing: no version, no snapshot, no
/// asset, and not even a change of which catalog is in force.
#[test]
fn seeding_the_same_dataset_twice_creates_no_second_version() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let catalog = client(&database, "px-kiosk");

    let first = seed_from(&database, &demo_dir());
    let before = catalog.catalog_status().unwrap();
    let in_force = before.active.clone().unwrap().snapshot;

    let second = seed_from(&database, &demo_dir());
    assert_eq!(second["catalog_version"], first["catalog_version"]);
    assert_eq!(second["catalog_upgraded"], false);
    assert_eq!(
        second["assets_imported"].as_i64().unwrap(),
        0,
        "the content-addressed store already had every asset"
    );
    assert_eq!(
        second["snapshot_id"], first["snapshot_id"],
        "a second snapshot"
    );

    let after = catalog.catalog_status().unwrap();
    assert_eq!(after.highest_version, before.highest_version);
    let still = after.active.unwrap().snapshot;
    assert_eq!(still.snapshot_id, in_force.snapshot_id);
    assert_eq!(
        still.activated_at, in_force.activated_at,
        "the catalog in force was re-activated for nothing"
    );
    assert!(
        after.pending.is_empty(),
        "an unused snapshot was left behind"
    );
    assert!(after.importing.is_empty(), "an import was left open");
}

/// The upgrade: an installation holding the old restaurant-only demo.
///
/// The old snapshot owns version 1 with different content, so the core would
/// refuse to reuse that number — rightly. The new content takes the next
/// version, the old snapshot is left exactly as it was, and both lanes work.
#[test]
fn an_installation_on_the_old_restaurant_only_demo_upgrades_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let catalog = client(&database, "px-kiosk");

    // --- what an earlier release left behind -----------------------------
    let old_dataset = restaurant_only(dir.path());
    let old = seed_from(&database, &old_dataset);
    assert_eq!(old["catalog_version"], 1);
    let old_snapshot = old["snapshot_id"].as_str().unwrap().to_owned();
    let old_products = catalog
        .menu(Some(RESTAURANT.to_owned()))
        .unwrap()
        .items
        .len();
    assert!(
        catalog
            .menu(Some(RETAIL.to_owned()))
            .unwrap()
            .items
            .is_empty(),
        "the old demo has no retail lane"
    );

    // --- the shipped dataset arrives -------------------------------------
    let new = seed_from(&database, &demo_dir());
    assert_eq!(
        new["catalog_version"], 2,
        "the new content took the next version"
    );
    assert_eq!(new["catalog_upgraded"], true);
    assert_ne!(new["snapshot_id"].as_str().unwrap(), old_snapshot);
    assert_eq!(new["activated"], true);

    let status = catalog.catalog_status().unwrap();
    assert_eq!(status.highest_version, 2);
    let active = status.active.clone().unwrap().snapshot;
    assert_eq!(active.version, 2);
    assert_eq!(active.snapshot_id, new["snapshot_id"].as_str().unwrap());

    // The old snapshot is still there, superseded rather than rewritten: the
    // sales that captured it still price against what they captured.
    let kept = catalog.catalog_import_status(&old_snapshot).unwrap();
    assert_eq!(kept.snapshot.version, 1);
    assert_eq!(kept.snapshot.status, "SUPERSEDED");
    assert!(
        kept.snapshot.content_hash.is_some(),
        "the old content was discarded"
    );

    // Both lanes, from the new snapshot.
    let restaurant = catalog.menu(Some(RESTAURANT.to_owned())).unwrap();
    let retail = catalog.menu(Some(RETAIL.to_owned())).unwrap();
    assert_eq!(
        restaurant.items.len(),
        old_products,
        "the upgrade changed what the restaurant sells"
    );
    assert!(!retail.items.is_empty(), "the retail lane is still empty");
    assert_eq!(restaurant.snapshot_id, retail.snapshot_id);
    for item in &retail.items {
        assert!(item.product_ref.starts_with("R-"), "{}", item.product_ref);
    }
    for item in &restaurant.items {
        assert!(!item.product_ref.starts_with("R-"), "{}", item.product_ref);
    }
}

/// And running it again after the upgrade does not climb to version 3.
#[test]
fn re_running_after_an_upgrade_stays_on_the_version_it_reached() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let catalog = client(&database, "px-kiosk");

    seed_from(&database, &restaurant_only(dir.path()));
    let upgraded = seed_from(&database, &demo_dir());
    assert_eq!(upgraded["catalog_version"], 2);

    for _ in 0..3 {
        let again = seed_from(&database, &demo_dir());
        assert_eq!(
            again["catalog_version"], 2,
            "an idempotent seed is climbing versions"
        );
        assert_eq!(again["catalog_upgraded"], false);
        assert_eq!(again["snapshot_id"], upgraded["snapshot_id"]);
    }
    assert_eq!(catalog.catalog_status().unwrap().highest_version, 2);
}

/// An installation that is somebody's actual shop is refused, not adopted.
///
/// There is no metadata marking a snapshot as demo-made, so the guard is the
/// device profile: this command owns exactly one tenant and store, and
/// `init_device` returns an existing profile rather than overwriting it. The
/// catalog in force prices real sales; superseding it because a dev tool was
/// pointed at the wrong database would be the worst thing this command could
/// do.
#[test]
fn the_demo_refuses_an_installation_it_does_not_own() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let catalog = client(&database, "px-kiosk");

    // Somebody else's shop, provisioned first.
    let device = catalog
        .init_device(InitDeviceRequestV1 {
            tenant_id: "TENANT-REAL".to_owned(),
            store_id: "STORE-REAL-07".to_owned(),
            currency: "MXN".to_owned(),
            allow_negative_inventory: false,
        })
        .unwrap();
    assert_eq!(device.store_id, "STORE-REAL-07");

    let refused = run_seed(&database, &demo_dir());
    assert!(!refused.status.success(), "the demo adopted a real store");
    let complaint = String::from_utf8_lossy(&refused.stderr);
    assert!(
        complaint.contains("STORE-REAL-07") && complaint.contains("Refusing"),
        "the refusal does not say whose installation it is: {complaint}"
    );

    // Nothing was imported, nothing activated, and the profile is untouched.
    let status = catalog.catalog_status().unwrap();
    assert!(status.active.is_none(), "a catalog was activated anyway");
    assert_eq!(status.highest_version, 0, "a snapshot was created anyway");
    assert!(status.importing.is_empty(), "an import was left open");
    assert_eq!(
        catalog
            .init_device(InitDeviceRequestV1 {
                tenant_id: "TENANT-NOVA".to_owned(),
                store_id: "STORE-NOVA-01".to_owned(),
                currency: "MXN".to_owned(),
                allow_negative_inventory: true,
            })
            .unwrap()
            .store_id,
        "STORE-REAL-07",
        "the demo overwrote the device profile"
    );
}
