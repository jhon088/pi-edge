//! The promotion surface over the real IPC transport.
//!
//! The engine is covered by unit and integration tests; this file checks the
//! contract a UI actually sees: the new operations, the adjustments on the
//! transaction DTO, the stable refusal code, and promotions travelling through
//! a chunked import.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use edge_client::EdgeClient;
use edge_protocol::{
    AddItemRequestV1, ChangeQuantityRequestV1, ProductSelectorV1, PromotionRuleV1,
    PutCatalogChunkRequestV1, RemoveItemRequestV1,
};

const BARCODE_A: &str = "7500000002001";
const BARCODE_B: &str = "7500000002002";

fn scan(barcode: &str, units: i64) -> AddItemRequestV1 {
    AddItemRequestV1 {
        product: ProductSelectorV1::Barcode(barcode.to_owned()),
        quantity_thousandths: units * 1_000,
        selections: Vec::new(),
        preparation_note: None,
    }
}

/// Device, a promotional catalog, the day and a session.
fn open_register_with(client: &edge_client::IpcEdgeClient, spec: &CatalogSpec) {
    client
        .init_device(edge_protocol::InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
            allow_negative_inventory: true,
        })
        .unwrap();
    spec.import(client, true);
    client.open_business_day(None).unwrap();
    client
        .open_session(
            edge_protocol::OpenRegisterSessionRequestV1 {
                register_id: "REG-001".to_owned(),
                operator_id: "CASHIER-001".to_owned(),
                opening_cash_minor: 100_000,
            },
            None,
        )
        .unwrap();
}

fn promotional_catalog() -> CatalogSpec {
    CatalogSpec::with_products(
        1,
        vec![
            product(BARCODE_A, "SKU-A", 10_000, "IVA16"),
            product(BARCODE_B, "SKU-B", 5_000, "IVA16"),
        ],
    )
    .promoting(percentage_off_record("PCT-A", "SKU-A", 1_000))
    .promoting(promotion_record(
        "BOGO-B",
        0,
        "STACKABLE",
        PromotionRuleV1::BuyNGetM {
            product_ref: Some("SKU-B".to_owned()),
            buy_thousandths: 2_000,
            get_thousandths: 1_000,
        },
    ))
}

#[test]
fn the_transaction_dto_carries_every_adjustment_the_core_applied() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register_with(&client, &promotional_catalog());

    client.start_transaction(None).unwrap();
    client.add_item(scan(BARCODE_A, 1), None).unwrap();
    let sale = client.add_item(scan(BARCODE_B, 3), None).unwrap();

    assert_eq!(sale.applied_promotions.len(), 2);
    let pct = &sale.applied_promotions[0];
    assert_eq!(pct.code, "PCT-A");
    assert_eq!(pct.target, "LINE");
    assert_eq!(pct.line_number, Some(1));
    assert_eq!(pct.amount.minor, 1_000);
    assert_eq!(pct.reason, "PERCENTAGE_OFF");
    assert_eq!(pct.stackability, "STACKABLE");
    assert_eq!(pct.adjustment_id, "PCT-A#line:1");

    let bogo = &sale.applied_promotions[1];
    assert_eq!(bogo.code, "BOGO-B");
    assert_eq!(bogo.line_number, Some(2));
    assert_eq!(bogo.amount.minor, 5_000);
    assert_eq!(
        bogo.detail.as_deref(),
        Some("1 application(s), 1 free unit(s)")
    );

    // The discounts the lines carry and the adjustments reported agree.
    let line_discounts: i64 = sale
        .lines
        .iter()
        .map(|line| line.discount_amount.minor)
        .sum();
    assert_eq!(line_discounts, 6_000);
    // Net + tax is still gross, on every line.
    for line in &sale.lines {
        assert_eq!(
            line.taxable_amount.minor + line.tax_amount.minor,
            line.line_total.minor
        );
    }

    // The totals are only official once the sale is totalized, and then they
    // report the same discount the lines carry.
    let totalized = client.totalize(None).unwrap();
    assert_eq!(totalized.totals.discount_total.minor, 6_000);
    assert_eq!(totalized.totals.subtotal.minor, 10_000 + 15_000);
    assert_eq!(
        totalized.totals.net_total.minor + totalized.totals.tax_total.minor,
        totalized.totals.grand_total.minor
    );
    // And the adjustments survive totalization unchanged.
    assert_eq!(totalized.applied_promotions.len(), 2);
}

#[test]
fn remove_item_and_change_quantity_re_evaluate_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register_with(&client, &promotional_catalog());

    client.start_transaction(None).unwrap();
    client.add_item(scan(BARCODE_A, 1), None).unwrap();
    client.add_item(scan(BARCODE_B, 3), None).unwrap();

    // Six units earn the buy-2-get-1 twice.
    let changed = client
        .change_quantity(
            ChangeQuantityRequestV1 {
                line_number: 2,
                quantity_thousandths: 6_000,
            },
            None,
        )
        .unwrap();
    assert_eq!(changed.lines[1].discount_amount.minor, 10_000);

    // Removing that line retracts its adjustment.
    let removed = client
        .remove_item(RemoveItemRequestV1 { line_number: 2 }, None)
        .unwrap();
    assert_eq!(removed.lines.len(), 1);
    assert_eq!(removed.applied_promotions.len(), 1);
    assert_eq!(removed.applied_promotions[0].code, "PCT-A");

    // A line number that is not there is a stable, specific refusal.
    let error = client
        .remove_item(RemoveItemRequestV1 { line_number: 99 }, None)
        .expect_err("no such line");
    assert_eq!(error.code(), "LINE_NOT_FOUND");
}

#[test]
fn two_exclusive_promotions_at_one_priority_surface_as_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");

    let spec = CatalogSpec::with_products(1, vec![product(BARCODE_A, "SKU-A", 10_000, "IVA16")])
        .promoting(promotion_record(
            "EXC-A",
            10,
            "EXCLUSIVE",
            PromotionRuleV1::PercentageOff {
                product_ref: Some("SKU-A".to_owned()),
                percentage_bps: 1_000,
            },
        ))
        .promoting(promotion_record(
            "EXC-B",
            10,
            "EXCLUSIVE",
            PromotionRuleV1::FixedAmountOff {
                product_ref: Some("SKU-A".to_owned()),
                amount_minor: 2_000,
            },
        ));
    open_register_with(&client, &spec);

    client.start_transaction(None).unwrap();
    let error = client
        .add_item(scan(BARCODE_A, 1), None)
        .expect_err("the core must refuse to guess");
    assert_eq!(error.code(), "AMBIGUOUS_PROMOTION");

    // The refusal left nothing behind: the sale is still empty.
    let state = client.get_edge_state().unwrap();
    assert!(state.active_transaction.unwrap().lines.is_empty());
}

#[test]
fn promotions_are_part_of_the_imported_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    let spec = promotional_catalog();
    open_register_with(&client, &spec);

    // Counted, and verified against the declared count at commit.
    let status = client.catalog_status().unwrap();
    let active = status.active.expect("a snapshot is in force");
    assert_eq!(active.stored.promotions, 2);
    assert_eq!(active.snapshot.expected.promotions, 2);
    // The hash the core computed from its own rows matched the declared one,
    // which now covers promotions.
    assert_eq!(
        active.snapshot.content_hash.as_deref(),
        Some(active.snapshot.declared_content_hash.as_str())
    );
}

#[test]
fn a_declared_promotion_count_that_does_not_match_blocks_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    client
        .init_device(edge_protocol::InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
            allow_negative_inventory: true,
        })
        .unwrap();

    let spec = promotional_catalog();
    let mut request = spec.begin_request();
    // Claim one promotion too many.
    request.expected.promotions += 1;
    let opened = client.begin_catalog_import(request, None).unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();
    client
        .put_catalog_chunk(spec.chunk(&snapshot_id, 0, &spec.products))
        .unwrap();

    let error = client
        .commit_catalog_import(
            edge_protocol::CommitCatalogImportRequestV1 {
                snapshot_id: snapshot_id.clone(),
                chunk_count: 1,
            },
            None,
        )
        .expect_err("the count is wrong");
    assert_eq!(error.code(), "CATALOG_COUNT_MISMATCH");

    // The snapshot stays IMPORTING and resumable, never half-usable.
    let progress = client.catalog_import_status(&snapshot_id).unwrap();
    assert_eq!(progress.snapshot.status, "IMPORTING");
    assert!(client.catalog_status().unwrap().active.is_none());
}

#[test]
fn a_promotion_naming_a_product_from_another_chunk_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    client
        .init_device(edge_protocol::InitDeviceRequestV1 {
            tenant_id: "TENANT-DEMO".to_owned(),
            store_id: "STORE-DEMO".to_owned(),
            currency: "MXN".to_owned(),
            allow_negative_inventory: true,
        })
        .unwrap();

    let spec = promotional_catalog();
    let opened = client
        .begin_catalog_import(spec.begin_request(), None)
        .unwrap();
    let snapshot_id = opened.snapshot.snapshot_id.clone();

    // Send only product B, but carry the promotion that names product A.
    let mut chunk = spec.chunk(&snapshot_id, 0, &spec.products[1..]);
    chunk.promotions = vec![percentage_off_record("PCT-A", "SKU-A", 1_000)];
    let error = client
        .put_catalog_chunk(chunk)
        .expect_err("the chunk is internally inconsistent");
    assert_eq!(error.code(), "CATALOG_INVALID_RECORD");

    // And the chunk was rejected whole: nothing from it was stored.
    let progress = client.catalog_import_status(&snapshot_id).unwrap();
    assert!(progress.progress.received_chunks.is_empty());
    assert_eq!(progress.progress.stored.products, 0);

    // The import is still open, so the client can simply send it correctly.
    let good = PutCatalogChunkRequestV1 {
        ..spec.chunk(&snapshot_id, 0, &spec.products)
    };
    client.put_catalog_chunk(good).unwrap();
    let progress = client.catalog_import_status(&snapshot_id).unwrap();
    assert_eq!(progress.progress.stored.promotions, 2);
}
