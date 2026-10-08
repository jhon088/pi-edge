//! A sale in progress never changes price because a new catalog arrived.
//!
//! This is the property that makes a catalog update safe to apply during
//! business hours (ADR 013).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixtureProduct};
use edge_catalog::CatalogSnapshotStatus;
use edge_integration_tests::*;

const BARCODE: &str = "7500000000777";

fn catalog(version: i64, price_minor: i64) -> CatalogFixture {
    CatalogFixture::new(version).with(
        FixtureProduct::new("SKU-X", "Producto X", price_minor)
            .sku("SKU-X")
            .barcode(BARCODE),
    )
}

#[test]
fn an_open_sale_keeps_the_prices_of_the_snapshot_it_captured() {
    let core = in_memory_core();
    init_device(&core);

    // --- snapshot A: X costs 100.00 ---------------------------------------
    let snapshot_a = catalog(1, 10_000).activate(&core).unwrap();
    open_day_and_session(&core);

    let sale = core
        .start_transaction(StartTransactionCommand::default())
        .unwrap();
    assert_eq!(sale.catalog().unwrap().snapshot_id, snapshot_a);
    assert_eq!(sale.catalog().unwrap().version, 1);
    core.add_item(scan(BARCODE, 1)).unwrap();

    // --- snapshot B: X costs 120.00, imported and activated mid-sale ------
    let snapshot_b = catalog(2, 12_000).activate(&core).unwrap();
    assert_ne!(snapshot_a, snapshot_b);

    let status = core.get_catalog_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().snapshot.id(), snapshot_b);
    assert_eq!(status.active.as_ref().unwrap().snapshot.version(), 2);
    assert!(status.pending.is_empty());

    // --- the open sale is untouched ---------------------------------------
    let open_sale = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(open_sale.catalog().unwrap().snapshot_id, snapshot_a);
    assert_eq!(open_sale.state().lines[0].unit_price.minor(), 10_000);

    // ... and a line added now still resolves against snapshot A.
    let still_a = core.add_item(scan(BARCODE, 1)).unwrap();
    assert_eq!(still_a.state().lines.len(), 2);
    assert_eq!(still_a.state().lines[1].unit_price.minor(), 10_000);
    assert_eq!(still_a.state().lines[1].description, "Producto X");

    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().totals.net_total.minor(), 20_000);
    assert_eq!(totalized.state().totals.grand_total.minor(), 23_200);

    core.commit_cash_payment(CommitCashPaymentCommand {
        request_id: None,
        amount_minor: 23_200,
    })
    .unwrap();
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.catalog().unwrap().snapshot_id, snapshot_a);

    // --- the next sale uses the new catalog -------------------------------
    let next = core
        .start_transaction(StartTransactionCommand::default())
        .unwrap();
    assert_eq!(next.catalog().unwrap().snapshot_id, snapshot_b);
    assert_eq!(next.catalog().unwrap().version, 2);
    let priced = core.add_item(scan(BARCODE, 1)).unwrap();
    assert_eq!(priced.state().lines[0].unit_price.minor(), 12_000);
}

#[test]
fn activation_leaves_exactly_one_active_snapshot() {
    let core = in_memory_core();
    init_device(&core);
    let first = catalog(1, 10_000).activate(&core).unwrap();
    let second = catalog(2, 12_000).activate(&core).unwrap();
    let third = catalog(3, 13_000).activate(&core).unwrap();

    let status = core.get_catalog_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().snapshot.id(), third);
    assert!(status.pending.is_empty());
    assert!(status.importing.is_empty());

    // The older ones are kept, superseded, so any sale that captured them can
    // still be priced and audited.
    let all = core.get_catalog_status().unwrap();
    assert!(all.active.is_some());
    for snapshot_id in [first, second] {
        let import = core.get_catalog_import_status(snapshot_id).unwrap();
        assert_eq!(import.snapshot.status(), CatalogSnapshotStatus::Superseded);
        assert!(import.snapshot.resolves_sales());
    }
}

#[test]
fn a_superseded_snapshot_still_prices_the_sale_that_captured_it() {
    let core = in_memory_core();
    init_device(&core);
    catalog(1, 10_000).activate(&core).unwrap();
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(scan(BARCODE, 1)).unwrap();

    // Two further activations: the captured snapshot is now two generations old.
    catalog(2, 12_000).activate(&core).unwrap();
    catalog(3, 15_000).activate(&core).unwrap();

    let transaction = core.add_item(scan(BARCODE, 2)).unwrap();
    assert_eq!(transaction.state().lines[1].unit_price.minor(), 10_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().totals.net_total.minor(), 30_000);
}

#[test]
fn re_importing_identical_content_is_idempotent() {
    let core = in_memory_core();
    init_device(&core);
    let fixture = catalog(1, 10_000);
    let first = fixture.import(&core).unwrap();
    let again = fixture.import(&core).unwrap();
    assert_eq!(first.snapshot.id(), again.snapshot.id());
    assert_eq!(
        first.snapshot.state().content_hash,
        again.snapshot.state().content_hash
    );

    let status = core.get_catalog_status().unwrap();
    assert_eq!(status.pending.len(), 1);
}

#[test]
fn refuses_a_different_snapshot_under_an_existing_version() {
    let core = in_memory_core();
    init_device(&core);
    catalog(1, 10_000).import(&core).unwrap();

    let error = catalog(1, 12_000)
        .import(&core)
        .expect_err("the same version cannot hold different content");
    assert_eq!(error.kind(), "CATALOG_VERSION_CONFLICT");
}
