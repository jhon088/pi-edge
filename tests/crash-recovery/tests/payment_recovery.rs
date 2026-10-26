//! Financial state survives a process death.
//!
//! Every case here is the same shape: do something with money, throw the
//! application object away, build a brand new one against the same database,
//! and check that what comes back is both correct and *safe* — in particular
//! that an interaction we could not see the end of is never assumed to have
//! failed (ADR 018, ADR 020).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::ports::EdgeStore;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;
use edge_payments::{AttemptStatus, PaymentStatus, SimulatedOutcome, SimulatorScript};

/// The demo sale: 100.00 at 16% = 116.00.
const TOTAL: i64 = 11_600;

fn open_totalized_sale(core: &ScriptedCore) {
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory: true,
    })
    .unwrap();
    edge_application::testing::CatalogFixture::demo(1)
        .activate(core)
        .unwrap();
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(AddItemCommand {
        request_id: None,
        selector: edge_catalog::ProductSelector::Barcode(
            edge_catalog::Barcode::new(edge_application::testing::DEMO_BARCODE).unwrap(),
        ),
        quantity: Quantity::from_units(1).unwrap(),
        selections: Vec::new(),
        preparation_note: None,
    })
    .unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
}

fn card(core: &ScriptedCore, amount_minor: i64) -> edge_application::PaymentOutcome {
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: edge_payments::TenderType::Card,
        amount_minor,
    })
    .unwrap()
}

fn cash(core: &ScriptedCore, amount_minor: i64) -> edge_application::PaymentOutcome {
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: edge_payments::TenderType::Cash,
        amount_minor,
    })
    .unwrap()
}

/// A card whose result never came back: the attempt stays `UNKNOWN`, and a new
/// process must still refuse to guess.
#[test]
fn an_unknown_card_result_survives_a_restart_and_is_still_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let payment_id = {
        let core = scripted_core_at(
            &db,
            1,
            SimulatorScript::approving().defaulting_to(SimulatedOutcome::Unknown),
        );
        open_totalized_sale(&core);
        let outcome = card(&core, TOTAL);
        assert_eq!(outcome.payment.attempts[0].status, AttemptStatus::Unknown);
        outcome.payment.payment.payment_id
    };

    // --- a brand new application object, same database --------------------
    let core = scripted_core_at(
        &db,
        1_000,
        SimulatorScript::approving().resolving_queries_to(SimulatedOutcome::Approve),
    );
    let recovered = core.get_payment(payment_id).unwrap();
    assert_eq!(
        recovered.payment.attempts[0].status,
        AttemptStatus::Unknown,
        "never promoted to FAILED by a restart"
    );
    assert_eq!(recovered.payment.payment.status, PaymentStatus::InProgress);
    assert!(recovered.settlement.requires_reconciliation);
    assert!(!recovered.settlement.can_finalize());

    // The sale is still blocked and still not finalizable.
    assert_eq!(
        core.finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap_err()
            .kind(),
        "INVALID_TRANSACTION_STATE"
    );

    // And the safe way out still works across the restart.
    let resolved = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: None,
            payment_id,
        })
        .unwrap();
    assert_eq!(resolved.payment.payment.status, PaymentStatus::Committed);
    assert_eq!(resolved.settlement.remaining_balance.minor(), 0);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

/// The crash the ordering in ADR 020 exists for: the attempt is committed as
/// `PROCESSING` before the provider is called, so a process that dies around
/// the call leaves a record that can be reconciled.
#[test]
fn a_processing_attempt_recovered_after_a_crash_is_reconcilable_never_failed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    // Simulate the crash by writing the phase-1 state and never recording a
    // result: the attempt is left exactly as a killed process would leave it.
    let (payment_id, attempt_id) = {
        let core = scripted_core_at(&db, 1, SimulatorScript::approving());
        open_totalized_sale(&core);
        let outcome = card(&core, TOTAL);
        let payment_id = outcome.payment.payment.payment_id;
        let attempt_id = outcome.payment.attempts[0].attempt_id;
        // Roll the attempt back to the state phase 1 committed.
        core.store()
            .transact(|uow| {
                let mut attempt = uow.find_payment_attempt(attempt_id).unwrap().unwrap();
                attempt.status = AttemptStatus::Processing;
                attempt.authorized_amount = None;
                attempt.provider_reference = None;
                uow.update_payment_attempt(&attempt)?;
                let mut payment = uow.find_payment(payment_id).unwrap().unwrap().payment;
                payment.status = PaymentStatus::InProgress;
                payment.committed_amount = Money::zero(Currency::Mxn);
                payment.authorized_amount = None;
                uow.update_payment(&payment)
            })
            .unwrap();
        (payment_id, attempt_id)
    };

    let core = scripted_core_at(
        &db,
        1_000,
        SimulatorScript::approving().resolving_queries_to(SimulatedOutcome::Approve),
    );
    let recovered = core.get_payment(payment_id).unwrap();
    assert_eq!(
        recovered.payment.attempts[0].status,
        AttemptStatus::Processing
    );
    // A PROCESSING attempt is unresolved: the honest state is "we asked and we
    // do not know", so it blocks and it is reconcilable (ADR 017).
    assert!(recovered.settlement.requires_resolution);
    assert!(recovered.settlement.requires_reconciliation);
    assert!(!recovered.settlement.can_finalize());
    assert!(recovered.settlement.blocks_basket);

    // Asking the provider is the only way forward, and it resolves the same
    // interaction rather than starting a new one.
    let resolved = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: None,
            payment_id,
        })
        .unwrap();
    assert_eq!(resolved.payment.attempts.len(), 1, "no new charge");
    assert_eq!(resolved.payment.attempts[0].attempt_id, attempt_id);
    assert_eq!(resolved.payment.payment.status, PaymentStatus::Committed);
}

#[test]
fn an_authorized_card_survives_a_restart_and_can_be_completed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let payment_id = {
        let core = scripted_core_at(&db, 1, SimulatorScript::approving());
        open_totalized_sale(&core);
        let outcome = card(&core, TOTAL);
        assert_eq!(
            outcome.payment.attempts[0].status,
            AttemptStatus::Authorized
        );
        outcome.payment.payment.payment_id
    };

    let core = scripted_core_at(&db, 1_000, SimulatorScript::approving());
    let recovered = core.get_payment(payment_id).unwrap();
    assert_eq!(
        recovered.payment.attempts[0].status,
        AttemptStatus::Authorized
    );
    assert_eq!(recovered.payment.payment.status, PaymentStatus::Committed);
    assert_eq!(recovered.payment.payment.committed_amount.minor(), TOTAL);
    assert!(recovered.settlement.can_finalize());
    // The safe path forward is available: finish the sale.
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.status(), TransactionStatus::Finalized);
}

#[test]
fn a_committed_cash_payment_survives_a_restart_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let (payment_id, expected) = {
        let core = scripted_core_at(&db, 1, SimulatorScript::approving());
        open_totalized_sale(&core);
        // 200.00 against a 116.00 sale: 116.00 applied, 84.00 back.
        let outcome = cash(&core, 20_000);
        (outcome.payment.payment.payment_id, outcome)
    };

    let core = scripted_core_at(&db, 1_000, SimulatorScript::approving());
    let recovered = core.get_payment(payment_id).unwrap();
    assert_eq!(recovered.payment, expected.payment);
    assert_eq!(recovered.payment.payment.requested_amount.minor(), 20_000);
    assert_eq!(recovered.payment.payment.committed_amount.minor(), TOTAL);
    assert_eq!(
        recovered.payment.payment.change_due().unwrap().minor(),
        8_400
    );
    assert_eq!(recovered.settlement.committed_total.minor(), TOTAL);
    assert_eq!(recovered.settlement.change_due.minor(), 8_400);
    assert_eq!(recovered.settlement.remaining_balance.minor(), 0);

    // The sale agrees with the payments it is told about.
    let sale = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(sale.paid_total().unwrap().minor(), TOTAL);
    assert_eq!(sale.change_due().unwrap().minor(), 8_400);
    assert_eq!(sale.balance_due().unwrap().minor(), 0);
    assert_eq!(sale.status(), TransactionStatus::Committed);
}

#[test]
fn a_partial_mixed_tender_rebuilds_the_exact_remaining_balance() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let transaction_id = {
        let core = scripted_core_at(&db, 1, SimulatorScript::approving());
        open_totalized_sale(&core);
        // 30.00 cash and 20.00 card leave 66.00 owed.
        cash(&core, 3_000);
        let outcome = card(&core, 2_000);
        assert_eq!(outcome.settlement.remaining_balance.minor(), 6_600);
        outcome.transaction.id()
    };

    let core = scripted_core_at(&db, 1_000, SimulatorScript::approving());
    let (payments, settlement) = core.get_transaction_payments(transaction_id).unwrap();
    assert_eq!(payments.len(), 2);
    assert_eq!(settlement.committed_total.minor(), 5_000);
    assert_eq!(settlement.remaining_balance.minor(), 6_600);
    assert!(!settlement.can_finalize());
    // Attempt history is intact: the cash payment has none, the card has one.
    assert!(payments[0].attempts.is_empty());
    assert_eq!(payments[1].attempts.len(), 1);
    assert_eq!(payments[1].attempts[0].attempt_number, 1);

    // And the sale completes from the recovered balance.
    let finished = cash(&core, 6_600);
    assert_eq!(finished.settlement.remaining_balance.minor(), 0);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

#[test]
fn a_reversal_left_pending_by_a_crash_still_blocks_the_sale() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let payment_id = {
        let core = scripted_core_at(
            &db,
            1,
            SimulatorScript::approving().with_reversal_confirmed(false),
        );
        open_totalized_sale(&core);
        let committed = card(&core, TOTAL);
        let pending = core
            .reverse_payment(ReversePaymentCommand {
                request_id: None,
                payment_id: committed.payment.payment.payment_id,
            })
            .unwrap();
        assert_eq!(
            pending.payment.attempts[0].status,
            AttemptStatus::ReversalPending
        );
        committed.payment.payment.payment_id
    };

    // A new process sees unfinished financial business, not a clean slate.
    let core = scripted_core_at(&db, 1_000, SimulatorScript::approving());
    let recovered = core.get_payment(payment_id).unwrap();
    assert_eq!(
        recovered.payment.attempts[0].status,
        AttemptStatus::ReversalPending
    );
    assert!(!recovered.settlement.can_finalize());

    // Retrying the reversal confirms it — the provider is idempotent on the
    // attempt's correlation id, so this cannot double-reverse.
    let confirmed = core
        .reverse_payment(ReversePaymentCommand {
            request_id: None,
            payment_id,
        })
        .unwrap();
    assert_eq!(confirmed.payment.payment.status, PaymentStatus::Reversed);
    assert_eq!(confirmed.payment.attempts.len(), 1);
    assert_eq!(confirmed.settlement.remaining_balance.minor(), TOTAL);
}

#[test]
fn a_finalized_sale_keeps_its_payment_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let transaction_id = {
        let core = scripted_core_at(&db, 1, SimulatorScript::approving());
        open_totalized_sale(&core);
        cash(&core, 3_000);
        card(&core, 8_600);
        let finalized = core
            .finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap();
        assert_eq!(finalized.status(), TransactionStatus::Finalized);
        finalized.id()
    };

    let core = scripted_core_at(&db, 1_000, SimulatorScript::approving());
    let (payments, settlement) = core.get_transaction_payments(transaction_id).unwrap();
    assert_eq!(payments.len(), 2);
    assert_eq!(settlement.committed_total.minor(), TOTAL);
    assert_eq!(settlement.remaining_balance.minor(), 0);
    // Both tenders, with the card's provider reference, still readable.
    assert_eq!(payments[0].payment.tender_type.as_str(), "CASH");
    assert_eq!(payments[1].payment.tender_type.as_str(), "CARD");
    assert!(payments[1].attempts[0].provider_reference.is_some());
}

/// The window between the last payment and the commercial close.
///
/// Cash is durable the moment the payment commits — the drawer ledger does not
/// wait for finalization (ADR 021) — so a crash here must leave exactly one
/// cash movement, and the sale must still be finalizable.
#[test]
fn a_crash_between_the_last_cash_payment_and_the_close_loses_and_doubles_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    {
        let core = scripted_core_at(&db, 1, SimulatorScript::approving());
        open_totalized_sale(&core);
        cash(&core, TOTAL);
        // The money is in the drawer and the sale is not closed yet.
        assert_eq!(
            core.get_cash_position()
                .unwrap()
                .position
                .cash_sales
                .minor(),
            TOTAL
        );
        core.store().arm_commit_failure();
        assert!(
            core.finalize_transaction(FinalizeTransactionCommand::default())
                .is_err(),
            "the commercial commit must have failed"
        );
    }

    let core = scripted_core_at(&db, 1_000, SimulatorScript::approving());
    // Still one payment, one cash movement, and a sale waiting to be closed.
    let sale = core.get_active_transaction().unwrap().unwrap();
    assert_eq!(sale.status(), TransactionStatus::Committed);
    assert!(sale.state().finalized_at.is_none());
    let position = core.get_cash_position().unwrap().position;
    assert_eq!(position.cash_sales.minor(), TOTAL);
    assert_eq!(core.get_transaction_payments(sale.id()).unwrap().0.len(), 1);
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 0);

    // Closing it now moves the stock once and the cash not at all: the cash
    // already moved when the payment did.
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.status(), TransactionStatus::Finalized);
    assert_eq!(
        core.get_cash_position()
            .unwrap()
            .position
            .cash_sales
            .minor(),
        TOTAL,
        "no second cash movement"
    );
    assert_eq!(core.get_edge_state().unwrap().pending_outbox, 1);
    // And the obligation was created by the close, then satisfied by the
    // simulated printer this harness configures (ADR 043).
    assert_eq!(
        core.get_receipt(finalized.id()).unwrap().status,
        ReceiptStatus::Printed
    );
}

/// The same window with a card: closing the sale must not talk to the provider
/// again.
#[test]
fn a_crash_between_a_card_authorization_and_the_close_never_charges_twice() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");

    let authorized = {
        let core = scripted_core_at(&db, 1, SimulatorScript::approving());
        open_totalized_sale(&core);
        let outcome = card(&core, TOTAL);
        assert_eq!(
            outcome.payment.attempts[0].status,
            AttemptStatus::Authorized
        );
        core.store().arm_commit_failure();
        assert!(core
            .finalize_transaction(FinalizeTransactionCommand::default())
            .is_err());
        outcome.payment.attempts[0].clone()
    };

    let core = scripted_core_at(&db, 1_000, SimulatorScript::approving());
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.status(), TransactionStatus::Finalized);

    // One attempt, the same one: a second charge would be a second attempt
    // with a new correlation id (ADR 018, ADR 020).
    let (payments, settlement) = core.get_transaction_payments(finalized.id()).unwrap();
    assert_eq!(payments.len(), 1);
    let attempts = &payments[0].attempts;
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].correlation_id, authorized.correlation_id);
    assert_eq!(
        attempts[0].provider_reference, authorized.provider_reference,
        "finalizing reads the authorization; it never asks for a new one"
    );
    assert_eq!(settlement.committed_total.minor(), TOTAL);
}
