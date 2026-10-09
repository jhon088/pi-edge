//! The payment engine against real SQLite.
//!
//! The arithmetic and the state machines are pinned by unit tests in
//! `edge-payments`; these check what only the whole runtime shows: that money
//! survives a round trip through the database, that an unknown outcome blocks
//! the right things, that a retried command does not move money twice, and
//! that the basket freezes the moment a payment starts.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::AppError;
use edge_domain::prelude::*;
use edge_integration_tests::*;
use edge_payments::{AttemptStatus, PaymentStatus, SimulatedOutcome, SimulatorScript, TenderType};

/// The demo sale: 100.00 at 16% = 116.00.
const TOTAL: i64 = 11_600;

fn totalized(core: &TestCore) -> RetailTransaction {
    open_register(core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap()
}

fn totalized_scripted(core: &ScriptedCore) -> RetailTransaction {
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
    })
    .unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap()
}

fn cash(core: &ScriptedCore, amount_minor: i64) -> edge_application::PaymentOutcome {
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor,
    })
    .unwrap()
}

fn card(core: &ScriptedCore, amount_minor: i64) -> edge_application::PaymentOutcome {
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor,
    })
    .unwrap()
}

// ------------------------------------------------------------------- cash

#[test]
fn cash_exactly_covering_the_sale_commits_it() {
    let core = in_memory_core();
    totalized(&core);
    let outcome = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: TOTAL,
        })
        .unwrap();

    assert_eq!(outcome.payment.payment.status, PaymentStatus::Committed);
    assert_eq!(outcome.payment.payment.committed_amount.minor(), TOTAL);
    // Cash has no provider, so it has no attempts (ADR 016).
    assert!(outcome.payment.attempts.is_empty());
    assert_eq!(outcome.settlement.remaining_balance.minor(), 0);
    assert_eq!(outcome.settlement.change_due.minor(), 0);
    assert!(outcome.settlement.can_finalize());
    assert_eq!(outcome.transaction.status(), TransactionStatus::Committed);
}

#[test]
fn cash_overpayment_commits_the_debt_and_returns_the_change() {
    let core = in_memory_core();
    totalized(&core);
    let outcome = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: 20_000,
        })
        .unwrap();

    // 200.00 handed over: the sale collects 116.00 and owes 84.00 back. The
    // tendered note must not inflate what the sale actually took.
    assert_eq!(outcome.payment.payment.requested_amount.minor(), 20_000);
    assert_eq!(outcome.payment.payment.committed_amount.minor(), TOTAL);
    assert_eq!(outcome.payment.payment.change_due().unwrap().minor(), 8_400);
    assert_eq!(outcome.settlement.committed_total.minor(), TOTAL);
    assert_eq!(outcome.settlement.tendered_total.minor(), 20_000);
    assert_eq!(outcome.settlement.change_due.minor(), 8_400);
    assert!(outcome.settlement.can_finalize());
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

#[test]
fn cash_can_be_partial_and_several_partials_add_up() {
    let core = in_memory_core();
    totalized(&core);

    let first = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: 5_000,
        })
        .unwrap();
    assert_eq!(first.settlement.remaining_balance.minor(), 6_600);
    assert!(!first.settlement.can_finalize());
    assert_eq!(
        first.transaction.status(),
        TransactionStatus::PaymentInProgress
    );

    let second = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: 1_000,
        })
        .unwrap();
    assert_eq!(second.settlement.remaining_balance.minor(), 5_600);

    let third = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: 5_600,
        })
        .unwrap();
    assert_eq!(third.settlement.remaining_balance.minor(), 0);
    assert!(third.settlement.can_finalize());

    // Three payments, all kept.
    let (payments, settlement) = core
        .get_transaction_payments(third.transaction.id())
        .unwrap();
    assert_eq!(payments.len(), 3);
    assert_eq!(settlement.committed_total.minor(), TOTAL);
}

// ------------------------------------------------------------------- card

#[test]
fn an_approved_card_authorizes_and_commits() {
    let core = scripted_core(SimulatorScript::approving());
    totalized_scripted(&core);
    let outcome = card(&core, TOTAL);

    assert_eq!(outcome.payment.payment.status, PaymentStatus::Committed);
    assert_eq!(outcome.payment.payment.committed_amount.minor(), TOTAL);
    // One attempt, with the provider's reference recorded.
    assert_eq!(outcome.payment.attempts.len(), 1);
    let attempt = &outcome.payment.attempts[0];
    assert_eq!(attempt.status, AttemptStatus::Authorized);
    assert_eq!(attempt.attempt_number, 1);
    assert!(attempt.provider_reference.is_some());
    assert_eq!(attempt.authorized_amount.unwrap().minor(), TOTAL);
    assert!(outcome.settlement.can_finalize());
}

#[test]
fn a_declined_card_applies_nothing_and_leaves_the_sale_payable() {
    let core = scripted_core(SimulatorScript::approving().defaulting_to(SimulatedOutcome::Decline));
    totalized_scripted(&core);
    let outcome = card(&core, TOTAL);

    assert_eq!(outcome.payment.payment.status, PaymentStatus::Failed);
    assert_eq!(outcome.payment.payment.committed_amount.minor(), 0);
    assert_eq!(
        outcome.payment.attempts[0].failure_code.as_deref(),
        Some("SIMULATED_DECLINED")
    );
    assert_eq!(outcome.settlement.remaining_balance.minor(), TOTAL);
    // A failed payment is terminal, so nothing holds the sale any more.
    assert!(!outcome.settlement.requires_resolution);
    assert!(!outcome.settlement.blocks_basket);
    assert_eq!(outcome.transaction.status(), TransactionStatus::Totalized);
}

#[test]
fn a_declined_card_can_be_retried_and_the_history_is_kept() {
    let core =
        scripted_core(SimulatorScript::approving().on_amount(TOTAL, SimulatedOutcome::Decline));
    let sale = totalized_scripted(&core);
    card(&core, TOTAL);

    // A second, smaller card payment goes through: two payments, each with its
    // own attempt, and the declined one is still on record.
    let retried = card(&core, 5_000);
    assert_eq!(retried.payment.payment.status, PaymentStatus::Committed);

    let (payments, _) = core.get_transaction_payments(sale.id()).unwrap();
    assert_eq!(payments.len(), 2);
    assert_eq!(payments[0].payment.status, PaymentStatus::Failed);
    assert_eq!(payments[0].attempts[0].status, AttemptStatus::Failed);
    assert_eq!(payments[1].payment.status, PaymentStatus::Committed);
}

#[test]
fn a_card_cannot_overpay_the_remaining_balance() {
    let core = scripted_core(SimulatorScript::approving());
    totalized_scripted(&core);
    let error = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Card,
            amount_minor: TOTAL + 1,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "PAYMENT_EXCEEDS_BALANCE");

    // Nothing was recorded, so nothing was sent.
    let (payments, settlement) = core
        .get_transaction_payments(core.get_active_transaction().unwrap().unwrap().id())
        .unwrap();
    assert!(payments.is_empty());
    assert_eq!(settlement.remaining_balance.minor(), TOTAL);
}

// ------------------------------------------------------------ mixed tender

#[test]
fn cash_and_card_together_cover_the_sale_exactly() {
    let core = scripted_core(SimulatorScript::approving());
    totalized_scripted(&core);
    // The demo sale is 116.00: 30.00 cash and 86.00 card.
    let partial = cash(&core, 3_000);
    assert_eq!(partial.settlement.remaining_balance.minor(), 8_600);

    let completed = card(&core, 8_600);
    assert_eq!(completed.settlement.committed_total.minor(), TOTAL);
    assert_eq!(completed.settlement.remaining_balance.minor(), 0);
    assert_eq!(completed.settlement.change_due.minor(), 0);
    assert!(completed.settlement.can_finalize());
    assert_eq!(completed.transaction.status(), TransactionStatus::Committed);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

#[test]
fn a_card_cannot_exceed_what_cash_left_unpaid() {
    let core = scripted_core(SimulatorScript::approving());
    totalized_scripted(&core);
    cash(&core, 3_000);
    let error = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Card,
            amount_minor: 8_601,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "PAYMENT_EXCEEDS_BALANCE");
}

#[test]
fn cash_may_still_overpay_the_remainder_of_a_mixed_sale() {
    let core = scripted_core(SimulatorScript::approving());
    totalized_scripted(&core);
    card(&core, 8_600);
    let outcome = cash(&core, 5_000);
    // 30.00 was owed; 50.00 came in, so 20.00 goes back.
    assert_eq!(outcome.payment.payment.committed_amount.minor(), 3_000);
    assert_eq!(outcome.settlement.change_due.minor(), 2_000);
    assert_eq!(outcome.settlement.remaining_balance.minor(), 0);
    assert!(outcome.settlement.can_finalize());
}

// ----------------------------------------------------------------- UNKNOWN

#[test]
fn an_unknown_card_result_stays_unknown() {
    let core = scripted_core(SimulatorScript::approving().defaulting_to(SimulatedOutcome::Unknown));
    totalized_scripted(&core);
    let outcome = card(&core, TOTAL);

    // Never assumed failed: we asked and we do not know (ADR 018).
    assert_eq!(outcome.payment.attempts[0].status, AttemptStatus::Unknown);
    assert_eq!(outcome.payment.payment.status, PaymentStatus::InProgress);
    assert_eq!(outcome.payment.payment.committed_amount.minor(), 0);
    assert!(outcome.settlement.requires_resolution);
    assert!(outcome.settlement.requires_reconciliation);
    assert!(outcome.settlement.blocks_basket);
    assert!(!outcome.settlement.can_finalize());
}

#[test]
fn an_unknown_payment_blocks_a_second_attempt_at_the_same_charge() {
    let core = scripted_core(SimulatorScript::approving().defaulting_to(SimulatedOutcome::Unknown));
    totalized_scripted(&core);
    card(&core, TOTAL);

    // Another card payment could duplicate the charge we cannot see, so it is
    // refused — and so is cash, because the remaining balance is unknowable.
    for tender in [TenderType::Card, TenderType::Cash] {
        let error = core
            .start_payment(StartPaymentCommand {
                request_id: None,
                tender_type: tender,
                amount_minor: TOTAL,
            })
            .unwrap_err();
        assert_eq!(
            error.kind(),
            "PAYMENT_REQUIRES_RECONCILIATION",
            "{tender:?} must be refused"
        );
    }
}

#[test]
fn an_unknown_payment_blocks_finalization_and_the_basket() {
    let core = scripted_core(SimulatorScript::approving().defaulting_to(SimulatedOutcome::Unknown));
    totalized_scripted(&core);
    card(&core, TOTAL);

    assert_eq!(
        core.finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap_err()
            .kind(),
        "INVALID_TRANSACTION_STATE"
    );
    assert_eq!(
        core.remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 1,
        })
        .unwrap_err()
        .kind(),
        "BASKET_LOCKED_BY_PAYMENT"
    );
}

#[test]
fn reconciling_an_unknown_payment_can_resolve_it_as_authorized() {
    let core = scripted_core(
        SimulatorScript::approving()
            .defaulting_to(SimulatedOutcome::Unknown)
            .resolving_queries_to(SimulatedOutcome::Approve),
    );
    totalized_scripted(&core);
    let unknown = card(&core, TOTAL);

    let resolved = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: None,
            payment_id: unknown.payment.payment.payment_id,
        })
        .unwrap();

    // The provider was charged after all, so the money is applied — no new
    // charge was created to get here.
    assert_eq!(resolved.payment.attempts.len(), 1, "no new attempt");
    assert_eq!(
        resolved.payment.attempts[0].status,
        AttemptStatus::Authorized
    );
    assert_eq!(resolved.payment.payment.status, PaymentStatus::Committed);
    assert_eq!(resolved.settlement.remaining_balance.minor(), 0);
    assert!(resolved.settlement.can_finalize());
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

#[test]
fn reconciling_an_unknown_payment_can_resolve_it_as_failed() {
    let core = scripted_core(
        SimulatorScript::approving()
            .defaulting_to(SimulatedOutcome::Unknown)
            .resolving_queries_to(SimulatedOutcome::Decline),
    );
    totalized_scripted(&core);
    let unknown = card(&core, TOTAL);

    let resolved = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: None,
            payment_id: unknown.payment.payment.payment_id,
        })
        .unwrap();

    assert_eq!(resolved.payment.attempts[0].status, AttemptStatus::Failed);
    assert_eq!(resolved.payment.payment.status, PaymentStatus::Failed);
    assert_eq!(resolved.settlement.remaining_balance.minor(), TOTAL);
    assert!(!resolved.settlement.requires_resolution);
    // The sale is payable again, by any tender.
    let paid = cash(&core, TOTAL);
    assert!(paid.settlement.can_finalize());
}

#[test]
fn reconciling_a_resolved_payment_changes_nothing() {
    let core = scripted_core(SimulatorScript::approving());
    totalized_scripted(&core);
    let committed = card(&core, TOTAL);

    let again = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: None,
            payment_id: committed.payment.payment.payment_id,
        })
        .unwrap();
    assert_eq!(again.payment, committed.payment);
    assert_eq!(again.settlement.committed_total.minor(), TOTAL);
}

// ---------------------------------------------------------------- reversal

#[test]
fn reversing_a_committed_card_returns_the_sale_to_payable() {
    let core = scripted_core(SimulatorScript::approving());
    totalized_scripted(&core);
    let committed = card(&core, TOTAL);
    assert_eq!(committed.transaction.status(), TransactionStatus::Committed);

    let reversed = core
        .reverse_payment(ReversePaymentCommand {
            request_id: None,
            payment_id: committed.payment.payment.payment_id,
        })
        .unwrap();

    assert_eq!(reversed.payment.payment.status, PaymentStatus::Reversed);
    assert_eq!(reversed.payment.payment.committed_amount.minor(), 0);
    // History is not erased: the attempt records that money moved and was
    // undone.
    assert_eq!(reversed.payment.attempts[0].status, AttemptStatus::Reversed);
    assert_eq!(reversed.settlement.remaining_balance.minor(), TOTAL);
    assert!(!reversed.settlement.blocks_basket);
    assert_eq!(reversed.transaction.status(), TransactionStatus::Totalized);

    // And another tender can now settle it.
    let paid = cash(&core, TOTAL);
    assert!(paid.settlement.can_finalize());
}

#[test]
fn an_unconfirmed_reversal_stays_pending_and_blocks_finalization() {
    let core = scripted_core(SimulatorScript::approving().with_reversal_confirmed(false));
    totalized_scripted(&core);
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
    assert!(pending.settlement.requires_resolution);
    assert!(!pending.settlement.can_finalize());
    assert_eq!(
        core.finalize_transaction(FinalizeTransactionCommand::default())
            .unwrap_err()
            .kind(),
        "INVALID_TRANSACTION_STATE"
    );
}

#[test]
fn a_failed_payment_cannot_be_reversed() {
    let core = scripted_core(SimulatorScript::approving().defaulting_to(SimulatedOutcome::Decline));
    totalized_scripted(&core);
    let declined = card(&core, TOTAL);
    let error = core
        .reverse_payment(ReversePaymentCommand {
            request_id: None,
            payment_id: declined.payment.payment.payment_id,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "PAYMENT_REVERSAL_NOT_ALLOWED");
}

#[test]
fn a_cash_payment_cannot_be_reversed_through_a_provider() {
    let core = in_memory_core();
    totalized(&core);
    let paid = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: TOTAL,
        })
        .unwrap();
    // Cash has no attempt to reverse: undoing a drawer movement is a physical
    // act, and modelling it as a provider reversal would be a lie.
    let error = core
        .reverse_payment(ReversePaymentCommand {
            request_id: None,
            payment_id: paid.payment.payment.payment_id,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "PAYMENT_REVERSAL_NOT_ALLOWED");
}

// ------------------------------------------------------------- idempotency

#[test]
fn replaying_start_payment_does_not_charge_twice() {
    let core = scripted_core(SimulatorScript::approving());
    let sale = totalized_scripted(&core);
    let request_id = uuid::Uuid::from_u128(0xC0FFEE);

    let first = core
        .start_payment(StartPaymentCommand {
            request_id: Some(request_id),
            tender_type: TenderType::Card,
            amount_minor: 5_000,
        })
        .unwrap();
    let replay = core
        .start_payment(StartPaymentCommand {
            request_id: Some(request_id),
            tender_type: TenderType::Card,
            amount_minor: 5_000,
        })
        .unwrap();

    assert_eq!(
        first.payment.payment.payment_id, replay.payment.payment.payment_id,
        "the same request must resolve the same payment"
    );
    let (payments, settlement) = core.get_transaction_payments(sale.id()).unwrap();
    assert_eq!(payments.len(), 1, "no second payment");
    assert_eq!(payments[0].attempts.len(), 1, "no second attempt");
    assert_eq!(settlement.committed_total.minor(), 5_000);
}

#[test]
fn replaying_reconcile_and_reversal_does_not_duplicate_their_effects() {
    let core = scripted_core(
        SimulatorScript::approving()
            .on_amount(TOTAL, SimulatedOutcome::Unknown)
            .resolving_queries_to(SimulatedOutcome::Approve),
    );
    let sale = totalized_scripted(&core);
    let unknown = card(&core, TOTAL);
    let payment_id = unknown.payment.payment.payment_id;

    let reconcile_id = uuid::Uuid::from_u128(1);
    let first = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: Some(reconcile_id),
            payment_id,
        })
        .unwrap();
    let replay = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: Some(reconcile_id),
            payment_id,
        })
        .unwrap();
    assert_eq!(first.payment, replay.payment);
    assert_eq!(first.settlement.committed_total.minor(), TOTAL);

    let reversal_id = uuid::Uuid::from_u128(2);
    let reversed = core
        .reverse_payment(ReversePaymentCommand {
            request_id: Some(reversal_id),
            payment_id,
        })
        .unwrap();
    let reversed_again = core
        .reverse_payment(ReversePaymentCommand {
            request_id: Some(reversal_id),
            payment_id,
        })
        .unwrap();
    assert_eq!(reversed.payment.payment.status, PaymentStatus::Reversed);
    assert_eq!(reversed_again.payment, reversed.payment);

    // One payment, one attempt, from start to reversal.
    let (payments, _) = core.get_transaction_payments(sale.id()).unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].attempts.len(), 1);
}

// ---------------------------------------------------------- basket freeze

#[test]
fn the_basket_freezes_as_soon_as_a_payment_starts() {
    let core = in_memory_core();
    totalized(&core);
    // Before: the basket is editable and a change re-opens the sale.
    let reopened = core
        .change_quantity(ChangeQuantityCommand {
            request_id: None,
            line_number: 1,
            quantity: Quantity::from_units(2).unwrap(),
        })
        .unwrap();
    assert_eq!(reopened.status(), TransactionStatus::Open);
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    // A partial payment is enough to freeze it.
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: 1_000,
    })
    .unwrap();
    for error in [
        core.add_item(demo_item()).unwrap_err(),
        core.remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 1,
        })
        .unwrap_err(),
        core.change_quantity(ChangeQuantityCommand {
            request_id: None,
            line_number: 1,
            quantity: Quantity::from_units(3).unwrap(),
        })
        .unwrap_err(),
    ] {
        assert_eq!(error.kind(), "BASKET_LOCKED_BY_PAYMENT");
    }
}

#[test]
fn cannot_pay_a_sale_whose_totals_are_not_official() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    let error = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: TOTAL,
        })
        .unwrap_err();
    assert!(matches!(
        error,
        AppError::Domain(DomainError::InvalidTransition { .. })
    ));
}

#[test]
fn nothing_can_be_tendered_once_the_balance_is_covered() {
    let core = in_memory_core();
    totalized(&core);
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: TOTAL,
    })
    .unwrap();
    // A covered sale is `COMMITTED`, and a committed sale takes no further
    // tenders: the lifecycle refuses before the amounts are even considered.
    let error = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: TenderType::Cash,
            amount_minor: 100,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "INVALID_TRANSACTION_STATE");
    assert!(matches!(
        error,
        AppError::Domain(DomainError::InvalidTransition { .. })
    ));
}

// ---------------------------------------------------------------- journal

#[test]
fn every_payment_transition_is_journalled_exactly_once() {
    let core = scripted_core(
        SimulatorScript::approving()
            .on_amount(TOTAL, SimulatedOutcome::Unknown)
            .resolving_queries_to(SimulatedOutcome::Approve),
    );
    totalized_scripted(&core);
    let unknown = card(&core, TOTAL);
    let payment_id = unknown.payment.payment.payment_id;
    core.reconcile_payment(ReconcilePaymentCommand {
        request_id: None,
        payment_id,
    })
    .unwrap();
    core.reverse_payment(ReversePaymentCommand {
        request_id: None,
        payment_id,
    })
    .unwrap();

    let events: Vec<String> = core
        .get_journal(100)
        .unwrap()
        .into_iter()
        .rev()
        .map(|event| event.event_type.as_str().to_owned())
        .filter(|name| name.starts_with("PAYMENT_"))
        .collect();
    assert_eq!(
        events,
        vec![
            "PAYMENT_STARTED",
            "PAYMENT_UNKNOWN",
            "PAYMENT_COMMITTED",
            "PAYMENT_REVERSAL_REQUESTED",
            "PAYMENT_REVERSED",
        ]
    );

    // No secret, token or card data anywhere in the payloads.
    let payloads = serde_json::to_string(
        &core
            .get_journal(100)
            .unwrap()
            .into_iter()
            .map(|event| event.payload_json)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .to_lowercase();
    for forbidden in ["pan", "cvv", "token", "secret", "track"] {
        assert!(
            !payloads.contains(forbidden),
            "`{forbidden}` must never reach the journal"
        );
    }
}

#[test]
fn a_declined_payment_is_journalled_as_failed() {
    let core = scripted_core(SimulatorScript::approving().defaulting_to(SimulatedOutcome::Decline));
    totalized_scripted(&core);
    card(&core, TOTAL);
    let events: Vec<String> = core
        .get_journal(100)
        .unwrap()
        .into_iter()
        .rev()
        .map(|event| event.event_type.as_str().to_owned())
        .filter(|name| name.starts_with("PAYMENT_"))
        .collect();
    assert_eq!(events, vec!["PAYMENT_STARTED", "PAYMENT_FAILED"]);
}

// ------------------------------------------------- the rest stays unchanged

#[test]
fn promotions_pricing_and_snapshots_are_untouched_by_a_payment() {
    let core = in_memory_core();
    open_register(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let before_lines = totalized.state().lines.clone();
    let before_totals = totalized.state().totals.clone();
    let before_promotions = totalized.applied_promotions().to_vec();
    let before_pricing = totalized.pricing();

    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: 20_000,
    })
    .unwrap();
    let after = core.get_active_transaction().unwrap().unwrap();

    // Paying is not a pricing event: lines, totals, promotions and the
    // captured snapshot are all exactly as they were.
    assert_eq!(after.state().lines, before_lines);
    assert_eq!(after.state().totals, before_totals);
    assert_eq!(after.applied_promotions(), before_promotions.as_slice());
    assert_eq!(after.pricing(), before_pricing);
}
