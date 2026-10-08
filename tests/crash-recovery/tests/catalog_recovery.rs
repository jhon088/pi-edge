//! The catalog and the price it produced survive a process death.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixtureProduct};
use edge_crash_recovery_tests::*;

const BARCODE: &str = "7500000000777";

fn catalog(version: i64, price_minor: i64) -> CatalogFixture {
    CatalogFixture::new(version).with(
        FixtureProduct::new("SKU-X", "Producto X", price_minor)
            .sku("SKU-X")
            .barcode(BARCODE),
    )
}

#[test]
fn snapshot_product_resolution_and_totals_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    // --- lifetime 1: catalog active, sale open with one line --------------
    let (expected_state, expected_catalog, expected_status) = {
        let core = core_at(&db, 1);
        open_register(&core);
        catalog(2, 12_345).activate(&core).unwrap();
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(AddItemCommand {
            request_id: None,
            selector: edge_catalog::ProductSelector::Barcode(
                edge_catalog::Barcode::new(BARCODE).unwrap(),
            ),
            quantity: edge_domain::prelude::Quantity::from_units(2).unwrap(),
        })
        .unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        (
            core.get_edge_state().unwrap(),
            core.get_active_transaction().unwrap().unwrap().catalog(),
            core.get_catalog_status().unwrap(),
        )
    };

    let expected_transaction = expected_state.active_transaction.as_ref().unwrap();
    assert_eq!(
        expected_transaction.state().lines[0].unit_price.minor(),
        12_345
    );
    assert_eq!(
        expected_transaction.state().totals.net_total.minor(),
        24_690
    );

    // --- lifetime 2: a brand new core reads SQLite ------------------------
    let core = core_at(&db, 1_000);
    let recovered = core.get_edge_state().unwrap();
    assert_eq!(
        recovered.active_transaction,
        expected_state.active_transaction
    );

    let transaction = recovered.active_transaction.unwrap();
    assert_eq!(transaction.catalog(), expected_catalog);
    assert_eq!(transaction.state().lines[0].unit_price.minor(), 12_345);
    assert_eq!(transaction.state().lines[0].description, "Producto X");
    assert_eq!(transaction.state().totals.net_total.minor(), 24_690);
    assert_eq!(transaction.state().totals.tax_total.minor(), 3_950);

    // The catalog itself came back identical, hash included.
    let status = core.get_catalog_status().unwrap();
    assert_eq!(status, expected_status);
    let active = status.active.as_ref().unwrap();
    assert_eq!(active.snapshot.version(), 2);
    assert!(active.snapshot.state().content_hash.is_some());

    // ... and the recovered sale still resolves against its own snapshot.
    let continued = core
        .add_item(AddItemCommand {
            request_id: None,
            selector: edge_catalog::ProductSelector::Sku("SKU-X".to_owned()),
            quantity: edge_domain::prelude::Quantity::from_units(1).unwrap(),
        })
        .unwrap();
    assert_eq!(continued.state().lines[1].unit_price.minor(), 12_345);
}

#[test]
fn a_sale_keeps_its_snapshot_even_if_a_newer_one_is_activated_after_the_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let captured = {
        let core = core_at(&db, 1);
        open_register(&core);
        catalog(2, 10_000).activate(&core).unwrap();
        let sale = core
            .start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(AddItemCommand {
            request_id: None,
            selector: edge_catalog::ProductSelector::Barcode(
                edge_catalog::Barcode::new(BARCODE).unwrap(),
            ),
            quantity: edge_domain::prelude::Quantity::from_units(1).unwrap(),
        })
        .unwrap();
        sale.catalog().unwrap()
    };

    // New process, new catalog activated while the sale is still open.
    let core = core_at(&db, 1_000);
    catalog(3, 20_000).activate(&core).unwrap();

    let transaction = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(transaction.catalog().unwrap(), captured);
    let continued = core
        .add_item(AddItemCommand {
            request_id: None,
            selector: edge_catalog::ProductSelector::Barcode(
                edge_catalog::Barcode::new(BARCODE).unwrap(),
            ),
            quantity: edge_domain::prelude::Quantity::from_units(1).unwrap(),
        })
        .unwrap();
    assert!(continued
        .state()
        .lines
        .iter()
        .all(|line| line.unit_price.minor() == 10_000));
}
