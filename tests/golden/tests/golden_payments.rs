//! Deterministic protocol output for paid sales.
//!
//! Frozen clock, sequential ids and a scripted provider, so the money a client
//! sees — including an unresolved charge and a reversal — is byte-stable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::PaymentOutcome;
use edge_golden_tests::*;
use edge_payments::{SimulatedOutcome, SimulatorScript, TenderType};
use edge_protocol::{PaymentV1, SettlementV1, TransactionPaymentsV1};

/// The 16% demo line: 100.00 + 16.00 = 116.00.
const TOTAL: i64 = 11_600;

fn start(core: &ScriptedGoldenCore, tender: TenderType, amount_minor: i64) -> PaymentOutcome {
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: tender,
        amount_minor,
    })
    .unwrap()
}

/// Everything a client is told about one sale's payments.
fn payments_dto(core: &ScriptedGoldenCore, outcome: &PaymentOutcome) -> serde_json::Value {
    let transaction_id = outcome.transaction.id();
    let (payments, settlement) = core.get_transaction_payments(transaction_id).unwrap();
    let dto = TransactionPaymentsV1::from_domain(transaction_id, &payments, &settlement).unwrap();
    serde_json::to_value(dto).unwrap()
}

fn sale(core: &ScriptedGoldenCore) {
    open_scripted_register(core, &golden_catalog(1));
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scripted_scan(core, BARCODE_16, 1_000);
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
}

/// Exact cash: the simplest possible settlement.
#[test]
fn golden_cash_exact() {
    let core = scripted_golden_core(SimulatorScript::approving());
    sale(&core);
    let outcome = start(&core, TenderType::Cash, TOTAL);

    assert_eq!(outcome.settlement.committed_total.minor(), TOTAL);
    assert_eq!(outcome.settlement.change_due.minor(), 0);
    assert!(outcome.settlement.can_finalize());
    assert_golden("payment_cash_exact", &payments_dto(&core, &outcome));
}

/// Cash overpayment: 200.00 against 116.00. The sale collects 116.00 and owes
/// 84.00 back — the note must not inflate what was collected.
#[test]
fn golden_cash_change() {
    let core = scripted_golden_core(SimulatorScript::approving());
    sale(&core);
    let outcome = start(&core, TenderType::Cash, 20_000);

    assert_eq!(outcome.payment.payment.requested_amount.minor(), 20_000);
    assert_eq!(outcome.settlement.committed_total.minor(), TOTAL);
    assert_eq!(outcome.settlement.change_due.minor(), 8_400);
    assert_golden("payment_cash_change", &payments_dto(&core, &outcome));
}

/// 30.00 cash plus 86.00 card. Two payments, one attempt, nothing owed.
#[test]
fn golden_mixed_tender() {
    let core = scripted_golden_core(SimulatorScript::approving());
    sale(&core);
    start(&core, TenderType::Cash, 3_000);
    let outcome = start(&core, TenderType::Card, 8_600);

    assert_eq!(outcome.settlement.committed_total.minor(), TOTAL);
    assert_eq!(outcome.settlement.remaining_balance.minor(), 0);
    assert!(outcome.settlement.can_finalize());
    assert_golden("payment_mixed_tender", &payments_dto(&core, &outcome));
}

/// A card whose result never came back. Nothing is applied, nothing is
/// assumed, and the sale cannot finalize.
#[test]
fn golden_card_unknown() {
    let core =
        scripted_golden_core(SimulatorScript::approving().defaulting_to(SimulatedOutcome::Unknown));
    sale(&core);
    let outcome = start(&core, TenderType::Card, TOTAL);

    assert_eq!(outcome.settlement.committed_total.minor(), 0);
    assert!(outcome.settlement.requires_reconciliation);
    assert!(!outcome.settlement.can_finalize());
    assert_golden("payment_card_unknown", &payments_dto(&core, &outcome));
}

/// The same charge after reconciliation: the provider had taken it, so the
/// same attempt becomes authorized and the payment commits.
#[test]
fn golden_card_reconciled() {
    let core = scripted_golden_core(
        SimulatorScript::approving()
            .defaulting_to(SimulatedOutcome::Unknown)
            .resolving_queries_to(SimulatedOutcome::Approve),
    );
    sale(&core);
    let unknown = start(&core, TenderType::Card, TOTAL);
    let outcome = core
        .reconcile_payment(ReconcilePaymentCommand {
            request_id: None,
            payment_id: unknown.payment.payment.payment_id,
        })
        .unwrap();

    assert_eq!(outcome.payment.attempts.len(), 1, "no second charge");
    assert_eq!(outcome.settlement.committed_total.minor(), TOTAL);
    assert!(outcome.settlement.can_finalize());
    assert_golden("payment_card_reconciled", &payments_dto(&core, &outcome));
}

/// A reversal: the money is un-applied, the history is kept, and the sale owes
/// its full total again.
#[test]
fn golden_card_reversed() {
    let core = scripted_golden_core(SimulatorScript::approving());
    sale(&core);
    let committed = start(&core, TenderType::Card, TOTAL);
    let outcome = core
        .reverse_payment(ReversePaymentCommand {
            request_id: None,
            payment_id: committed.payment.payment.payment_id,
        })
        .unwrap();

    assert_eq!(outcome.settlement.committed_total.minor(), 0);
    assert_eq!(outcome.settlement.remaining_balance.minor(), TOTAL);
    assert!(!outcome.settlement.blocks_basket);
    assert_golden("payment_card_reversed", &payments_dto(&core, &outcome));
}

/// The settlement block a UI renders, on its own, for a half-paid sale.
#[test]
fn golden_partial_settlement() {
    let core = scripted_golden_core(SimulatorScript::approving());
    sale(&core);
    let outcome = start(&core, TenderType::Cash, 5_000);

    assert_eq!(outcome.settlement.remaining_balance.minor(), 6_600);
    assert_golden(
        "payment_partial_settlement",
        &serde_json::to_value(SettlementV1::from(&outcome.settlement)).unwrap(),
    );
    // And the payment on its own, so the shape a receipt uses is pinned too.
    assert_golden(
        "payment_partial_cash",
        &serde_json::to_value(PaymentV1::from_domain(&outcome.payment).unwrap()).unwrap(),
    );
}
