//! Deterministic protocol output for commercial refunds.
//!
//! Frozen clock and sequential ids, so the refund a client sees — including a
//! promotional partial, a mixed-tender spread, an unresolved card refund and
//! an invoiced sale's pending credit note — is byte-stable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_golden_tests::*;
use edge_payments::{SimulatedOutcome, SimulatorScript, TenderType};
use edge_protocol::{RefundEligibilityV1, RefundV1};
use edge_refunds::RefundFiscalStatus;

/// What a sale can still give back, before anything is refunded.
#[test]
fn golden_refund_eligibility() {
    let core = golden_core();
    open_register_with_float(&core, 100_000);
    let sale = golden_finalized_sale(&core, BARCODE_16, 3_000, 40_000);

    let view = core.get_refund_eligibility(sale).unwrap();
    assert_eq!(view.refundable_total.minor(), 34_800);
    assert_golden(
        "refund_eligibility",
        &serde_json::to_value(RefundEligibilityV1::from(&view)).unwrap(),
    );
}

/// A whole sale given back in cash.
#[test]
fn golden_refund_full_cash() {
    let core = golden_core();
    open_register_with_float(&core, 100_000);
    let sale = golden_finalized_sale(&core, BARCODE_16, 1_000, 20_000);

    let view = golden_refund(
        &core,
        sale,
        refund_lines(&[(1, 1_000)]),
        RefundFiscalStatus::None,
    );
    assert_eq!(view.refund.total.gross.minor(), 11_600);
    assert_golden(
        "refund_full_cash",
        &serde_json::to_value(RefundV1::from(&view)).unwrap(),
    );
}

/// One unit of a buy-2-get-1 line: the case where naive arithmetic leaks money.
#[test]
fn golden_refund_promotional_partial() {
    let core = golden_core();
    core.init_device(InitDeviceCommand {
        tenant_id: edge_domain::prelude::TenantId::new("TENANT-DEMO").unwrap(),
        store_id: edge_domain::prelude::StoreId::new("STORE-DEMO").unwrap(),
        currency: edge_domain::prelude::Currency::Mxn,
        allow_negative_inventory: true,
    })
    .unwrap();
    promotional_catalog(1).activate(&core).unwrap();
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: edge_domain::prelude::RegisterId::new("REG-001").unwrap(),
        operator_id: edge_domain::prelude::OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();

    // Buy 2 get 1 on the 8% product: three units, two charged.
    let sale = golden_finalized_sale(&core, BARCODE_8, 3_000, 30_000);
    let view = golden_refund(
        &core,
        sale,
        refund_lines(&[(1, 1_000)]),
        RefundFiscalStatus::None,
    );

    // Never the shelf price: the unit gives back what it cost.
    assert!(view.refund.total.gross.minor() < 10_800);
    assert_golden(
        "refund_promotional_partial",
        &serde_json::to_value(RefundV1::from(&view)).unwrap(),
    );
}

/// A refund spread over the two tenders that paid the sale.
#[test]
fn golden_refund_mixed_tender() {
    let core = scripted_golden_core(SimulatorScript::approving());
    open_scripted_register(&core, &golden_catalog(1));

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scripted_scan(&core, BARCODE_16, 10_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let due = totalized.state().totals.grand_total.minor();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Cash,
        amount_minor: 30_000,
    })
    .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: due - 30_000,
    })
    .unwrap();
    let sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id();

    let refund = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 5_000)]),
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action: RefundFiscalStatus::None,
        })
        .unwrap();
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();
    let view = core
        .start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id: refund.refund.refund_id,
        })
        .unwrap();

    assert_eq!(view.payments.len(), 2);
    let spread: i64 = view
        .payments
        .iter()
        .map(|payment| payment.payment.requested_amount.minor())
        .sum();
    assert_eq!(spread, view.refund.total.gross.minor());
    assert_golden(
        "refund_mixed_tender",
        &serde_json::to_value(RefundV1::from(&view)).unwrap(),
    );
}

/// A card refund whose answer was lost: unresolved, never failed.
#[test]
fn golden_refund_card_unknown() {
    let core =
        scripted_golden_core(SimulatorScript::approving().refunding(SimulatedOutcome::Unknown));
    open_scripted_register(&core, &golden_catalog(1));

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scripted_scan(&core, BARCODE_16, 1_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: TenderType::Card,
        amount_minor: totalized.state().totals.grand_total.minor(),
    })
    .unwrap();
    let sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap()
        .id();

    let refund = core
        .create_refund(CreateRefundCommand {
            request_id: None,
            authorization: central_authority(sale),
            lines: refund_lines(&[(1, 1_000)]),
            reason_code: "CUSTOMER_RETURN".to_owned(),
            fiscal_action: RefundFiscalStatus::None,
        })
        .unwrap();
    core.accept_refund(AcceptRefundCommand {
        request_id: None,
        refund_id: refund.refund.refund_id,
    })
    .unwrap();
    let view = core
        .start_refund_payment(StartRefundPaymentCommand {
            request_id: None,
            refund_id: refund.refund.refund_id,
        })
        .unwrap();

    // The goods are back and the money is in doubt: two facts, recorded
    // separately (ADR 033).
    assert!(view.requires_reconciliation);
    assert!(view.inventory_returned);
    assert_golden(
        "refund_card_unknown",
        &serde_json::to_value(RefundV1::from(&view)).unwrap(),
    );
}

/// An invoiced sale refunded with its credit note still owed.
#[test]
fn golden_refund_invoiced_credit_note_pending() {
    let core = golden_core();
    open_register_with_float(&core, 100_000);
    let sale = golden_finalized_sale(&core, BARCODE_16, 1_000, 20_000);
    core.mark_transaction_invoiced(MarkTransactionInvoicedCommand {
        request_id: None,
        transaction_id: sale,
        invoice_ref: "INV-GOLDEN-1".to_owned(),
    })
    .unwrap();

    let view = golden_refund(
        &core,
        sale,
        refund_lines(&[(1, 1_000)]),
        RefundFiscalStatus::CreditNotePending,
    );

    // Commercially finished, fiscally owed. No XML, no PAC, no stamp.
    assert_eq!(
        view.refund.fiscal_status,
        RefundFiscalStatus::CreditNotePending
    );
    assert_golden(
        "refund_invoiced_credit_note_pending",
        &serde_json::to_value(RefundV1::from(&view)).unwrap(),
    );
}
