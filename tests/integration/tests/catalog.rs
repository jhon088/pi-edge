//! Catalog resolution: a scanned code becomes a priced line, and everything
//! that should be refused is refused.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixtureProduct, DEMO_BARCODE};
use edge_application::AppError;
use edge_catalog::{Barcode, CatalogError, Currency, ProductSelector};
use edge_domain::prelude::*;
use edge_integration_tests::*;
use edge_protocol::CatalogProductRecordV1;

fn selector(barcode: &str) -> ProductSelector {
    ProductSelector::Barcode(Barcode::new(barcode).unwrap())
}

#[test]
fn resolves_barcode_to_product_price_and_tax() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let transaction = core.add_item(demo_item()).unwrap();
    let line = &transaction.state().lines[0];
    assert_eq!(line.product_ref.as_str(), "SKU-001");
    assert_eq!(line.description, "Producto prueba");
    assert_eq!(line.sku.as_deref(), Some("SKU-001"));
    assert_eq!(line.barcode.as_deref(), Some(DEMO_BARCODE));
    assert_eq!(line.unit_price.minor(), 10_000);
    assert_eq!(line.tax_rate.bps(), 1_600);
    assert_eq!(line.price_mode, PriceMode::TaxExclusive);
    assert_eq!(line.tax_amount.minor(), 1_600);
    assert_eq!(line.line_total.minor(), 11_600);
}

#[test]
fn resolves_the_same_product_by_sku_and_by_product_ref() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    core.add_item(AddItemCommand {
        request_id: None,
        selector: ProductSelector::Sku("SKU-001".to_owned()),
        quantity: Quantity::from_units(1).unwrap(),
    })
    .unwrap();
    let transaction = core.add_item(item("SKU-001", 1)).unwrap();

    let lines = &transaction.state().lines;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].product_ref, lines[1].product_ref);
    assert_eq!(lines[0].unit_price, lines[1].unit_price);
    // Only the scanned line records a barcode.
    assert!(lines[0].barcode.is_none());
    assert!(lines[1].barcode.is_none());
}

#[test]
fn applies_quantity_to_totals() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(scan(DEMO_BARCODE, 3)).unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let totals = &totalized.state().totals;
    assert_eq!(totals.net_total.minor(), 30_000);
    assert_eq!(totals.tax_total.minor(), 4_800);
    assert_eq!(totals.grand_total.minor(), 34_800);
}

#[test]
fn supports_fractional_quantities() {
    let core = in_memory_core();
    init_device(&core);
    CatalogFixture::new(1)
        .with(FixtureProduct::new("SKU-BULK", "Granel", 333).barcode("7500000000333"))
        .activate(&core)
        .unwrap();
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    core.add_item(AddItemCommand {
        request_id: None,
        selector: selector("7500000000333"),
        quantity: Quantity::from_thousandths(2_500).unwrap(),
    })
    .unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    // 333 x 2.5 = 832.5 -> 833 ; 16% of 833 = 133.28 -> 133
    assert_eq!(totalized.state().totals.net_total.minor(), 833);
    assert_eq!(totalized.state().totals.tax_total.minor(), 133);
}

#[test]
fn refuses_an_unknown_barcode() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let error = core
        .add_item(scan("0000000000000", 1))
        .expect_err("an unknown barcode must not resolve");
    assert_eq!(error.kind(), "PRODUCT_NOT_FOUND");
    assert!(matches!(
        error,
        AppError::Catalog(CatalogError::ProductNotFound { .. })
    ));
}

#[test]
fn refuses_an_inactive_product() {
    let core = in_memory_core();
    init_device(&core);
    pricing_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let error = core
        .add_item(scan("7501000000003", 1))
        .expect_err("an inactive product must not be sold");
    assert_eq!(error.kind(), "PRODUCT_INACTIVE");
}

#[test]
fn refuses_a_product_that_is_not_sellable() {
    let core = in_memory_core();
    init_device(&core);
    pricing_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let error = core
        .add_item(scan("7501000000004", 1))
        .expect_err("a non-sellable product must not be sold");
    assert_eq!(error.kind(), "PRODUCT_NOT_SELLABLE");
}

#[test]
fn refuses_a_snapshot_with_a_duplicate_barcode() {
    let core = in_memory_core();
    init_device(&core);
    let error = CatalogFixture::new(1)
        .with(FixtureProduct::new("SKU-A", "A", 100).barcode("7500000000111"))
        .with(FixtureProduct::new("SKU-B", "B", 200).barcode("7500000000111"))
        .activate(&core)
        .expect_err("two products cannot share a barcode");
    assert_eq!(error.kind(), "CATALOG_DUPLICATE_BARCODE");
}

#[test]
fn refuses_a_record_without_a_usable_price_or_name() {
    // Record validation happens at the boundary, before a use case or the
    // database ever sees the row.
    let base = CatalogProductRecordV1 {
        product_ref: "SKU-FREE".to_owned(),
        external_ref: None,
        sku: None,
        name: "Sin precio".to_owned(),
        active: true,
        sellable: true,
        tax_profile: "IVA16".to_owned(),
        unit_price_minor: -1,
        barcodes: vec!["7500000000222".to_owned()],
    };
    let error = base.clone().into_domain(Currency::Mxn).unwrap_err();
    assert_eq!(error.code(), "CATALOG_INVALID_RECORD");

    let nameless = CatalogProductRecordV1 {
        name: "   ".to_owned(),
        unit_price_minor: 100,
        ..base.clone()
    };
    assert_eq!(
        nameless.into_domain(Currency::Mxn).unwrap_err().code(),
        "CATALOG_INVALID_RECORD"
    );

    let unnamed_product = CatalogProductRecordV1 {
        product_ref: "  ".to_owned(),
        unit_price_minor: 100,
        ..base
    };
    assert_eq!(
        unnamed_product
            .into_domain(Currency::Mxn)
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
}

#[test]
fn refuses_a_snapshot_whose_product_names_an_unknown_tax_profile() {
    let core = in_memory_core();
    init_device(&core);
    let error = CatalogFixture::new(1)
        .with(
            FixtureProduct::new("SKU-X", "Perfil inexistente", 100)
                .barcode("7500000000444")
                .tax_profile("IVA-DOES-NOT-EXIST"),
        )
        .activate(&core)
        .expect_err("a product must reference a declared profile");
    assert_eq!(error.kind(), "CATALOG_UNKNOWN_TAX_PROFILE");
}

#[test]
fn refuses_to_start_a_sale_without_an_active_catalog() {
    let core = in_memory_core();
    init_device(&core);
    open_day_and_session(&core);

    let error = core
        .start_transaction(StartTransactionCommand::default())
        .expect_err("a sale needs a catalog to price it");
    assert_eq!(error.kind(), "NO_ACTIVE_CATALOG");
}

#[test]
fn a_committed_snapshot_cannot_price_a_sale_until_it_is_activated() {
    let core = in_memory_core();
    init_device(&core);
    // Imported and verified, but not in force.
    let view = CatalogFixture::demo(1).import(&core).unwrap();
    assert_eq!(view.snapshot.status().as_str(), "PENDING");
    open_day_and_session(&core);

    assert_eq!(
        core.start_transaction(StartTransactionCommand::default())
            .unwrap_err()
            .kind(),
        "NO_ACTIVE_CATALOG"
    );
}

#[test]
fn journals_the_snapshot_that_priced_each_line() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();

    let events = core.get_journal(100).unwrap();
    let item_added = events
        .iter()
        .find(|event| event.event_type == JournalEventType::ItemAdded)
        .expect("ITEM_ADDED must be journalled");
    assert_eq!(item_added.payload_json["catalog_version"], 1);
    assert_eq!(item_added.payload_json["price_mode"], "TAX_EXCLUSIVE");
    assert_eq!(item_added.payload_json["unit_price_minor"], 10_000);
    assert!(item_added.payload_json["resolved_by"]
        .as_str()
        .unwrap()
        .contains(DEMO_BARCODE));

    // Importing and activating are business facts too.
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert!(types.contains(&"CATALOG_SNAPSHOT_IMPORTED"));
    assert!(types.contains(&"CATALOG_SNAPSHOT_ACTIVATED"));
}

#[test]
fn looks_up_a_product_without_touching_a_sale() {
    let core = in_memory_core();
    open_register(&core);

    let lookup = core.lookup_product(selector(DEMO_BARCODE)).unwrap();
    assert_eq!(lookup.product.product_ref.as_str(), "SKU-001");
    assert_eq!(lookup.product.unit_price.minor(), 10_000);
    assert_eq!(lookup.tax_profile.rate.bps(), 1_600);
    assert_eq!(lookup.snapshot_version, 1);
    assert!(core.get_active_transaction().unwrap().is_none());

    assert_eq!(
        core.lookup_product(selector("0000000000000"))
            .unwrap_err()
            .kind(),
        "PRODUCT_NOT_FOUND"
    );
}

#[test]
fn catalog_status_reports_what_the_register_is_running() {
    let core = in_memory_core();
    init_device(&core);
    CatalogFixture::demo(1).activate(&core).unwrap();
    CatalogFixture::demo(2).import(&core).unwrap();

    let status = core.get_catalog_status().unwrap();
    let active = status.active.as_ref().unwrap();
    assert_eq!(active.snapshot.version(), 1);
    assert_eq!(active.stored.products, 1);
    assert!(active.snapshot.state().activated_at.is_some());
    assert_eq!(status.pending.len(), 1);
    assert_eq!(status.pending[0].snapshot.version(), 2);
    assert!(status.importing.is_empty());
}
