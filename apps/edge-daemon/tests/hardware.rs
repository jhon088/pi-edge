//! Hardware over the real IPC transport.
//!
//! What only this layer can show: that the **protocol carries intent, never
//! capability** (ADR 039). There is no operation that names a host, a port, a
//! driver or a byte, so a client cannot print to an arbitrary machine, cannot
//! choose a driver and cannot send an escape sequence. And a device that is
//! down is reported over the wire without touching a cent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use edge_client::EdgeClient;
use edge_protocol::{PrintTransactionRequestV1, SubmitScanRequestV1, TransactionRequestV1};

const SKU: &str = "SKU-001";
const UNIT: i64 = 11_600;

/// Simulated hardware, asked for explicitly. A test that uses fake devices
/// says so out loud (ADR 038).
const SIMULATED: [(&str, &str); 2] = [
    ("PI_EDGE_PRINTER", "simulator"),
    ("PI_EDGE_DRAWER", "simulator"),
];

fn scan(barcode: &str) -> SubmitScanRequestV1 {
    SubmitScanRequestV1 {
        event_id: uuid::Uuid::new_v4().to_string(),
        barcode: barcode.to_owned(),
        device_id: None,
    }
}

/// The production default, over the real transport: a daemon started with no
/// hardware configuration must not report simulators (ADR 038).
#[test]
fn a_daemon_with_no_hardware_configuration_reports_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    // No PI_EDGE_PRINTER, no PI_EDGE_DRAWER.
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let status = client.hardware_status().unwrap();
    for device in [&status.printer, &status.drawer] {
        assert_eq!(device.driver.as_deref(), Some("unconfigured"));
        assert_ne!(device.driver.as_deref(), Some("simulator"));
        assert_eq!(device.state, "UNAVAILABLE");
        assert_eq!(
            device.last_error_code.as_deref(),
            Some("HARDWARE_NOT_CONFIGURED")
        );
    }

    assert_eq!(
        client.print_test_page(None).unwrap_err().code(),
        "HARDWARE_NOT_CONFIGURED"
    );
    assert_eq!(
        client.open_drawer(None).unwrap_err().code(),
        "HARDWARE_NOT_CONFIGURED"
    );

    // And a sale still works end to end: hardware that is not there changes
    // no money and no stock (ADR 037).
    let sale = finalized_cash_sale(&client, UNIT);
    assert_eq!(
        client.cash_position().unwrap().cash_sales.formatted,
        "116.00"
    );
    assert_eq!(inventory_position(&client, SKU).on_hand, "9.000");
    assert_eq!(
        client
            .print_transaction(
                PrintTransactionRequestV1 {
                    transaction_id: sale,
                    reprint: false,
                },
                None,
            )
            .unwrap_err()
            .code(),
        "HARDWARE_NOT_CONFIGURED"
    );
}

#[test]
fn the_status_view_reports_every_device_without_disclosing_its_address() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    // A real ESC/POS address, so the test can prove it never comes back out.
    let _daemon = DaemonProcess::start_with_env(
        &database,
        None,
        true,
        &[
            ("PI_EDGE_PRINTER", "escpos_tcp:127.0.0.1:9100"),
            ("PI_EDGE_DRAWER", "escpos_kick"),
        ],
    );
    let client = client(&database, "tauri-pos");
    open_register(&client);

    let status = client.hardware_status().unwrap();
    assert_eq!(status.printer.driver.as_deref(), Some("escpos_tcp"));
    assert_eq!(status.drawer.driver.as_deref(), Some("escpos_kick"));
    assert_eq!(status.scanner.driver.as_deref(), Some("keyboard_wedge"));
    // Nothing is listening on that port in a test, so the honest answer is
    // unavailable — not "configured, therefore fine" (ADR 035).
    assert_eq!(status.printer.state, "UNAVAILABLE");
    assert_eq!(
        status.printer.last_error_code.as_deref(),
        Some("HARDWARE_UNAVAILABLE")
    );

    // The host and the port never cross the wire.
    let rendered = serde_json::to_string(&status).unwrap();
    assert!(!rendered.contains("127.0.0.1"), "{rendered}");
    assert!(!rendered.contains("9100"), "{rendered}");
}

#[test]
fn the_imin_driver_is_selected_and_refuses_without_substituting_another() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(
        &database,
        None,
        true,
        &[
            ("PI_EDGE_PRINTER", "escpos_tcp:127.0.0.1:9100"),
            ("PI_EDGE_DRAWER", "imin"),
        ],
    );
    let client = client(&database, "edge-cli");
    open_register(&client);

    let status = client.hardware_status().unwrap();
    // Selected as iMin, and reported as iMin: nothing silently became an
    // ESC/POS pulse, even though a printer is configured (ADR 038).
    assert_eq!(status.drawer.driver.as_deref(), Some("imin"));
    assert_eq!(status.drawer.state, "UNAVAILABLE");
    assert_eq!(
        status.drawer.last_error_code.as_deref(),
        Some("HARDWARE_NOT_CONFIGURED")
    );

    let error = client.open_drawer(None).unwrap_err();
    assert_eq!(error.code(), "HARDWARE_NOT_CONFIGURED");
}

#[test]
fn a_scan_crosses_the_wire_and_becomes_one_item() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    client.start_transaction(None).unwrap();

    let sale = client.submit_scan(scan(DEMO_BARCODE)).unwrap();
    assert_eq!(sale.lines.len(), 1);
    assert_eq!(sale.lines[0].quantity, "1.000");

    // The same event resent is one unit; a second physical scan is two.
    let request = scan(DEMO_BARCODE);
    client.submit_scan(request.clone()).unwrap();
    let replayed = client.submit_scan(request).unwrap();
    let total: i64 = replayed
        .lines
        .iter()
        .map(|line| line.quantity_thousandths)
        .sum();
    assert_eq!(total, 2_000);

    let third = client.submit_scan(scan(DEMO_BARCODE)).unwrap();
    let total: i64 = third
        .lines
        .iter()
        .map(|line| line.quantity_thousandths)
        .sum();
    assert_eq!(total, 3_000);
}

#[test]
fn a_scan_with_no_sale_open_answers_a_scanner_specific_code() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let error = client.submit_scan(scan(DEMO_BARCODE)).unwrap_err();
    // A till can tell this apart from a command issued with no sale open, and
    // PI.Edge still does not open one on its own (ADR 036).
    assert_eq!(error.code(), "SCAN_NO_ACTIVE_TRANSACTION");
}

#[test]
fn a_malformed_scan_payload_is_invalid_argument() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    client.start_transaction(None).unwrap();

    let error = client
        .submit_scan(SubmitScanRequestV1 {
            event_id: "not-a-uuid".to_owned(),
            barcode: DEMO_BARCODE.to_owned(),
            device_id: None,
        })
        .unwrap_err();
    assert_eq!(error.code(), "INVALID_ARGUMENT");
}

#[test]
fn a_whole_cash_sale_prints_and_opens_the_drawer_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED);
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let sale = finalized_cash_sale(&client, UNIT);
    // The simulated drawer answered, so the device is known to work and the
    // log says when.
    let status = client.hardware_status().unwrap();
    assert_eq!(status.drawer.state, "AVAILABLE");
    assert!(status.drawer.last_success_at.is_some());

    let after_print = client
        .print_transaction(
            PrintTransactionRequestV1 {
                transaction_id: sale.clone(),
                reprint: false,
            },
            None,
        )
        .unwrap();
    assert_eq!(after_print.printer.state, "AVAILABLE");

    // A reprint is always allowed and changes nothing.
    let cash_before = client.cash_position().unwrap().expected_cash.formatted;
    client
        .print_transaction(
            PrintTransactionRequestV1 {
                transaction_id: sale,
                reprint: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(
        client.cash_position().unwrap().expected_cash.formatted,
        cash_before
    );
    assert_eq!(inventory_position(&client, SKU).on_hand, "9.000");
}

#[test]
fn a_printer_that_is_down_is_reported_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(
        &database,
        None,
        true,
        &[("PI_EDGE_PRINTER", "simulator:unavailable")],
    );
    let client = client(&database, "edge-cli");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&client, UNIT);
    let cash_before = client.cash_position().unwrap().expected_cash.formatted;

    let error = client
        .print_transaction(
            PrintTransactionRequestV1 {
                transaction_id: sale.clone(),
                reprint: false,
            },
            None,
        )
        .unwrap_err();
    assert_eq!(error.code(), "HARDWARE_UNAVAILABLE");

    // The sale is still finalized and paid, the stock still moved, the drawer
    // ledger is still right. Paper changes none of it (ADR 005).
    let state = client.get_edge_state().unwrap();
    assert!(state.active_transaction.is_none());
    assert_eq!(
        client.cash_position().unwrap().expected_cash.formatted,
        cash_before
    );
    assert_eq!(inventory_position(&client, SKU).on_hand, "9.000");
    assert_eq!(
        client
            .transaction_refunds(TransactionRequestV1 {
                transaction_id: sale
            })
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn a_diagnostic_print_and_a_diagnostic_open_carry_no_payload_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED);
    let client = client(&database, "tauri-pos");
    open_register(&client);

    // The two diagnostic operations take nothing but a request id. There is no
    // field for a host, a driver, a byte or a copy count, which is what makes
    // them safe to expose to a UI (ADR 039).
    let printed = client.print_test_page(Some(uuid::Uuid::new_v4())).unwrap();
    assert_eq!(printed.printer.state, "AVAILABLE");
    let opened = client.open_drawer(Some(uuid::Uuid::new_v4())).unwrap();
    assert_eq!(opened.drawer.state, "AVAILABLE");
}

#[test]
fn the_cli_drives_the_hardware_surface() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start_with_env(&database, None, true, &SIMULATED);
    let client = client(&database, "tauri-pos");
    open_register(&client);
    bootstrap_stock(&client, &[(SKU, 10)]);

    let status = run_cli(&database, &["hardware-status"]);
    assert_eq!(status["printer"]["driver"], "simulator");

    run_cli(&database, &["printer-test"]);
    run_cli(&database, &["drawer-open"]);

    // A scan through the CLI reaches the same sale.
    run_cli(&database, &["start-sale"]);
    let sale = run_cli(&database, &["scan", "--barcode", DEMO_BARCODE]);
    assert_eq!(sale["lines"].as_array().unwrap().len(), 1);
}

/// Run the CLI against a daemon and parse its stdout.
fn run_cli(database: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = std::process::Command::new(cli_binary())
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
