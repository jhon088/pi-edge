//! Hardware side effects across a process death.
//!
//! A simulator's counter lives in memory, so it starts at zero in every new
//! lifetime. That is exactly what makes these tests meaningful: whatever stops
//! a drawer from opening twice has to be **durable**, and the only durable
//! thing is the action log (ADR 037).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_crash_recovery_tests::*;
use edge_hardware::{HardwareDeviceState, SimulatedHardware, SimulatorDrawer};

const SKU: &str = "SKU-001";
const UNIT: i64 = 11_600;

#[test]
fn a_replayed_cash_payment_across_a_restart_opens_the_drawer_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let request_id = uuid::Uuid::from_u128(0xCAFE);
    let payment = || StartPaymentCommand {
        request_id: Some(request_id),
        tender_type: edge_payments::TenderType::Cash,
        amount_minor: UNIT,
    };
    {
        let life = HardwareLifetime::open(&db, 1);
        open_register_with_float(&life.core, 100_000);
        bootstrap_stock(&life.core, &[(SKU, 10)]);
        life.core
            .start_transaction(StartTransactionCommand::default())
            .unwrap();
        life.core.add_item(demo_item()).unwrap();
        life.core
            .totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        life.core.start_payment(payment()).unwrap();
        assert_eq!(life.opens(), 1);
    }

    // A new process, a new drawer object, a count back at zero.
    let life = HardwareLifetime::open(&db, 1_000);
    assert_eq!(life.opens(), 0, "the counter is per process");
    life.core.start_payment(payment()).unwrap();

    // Nothing opened: the action log remembers that this payment already
    // opened it, and that is the only thing that could (ADR 037).
    assert_eq!(life.opens(), 0);
    assert_eq!(
        life.core
            .get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
        UNIT
    );
}

#[test]
fn a_replayed_cash_refund_across_a_restart_opens_the_drawer_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let refund_id;
    {
        let life = HardwareLifetime::open(&db, 1);
        open_register_with_float(&life.core, 100_000);
        bootstrap_stock(&life.core, &[(SKU, 10)]);
        let sale = finalized_cash_sale(&life.core, UNIT);
        refund_id = life
            .core
            .create_refund(CreateRefundCommand {
                request_id: None,
                authorization: central_authority(sale),
                lines: refund_lines(&[(1, 1)]),
                reason_code: "CUSTOMER_RETURN".to_owned(),
                fiscal_action: edge_refunds::RefundFiscalStatus::None,
            })
            .unwrap()
            .refund
            .refund_id;
        life.core
            .accept_refund(AcceptRefundCommand {
                request_id: None,
                refund_id,
            })
            .unwrap();
        life.core
            .start_refund_payment(StartRefundPaymentCommand {
                request_id: None,
                refund_id,
            })
            .unwrap();
        assert_eq!(life.opens(), 2, "the sale and the refund");
    }

    let life = HardwareLifetime::open(&db, 1_000);
    life.core
        .start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id,
        })
        .unwrap();

    assert_eq!(life.opens(), 0, "the refund already opened it, once");
}

#[test]
fn a_drawer_that_was_down_can_be_tried_again_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        // The drawer is unplugged while the sale happens.
        let life = HardwareLifetime::with(
            &db,
            1,
            SimulatorDrawer::answering(SimulatedHardware::Unavailable),
        );
        open_register_with_float(&life.core, 100_000);
        bootstrap_stock(&life.core, &[(SKU, 10)]);
        cash_sale(&life.core, UNIT);
        assert_eq!(life.opens(), 0);
        // The sale is still a sale.
        assert_eq!(
            life.core
                .get_cash_position()
                .unwrap()
                .position
                .cash_sales
                .minor(),
            UNIT
        );
    }

    // Plugged back in. A *failed* action does not block a retry, so the
    // diagnostic button works and the operator can get at the cash.
    let life = HardwareLifetime::open(&db, 1_000);
    life.core
        .open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
        .unwrap();
    assert_eq!(life.opens(), 1);
}

#[test]
fn a_restart_reports_every_device_as_unknown_until_it_is_probed_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        let life = HardwareLifetime::open(&db, 1);
        open_register_with_float(&life.core, 100_000);
        bootstrap_stock(&life.core, &[(SKU, 10)]);
        life.core
            .open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
            .unwrap();
        let status = life.core.get_hardware_status().unwrap();
        assert_eq!(status.drawer.state, HardwareDeviceState::Available);
    }

    // A new process. The *claim* that the drawer is available died with the
    // old one; only what it did survives (ADR 035).
    let life = HardwareLifetime::open(&db, 1_000);
    let status = life.core.get_hardware_status().unwrap();
    assert!(
        status.drawer.last_success_at.is_some(),
        "what it did survives, rebuilt from the action log"
    );
    assert_eq!(
        status.scanner.state,
        HardwareDeviceState::Unknown,
        "and a device nobody probed is unknown, not available"
    );
}

#[test]
fn a_scanner_that_worked_before_a_restart_is_unknown_again_afterwards() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        let life = HardwareLifetime::open(&db, 1);
        open_register_with_float(&life.core, 100_000);
        bootstrap_stock(&life.core, &[(SKU, 10)]);
        life.core
            .start_transaction(StartTransactionCommand::default())
            .unwrap();
        life.core.submit_scan(scan_of("7501000000001")).unwrap();
        let status = life.core.get_hardware_status().unwrap();
        assert_eq!(status.scanner.state, HardwareDeviceState::Available);
    }

    let life = HardwareLifetime::open(&db, 1_000);
    let status = life.core.get_hardware_status().unwrap();
    // A scan from the previous process is not evidence the scanner is plugged
    // in now. What survives is when it last worked, not a claim that it does
    // (ADR 035).
    assert_eq!(status.scanner.state, HardwareDeviceState::Unknown);
    assert!(status.scanner.last_success_at.is_some());
}

#[test]
fn a_crash_before_commit_leaves_no_claim_and_no_opening() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let life = HardwareLifetime::open(&db, 1);
    open_register_with_float(&life.core, 100_000);
    bootstrap_stock(&life.core, &[(SKU, 10)]);

    // The database refuses to commit the claim, so the device is never
    // touched: the claim comes first on purpose (ADR 020, ADR 037).
    life.core.store().arm_commit_failure();
    assert!(life
        .core
        .open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
        .is_err());
    assert_eq!(life.opens(), 0);

    // And a retry works, because nothing was recorded.
    life.core
        .open_drawer_diagnostic(OpenDrawerCommand { request_id: None })
        .unwrap();
    assert_eq!(life.opens(), 1);
}

#[test]
fn a_reprint_after_a_restart_prints_and_still_moves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let sale;
    {
        let life = HardwareLifetime::open(&db, 1);
        open_register_with_float(&life.core, 100_000);
        bootstrap_stock(&life.core, &[(SKU, 10)]);
        sale = finalized_cash_sale(&life.core, UNIT);
    }

    let life = HardwareLifetime::open(&db, 1_000);
    let cash_before = life
        .core
        .get_cash_position()
        .unwrap()
        .position
        .expected_cash;
    let stock_before = on_hand(&life.core, SKU);

    life.core
        .print_transaction(PrintTransactionCommand {
            request_id: None,
            transaction_id: sale,
            reprint: true,
        })
        .unwrap();

    // Paper, and nothing else. The source of a reprint is the sale, which is
    // still there after the restart (ADR 039).
    assert_eq!(life.printer.job_count(), 1);
    assert_eq!(life.opens(), 0);
    assert_eq!(
        life.core
            .get_cash_position()
            .unwrap()
            .position
            .expected_cash,
        cash_before
    );
    assert_eq!(on_hand(&life.core, SKU), stock_before);
}
