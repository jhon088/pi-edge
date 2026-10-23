//! Hardware against real SQLite.
//!
//! The parser and the renderer are pinned by unit tests in `edge-hardware`;
//! these check what only the whole runtime shows: that a scan becomes exactly
//! one item, that the drawer opens for the commercial causes that justify it
//! and for nothing else, that a replay opens it once, and that **no device
//! failure ever changes a cent, a unit of stock or the state of a sale**.
//!
//! The scenarios are the ones the previous Flutter POS had already validated
//! physically — one scan one product, drawer on sale, drawer on diagnostic, no
//! drawer on reprint, failure not corrupting a transaction — turned into
//! executable requirements here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_cash::MovementType as CashMovementType;
use edge_domain::prelude::*;
use edge_hardware::{
    DrawerOpenReason, HardwareDeviceState, PrintKind, SimulatedHardware, SimulatorDrawer,
    SimulatorPrinter,
};
use edge_integration_tests::*;
use edge_inventory::MovementType as InventoryMovementType;
use edge_payments::TenderType;
use edge_refunds::RefundFiscalStatus;

const SKU: &str = "SKU-001";
const BARCODE: &str = "7501000000001";
const UNIT: i64 = 11_600;

// ------------------------------------------------------------------- scanner

#[test]
fn one_physical_scan_adds_exactly_one_product() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let sale = core.submit_scan(scan_of(BARCODE)).unwrap();

    assert_eq!(sale.state().lines.len(), 1);
    assert_eq!(
        sale.state().lines[0].quantity,
        Quantity::from_units(1).unwrap()
    );
    assert_eq!(sale.state().lines[0].product_ref.as_str(), SKU);
}

#[test]
fn the_same_barcode_scanned_twice_is_two_units() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    core.submit_scan(scan_of(BARCODE)).unwrap();
    let sale = core.submit_scan(scan_of(BARCODE)).unwrap();

    // Two physical scans mean two units. Nothing deduplicates by barcode: a
    // customer buying two of something is the ordinary case (ADR 036).
    let total: i64 = sale
        .state()
        .lines
        .iter()
        .map(|line| line.quantity.thousandths())
        .sum();
    assert_eq!(total, 2_000);
}

#[test]
fn the_same_scan_event_resent_adds_one_unit() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let event_id = uuid::Uuid::new_v4();
    core.submit_scan(scan_with(event_id, BARCODE)).unwrap();
    let sale = core.submit_scan(scan_with(event_id, BARCODE)).unwrap();

    // One physical scan, delivered twice by plumbing, is one unit.
    let total: i64 = sale
        .state()
        .lines
        .iter()
        .map(|line| line.quantity.thousandths())
        .sum();
    assert_eq!(total, 1_000);
}

#[test]
fn a_scan_with_no_sale_open_is_refused_and_opens_nothing() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();

    let error = core.submit_scan(scan_of(BARCODE)).unwrap_err();

    // PI.Edge does not open a sale on its own (ADR 036).
    assert_eq!(error.kind(), "SCAN_NO_ACTIVE_TRANSACTION");
    assert!(core.get_active_transaction().unwrap().is_none());
}

#[test]
fn an_unknown_barcode_is_refused_through_the_ordinary_catalog_path() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    let error = core.submit_scan(scan_of("0000000000000")).unwrap_err();
    // The same code a typed barcode produces: there is no hardware-specific
    // product lookup (ADR 036).
    assert_eq!(error.kind(), "PRODUCT_NOT_FOUND");
}

#[test]
fn a_scan_after_the_basket_froze_is_refused_by_the_sale_not_by_the_scanner() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.submit_scan(scan_of(BARCODE)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, UNIT);

    let error = core.submit_scan(scan_of(BARCODE)).unwrap_err();
    // The basket freeze is a sale rule and it applies to a scan exactly as it
    // applies to a command (ADR 019).
    assert_eq!(error.kind(), "BASKET_LOCKED_BY_PAYMENT");
}

// -------------------------------------------------------------------- drawer

#[test]
fn a_cash_sale_opens_the_drawer_exactly_once() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    assert_eq!(fixture.opens(), 0);

    cash_sale(core, UNIT);

    assert_eq!(fixture.opens(), 1);
    assert_eq!(fixture.drawer.reasons(), vec![DrawerOpenReason::CashSale]);
}

#[test]
fn a_card_sale_does_not_open_the_drawer() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: UNIT,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // No cash moved, so there is nothing to put in the drawer.
    assert_eq!(fixture.opens(), 0);
}

#[test]
fn a_mixed_tender_sale_opens_the_drawer_once_for_the_cash_part() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 3)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: 10_000,
    })
    .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: 3 * UNIT - 10_000,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    assert_eq!(fixture.opens(), 1, "one cash payment, one opening");
}

#[test]
fn a_replayed_cash_payment_opens_the_drawer_once() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let request_id = uuid::Uuid::new_v4();
    for _ in 0..2 {
        core.start_payment(StartPaymentCommand {
            request_id: Some(request_id),
            tender_type: TenderType::Cash,
            amount_minor: UNIT,
        })
        .unwrap();
    }

    // The drawer is keyed by the payment, so a replayed command cannot open it
    // again (ADR 037).
    assert_eq!(fixture.opens(), 1);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .movements
            .iter()
            .filter(|m| m.movement_type == CashMovementType::CashSale)
            .count(),
        1
    );
}

#[test]
fn a_cash_refund_opens_the_drawer_and_a_card_refund_does_not() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    let sale = finalized_cash_sale(core, demo_item(), UNIT);
    assert_eq!(fixture.opens(), 1, "the sale itself");

    full_refund(core, sale, refund_lines(&[(1, 1)]));
    // Cash physically left the drawer, so the drawer opens (ADR 037).
    assert_eq!(fixture.opens(), 2);
    assert_eq!(fixture.drawer.reasons()[1], DrawerOpenReason::CashRefund);

    // A card sale refunded to the card moves no cash and opens nothing.
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: UNIT,
    })
    .unwrap();
    let card_sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id();
    full_refund(core, card_sale, refund_lines(&[(1, 1)]));
    assert_eq!(fixture.opens(), 2, "no cash moved, no drawer");
}

#[test]
fn a_replayed_cash_refund_opens_the_drawer_once() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    let sale = finalized_cash_sale(core, demo_item(), UNIT);
    let refund = create_refund(core, sale, refund_lines(&[(1, 1)])).unwrap();
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();

    let request_id = uuid::Uuid::new_v4();
    for _ in 0..2 {
        core.start_refund_payment(StartRefundPaymentCommand {
            request_id: Some(request_id),
            refund_id: refund.refund.refund_id,
        })
        .unwrap();
    }

    assert_eq!(fixture.opens(), 2, "the sale, then the refund, once each");
}

#[test]
fn a_diagnostic_opens_the_drawer_every_time_it_is_asked() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();

    for _ in 0..3 {
        core.open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
            .unwrap();
    }

    // An operator asking is not a replay: a diagnostic has no commercial cause
    // to be keyed by, and refusing the second one would make the button
    // useless (ADR 037).
    assert_eq!(fixture.opens(), 3);
    assert!(fixture
        .drawer
        .reasons()
        .iter()
        .all(|reason| *reason == DrawerOpenReason::Diagnostic));
}

#[test]
fn a_drawer_that_will_not_open_changes_no_money_and_no_stock() {
    let fixture = HardwareFixture::new(
        SimulatorPrinter::working(),
        SimulatorDrawer::answering(SimulatedHardware::Unavailable),
    );
    let core = fixture.open_till();

    // The sale goes through regardless: the customer paid and the goods left.
    cash_sale(core, UNIT);

    assert_eq!(fixture.opens(), 0, "it never opened");
    let position = core.get_cash_position().unwrap();
    assert_eq!(position.position.cash_sales.minor(), UNIT);
    assert_eq!(
        position.position.expected_cash.minor(),
        100_000 + UNIT,
        "the ledger says what should be in the drawer, open or shut"
    );
    assert_eq!(on_hand(core, SKU), 9_000);
    let sale = core.get_journal(50).unwrap();
    assert!(sale
        .iter()
        .any(|event| event.event_type.as_str() == "TRANSACTION_FINALIZED"));
    // And the failure is on the record.
    let status = core.get_hardware_status().unwrap();
    assert_eq!(status.drawer.state, HardwareDeviceState::Unavailable);
    assert_eq!(
        status.drawer.last_error_code.as_deref(),
        Some("HARDWARE_UNAVAILABLE")
    );
}

#[test]
fn a_drawer_that_will_not_open_does_not_undo_a_refund() {
    let fixture = HardwareFixture::new(
        SimulatorPrinter::working(),
        SimulatorDrawer::answering(SimulatedHardware::Unavailable),
    );
    let core = fixture.open_till();
    let sale = finalized_cash_sale(core, demo_item(), UNIT);

    let view = full_refund(core, sale, refund_lines(&[(1, 1)]));

    assert_eq!(fixture.opens(), 0);
    // The refund is settled, the ledger paid it out, and the goods came back.
    assert_eq!(view.refund.status, edge_refunds::RefundStatus::Completed);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_refunds
            .minor(),
        UNIT
    );
    assert_eq!(on_hand(core, SKU), 10_000);
}

// ------------------------------------------------------------------- printer

#[test]
fn a_test_page_prints_and_touches_no_commercial_data() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    let before = core.get_cash_position().unwrap().position.expected_cash;

    core.print_test_page().unwrap();

    assert_eq!(fixture.jobs(), 1);
    assert_eq!(fixture.printer.jobs()[0].kind, PrintKind::TestPage);
    assert_eq!(fixture.opens(), 0, "printing is not opening");
    assert_eq!(
        core.get_cash_position().unwrap().position.expected_cash,
        before
    );
    assert_eq!(on_hand(core, SKU), 10_000);
}

#[test]
fn closing_a_sale_puts_its_receipt_on_paper_with_nobody_asking() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    let sale = finalized_cash_sale(core, demo_item(), UNIT);

    // Nobody called `print_transaction`: the core attempted the first receipt
    // itself, after the commercial commit (ADR 043).
    assert_eq!(fixture.jobs(), 1);
    let job = &fixture.printer.jobs()[0];
    assert_eq!(job.kind, PrintKind::Receipt);
    let text: String = job
        .lines
        .iter()
        .map(|line| line.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("116.00"), "{text}");
    assert!(text.contains("TOTAL"), "{text}");
    assert!(!text.contains("COPIA"), "a first receipt is not a copy");

    // And the obligation records it, once.
    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Printed);
    assert_eq!(receipt.attempt_count, 1);
    assert!(receipt.first_printed_at.is_some());
    assert!(core.get_receipt_attention().unwrap().is_clear());
}

#[test]
fn a_sale_that_has_not_finalized_cannot_be_printed() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    let open = core.get_active_transaction().unwrap().unwrap();

    let error = core
        .print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: open.id(),
            reprint: false,
        })
        .unwrap_err();

    assert_eq!(error.kind(), "INVALID_TRANSACTION_STATE");
    assert_eq!(fixture.jobs(), 0);
}

#[test]
fn a_reprint_prints_again_and_changes_nothing_else() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    let sale = finalized_cash_sale(core, demo_item(), UNIT);
    let opens_before = fixture.opens();
    let cash_before = core.get_cash_position().unwrap().position.expected_cash;
    let stock_before = on_hand(core, SKU);

    for _ in 0..2 {
        core.print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: sale,
            reprint: true,
        })
        .unwrap();
    }

    // Three jobs: the automatic receipt and two copies. A reprint is always
    // allowed, because paper jams and customers ask.
    assert_eq!(fixture.jobs(), 3);
    assert_eq!(fixture.printer.jobs()[1].kind, PrintKind::Reprint);
    let copy = fixture.printer.jobs()[1]
        .lines
        .iter()
        .any(|line| line.text.contains("COPIA"));
    assert!(copy, "a reprint says it is a copy");

    // And nothing else moved: no drawer, no cash, no stock, no sale change
    // (ADR 037).
    assert_eq!(fixture.opens(), opens_before);
    assert_eq!(
        core.get_cash_position().unwrap().position.expected_cash,
        cash_before
    );
    assert_eq!(on_hand(core, SKU), stock_before);
    assert_eq!(
        core.get_refund_eligibility(sale).unwrap().original_status,
        "FINALIZED"
    );
    // Not even the obligation: a copy printed today says nothing about the
    // first receipt, and the first receipt was attempted once (ADR 043).
    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Printed);
    assert_eq!(receipt.attempt_count, 1);
}

#[test]
fn asking_for_a_first_receipt_that_already_printed_is_refused() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    let sale = finalized_cash_sale(core, demo_item(), UNIT);

    let error = core
        .print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: sale,
            reprint: false,
        })
        .unwrap_err();

    // The obligation knows the paper came out, so asking for a first receipt
    // is refused by the receipt lifecycle — not by the device log, and not
    // with a code about hardware (ADR 043).
    assert_eq!(error.kind(), "RECEIPT_ALREADY_PRINTED");
    assert_eq!(fixture.jobs(), 1, "nothing else was printed");
}

/// The certainty matrix, end to end.
///
/// What a failed receipt attempt leaves behind depends on one question only:
/// did the printer get the job? Nothing was sent → `FAILED`, retryable.
/// Something may have been → `UNKNOWN`, never retried automatically
/// (ADR 043).
#[test]
fn what_a_failed_receipt_leaves_behind_depends_on_what_the_printer_got() {
    for (outcome, expected, retryable) in [
        (SimulatedHardware::Unavailable, ReceiptStatus::Failed, true),
        (SimulatedHardware::Timeout, ReceiptStatus::Unknown, false),
        (SimulatedHardware::IoError, ReceiptStatus::Unknown, false),
    ] {
        let fixture = HardwareFixture::new(
            SimulatorPrinter::answering(outcome),
            SimulatorDrawer::working(),
        );
        let core = fixture.open_till();
        let sale = finalized_cash_sale(core, demo_item(), UNIT);

        // The sale is finalized, paid, and its stock moved. Paper changes none
        // of that (ADR 005).
        let eligibility = core.get_refund_eligibility(sale).unwrap();
        assert_eq!(eligibility.original_status, "FINALIZED");
        assert_eq!(eligibility.original_total.minor(), UNIT);
        assert_eq!(on_hand(core, SKU), 9_000);
        assert_eq!(
            core.get_cash_position()
                .unwrap()
                .position
                .cash_sales
                .minor(),
            UNIT
        );
        assert_eq!(fixture.jobs(), 0, "no paper, no job");

        let receipt = core.get_receipt(sale).unwrap();
        assert_eq!(receipt.status, expected, "for {outcome:?}");
        assert_eq!(receipt.attempt_count, 1);
        assert!(receipt.first_printed_at.is_none());
        assert!(receipt.last_error_code.is_some());
        assert_eq!(
            core.get_receipt_attention().unwrap().total(),
            1,
            "it is on the attention list"
        );

        // And whether an operator may ask for the first receipt again.
        let retry = core.print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: sale,
            reprint: false,
        });
        if retryable {
            // Still refused by the device, but the receipt lifecycle let the
            // attempt through and counted it.
            assert_eq!(
                retry.unwrap_err().kind(),
                "HARDWARE_UNAVAILABLE",
                "a failure we know about may be retried"
            );
            assert_eq!(core.get_receipt(sale).unwrap().attempt_count, 2);
        } else {
            assert_eq!(
                retry.unwrap_err().kind(),
                "RECEIPT_OUTCOME_UNKNOWN",
                "paper may have come out; a person decides"
            );
            assert_eq!(
                core.get_receipt(sale).unwrap().attempt_count,
                1,
                "the refusal is not an attempt"
            );
        }

        // A reprint is always available, and it does not rewrite history.
        core.print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: sale,
            reprint: true,
        })
        .ok();
        assert_eq!(core.get_receipt(sale).unwrap().status, expected);
    }
}

#[test]
fn a_printer_timeout_leaves_the_device_degraded_not_unavailable() {
    let fixture = HardwareFixture::new(
        SimulatorPrinter::answering(SimulatedHardware::Timeout),
        SimulatorDrawer::working(),
    );
    let core = fixture.open_till();
    assert!(core.print_test_page().is_err());

    // "We asked and we do not know": the paper may have come out, so the
    // device is degraded rather than declared down (ADR 037).
    let status = core.get_hardware_status().unwrap();
    assert_eq!(status.printer.state, HardwareDeviceState::Degraded);
    assert_eq!(
        status.printer.last_error_code.as_deref(),
        Some("HARDWARE_TIMEOUT")
    );
}

// -------------------------------------------------------------------- status

#[test]
fn every_device_is_unknown_until_something_proves_otherwise() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();

    let status = core.get_hardware_status().unwrap();
    // The printer and the drawer answered a probe, so they are known.
    assert_eq!(status.printer.state, HardwareDeviceState::Available);
    assert_eq!(status.drawer.state, HardwareDeviceState::Available);
    // A scanner cannot be probed: it is input, and nothing has arrived yet.
    assert_eq!(status.scanner.state, HardwareDeviceState::Unknown);
    assert!(status.scanner.last_success_at.is_none());
}

#[test]
fn a_scan_is_the_only_thing_that_makes_a_scanner_known_to_have_worked() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.submit_scan(scan_of(BARCODE)).unwrap();

    let status = core.get_hardware_status().unwrap();
    assert!(
        status.scanner.last_success_at.is_some(),
        "a scan that became an item is the scanner working"
    );
    // And within this process that is enough to call it available: there is
    // nothing else to probe (ADR 035).
    assert_eq!(status.scanner.state, HardwareDeviceState::Available);
}

#[test]
fn status_reports_a_down_printer_and_a_working_drawer_separately() {
    let fixture = HardwareFixture::new(
        SimulatorPrinter::answering(SimulatedHardware::Unavailable),
        SimulatorDrawer::working(),
    );
    let core = fixture.open_till();

    let status = core.get_hardware_status().unwrap();
    assert_eq!(status.printer.state, HardwareDeviceState::Unavailable);
    assert_eq!(status.drawer.state, HardwareDeviceState::Available);
    assert!(status.printer.last_checked_at.is_some());
    assert!(status.printer.last_success_at.is_none());
    // No host, no port, no serial: a diagnostic view is not a way to read the
    // configuration (ADR 035).
    let rendered = serde_json::to_string(&status).unwrap();
    assert!(!rendered.contains("9100"), "{rendered}");
}

#[test]
fn last_success_comes_from_the_action_log_and_survives_a_probe_failure() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    core.open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
        .unwrap();

    let status = core.get_hardware_status().unwrap();
    assert!(
        status.drawer.last_success_at.is_some(),
        "the drawer opened once, and the log remembers"
    );
}

// --------------------------------------------------------- the fiscal refund

#[test]
fn a_refund_of_an_invoiced_sale_still_opens_the_drawer_for_its_cash() {
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();
    let sale = finalized_cash_sale(core, demo_item(), UNIT);
    mark_invoiced(core, sale);

    let refund = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 1)]),
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action: RefundFiscalStatus::CreditNotePending,
        })
        .unwrap();
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();
    core.start_refund_payment(StartRefundPaymentCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();

    // The fiscal consequence is a separate matter from the physical cash
    // (ADR 032, ADR 037).
    assert_eq!(fixture.opens(), 2);
    assert_eq!(on_hand(core, SKU), 10_000);
    assert_eq!(
        position(core, SKU)
            .movements
            .iter()
            .filter(|m| m.movement_type == InventoryMovementType::RefundReturn)
            .count(),
        1
    );
}

// -------------------------------------------------- nothing configured at all

/// The rule a misconfigured till depends on: **a simulator is never selected
/// implicitly**, so nothing reports healthy hardware nobody asked for
/// (ADR 038).
#[test]
fn an_unconfigured_runtime_reports_not_configured_and_never_a_simulator() {
    let core = HardwareFixture::unconfigured();
    open_register_with_float(&core, 100_000);
    bootstrap_stock(&core, &[(SKU, 10)]);

    let status = core.get_hardware_status().unwrap();
    for device in [&status.printer, &status.drawer] {
        assert_eq!(device.driver.as_deref(), Some("unconfigured"));
        assert_ne!(device.driver.as_deref(), Some("simulator"));
        assert_eq!(device.state, HardwareDeviceState::Unavailable);
        assert_eq!(
            device.last_error_code.as_deref(),
            Some("HARDWARE_NOT_CONFIGURED")
        );
        // The one combination the blocker forbids.
        assert!(
            !(device.state == HardwareDeviceState::Available
                && device.driver.as_deref() == Some("simulator")),
            "a device nobody configured must never be AVAILABLE on a simulator"
        );
    }

    assert_eq!(
        core.print_test_page().unwrap_err().kind(),
        "HARDWARE_NOT_CONFIGURED"
    );
    assert_eq!(
        core.open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
            .unwrap_err()
            .kind(),
        "HARDWARE_NOT_CONFIGURED"
    );
}

#[test]
fn a_cash_sale_with_no_drawer_configured_is_still_a_cash_sale() {
    let core = HardwareFixture::unconfigured();
    open_register_with_float(&core, 100_000);
    bootstrap_stock(&core, &[(SKU, 10)]);

    cash_sale(&core, UNIT);

    // The payment committed, the ledger moved, the stock moved, and the sale
    // finalized. A drawer nobody configured changes none of it (ADR 037).
    let sale = core
        .get_journal(50)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type.as_str() == "TRANSACTION_FINALIZED")
        .expect("the sale finalized");
    assert_eq!(sale.payload_json["inventory_movements"], 1);
    let position = core.get_cash_position().unwrap();
    assert_eq!(position.position.cash_sales.minor(), UNIT);
    assert_eq!(position.position.expected_cash.minor(), 100_000 + UNIT);
    assert_eq!(
        position
            .movements
            .iter()
            .filter(|m| m.movement_type == CashMovementType::CashSale)
            .count(),
        1,
        "the CashMovement is durable regardless"
    );
    assert_eq!(on_hand(&core, SKU), 9_000);

    // And the hardware failure is on the record, not swallowed silently.
    assert_eq!(
        core.get_hardware_status()
            .unwrap()
            .drawer
            .last_error_code
            .as_deref(),
        Some("HARDWARE_NOT_CONFIGURED")
    );
}

#[test]
fn a_cash_refund_with_no_drawer_configured_is_still_completed() {
    let core = HardwareFixture::unconfigured();
    open_register_with_float(&core, 100_000);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    let view = full_refund(&core, sale, refund_lines(&[(1, 1)]));

    assert_eq!(view.refund.status, edge_refunds::RefundStatus::Completed);
    assert_eq!(view.remaining_to_refund.minor(), 0);
    let position = core.get_cash_position().unwrap();
    assert_eq!(position.position.cash_refunds.minor(), UNIT);
    assert_eq!(
        position
            .movements
            .iter()
            .filter(|m| m.movement_type == CashMovementType::CashRefund)
            .count(),
        1,
        "the CashRefund movement is durable regardless"
    );
    assert_eq!(on_hand(&core, SKU), 10_000, "the goods came back");
}

#[test]
fn printing_with_no_printer_configured_refuses_without_touching_the_sale() {
    let core = HardwareFixture::unconfigured();
    open_register_with_float(&core, 100_000);
    bootstrap_stock(&core, &[(SKU, 10)]);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);

    let error = core
        .print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: sale,
            reprint: false,
        })
        .unwrap_err();

    // A refusal, not a false success: nothing came out of a machine that is
    // not there.
    assert_eq!(error.kind(), "HARDWARE_NOT_CONFIGURED");
    let eligibility = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(eligibility.original_status, "FINALIZED");
    assert_eq!(eligibility.original_total.minor(), UNIT);
    assert_eq!(on_hand(&core, SKU), 9_000);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
        UNIT
    );
}

#[test]
fn a_simulator_asked_for_explicitly_still_works() {
    // The other half of the rule: opting in is what makes a simulator
    // available, and tests say so out loud.
    let fixture = HardwareFixture::working();
    let core = fixture.open_till();

    let status = core.get_hardware_status().unwrap();
    assert_eq!(status.printer.driver.as_deref(), Some("simulator"));
    assert_eq!(status.printer.state, HardwareDeviceState::Available);
    assert_eq!(status.drawer.driver.as_deref(), Some("simulator"));
    assert_eq!(status.drawer.state, HardwareDeviceState::Available);
    core.print_test_page().unwrap();
    core.open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
        .unwrap();
    assert_eq!(fixture.jobs(), 1);
    assert_eq!(fixture.opens(), 1);
}
