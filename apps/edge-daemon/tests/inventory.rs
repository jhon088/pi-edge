//! The inventory ledger over the real IPC transport.
//!
//! What only this layer can show: that the five inventory operations exist on
//! the wire and nothing more, that the DTOs carry a rendered quantity and an
//! explicit direction rather than a signed number, that a strict installation
//! refuses the sale with a stable error code, and that the CLI — a separate
//! process speaking the same contract — reaches the same ledger.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

use common::*;
use edge_client::EdgeClient;
use edge_protocol::{
    ImportInventoryBootstrapRequestV1, InventoryBootstrapItemV1, InventoryMovementRequestV1,
    InventoryQueryV1, StartPaymentRequestV1,
};

const SKU: &str = "SKU-001";

fn cash_sale(client: &edge_client::IpcEdgeClient, tendered_minor: i64) {
    client.start_transaction(None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    client.totalize(None).unwrap();
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: tendered_minor,
            },
            None,
        )
        .unwrap();
    client.finalize_transaction(None).unwrap();
}

#[test]
fn the_position_reports_the_ledger_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register(&client);

    let state = bootstrap_stock(&client, &[(SKU, 10)]);
    assert_eq!(state.source_snapshot_id, "WMS-SNAPSHOT-1");
    assert_eq!(state.item_count, 1);
    assert_eq!(state.store_id, "STORE-DEMO");

    cash_sale(&client, 20_000);

    let position = inventory_position(&client, SKU);
    assert_eq!(position.product_ref, SKU);
    // A rendered quantity, the same way money is rendered: the client never
    // divides by a thousand.
    assert_eq!(position.on_hand, "9.000");
    assert_eq!(position.on_hand_thousandths, 9_000);
    assert_eq!(position.movement_count, 2);
    assert!(position.last_movement_at.is_some());

    assert_eq!(position.movements.len(), 2);
    let opening = &position.movements[0];
    assert_eq!(opening.movement_type, "OPENING_BALANCE");
    assert_eq!(opening.direction, "IN");
    assert_eq!(opening.quantity, "10.000");
    assert_eq!(opening.source_ref, "WMS-SNAPSHOT-1");
    assert!(opening.transaction_id.is_none());

    let sale = &position.movements[1];
    assert_eq!(sale.movement_type, "SALE");
    // The direction is a field, not the sign of the quantity: the magnitude
    // stays positive on the wire.
    assert_eq!(sale.direction, "OUT");
    assert_eq!(sale.quantity, "1.000");
    assert_eq!(sale.quantity_thousandths, 1_000);
    let transaction_id = sale.transaction_id.as_ref().unwrap();
    assert_eq!(sale.line_number, Some(1));
    assert_eq!(sale.source_ref, format!("{transaction_id}:1"));
}

#[test]
fn manual_movements_cross_the_wire_and_demand_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let after_in = client
        .inventory_in(
            InventoryMovementRequestV1 {
                product_ref: SKU.to_owned(),
                quantity_thousandths: 2_500,
                reason_code: "RECEIPT".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(after_in.on_hand, "12.500");

    let after_out = client
        .inventory_out(
            InventoryMovementRequestV1 {
                product_ref: SKU.to_owned(),
                quantity_thousandths: 500,
                reason_code: "DAMAGE".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(after_out.on_hand, "12.000");

    let refused = client
        .inventory_out(
            InventoryMovementRequestV1 {
                product_ref: SKU.to_owned(),
                quantity_thousandths: 500,
                reason_code: "  ".to_owned(),
            },
            None,
        )
        .unwrap_err();
    assert_eq!(refused.code(), "INVENTORY_REASON_REQUIRED");
}

#[test]
fn the_movement_list_is_its_own_operation() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    cash_sale(&client, 20_000);

    let movements = client
        .inventory_movements(InventoryQueryV1 {
            product_ref: SKU.to_owned(),
            movement_limit: 1,
            offset: 1,
        })
        .unwrap();

    // A page of the ledger, oldest first, without the position attached: a
    // long history does not have to travel to be read.
    assert_eq!(movements.len(), 1);
    assert_eq!(movements[0].movement_type, "SALE");
}

#[test]
fn a_position_with_no_movements_asked_for_carries_none() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let position = client
        .inventory_position(InventoryQueryV1 {
            product_ref: SKU.to_owned(),
            movement_limit: 0,
            offset: 0,
        })
        .unwrap();

    assert_eq!(position.on_hand, "10.000");
    assert_eq!(position.movement_count, 1);
    assert!(
        position.movements.is_empty(),
        "a caller that did not ask for movements is not sent any"
    );
}

#[test]
fn a_competing_opening_position_is_refused_with_a_stable_code() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let error = client
        .import_inventory_bootstrap(
            ImportInventoryBootstrapRequestV1 {
                source_snapshot_id: "WMS-SNAPSHOT-2".to_owned(),
                generated_at: "2026-10-09T14:00:00Z".to_owned(),
                items: vec![InventoryBootstrapItemV1 {
                    product_ref: SKU.to_owned(),
                    quantity_thousandths: 3_000,
                }],
            },
            None,
        )
        .unwrap_err();

    assert_eq!(error.code(), "INVENTORY_ALREADY_BOOTSTRAPPED");
    assert_eq!(inventory_position(&client, SKU).on_hand, "10.000");
}

/// A malformed quantity is a malformed request, whichever operation carries
/// it. A client must not have to branch on the command to recognise the same
/// mistake.
#[test]
fn a_non_positive_quantity_is_invalid_argument_in_every_inventory_operation() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);

    for quantity in [0, -1_000] {
        let bootstrap = client
            .import_inventory_bootstrap(
                ImportInventoryBootstrapRequestV1 {
                    source_snapshot_id: "WMS-SNAPSHOT-1".to_owned(),
                    generated_at: "2026-10-08T14:00:00Z".to_owned(),
                    items: vec![InventoryBootstrapItemV1 {
                        product_ref: SKU.to_owned(),
                        quantity_thousandths: quantity,
                    }],
                },
                None,
            )
            .unwrap_err();
        assert_eq!(bootstrap.code(), "INVALID_ARGUMENT", "bootstrap {quantity}");

        let movement = InventoryMovementRequestV1 {
            product_ref: SKU.to_owned(),
            quantity_thousandths: quantity,
            reason_code: "RECOUNT".to_owned(),
        };
        let moved_in = client.inventory_in(movement.clone(), None).unwrap_err();
        assert_eq!(moved_in.code(), "INVALID_ARGUMENT", "in {quantity}");
        let moved_out = client.inventory_out(movement, None).unwrap_err();
        assert_eq!(moved_out.code(), "INVALID_ARGUMENT", "out {quantity}");
    }

    // A rejected payload leaves no trace: the store is still unbootstrapped,
    // so a corrected file can be sent.
    assert_eq!(inventory_position(&client, SKU).movement_count, 0);
    assert_eq!(bootstrap_stock(&client, &[(SKU, 10)]).item_count, 1);
}

/// The same frontier for the other fields of the same payload: a malformed
/// timestamp or product reference is `INVALID_ARGUMENT`, not a business
/// refusal.
#[test]
fn a_malformed_bootstrap_field_is_invalid_argument() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);

    let item = || InventoryBootstrapItemV1 {
        product_ref: SKU.to_owned(),
        quantity_thousandths: 1_000,
    };
    let bad_timestamp = client
        .import_inventory_bootstrap(
            ImportInventoryBootstrapRequestV1 {
                source_snapshot_id: "WMS-SNAPSHOT-1".to_owned(),
                generated_at: "ayer".to_owned(),
                items: vec![item()],
            },
            None,
        )
        .unwrap_err();
    assert_eq!(bad_timestamp.code(), "INVALID_ARGUMENT");

    let blank_product = client
        .import_inventory_bootstrap(
            ImportInventoryBootstrapRequestV1 {
                source_snapshot_id: "WMS-SNAPSHOT-1".to_owned(),
                generated_at: "2026-10-08T14:00:00Z".to_owned(),
                items: vec![InventoryBootstrapItemV1 {
                    product_ref: "   ".to_owned(),
                    quantity_thousandths: 1_000,
                }],
            },
            None,
        )
        .unwrap_err();
    assert_eq!(blank_product.code(), "INVALID_ARGUMENT");

    // An *unknown* product is different: the payload was well formed and the
    // core refused it on a rule, so it keeps its own inventory code.
    let unknown_product = client
        .import_inventory_bootstrap(
            ImportInventoryBootstrapRequestV1 {
                source_snapshot_id: "WMS-SNAPSHOT-1".to_owned(),
                generated_at: "2026-10-08T14:00:00Z".to_owned(),
                items: vec![InventoryBootstrapItemV1 {
                    product_ref: "SKU-GHOST".to_owned(),
                    quantity_thousandths: 1_000,
                }],
            },
            None,
        )
        .unwrap_err();
    assert_eq!(unknown_product.code(), "INVENTORY_UNKNOWN_PRODUCT");
}

#[test]
fn a_strict_installation_refuses_the_payment_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register_with_policy(&client, false);
    bootstrap_stock(&client, &[(SKU, 1)]);

    client.start_transaction(None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    client.add_item(demo_item(), None).unwrap();
    client.totalize(None).unwrap();
    let error = client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: 23_200,
            },
            None,
        )
        .unwrap_err();

    // The till learns it cannot sell *before* it takes money, and it learns it
    // through a code it can branch on (ADR 029).
    assert_eq!(error.code(), "INVENTORY_INSUFFICIENT_STOCK");
    assert_eq!(inventory_position(&client, SKU).on_hand, "1.000");
}

#[test]
fn the_cli_reaches_the_same_ledger_as_the_client() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "tauri-pos");
    open_register(&client);

    // The opening position arrives the way it will in production: a file the
    // CLI streams, not an inline payload.
    let fixture = dir.path().join("opening.json");
    std::fs::write(
        &fixture,
        serde_json::json!({
            "source_snapshot_id": "WMS-SNAPSHOT-1",
            "generated_at": "2026-10-08T14:00:00Z",
            "items": [{ "product_ref": SKU, "quantity_thousandths": 7_500 }],
        })
        .to_string(),
    )
    .unwrap();

    let imported = run_cli(&database, &["inventory-import", fixture.to_str().unwrap()]);
    assert_eq!(imported["item_count"], 1);

    run_cli(
        &database,
        &[
            "inventory-out",
            "--product",
            SKU,
            "--qty",
            "500",
            "--reason",
            "DAMAGE",
        ],
    );

    // Two processes, one contract, one ledger.
    let from_cli = run_cli(&database, &["inventory-position", "--product", SKU]);
    assert_eq!(from_cli["on_hand"], "7.000");
    assert_eq!(inventory_position(&client, SKU).on_hand, "7.000");
}

/// Run the CLI against a daemon and parse its stdout.
fn run_cli(database: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(cli_binary())
        .arg("--db")
        .arg(database)
        .arg("--format")
        .arg("json")
        .args(args)
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "edge-cli {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
