//! The inventory ledger survives a process death.
//!
//! Each case moves stock, throws the application object away, builds a brand
//! new one against the same database, and checks that the position is exactly
//! what was written — never half of it, never twice.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_crash_recovery_tests::*;
use edge_domain::prelude::*;
use edge_inventory::MovementType;

const SKU: &str = "SKU-001";

#[test]
fn a_position_is_exact_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        let core = core_at(&db, 1);
        open_register(&core);
        bootstrap_stock(&core, &[(SKU, 10)]);
        core.inventory_out(InventoryOutCommand {
            request_id: None,
            product_ref: ProductRef::new(SKU).unwrap(),
            quantity: Quantity::from_thousandths(2_500).unwrap(),
            reason_code: "DAMAGE".to_owned(),
        })
        .unwrap();
        cash_sale(&core, 11_600);
    }

    // A new application object, same durable ledger. Nothing is cached in a
    // process, so the position is the fold over what is on disk.
    let core = core_at(&db, 1_000);
    assert_eq!(on_hand(&core, SKU), 6_500);
    let types: Vec<MovementType> = movements(&core, SKU)
        .iter()
        .map(|movement| movement.movement_type)
        .collect();
    assert_eq!(
        types,
        vec![
            MovementType::OpeningBalance,
            MovementType::ManualOut,
            MovementType::Sale
        ]
    );
}

#[test]
fn a_crash_before_commit_leaves_the_opening_position_unwritten() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let core = core_at(&db, 1);
    open_register(&core);

    core.store().arm_commit_failure();
    let result = core.import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", &[(SKU, 10)]));
    assert!(result.is_err(), "the import should have failed");

    // Not one movement, and no bootstrap fact either: a store that is half
    // bootstrapped cannot be told apart from one that is not, so it must not
    // exist (ADR 028).
    assert_eq!(on_hand(&core, SKU), 0);
    assert!(movements(&core, SKU).is_empty());
    assert!(!journal_types(&core).contains(&"INVENTORY_BOOTSTRAPPED".to_owned()));

    // Sending the same file again after recovery works, because nothing
    // claimed it had already been applied.
    let state = core
        .import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", &[(SKU, 10)]))
        .unwrap();
    assert_eq!(state.item_count, 1);
    assert_eq!(on_hand(&core, SKU), 10_000);
}

#[test]
fn a_crash_before_commit_leaves_a_manual_movement_unwritten() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let core = core_at(&db, 1);
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    core.store().arm_commit_failure();
    let request_id = uuid::Uuid::from_u128(0xBEEF);
    let command = || InventoryOutCommand {
        request_id: Some(request_id),
        product_ref: ProductRef::new(SKU).unwrap(),
        quantity: Quantity::from_units(1).unwrap(),
        reason_code: "DAMAGE".to_owned(),
    };
    assert!(core.inventory_out(command()).is_err());

    // The movement, its journal event and the idempotency record went down
    // together: the command never happened.
    assert_eq!(on_hand(&core, SKU), 10_000);
    assert_eq!(movements(&core, SKU).len(), 1);

    // So the operator's retry is a first attempt, not a refused replay.
    let view = core.inventory_out(command()).unwrap();
    assert_eq!(view.position.on_hand_thousandths, 9_000);
    assert_eq!(movements(&core, SKU).len(), 2);
}

#[test]
fn a_sale_and_its_stock_movements_commit_together_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let core = core_at(&db, 1);
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let committed = pay_cash(&core, 11_600);
    assert_eq!(committed.status(), TransactionStatus::Committed);

    core.store().arm_commit_failure();
    assert!(core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .is_err());

    // The sale is still unfinalized and the shelf is untouched. There is no
    // state in which stock moved for a sale that never closed.
    let state = core.get_edge_state().unwrap();
    assert_eq!(
        state.active_transaction.unwrap().status(),
        TransactionStatus::Committed
    );
    assert_eq!(on_hand(&core, SKU), 10_000);
    assert_eq!(movements(&core, SKU).len(), 1);

    // Retrying finalizes the sale and moves the stock exactly once.
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(on_hand(&core, SKU), 9_000);
    assert_eq!(movements(&core, SKU).len(), 2);
}

#[test]
fn a_replayed_finalization_across_a_restart_moves_stock_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    let request_id = uuid::Uuid::from_u128(0xF00D);
    {
        let core = core_at(&db, 1);
        open_register(&core);
        bootstrap_stock(&core, &[(SKU, 10)]);
        core.start_transaction(StartTransactionCommand::default())
            .unwrap();
        core.add_item(demo_item()).unwrap();
        core.totalize_transaction(TotalizeTransactionCommand::default())
            .unwrap();
        pay_cash(&core, 11_600);
        core.finalize_transaction(FinalizeTransactionCommand {
            request_id: Some(request_id),
        })
        .unwrap();
        assert_eq!(on_hand(&core, SKU), 9_000);
    }

    // A client that lost the answer retries after the restart. Two guards
    // agree: the command is already recorded, and the line's cause has
    // already moved this product.
    let core = core_at(&db, 1_000);
    core.finalize_transaction(FinalizeTransactionCommand {
        request_id: Some(request_id),
    })
    .unwrap();

    assert_eq!(on_hand(&core, SKU), 9_000);
    assert_eq!(movements(&core, SKU).len(), 2);
}

#[test]
fn the_negative_inventory_policy_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pi-edge.db");
    {
        let core = core_at(&db, 1);
        core.init_device(InitDeviceCommand {
            tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
            store_id: StoreId::new("STORE-DEMO").unwrap(),
            currency: Currency::Mxn,
            allow_negative_inventory: false,
        })
        .unwrap();
        edge_application::testing::CatalogFixture::demo(1)
            .activate(&core)
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
    }

    // The policy is part of the installation, not of a running process.
    let core = core_at(&db, 1_000);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let error = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: Tender::Cash,
            amount_minor: 11_600,
        })
        .unwrap_err();
    assert_eq!(error.kind(), "INVENTORY_INSUFFICIENT_STOCK");
}
