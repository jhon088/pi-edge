//! Deterministic protocol output for the inventory ledger.
//!
//! Frozen clock and sequential ids, so the position a client sees — including
//! a fractional balance, a negative one and a sale that moved more units than
//! it billed — is byte-stable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_domain::prelude::*;
use edge_golden_tests::*;
use edge_protocol::{InventoryBootstrapV1, InventoryPositionV1};

/// The opening position itself: the fact a store was provisioned.
#[test]
fn golden_inventory_bootstrap() {
    let core = golden_core();
    open_register_with_float(&core, 100_000);
    let state = golden_bootstrap(&core, &[("SKU-16", 10_000), ("SKU-8", 4_500)]);

    assert_eq!(state.item_count, 2);
    assert_golden(
        "inventory_bootstrap",
        &serde_json::to_value(InventoryBootstrapV1::from(&state)).unwrap(),
    );
}

/// A position built from all four movement types, in one ledger.
#[test]
fn golden_inventory_position() {
    let core = golden_core();
    open_register_with_float(&core, 100_000);
    golden_bootstrap(&core, &[("SKU-16", 10_000)]);
    core.inventory_in(InventoryInCommand {
        request_id: None,
        product_ref: ProductRef::new("SKU-16").unwrap(),
        quantity: Quantity::from_thousandths(2_500).unwrap(),
        reason_code: "RECEIPT".to_owned(),
    })
    .unwrap();
    core.inventory_out(InventoryOutCommand {
        request_id: None,
        product_ref: ProductRef::new("SKU-16").unwrap(),
        quantity: Quantity::from_thousandths(500).unwrap(),
        reason_code: "DAMAGE".to_owned(),
    })
    .unwrap();
    golden_cash_sale(&core, 20_000);

    // 10.000 + 2.500 − 0.500 − 1.000 = 11.000
    let view = golden_position(&core, "SKU-16");
    assert_eq!(view.position.on_hand_thousandths, 11_000);
    assert_eq!(view.movements.len(), 4);
    assert_golden(
        "inventory_position",
        &serde_json::to_value(InventoryPositionV1::from(&view)).unwrap(),
    );
}

/// A fractional balance, the case a float would ruin: 0.750 kg left of 1.000
/// after 0.250 went out.
#[test]
fn golden_inventory_fractional_position() {
    let core = golden_core();
    open_register_with_float(&core, 100_000);
    golden_bootstrap(&core, &[("SKU-FRACTIONAL", 1_000)]);
    core.inventory_out(InventoryOutCommand {
        request_id: None,
        product_ref: ProductRef::new("SKU-FRACTIONAL").unwrap(),
        quantity: Quantity::from_thousandths(250).unwrap(),
        reason_code: "SAMPLE".to_owned(),
    })
    .unwrap();

    let view = golden_position(&core, "SKU-FRACTIONAL");
    assert_eq!(view.position.on_hand_thousandths, 750);
    assert_eq!(view.on_hand_display, "0.750");
    assert_golden(
        "inventory_fractional_position",
        &serde_json::to_value(InventoryPositionV1::from(&view)).unwrap(),
    );
}

/// A position below zero, rendered rather than clamped (ADR 029).
#[test]
fn golden_inventory_negative_position() {
    let core = golden_core();
    open_register_with_float(&core, 100_000);
    golden_bootstrap(&core, &[("SKU-FRACTIONAL", 1_000)]);
    core.inventory_out(InventoryOutCommand {
        request_id: None,
        product_ref: ProductRef::new("SKU-FRACTIONAL").unwrap(),
        quantity: Quantity::from_thousandths(1_750).unwrap(),
        reason_code: "SHRINKAGE".to_owned(),
    })
    .unwrap();

    let view = golden_position(&core, "SKU-FRACTIONAL");
    assert_eq!(view.position.on_hand_thousandths, -750);
    assert_eq!(view.on_hand_display, "-0.750");
    assert_golden(
        "inventory_negative_position",
        &serde_json::to_value(InventoryPositionV1::from(&view)).unwrap(),
    );
}

/// Buy two get one, pinned where it matters: the discount is on the money and
/// three units still leave the shelf (ADR 027).
#[test]
fn golden_inventory_promotional_sale() {
    let core = golden_core();
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory: true,
    })
    .unwrap();
    promotional_catalog(1).activate(&core).unwrap();
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new("REG-001").unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
    golden_bootstrap(&core, &[("SKU-8", 10_000)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    scan(&core, BARCODE_8, 3_000);
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert!(
        !totalized.applied_promotions().is_empty(),
        "the buy-2-get-1 must apply, or this case proves nothing"
    );
    pay_cash(&core, 30_000);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // The customer was billed for two units and three left the store.
    let view = golden_position(&core, "SKU-8");
    assert_eq!(view.position.on_hand_thousandths, 7_000);
    assert_golden(
        "inventory_promotional_sale",
        &serde_json::to_value(InventoryPositionV1::from(&view)).unwrap(),
    );
}
