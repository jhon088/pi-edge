//! PI.Edge as a complete POS, with no PI.COMMERCE Server anywhere.
//!
//! Every test here runs on a core whose sync transport is **unconfigured** —
//! the production default — so nothing it proves depends on a server existing.
//! Together they walk the till's whole day: open, sell, price, promote, take
//! money, close the sale, print, reprint, refund, count the drawer, close the
//! session, close the day, restart, and carry on (ADR 043).
//!
//! Split into a handful of cohesive walks rather than one unmaintainable
//! mega-test: each one fails for a single, readable reason.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::queries::{BusinessDayCloseResult, SessionCloseResult};
use edge_domain::prelude::*;
use edge_hardware::{PrintKind, SimulatedHardware, SimulatorDrawer, SimulatorPrinter};
use edge_integration_tests::*;
use edge_payments::TenderType;
use edge_sync::unconfigured::UNCONFIGURED;

const SKU: &str = "SKU-001";
const SECOND: &str = "SKU-002";
const UNIT: i64 = 11_600;

/// Nobody configured a server, and nothing here needs one.
fn assert_standalone(core: &TestCore) {
    assert_eq!(core.sync().driver(), UNCONFIGURED);
    let status = core.get_sync_status().unwrap();
    assert_eq!(status.driver, UNCONFIGURED);
    assert_eq!(status.inbound_cursor, None);
}

/// A catalog with a second product and a buy-2-get-1, imported locally from a
/// fixture — the only way a standalone till gets a catalog today.
fn local_catalog() -> edge_application::testing::CatalogFixture {
    promotional_catalog(1).with(
        edge_application::testing::FixtureProduct::new(SECOND, "Segundo producto", 5_000)
            .sku(SECOND)
            .barcode("7501000000002"),
    )
}

/// Open the day and a session on a till with a local catalog and stock.
fn open_standalone_till(fixture: &HardwareFixture) -> &TestCore {
    let core = &fixture.core;
    init_device(core);
    local_catalog().activate(core).unwrap();
    open_day_and_session(core);
    bootstrap_stock(core, &[(SKU, 10), (SECOND, 10)]);
    assert_standalone(core);
    core
}

// ------------------------------------------------------------- the whole day

/// Open to close, with every tender shape, on one till, offline.
#[test]
fn a_whole_day_runs_from_open_to_close_with_no_server_anywhere() {
    let fixture = HardwareFixture::working();
    let core = open_standalone_till(&fixture);

    // --- a cash sale with change, priced and promoted by the core ----------
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(scan("7501000000001", 3)).unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    // Buy 2 get 1: three units billed as two, by the core's own evaluator.
    assert_eq!(totalized.state().totals.grand_total.minor(), 23_200);
    assert!(!totalized.state().applied_promotions.is_empty());
    let cash_sale = pay_cash(core, 30_000);
    assert_eq!(cash_sale.change_due().unwrap().minor(), 6_800);
    let cash_sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id();

    // --- a card sale ------------------------------------------------------
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SECOND, 1)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: 5_800,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // --- a mixed-tender sale ----------------------------------------------
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 1)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: 1_600,
    })
    .unwrap();
    // A card cannot overpay: it is charged the balance, never more (ADR 017).
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: 10_000,
    })
    .unwrap();
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // --- what the day did --------------------------------------------------
    // Stock moves the physical quantity, not the billed one: the promotional
    // sale took three units while charging for two (ADR 027), and the mixed
    // sale took one more.
    assert_eq!(on_hand(core, SKU), 10_000 - 3_000 - 1_000);
    assert_eq!(on_hand(core, SECOND), 9_000);
    let position = core.get_cash_position().unwrap().position;
    // Cash took the sale's cash share and nothing else: 23 200 − 6 800 change,
    // plus the 1 600 of the mixed sale.
    assert_eq!(position.cash_sales.minor(), 23_200 + 1_600);
    assert_eq!(position.expected_cash.minor(), 100_000 + 23_200 + 1_600);

    // Three receipts came out, with nobody asking for them.
    assert_eq!(fixture.jobs(), 3);
    assert!(core.get_receipt_attention().unwrap().is_clear());
    // The drawer opened for the two sales with cash in them, and not for the
    // card-only one (ADR 037).
    assert_eq!(fixture.opens(), 2);

    // --- count the drawer and close ---------------------------------------
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    let closed = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: 124_800,
        })
        .unwrap();
    assert_eq!(closed.variance.map(|v| v.minor()), Some(0));
    assert_eq!(closed.variance_label, "BALANCED");
    assert!(closed.receipt_attention.is_clear());
    core.close_business_day(CloseBusinessDayCommand::default())
        .unwrap();

    // Three sales, the session close and the day close are all on record,
    // waiting for a server that does not exist (ADR 040).
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 5);
    assert_eq!(
        core.run_sync_cycle().unwrap().outcome,
        edge_application::queries::SyncCycleOutcome::NotConfigured
    );

    // And the operational history knows the sales, newest first.
    let history = core.list_recent_sales(10, 0).unwrap();
    assert_eq!(history.sales.len(), 3);
    assert_eq!(history.sales[2].transaction_id, cash_sale);
    assert_eq!(history.sales[2].grand_total.minor(), 23_200);
    assert_eq!(
        history.sales[2].receipt_status,
        Some(ReceiptStatus::Printed)
    );
    assert!(history.attention.is_clear());
}

// --------------------------------------------------- the receipt is a consequence

/// A receipt is something a closed sale *owes*, never something it waits for.
#[test]
fn a_sale_closes_and_a_day_ends_whatever_the_printer_did() {
    // A printer that broke mid-write: paper may or may not have come out.
    let fixture = HardwareFixture::new(
        SimulatorPrinter::answering(SimulatedHardware::IoError),
        SimulatorDrawer::working(),
    );
    let core = open_standalone_till(&fixture);
    let sale = finalized_cash_sale(core, demo_item(), UNIT);

    // The sale is closed and paid, and the stock and cash moved.
    assert_eq!(
        core.get_refund_eligibility(sale).unwrap().original_status,
        "FINALIZED"
    );
    assert_eq!(on_hand(core, SKU), 9_000);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
        UNIT
    );

    // The receipt is unknown, visible, and not retried on a guess.
    let receipt = core.get_receipt(sale).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unknown);
    assert_eq!(core.get_receipt_attention().unwrap().unknown, 1);

    // The day closes anyway, and says what is pending.
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    let closed = core
        .declare_closing_cash(DeclareClosingCashCommand {
            request_id: None,
            counted_minor: 100_000 + UNIT,
        })
        .unwrap();
    assert_eq!(closed.variance.map(|v| v.minor()), Some(0));
    assert_eq!(
        closed.receipt_attention.unknown, 1,
        "the close reports it and does not refuse"
    );
    core.close_business_day(CloseBusinessDayCommand::default())
        .unwrap();
    assert!(core.get_edge_state().unwrap().business_day.is_none());
}

// ------------------------------------------------------------------ refunds

/// A partial refund, standalone, with the authority the runtime resolved.
#[test]
fn a_partial_refund_gives_back_goods_and_money_and_nothing_else() {
    let fixture = HardwareFixture::working();
    let core = open_standalone_till(&fixture);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 2)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, 23_200);
    let sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id();
    assert_eq!(on_hand(core, SKU), 8_000);
    let opens_after_sale = fixture.opens();

    // Give one unit back.
    let refund = full_refund(core, sale, refund_lines(&[(1, 1)]));
    assert_eq!(refund.refund.status, edge_refunds::RefundStatus::Completed);
    assert_eq!(refund.refund.total.gross.minor(), UNIT);

    // The goods came back once, the money left the drawer once, and the
    // drawer opened once for the cash refund (ADR 033, ADR 037).
    assert_eq!(on_hand(core, SKU), 9_000);
    let position = core.get_cash_position().unwrap().position;
    assert_eq!(position.cash_refunds.minor(), UNIT);
    assert_eq!(position.expected_cash.minor(), 100_000 + 23_200 - UNIT);
    assert_eq!(fixture.opens(), opens_after_sale + 1);

    // What is still refundable shrank by exactly what went back.
    let eligibility = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(eligibility.lines[0].remaining_thousandths, 1_000);

    // A refund prints nothing on its own: there is no refund receipt
    // lifecycle in this milestone, and the sale's own receipt is untouched.
    assert_eq!(fixture.jobs(), 1);
    assert_eq!(
        core.get_receipt(sale).unwrap().status,
        ReceiptStatus::Printed
    );
}

// ----------------------------------------------------------------- restarts

/// The till comes back and keeps trading.
#[test]
fn the_register_comes_back_after_a_restart_and_keeps_trading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");

    let (sale, pending) = {
        let core = offline_core_at(&path);
        init_device(&core);
        local_catalog().activate(&core).unwrap();
        open_day_and_session(&core);
        bootstrap_stock(&core, &[(SKU, 10)]);
        let sale = finalized_cash_sale(&core, demo_item(), UNIT);
        (sale, core.get_edge_state().unwrap().pending_outbox)
    };

    // A brand new application object over the same database.
    let core = core_two_days_later(&path);
    assert_standalone(&core);
    let state = core.get_edge_state().unwrap();
    assert!(state.device.is_some());
    assert!(state.business_day.is_some(), "the day is still open");
    assert!(state.session.is_some());
    assert!(state.active_transaction.is_none());
    assert_eq!(state.pending_outbox, pending);
    assert_eq!(on_hand(&core, SKU), 9_000);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
        UNIT
    );
    // The receipt the first lifetime printed is history, not a job to redo.
    assert_eq!(
        core.get_receipt(sale).unwrap().status,
        ReceiptStatus::Printed
    );

    // And the register sells again, continuing its own sequence.
    let next = finalized_cash_sale(&core, demo_item(), UNIT);
    assert_ne!(next, sale);
    assert_eq!(on_hand(&core, SKU), 8_000);
    let history = core.list_recent_sales(10, 0).unwrap();
    assert_eq!(history.sales.len(), 2);
    assert_eq!(history.sales[0].transaction_id, next, "newest first");
    assert_eq!(history.sales[0].register_sequence, 2);
}

// ----------------------------------------------------------- abandoning a sale

/// A sale with no money in it can be abandoned, and one with money cannot.
#[test]
fn a_sale_can_be_abandoned_before_any_money_moves_and_not_after() {
    let fixture = HardwareFixture::working();
    let core = open_standalone_till(&fixture);

    // From OPEN.
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    let cancelled = core
        .cancel_transaction(CancelTransactionCommand {
            request_id: None,
            reason_code: Some("CUSTOMER_LEFT".to_owned()),
        })
        .unwrap();
    assert_eq!(cancelled.status(), TransactionStatus::Cancelled);
    assert!(core.get_edge_state().unwrap().active_transaction.is_none());

    // From TOTALIZED, and with nothing moved either time.
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.cancel_transaction(CancelTransactionCommand::default())
        .unwrap();
    assert_eq!(on_hand(core, SKU), 10_000, "no stock moved");
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
        0
    );
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 0);
    assert_eq!(fixture.jobs(), 0, "an abandoned sale owes no receipt");

    // Once a payment is applied the way out is a reversal, never a
    // cancellation (ADR 019).
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, UNIT);
    let error = core
        .cancel_transaction(CancelTransactionCommand::default())
        .unwrap_err();
    assert_eq!(error.kind(), "INVALID_TRANSACTION_STATE");

    // And that sale still closes normally.
    let sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(sale.status(), TransactionStatus::Finalized);
    assert_eq!(fixture.printer.jobs()[0].kind, PrintKind::Receipt);
}

// ------------------------------------------------- what the day close reports

/// Close the till and then the day, answering what the day close said.
fn close_day(core: &TestCore, counted_minor: i64) -> BusinessDayCloseResult {
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    core.declare_closing_cash(DeclareClosingCashCommand {
        request_id: None,
        counted_minor,
    })
    .unwrap();
    core.close_business_day(CloseBusinessDayCommand::default())
        .unwrap()
}

/// A day whose receipts all came out reports nothing to do.
#[test]
fn a_day_with_every_receipt_printed_closes_with_nothing_to_report() {
    let fixture = HardwareFixture::working();
    let core = open_standalone_till(&fixture);
    let sale = finalized_cash_sale(core, demo_item(), UNIT);
    assert_eq!(
        core.get_receipt(sale).unwrap().status,
        ReceiptStatus::Printed
    );

    let closed = close_day(core, 100_000 + UNIT);
    assert_eq!(closed.day.status(), BusinessDayStatus::Closed);
    // A printed receipt is not an attention item (ADR 043).
    assert_eq!(closed.receipt_attention, ReceiptAttention::default());
    assert!(closed.receipt_attention.is_clear());
}

/// A receipt that certainly did not print is reported, and closes the day
/// anyway.
#[test]
fn a_day_with_a_failed_receipt_still_closes_and_says_so() {
    let fixture = HardwareFixture::new(
        SimulatorPrinter::answering(SimulatedHardware::Unavailable),
        SimulatorDrawer::working(),
    );
    let core = open_standalone_till(&fixture);
    let sale = finalized_cash_sale(core, demo_item(), UNIT);
    assert_eq!(
        core.get_receipt(sale).unwrap().status,
        ReceiptStatus::Failed
    );

    let closed = close_day(core, 100_000 + UNIT);
    assert_eq!(closed.day.status(), BusinessDayStatus::Closed);
    assert_eq!(closed.receipt_attention.failed, 1);
    assert_eq!(closed.receipt_attention.pending, 0);
    assert_eq!(closed.receipt_attention.unknown, 0);
}

/// A receipt that may have printed is reported, and closes the day anyway.
#[test]
fn a_day_with_an_unknown_receipt_still_closes_and_says_so() {
    let fixture = HardwareFixture::new(
        SimulatorPrinter::answering(SimulatedHardware::IoError),
        SimulatorDrawer::working(),
    );
    let core = open_standalone_till(&fixture);
    let sale = finalized_cash_sale(core, demo_item(), UNIT);
    assert_eq!(
        core.get_receipt(sale).unwrap().status,
        ReceiptStatus::Unknown
    );

    let closed = close_day(core, 100_000 + UNIT);
    assert_eq!(closed.day.status(), BusinessDayStatus::Closed);
    assert_eq!(closed.receipt_attention.unknown, 1);
    assert_eq!(closed.receipt_attention.failed, 0);
    assert_eq!(closed.receipt_attention.pending, 0);
}

/// Yesterday's unresolved receipt is not today's problem.
///
/// The scope is the sale's own `business_day_id`, never a window of time, so
/// a day is never judged by another day's leftovers (ADR 043).
#[test]
fn a_day_close_does_not_count_another_days_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");

    // Day one, with a printer that never got the job.
    let first = {
        let core = core_at_with_printer(
            &path,
            0,
            1,
            SimulatorPrinter::answering(SimulatedHardware::Unavailable),
        );
        init_device(&core);
        local_catalog().activate(&core).unwrap();
        open_day_and_session(&core);
        bootstrap_stock(&core, &[(SKU, 10)]);
        finalized_cash_sale(&core, demo_item(), UNIT);
        let closed = close_day(&core, 100_000 + UNIT);
        assert_eq!(closed.receipt_attention.failed, 1);
        closed.day.id()
    };

    // Day two, a new runtime with a printer that works.
    let core = core_at_with_printer(&path, 2, 10_000, SimulatorPrinter::working());
    open_day_and_session(&core);
    let sale = finalized_cash_sale(&core, demo_item(), UNIT);
    assert_eq!(
        core.get_receipt(sale).unwrap().status,
        ReceiptStatus::Printed
    );

    let closed = close_day(&core, 100_000 + UNIT);
    assert_ne!(closed.day.id(), first, "a different day");
    assert!(
        closed.receipt_attention.is_clear(),
        "yesterday's failed receipt belongs to yesterday: {:?}",
        closed.receipt_attention
    );
    // It has not gone away, though — the installation-wide view still has it.
    assert_eq!(core.get_receipt_attention().unwrap().failed, 1);
}

/// Attention is information, never a blocker.
#[test]
fn receipts_never_appear_among_the_reasons_a_day_cannot_close() {
    let fixture = HardwareFixture::new(
        SimulatorPrinter::answering(SimulatedHardware::IoError),
        SimulatorDrawer::working(),
    );
    let core = open_standalone_till(&fixture);
    finalized_cash_sale(core, demo_item(), UNIT);
    // A second sale, left open: *that* is what blocks a close.
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();

    let error = core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap_err();
    assert_eq!(error.kind(), "BUSINESS_DAY_CLOSE_BLOCKED");
    let reasons = error.to_string();
    assert!(reasons.contains("ACTIVE_TRANSACTION"), "{reasons}");
    for forbidden in ["RECEIPT", "receipt", "PENDING_RECEIPT", "UNKNOWN"] {
        assert!(
            !reasons.contains(forbidden),
            "a receipt is not a reason a day cannot close: {reasons}"
        );
    }

    // Abandon the open sale and the day closes, with the unknown receipt
    // reported rather than refused.
    core.cancel_transaction(CancelTransactionCommand::default())
        .unwrap();
    let closed = close_day(core, 100_000 + UNIT);
    assert_eq!(closed.day.status(), BusinessDayStatus::Closed);
    assert_eq!(closed.receipt_attention.unknown, 1);
}

// ------------------------------------------ what each close is responsible for

/// Two tills in one day, the first with an unresolved receipt.
///
/// Returned in order: what closing till A said, what closing till B said, and
/// what closing the day said. Two lifetimes because the printer answers
/// differently for each till, and the day stays open across both.
fn two_tills_one_day(
    path: &std::path::Path,
    first_printer: SimulatedHardware,
) -> (
    SessionCloseResult,
    SessionCloseResult,
    BusinessDayCloseResult,
) {
    let closed_a = {
        let core = core_at_with_printer(path, 0, 1, SimulatorPrinter::answering(first_printer));
        init_device(&core);
        local_catalog().activate(&core).unwrap();
        core.open_business_day(OpenBusinessDayCommand::default())
            .unwrap();
        open_till_on(&core, "REG-001");
        bootstrap_stock(&core, &[(SKU, 10)]);
        finalized_cash_sale(&core, demo_item(), UNIT);
        close_till(&core, 100_000 + UNIT)
    };

    // A second till, same day, with a printer that works.
    let core = core_at_with_printer(path, 0, 10_000, SimulatorPrinter::working());
    open_till_on(&core, "REG-002");
    finalized_cash_sale(&core, demo_item(), UNIT);
    let closed_b = close_till(&core, 100_000 + UNIT);
    let day = core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap();
    (closed_a, closed_b, day)
}

fn open_till_on(core: &TestCore, register: &str) {
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new(register).unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
}

/// Close the till only, answering what the close said.
fn close_till(core: &TestCore, counted_minor: i64) -> SessionCloseResult {
    core.request_session_close(RequestSessionCloseCommand::default())
        .unwrap();
    core.declare_closing_cash(DeclareClosingCashCommand {
        request_id: None,
        counted_minor,
    })
    .unwrap()
}

/// Closing a till reports *its* receipts, and the day reports both tills'.
#[test]
fn a_till_close_reports_its_own_receipts_and_the_day_reports_every_till() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    let (till_a, till_b, day) = two_tills_one_day(&path, SimulatedHardware::Unavailable);

    // The till whose receipt did not print owns that problem.
    assert_eq!(till_a.receipt_attention.failed, 1);
    assert_eq!(till_a.receipt_attention.unknown, 0);
    // The next till is clean, and closing it must not inherit the first
    // till's unresolved receipt (ADR 043).
    assert!(
        till_b.receipt_attention.is_clear(),
        "a till close reports only its own receipts: {:?}",
        till_b.receipt_attention
    );
    assert_ne!(till_a.session_id, till_b.session_id);

    // The day owns both tills, so it sees the one that is outstanding.
    assert_eq!(day.day.status(), BusinessDayStatus::Closed);
    assert_eq!(day.receipt_attention.failed, 1);
    assert_eq!(day.receipt_attention.unknown, 0);
}

/// The same, with a receipt that may have printed.
#[test]
fn an_unknown_receipt_on_one_till_does_not_contaminate_another() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    let (till_a, till_b, day) = two_tills_one_day(&path, SimulatedHardware::IoError);

    assert_eq!(till_a.receipt_attention.unknown, 1);
    assert_eq!(till_a.receipt_attention.failed, 0);
    assert!(till_b.receipt_attention.is_clear());
    assert_eq!(day.receipt_attention.unknown, 1);
}

/// And the installation-wide view still sees everything, whichever till or
/// day it belonged to.
#[test]
fn the_global_view_sees_every_unresolved_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-edge.db");
    let (till_a, _, day) = two_tills_one_day(&path, SimulatedHardware::Unavailable);
    assert_eq!(till_a.receipt_attention.failed, 1);
    assert_eq!(day.receipt_attention.failed, 1);

    // A second day, with its own unresolved receipt.
    let core = core_at_with_printer(
        &path,
        2,
        20_000,
        SimulatorPrinter::answering(SimulatedHardware::IoError),
    );
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    open_till_on(&core, "REG-001");
    finalized_cash_sale(&core, demo_item(), UNIT);
    let till = close_till(&core, 100_000 + UNIT);
    let second_day = core
        .close_business_day(CloseBusinessDayCommand::default())
        .unwrap();

    // Each close answers for itself…
    assert_eq!(till.receipt_attention.unknown, 1);
    assert_eq!(
        till.receipt_attention.failed, 0,
        "yesterday is not this till"
    );
    assert_eq!(second_day.receipt_attention.unknown, 1);
    assert_eq!(second_day.receipt_attention.failed, 0, "nor this day");

    // …and the installation still owes both.
    let global = core.get_receipt_attention().unwrap();
    assert_eq!(global.failed, 1);
    assert_eq!(global.unknown, 1);
    assert_eq!(global.total(), 2);
    assert_eq!(core.list_recent_sales(10, 0).unwrap().attention, global);
}
