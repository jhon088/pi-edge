//! Price resolution against real price books in SQLite.
//!
//! The unit tests in `edge-catalog` pin the ranking rules; these check that the
//! same rules hold once books and entries come back out of the database, and
//! that a sale refuses rather than guesses.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixturePrice, FixtureProduct, BASE_BOOK};
use edge_catalog::CatalogError;
use edge_domain::prelude::*;
use edge_integration_tests::*;

const BARCODE: &str = "7500000000900";
const STORE_BOOK: &str = "STORE-DEMO-BOOK";
const OTHER_BOOK: &str = "PROMO";

/// `2026-10-09T00:00:00Z`: the day after the integration clock starts.
fn tomorrow() -> Timestamp {
    Timestamp::parse_rfc3339("2026-10-09T00:00:00Z").unwrap()
}

fn product(price_minor: i64) -> FixtureProduct {
    FixtureProduct::new("SKU-PRICED", "Producto con precio", price_minor)
        .sku("SKU-PRICED")
        .barcode(BARCODE)
}

/// Open a register against a catalog built for this test.
fn register_with(core: &TestCore, catalog: CatalogFixture) {
    init_device(core);
    catalog.activate(core).unwrap();
    open_day_and_session(core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
}

#[test]
fn resolves_the_base_price() {
    let core = in_memory_core();
    register_with(&core, CatalogFixture::new(1).with(product(10_000)));

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    let line = &transaction.state().lines[0];
    assert_eq!(line.unit_price.minor(), 10_000);
    assert_eq!(
        line.price_book_id.as_ref().map(ToString::to_string),
        Some(BASE_BOOK.to_owned())
    );
    assert_eq!(line.price_mode, PriceMode::TaxExclusive);
}

#[test]
fn a_store_book_overrides_the_base() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with_store_book(STORE_BOOK, "STORE-DEMO", 0)
            .with(product(10_000).plus_price(FixturePrice::base(8_900).in_book(STORE_BOOK))),
    );

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    let line = &transaction.state().lines[0];
    assert_eq!(line.unit_price.minor(), 8_900);
    assert_eq!(
        line.price_book_id.as_ref().map(ToString::to_string),
        Some(STORE_BOOK.to_owned())
    );
}

#[test]
fn a_book_scoped_to_another_store_is_ignored() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with_store_book(STORE_BOOK, "STORE-ELSEWHERE", 99)
            .with(product(10_000).plus_price(FixturePrice::base(1).in_book(STORE_BOOK))),
    );

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    assert_eq!(transaction.state().lines[0].unit_price.minor(), 10_000);
}

#[test]
fn an_inactive_book_is_ignored() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with_store_book(STORE_BOOK, "STORE-DEMO", 0)
            .with(product(10_000).plus_price(FixturePrice::base(1).in_book(STORE_BOOK)))
            .deactivate_book(STORE_BOOK),
    );

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    assert_eq!(transaction.state().lines[0].unit_price.minor(), 10_000);
}

#[test]
fn priority_decides_between_books_of_the_same_scope() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with_tenant_book(OTHER_BOOK, 10)
            .with(product(10_000).plus_price(FixturePrice::base(9_500).in_book(OTHER_BOOK))),
    );

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    assert_eq!(transaction.state().lines[0].unit_price.minor(), 9_500);
    assert_eq!(
        transaction.state().lines[0]
            .price_book_id
            .as_ref()
            .map(ToString::to_string),
        Some(OTHER_BOOK.to_owned())
    );
}

#[test]
fn a_future_price_is_not_used_yet() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1).with(
            product(0)
                .priced(FixturePrice::base(10_000).until(tomorrow()))
                .plus_price(FixturePrice::base(12_000).from(tomorrow())),
        ),
    );

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    assert_eq!(transaction.state().lines[0].unit_price.minor(), 10_000);
}

#[test]
fn an_expired_price_is_ignored() {
    let core = in_memory_core();
    let yesterday = Timestamp::parse_rfc3339("2026-10-07T00:00:00Z").unwrap();
    register_with(
        &core,
        CatalogFixture::new(1).with(
            product(0)
                .priced(FixturePrice::base(10_000).until(yesterday))
                .plus_price(FixturePrice::base(12_000).from(yesterday)),
        ),
    );

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    assert_eq!(transaction.state().lines[0].unit_price.minor(), 12_000);
}

#[test]
fn a_product_without_a_price_cannot_be_sold() {
    let core = in_memory_core();
    register_with(&core, CatalogFixture::new(1).with(product(0).unpriced()));

    let error = core
        .add_item(scan(BARCODE, 1))
        .expect_err("a product nobody priced cannot be sold");
    assert_eq!(error.kind(), "PRICE_NOT_FOUND");
    assert!(matches!(
        error,
        edge_application::AppError::Catalog(CatalogError::PriceNotFound { .. })
    ));
    // Nothing was added to the sale.
    assert!(core
        .get_active_transaction()
        .unwrap()
        .unwrap()
        .state()
        .lines
        .is_empty());
}

#[test]
fn two_equally_specific_prices_are_refused_instead_of_guessed() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            // Same scope, same priority as the base book.
            .with_tenant_book(OTHER_BOOK, 0)
            .with(product(10_000).plus_price(FixturePrice::base(11_000).in_book(OTHER_BOOK))),
    );

    let error = core
        .add_item(scan(BARCODE, 1))
        .expect_err("an ambiguous price must be refused");
    assert_eq!(error.kind(), "AMBIGUOUS_PRICE");
    match error {
        edge_application::AppError::Catalog(CatalogError::AmbiguousPrice {
            price_books, ..
        }) => assert!(price_books.contains(BASE_BOOK) && price_books.contains(OTHER_BOOK)),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn overlapping_schedules_in_one_book_are_refused() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1).with(
            product(0)
                // Both windows contain the clock's instant.
                .priced(FixturePrice::base(10_000).until(tomorrow()))
                .plus_price(
                    FixturePrice::base(12_000)
                        .from(Timestamp::parse_rfc3339("2026-10-07T00:00:00Z").unwrap()),
                ),
        ),
    );

    assert_eq!(
        core.add_item(scan(BARCODE, 1)).unwrap_err().kind(),
        "AMBIGUOUS_PRICE"
    );
}

#[test]
fn both_price_modes_resolve_from_the_same_tax_profile() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1).with(product(10_000)).with(
            FixtureProduct::new("SKU-INC", "Precio con IVA", 0)
                .barcode("7500000000901")
                .priced(FixturePrice::base(11_600).inclusive()),
        ),
    );

    let transaction = core.add_item(scan(BARCODE, 1)).unwrap();
    let exclusive = &transaction.state().lines[0];
    assert_eq!(exclusive.taxable_amount.minor(), 10_000);
    assert_eq!(exclusive.tax_amount.minor(), 1_600);
    assert_eq!(exclusive.line_total.minor(), 11_600);

    let transaction = core.add_item(scan("7500000000901", 1)).unwrap();
    let inclusive = &transaction.state().lines[1];
    assert_eq!(inclusive.price_mode, PriceMode::TaxInclusive);
    assert_eq!(inclusive.taxable_amount.minor(), 10_000);
    assert_eq!(inclusive.tax_amount.minor(), 1_600);
    assert_eq!(inclusive.line_total.minor(), 11_600);

    // Same rate, same totals, different quoted price.
    assert_eq!(exclusive.tax_rate, inclusive.tax_rate);
    assert_eq!(exclusive.unit_price.minor(), 10_000);
    assert_eq!(inclusive.unit_price.minor(), 11_600);
}

#[test]
fn the_journal_records_which_book_priced_the_line() {
    let core = in_memory_core();
    register_with(
        &core,
        CatalogFixture::new(1)
            .with_store_book(STORE_BOOK, "STORE-DEMO", 0)
            .with(product(10_000).plus_price(FixturePrice::base(8_900).in_book(STORE_BOOK))),
    );
    core.add_item(scan(BARCODE, 1)).unwrap();

    let events = core.get_journal(100).unwrap();
    let item_added = events
        .iter()
        .find(|event| event.event_type == JournalEventType::ItemAdded)
        .expect("ITEM_ADDED must be journalled");
    assert_eq!(item_added.payload_json["price_book_id"], STORE_BOOK);
    assert_eq!(item_added.payload_json["unit_price_minor"], 8_900);
    assert!(item_added.payload_json["priced_at"].is_string());
}

#[test]
fn a_snapshot_without_a_price_book_is_refused_at_import() {
    let core = in_memory_core();
    init_device(&core);
    let mut fixture = CatalogFixture::new(1).with(product(10_000));
    fixture.books.clear();
    let error = fixture
        .import(&core)
        .expect_err("a catalog with no price book can sell nothing");
    assert_eq!(error.kind(), "CATALOG_INVALID_RECORD");
}

#[test]
fn a_price_naming_an_undeclared_book_is_refused_at_import() {
    let core = in_memory_core();
    init_device(&core);
    let error = CatalogFixture::new(1)
        .with(product(10_000).plus_price(FixturePrice::base(1).in_book("GHOST")))
        .import(&core)
        .expect_err("a price must name a declared book");
    assert_eq!(error.kind(), "CATALOG_UNKNOWN_PRICE_BOOK");
}
