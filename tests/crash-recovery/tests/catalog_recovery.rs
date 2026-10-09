//! The catalog and the price it produced survive a process death.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixturePrice, FixtureProduct};
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
    let (expected_state, expected_pricing, expected_status) = {
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
            core.get_active_transaction().unwrap().unwrap().pricing(),
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
    assert_eq!(transaction.pricing(), expected_pricing);
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
        sale.pricing().unwrap()
    };

    // New process, new catalog activated while the sale is still open.
    let core = core_at(&db, 1_000);
    catalog(3, 20_000).activate(&core).unwrap();

    let transaction = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(transaction.pricing().unwrap(), captured);
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

/// A price change scheduled for later must not move an open sale, even after
/// the register has restarted on the other side of the boundary.
#[test]
fn a_scheduled_price_change_cannot_move_an_open_sale() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let boundary = edge_domain::prelude::Timestamp::parse_rfc3339("2026-10-10T00:00:00Z").unwrap();

    let catalog = |version: i64| {
        CatalogFixture::new(version).with(
            FixtureProduct::new("SKU-X", "Producto X", 0)
                .sku("SKU-X")
                .barcode(BARCODE)
                .priced(FixturePrice::base(10_000).until(boundary))
                .plus_price(FixturePrice::base(12_000).from(boundary)),
        )
    };

    // --- before the boundary: the sale opens at the old price -------------
    let (sale_id, captured) = {
        let core = core_at(&db, 1);
        open_register(&core);
        catalog(2).activate(&core).unwrap();
        let sale = core
            .start_transaction(StartTransactionCommand::default())
            .unwrap();
        let transaction = core
            .add_item(AddItemCommand {
                request_id: None,
                selector: edge_catalog::ProductSelector::Barcode(
                    edge_catalog::Barcode::new(BARCODE).unwrap(),
                ),
                quantity: edge_domain::prelude::Quantity::from_units(1).unwrap(),
            })
            .unwrap();
        assert_eq!(transaction.state().lines[0].unit_price.minor(), 10_000);
        (sale.id(), sale.pricing().unwrap())
    };
    assert!(captured.effective_at < boundary);

    // --- a new runtime, two days later, same open sale --------------------
    let core = core_at_instant(&db, 1_000, "2026-10-11T09:00:00Z");
    let recovered = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(recovered.id(), sale_id);
    // The pricing context came back exactly as captured.
    assert_eq!(recovered.pricing().unwrap(), captured);

    let continued = core
        .add_item(AddItemCommand {
            request_id: None,
            selector: edge_catalog::ProductSelector::Barcode(
                edge_catalog::Barcode::new(BARCODE).unwrap(),
            ),
            quantity: edge_domain::prelude::Quantity::from_units(1).unwrap(),
        })
        .unwrap();
    assert!(
        continued
            .state()
            .lines
            .iter()
            .all(|line| line.unit_price.minor() == 10_000),
        "the open sale must keep the price it started with"
    );
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(totalized.state().totals.net_total.minor(), 20_000);

    // Close it out, then start a fresh sale: now the new price applies.
    core.commit_cash_payment(CommitCashPaymentCommand {
        request_id: None,
        amount_minor: 23_200,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    let next = core
        .start_transaction(StartTransactionCommand::default())
        .unwrap();
    assert!(next.pricing().unwrap().effective_at > boundary);
    let priced = core
        .add_item(AddItemCommand {
            request_id: None,
            selector: edge_catalog::ProductSelector::Barcode(
                edge_catalog::Barcode::new(BARCODE).unwrap(),
            ),
            quantity: edge_domain::prelude::Quantity::from_units(1).unwrap(),
        })
        .unwrap();
    assert_eq!(priced.state().lines[0].unit_price.minor(), 12_000);
}

#[test]
fn applied_promotions_and_the_line_counter_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let promoted = CatalogFixture::new(2)
        .with(
            FixtureProduct::new("SKU-X", "Producto X", 1_000)
                .sku("SKU-X")
                .barcode(BARCODE),
        )
        .promoting(
            edge_catalog::promotions::Promotion::import(edge_catalog::promotions::NewPromotion {
                promotion_id: edge_domain::prelude::PromotionId::new("BOGO").unwrap(),
                code: "BOGO".to_owned(),
                name: "Lleva 3 paga 2".to_owned(),
                active: true,
                priority: 0,
                stackability: edge_catalog::promotions::Stackability::Stackable,
                scope: edge_catalog::scope::CatalogScope::Tenant,
                valid_from: None,
                valid_until: None,
                rule: edge_catalog::promotions::PromotionRule::BuyNGetM {
                    applies_to: edge_catalog::promotions::ProductApplicability::Product {
                        product_ref: edge_domain::prelude::ProductRef::new("SKU-X").unwrap(),
                    },
                    buy: edge_domain::prelude::Quantity::from_units(2).unwrap(),
                    get: edge_domain::prelude::Quantity::from_units(1).unwrap(),
                },
            })
            .unwrap(),
        );

    // --- lifetime 1: a promoted line, and a removed one before it ---------
    let (expected_adjustments, expected_discount) = {
        let core = core_at(&db, 1);
        open_register(&core);
        promoted.activate(&core).unwrap();
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        // Line 1 is added and removed, so line 1 is retired for good.
        core.add_item(scan_x(1)).unwrap();
        core.remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 1,
        })
        .unwrap();
        let sale = core.add_item(scan_x(6)).unwrap();
        assert_eq!(sale.state().lines[0].line_number, 2);
        assert_eq!(sale.state().lines[0].discount_amount.minor(), 2_000);
        (
            sale.applied_promotions().to_vec(),
            sale.state().lines[0].discount_amount.minor(),
        )
    };

    // --- lifetime 2: everything the sale granted is still there -----------
    // The id offset keeps the new lifetime's ids clear of the first one's,
    // exactly as a restarted daemon's random UUIDs would be.
    let core = core_at(&db, 1_000);
    let sale = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(sale.applied_promotions(), expected_adjustments.as_slice());
    assert_eq!(
        sale.state().lines[0].discount_amount.minor(),
        expected_discount
    );
    // The retired line number is not handed out again after the restart.
    let after = core.add_item(scan_x(1)).unwrap();
    let numbers: Vec<i64> = after
        .state()
        .lines
        .iter()
        .map(|line| line.line_number)
        .collect();
    assert_eq!(numbers, vec![2, 3]);
    // And re-evaluation across the restart is still deterministic: the second
    // line earns nothing, the first keeps exactly what it had.
    assert_eq!(after.state().lines[0].discount_amount.minor(), 2_000);
    assert_eq!(after.state().lines[1].discount_amount.minor(), 0);
}

fn scan_x(units: i64) -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        selector: edge_catalog::ProductSelector::Barcode(
            edge_catalog::Barcode::new(BARCODE).unwrap(),
        ),
        quantity: edge_domain::prelude::Quantity::from_units(units).unwrap(),
    }
}
