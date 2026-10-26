//! The admin screen's contract: configure a receipt printer, keep it across
//! restarts, and never let paper decide what happened to a sale.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{client, DaemonProcess};
use edge_client::EdgeClient;
use edge_protocol::{
    AddItemRequestV1, PrinterSettingsV1, ProductSelectorV1, StartPaymentRequestV1,
};

fn settings(queue: &str, auto_print: bool) -> PrinterSettingsV1 {
    PrinterSettingsV1 {
        enabled: true,
        model: "RPT008".to_owned(),
        queue: queue.to_owned(),
        columns: 48,
        auto_print,
    }
}

/// What an operator chose is still chosen after the runtime restarts.
///
/// The whole point of the screen: configure the printer once, on the machine,
/// and have the kiosk come back up driving it. Stored beside the lock and the
/// token rather than in the sales database, because which box is on this desk
/// is not a commercial fact.
#[test]
fn a_configured_printer_survives_a_restart_of_the_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");

    {
        let _daemon = DaemonProcess::start(&database);
        let client = client(&database, "edge-cli");

        // Nothing configured is nothing configured — not a simulator.
        let before = client.printer_settings().unwrap();
        assert!(!before.settings.enabled);
        assert_eq!(before.driver, "unconfigured");
        assert_eq!(before.reachable, None, "nothing to probe is not `false`");

        // Every model the build offers is real and complete.
        assert_eq!(before.models.len(), 3);
        for model in &before.models {
            assert!(!model.label.is_empty());
            assert!(model.columns >= 24 && model.paper_mm > 0);
        }
        let ids: Vec<&str> = before.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["RPT004", "RPT008", "RPT015"]);

        let after = client
            .set_printer_settings(settings("PX_RECEIPT", true), None)
            .unwrap();
        assert!(after.settings.enabled);
        assert_eq!(after.settings.model, "RPT008");
        assert_eq!(after.settings.queue, "PX_RECEIPT");
        assert!(after.settings.auto_print);
        assert_eq!(
            after.driver, "escpos_spooler",
            "the adapter changed without a restart"
        );
    }

    // Same installation, new process.
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");
    let reopened = client.printer_settings().unwrap();
    assert_eq!(reopened.settings, settings("PX_RECEIPT", true));
    assert_eq!(reopened.driver, "escpos_spooler");
}

/// A configuration that could not print is refused before it is stored.
#[test]
fn a_printer_configuration_that_cannot_work_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");

    for bad in [
        settings("", false),          // no printer chosen
        settings("-o raw", false),    // an option, not a queue
        settings("two words", false), // not a queue name either
        PrinterSettingsV1 {
            model: "RPT999".to_owned(), // not a model this build drives
            ..settings("PX_RECEIPT", false)
        },
        PrinterSettingsV1 {
            columns: 4, // narrower than any paper
            ..settings("PX_RECEIPT", false)
        },
    ] {
        let refused = client.set_printer_settings(bad.clone(), None);
        assert!(refused.is_err(), "`{bad:?}` must be refused");
    }

    // And nothing was stored by the attempts.
    assert!(!client.printer_settings().unwrap().settings.enabled);
}

/// Paper never decides what happened to the money.
///
/// Auto-print is configured and the queue does not exist, so printing fails.
/// The sale must still be finalized, still be paid, and still be gone from
/// the register — the receipt's own fate is recorded separately (ADR 043).
#[test]
fn a_sale_stays_finalized_when_its_receipt_cannot_be_printed() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let seeder = client(&database, "edge-cli");
    let output = std::process::Command::new(common::cli_binary())
        .arg("--db")
        .arg(&database)
        .arg("--format")
        .arg("json")
        .arg("demo-seed")
        .arg("--path")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo"))
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "demo-seed failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A printer pointed at a queue this host does not have.
    seeder
        .set_printer_settings(settings("PX_NO_SUCH_QUEUE", true), None)
        .unwrap();

    let client = client(&database, "tauri-pos");
    client.start_transaction(None).unwrap();
    client
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("NUGGETS".to_owned()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    let totalized = client.totalize(None).unwrap();
    client
        .start_payment(
            StartPaymentRequestV1 {
                tender_type: "CASH".to_owned(),
                amount_minor: totalized.totals.grand_total.minor,
            },
            None,
        )
        .unwrap();
    let finalized = client.finalize_transaction(None).unwrap();
    assert_eq!(finalized.status, "FINALIZED");
    assert_eq!(finalized.balance_due.minor, 0);

    // Now the paper. It cannot succeed, and it must not matter.
    let printed = client.print_transaction(
        edge_protocol::PrintTransactionRequestV1 {
            transaction_id: finalized.transaction_id.clone(),
            reprint: false,
        },
        None,
    );
    assert!(printed.is_err(), "there is no such queue to print to");

    // The receipt knows it did not come out, and says so in the vocabulary
    // the lifecycle already has. That is an operational obligation, not a
    // doubt about the sale (ADR 043).
    let receipt = client.get_receipt(&finalized.transaction_id).unwrap();
    assert!(
        matches!(receipt.status.as_str(), "FAILED" | "UNKNOWN" | "PENDING"),
        "the receipt reported `{}`",
        receipt.status
    );
    assert!(receipt.last_error_code.is_some(), "and why it failed");

    // The sale is untouched: still closed, still settled, still gone from the
    // register.
    assert!(
        client.get_active_transaction().unwrap().is_none(),
        "a failed print must not reopen the register"
    );
    let recent = client.list_recent_sales(1, 0).unwrap();
    let sale = recent.sales.first().expect("the sale is on the record");
    assert_eq!(sale.transaction_id, finalized.transaction_id);

    // And the till can keep selling.
    client.start_transaction(None).unwrap();
    assert!(client.get_active_transaction().unwrap().is_some());
}

/// The host's own printers reach the admin screen.
///
/// Not an assertion that this machine has one — a build server has none, and
/// an empty list is a correct answer. What is asserted is that whatever the
/// spooler reports arrives intact and usable: a bare queue name, not a
/// sentence in whatever language the host is set to.
#[test]
fn whatever_printers_the_host_has_are_offered_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);
    let client = client(&database, "edge-cli");

    let config = client.printer_settings().unwrap();
    for queue in &config.available {
        assert!(!queue.name.is_empty());
        assert!(
            !queue.name.contains(char::is_whitespace),
            "`{}` is a sentence, not a queue name",
            queue.name
        );
        // And every reported queue is one the core would accept.
        let accepted = client.set_printer_settings(
            PrinterSettingsV1 {
                enabled: true,
                model: "RPT004".to_owned(),
                queue: queue.name.clone(),
                columns: 48,
                auto_print: false,
            },
            None,
        );
        assert!(accepted.is_ok(), "`{}` was offered but refused", queue.name);
    }
}

/// Resetting the demo must not cost the operator their printer.
///
/// "Restablecer demo" exists so a stand can be handed to the next visitor
/// clean. It abandons the open order — and that is all it may do: the printer
/// was configured once, physically, by somebody who is not going to do it
/// again between customers.
#[test]
fn resetting_the_demo_keeps_the_printer_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("pi-edge.db");
    let _daemon = DaemonProcess::start(&database);

    let admin = client(&database, "edge-cli");
    let configured = admin
        .set_printer_settings(settings("PX_RECEIPT", true), None)
        .unwrap()
        .settings;

    // A sale in progress, as there would be mid-visit.
    let output = std::process::Command::new(common::cli_binary())
        .arg("--db")
        .arg(&database)
        .arg("--format")
        .arg("json")
        .arg("demo-seed")
        .arg("--path")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo"))
        .env("PI_EDGE_LOG", "warn")
        .output()
        .unwrap();
    assert!(output.status.success());

    let kiosk = client(&database, "tauri-pos");
    kiosk.start_transaction(None).unwrap();
    kiosk
        .add_item(
            AddItemRequestV1 {
                product: ProductSelectorV1::ProductRef("NUGGETS".to_owned()),
                quantity_thousandths: 1_000,
                selections: Vec::new(),
                preparation_note: None,
            },
            None,
        )
        .unwrap();
    assert!(kiosk.get_active_transaction().unwrap().is_some());

    // What the admin screen's reset does.
    kiosk
        .cancel_transaction(Some("DEMO_RESET".to_owned()), None)
        .unwrap();

    assert!(
        kiosk.get_active_transaction().unwrap().is_none(),
        "the order should be gone"
    );
    assert_eq!(
        admin.printer_settings().unwrap().settings,
        configured,
        "the printer configuration must survive a demo reset"
    );
}
