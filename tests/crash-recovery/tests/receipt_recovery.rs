//! The receipt windows a process death opens.
//!
//! Five of them, and the whole point is that two superficially similar states
//! get opposite treatment (ADR 043):
//!
//! | After the crash | What recovery does |
//! | --- | --- |
//! | obligation `PENDING`, no device evidence | attempt it: nothing was sent |
//! | obligation `PENDING`, a claim with no answer | `UNKNOWN`, print nothing |
//!
//! Confusing those two is how a till prints a second ticket for a customer who
//! already has one, or silently loses a first one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;
use edge_hardware::{PrintKind, SimulatedHardware, SimulatorPrinter};

const SKU: &str = "SKU-001";
const UNIT: i64 = 11_600;

/// A till with stock, ready to sell.
fn open_till(core: &TestCore) {
    open_register_with_float(core, 100_000);
    bootstrap_stock(core, &[(SKU, 10)]);
}

/// Everything the sale moved, so a test can say "and none of this changed".
fn commercial_state(core: &TestCore, sale: TransactionId) -> (String, i64, i64) {
    let eligibility = core.get_refund_eligibility(sale).unwrap();
    (
        eligibility.original_status,
        on_hand(core, SKU),
        core.get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
    )
}

/// Window 3: the sale closed and the process died before the printer was
/// touched at all.
///
/// A cycle commits the sale, then reads the obligation, then claims the
/// printer. Killing the claim's commit is exactly this window: the sale is
/// closed and owes a receipt, and no device evidence exists.
#[test]
fn a_sale_that_closed_before_the_printer_was_touched_still_owes_its_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let sale = {
        let (core, printer) = printing_core_at(&db, 1, SimulatorPrinter::working());
        open_till(&core);
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(demo_item()).unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        pay_cash(&core, UNIT);
        // Commit 1 closes the sale, commit 2 reads the obligation, commit 3 is
        // the printer claim. The third one dies.
        core.store().arm_commit_failure_after(2);
        let sale = core
            .finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap()
            .id();

        // The sale is closed and the receipt is owed, with nothing attempted.
        assert_eq!(printer.job_count(), 0, "the printer was never given a job");
        let receipt = core.get_receipt(sale).unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Pending);
        assert_eq!(receipt.attempt_count, 0);
        sale
    };

    // A new runtime. Nothing was ever sent, so attempting it now is safe —
    // and that is what recovery does.
    let (core, printer) = printing_core_at(&db, 10_000, SimulatorPrinter::working());
    let report = core.recover_receipts().unwrap();
    assert_eq!(report.resumed, 1);
    assert_eq!(report.marked_unknown, 0);
    assert_eq!(report.resolved_from_evidence, 0);
    assert_eq!(printer.job_count(), 1);
    assert_eq!(printer.jobs()[0].kind, PrintKind::Receipt);
    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Printed);
    assert!(core.get_receipt_attention().unwrap().is_clear());
}

/// Window 4: the printer had the job and the answer was never written.
///
/// Commit 4 of the cycle is the one that records what the device said. Killing
/// it leaves a claim with no resolution — the printer was handed the job and
/// nobody knows what it did.
#[test]
fn a_receipt_whose_printer_call_was_interrupted_becomes_unknown_and_is_not_printed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let (sale, before) = {
        let (core, printer) = printing_core_at(&db, 1, SimulatorPrinter::working());
        open_till(&core);
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(demo_item()).unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        pay_cash(&core, UNIT);
        // 1 the sale, 2 the obligation read, 3 the claim, 4 the device answer.
        core.store().arm_commit_failure_after(3);
        let sale = core
            .finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap()
            .id();

        // The printer really did get the job, and nothing durable says so.
        assert_eq!(printer.job_count(), 1);
        let receipt = core.get_receipt(sale).unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Pending);
        assert_eq!(receipt.attempt_count, 0);
        (sale, commercial_state(&core, sale))
    };

    let (core, printer) = printing_core_at(&db, 10_000, SimulatorPrinter::working());
    let report = core.recover_receipts().unwrap();
    assert_eq!(report.marked_unknown, 1);
    assert_eq!(report.resumed, 0);
    assert_eq!(
        printer.job_count(),
        0,
        "nothing is printed on a guess: paper may already have come out"
    );

    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unknown);
    assert!(receipt.first_printed_at.is_none());
    assert_eq!(
        receipt.last_error_code.as_deref(),
        Some("RECEIPT_INTERRUPTED")
    );
    assert_eq!(core.get_receipt_attention().unwrap().unknown, 1);

    // The sale is exactly as it was. A receipt never touches money or stock.
    assert_eq!(commercial_state(&core, sale), before);

    // And a second recovery pass changes nothing, because `UNKNOWN` is
    // settled history.
    let again = core.recover_receipts().unwrap();
    assert_eq!(again.marked_unknown, 0);
    assert_eq!(again.resumed, 0);
    assert_eq!(printer.job_count(), 0);
}

/// Window 5: the printer certainly did not print, so an operator may retry.
#[test]
fn a_receipt_that_certainly_did_not_print_can_be_retried_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let sale = {
        let (core, _) = printing_core_at(
            &db,
            1,
            SimulatorPrinter::answering(SimulatedHardware::Unavailable),
        );
        open_till(&core);
        let sale = finalized_cash_sale(&core, UNIT);
        let receipt = core.get_receipt(sale).unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Failed);
        assert_eq!(receipt.attempt_count, 1);
        sale
    };

    // A restart does not retry it on its own: the obligation is resolved, and
    // nothing here loops.
    let (core, printer) = printing_core_at(&db, 10_000, SimulatorPrinter::working());
    let report = core.recover_receipts().unwrap();
    assert_eq!(report.resumed, 0, "only PENDING obligations are resumed");
    assert_eq!(printer.job_count(), 0);
    assert_eq!(
        core.get_receipt(sale).unwrap().status,
        ReceiptStatus::Failed
    );

    // The operator asks, and the sale is still FINALIZED throughout.
    core.print_transaction(PrintTransactionCommand {
        request_id: None,
        transaction_id: sale,
        reprint: false,
    })
    .unwrap();
    assert_eq!(printer.job_count(), 1);
    assert_eq!(printer.jobs()[0].kind, PrintKind::Receipt);
    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Printed);
    assert_eq!(receipt.attempt_count, 2);
}

/// Window 6: a receipt that came out does not come out again.
#[test]
fn a_printed_receipt_is_not_printed_again_by_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let (sale, printed_at) = {
        let (core, printer) = printing_core_at(&db, 1, SimulatorPrinter::working());
        open_till(&core);
        let sale = finalized_cash_sale(&core, UNIT);
        assert_eq!(printer.job_count(), 1);
        let receipt = core.get_receipt(sale).unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Printed);
        (sale, receipt.first_printed_at)
    };

    // The simulator's counter resets with the process, so only durable state
    // can stop a second ticket.
    let (core, printer) = printing_core_at(&db, 10_000, SimulatorPrinter::working());
    core.recover_receipts().unwrap();
    assert_eq!(printer.job_count(), 0);
    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Printed);
    assert_eq!(receipt.attempt_count, 1);
    assert_eq!(
        receipt.first_printed_at, printed_at,
        "the time it printed is history, not something a restart restamps"
    );

    // And asking for the first receipt is refused.
    assert_eq!(
        core.print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: sale,
            reprint: false,
        })
        .unwrap_err()
        .kind(),
        "RECEIPT_ALREADY_PRINTED"
    );
    assert_eq!(printer.job_count(), 0);
}

/// Window 7: a reprint over an unknown first receipt does not invent
/// knowledge.
#[test]
fn a_reprint_over_an_unknown_receipt_prints_a_copy_and_rewrites_no_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let sale = {
        let (core, _) = printing_core_at(
            &db,
            1,
            SimulatorPrinter::answering(SimulatedHardware::IoError),
        );
        open_till(&core);
        let sale = finalized_cash_sale(&core, UNIT);
        // The transport broke mid-write: paper may have come out.
        assert_eq!(
            core.get_receipt(sale).unwrap().status,
            ReceiptStatus::Unknown
        );
        sale
    };

    let (core, printer) = printing_core_at(&db, 10_000, SimulatorPrinter::working());
    let before = commercial_state(&core, sale);
    let opens_before = core.get_hardware_status().unwrap();

    core.print_transaction(PrintTransactionCommand {
        request_id: None,
        transaction_id: sale,
        reprint: true,
    })
    .unwrap();

    // A copy came out, marked as one.
    assert_eq!(printer.job_count(), 1);
    assert_eq!(printer.jobs()[0].kind, PrintKind::Reprint);
    assert!(printer.jobs()[0]
        .lines
        .iter()
        .any(|line| line.text.contains("COPIA")));

    // The first receipt's fate is still unknown, and still counted as needing
    // attention. Printing a copy today says nothing about yesterday.
    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unknown);
    assert!(receipt.first_printed_at.is_none());
    assert_eq!(receipt.attempt_count, 1);
    assert_eq!(core.get_receipt_attention().unwrap().unknown, 1);

    // No drawer, no cash, no stock, no sale change.
    assert_eq!(commercial_state(&core, sale), before);
    assert_eq!(
        core.get_hardware_status().unwrap().drawer.last_success_at,
        opens_before.drawer.last_success_at
    );
}
