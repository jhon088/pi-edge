//! The inventory ledger against real SQLite.
//!
//! The fold and the movement invariants are pinned by unit tests in
//! `edge-inventory`; these check what only the whole runtime shows: that stock
//! moves when a sale finalizes and at no other moment, that the physical
//! quantity moves rather than the billed one, that a replay writes nothing
//! twice, that the ledger cannot be edited, and that two products and two
//! stores never contaminate each other.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::testing::{CatalogFixture, FixedClock, FixtureProduct, SequentialIdFactory};
use edge_catalog::promotions::{
    NewPromotion, ProductApplicability, Promotion, PromotionRule, Stackability,
};
use edge_catalog::scope::CatalogScope;
use edge_domain::prelude::*;
use edge_integration_tests::*;
use edge_inventory::{Direction, MovementType};

/// The demo product, at 100.00 + 16%.
const SKU: &str = "SKU-001";
const SECOND_SKU: &str = "SKU-002";

fn manual_in(core: &TestCore, product_ref: &str, thousandths: i64, reason: &str) {
    core.inventory_in(InventoryInCommand {
        request_id: None,
        product_ref: ProductRef::new(product_ref).unwrap(),
        quantity: Quantity::from_thousandths(thousandths).unwrap(),
        reason_code: reason.to_owned(),
    })
    .unwrap();
}

fn manual_out(core: &TestCore, product_ref: &str, thousandths: i64, reason: &str) {
    core.inventory_out(InventoryOutCommand {
        request_id: None,
        product_ref: ProductRef::new(product_ref).unwrap(),
        quantity: Quantity::from_thousandths(thousandths).unwrap(),
        reason_code: reason.to_owned(),
    })
    .unwrap();
}

/// Buy two, get one: the discount is monetary, the stock movement is not.
fn buy_two_get_one() -> Promotion {
    Promotion::import(NewPromotion {
        promotion_id: PromotionId::new("BOGO").unwrap(),
        code: "BOGO".to_owned(),
        name: "Lleva 3, paga 2".to_owned(),
        active: true,
        priority: 0,
        stackability: Stackability::Stackable,
        scope: CatalogScope::Tenant,
        valid_from: None,
        valid_until: None,
        rule: PromotionRule::BuyNGetM {
            applies_to: ProductApplicability::Product {
                product_ref: ProductRef::new(SKU).unwrap(),
            },
            buy: Quantity::from_units(2).unwrap(),
            get: Quantity::from_units(1).unwrap(),
        },
    })
    .unwrap()
}

/// A catalog with a second product, so independence can be shown.
fn two_product_catalog(version: i64) -> CatalogFixture {
    CatalogFixture::demo(version).with(
        FixtureProduct::new(SECOND_SKU, "Segundo producto", 5_000)
            .sku(SECOND_SKU)
            .barcode("7501000000002"),
    )
}

// --------------------------------------------------------------- the bootstrap

#[test]
fn a_store_starts_with_no_position_at_all() {
    let core = in_memory_core();
    open_register(&core);

    // Not zero-as-a-guess: zero because the ledger is empty, and the view says
    // so by carrying no movements and no last movement.
    let view = position(&core, SKU);
    assert_eq!(view.position.on_hand_thousandths, 0);
    assert_eq!(view.position.movement_count, 0);
    assert!(view.position.last_movement_at.is_none());
    assert_eq!(view.on_hand_display, "0.000");
}

#[test]
fn the_opening_position_becomes_one_movement_per_product() {
    let core = in_memory_core();
    init_device(&core);
    two_product_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);

    let state = bootstrap_stock(&core, &[(SKU, 10), (SECOND_SKU, 4)]);
    assert_eq!(state.item_count, 2);
    assert_eq!(state.source_snapshot_id, "WMS-SNAPSHOT-1");

    assert_eq!(on_hand(&core, SKU), 10_000);
    assert_eq!(on_hand(&core, SECOND_SKU), 4_000);

    let view = position(&core, SKU);
    assert_eq!(view.movements.len(), 1);
    assert_eq!(
        view.movements[0].movement_type,
        MovementType::OpeningBalance
    );
    assert_eq!(view.movements[0].direction, Direction::In);
    assert_eq!(view.movements[0].source_ref, "WMS-SNAPSHOT-1");
    assert!(view.movements[0].reason_code.is_none());
}

#[test]
fn replaying_the_same_opening_position_changes_nothing() {
    let core = in_memory_core();
    open_register(&core);
    let first = bootstrap_stock(&core, &[(SKU, 10)]);

    let again = core
        .import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", &[(SKU, 999)]))
        .unwrap();

    // The recorded fact is the first one, and the ledger did not grow. A
    // replay is a repeat of the same provisioning, not a new one.
    assert_eq!(again.bootstrapped_at, first.bootstrapped_at);
    assert_eq!(again.item_count, 1);
    assert_eq!(on_hand(&core, SKU), 10_000);
    assert_eq!(position(&core, SKU).movements.len(), 1);
}

#[test]
fn a_different_opening_position_is_refused_rather_than_guessed_at() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    let error = core
        .import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-2", &[(SKU, 3)]))
        .unwrap_err();

    // Reconciling two positions is a decision nobody has made yet, and a
    // half-applied reconciliation is worse than none (ADR 028).
    assert_eq!(error.kind(), "INVENTORY_ALREADY_BOOTSTRAPPED");
    assert_eq!(on_hand(&core, SKU), 10_000);
}

#[test]
fn an_opening_position_is_all_or_nothing() {
    let core = in_memory_core();
    open_register(&core);

    let error = core
        .import_inventory_bootstrap(bootstrap_command(
            "WMS-SNAPSHOT-1",
            &[(SKU, 10), ("SKU-GHOST", 5)],
        ))
        .unwrap_err();

    assert_eq!(error.kind(), "INVENTORY_UNKNOWN_PRODUCT");
    // The product that *was* valid did not get in either, and the store is
    // still unbootstrapped, so a corrected file can be sent.
    assert_eq!(on_hand(&core, SKU), 0);
    let retry = core
        .import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", &[(SKU, 10)]))
        .unwrap();
    assert_eq!(retry.item_count, 1);
    assert_eq!(on_hand(&core, SKU), 10_000);
}

#[test]
fn an_opening_position_must_name_each_product_once() {
    let core = in_memory_core();
    open_register(&core);

    let error = core
        .import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", &[(SKU, 10), (SKU, 4)]))
        .unwrap_err();

    // 10 or 4 or 14 are three different answers and the core picks none.
    assert_eq!(error.kind(), "INVENTORY_INVALID_BOOTSTRAP");
    assert_eq!(on_hand(&core, SKU), 0);
}

#[test]
fn an_empty_opening_position_is_refused() {
    let core = in_memory_core();
    open_register(&core);

    let error = core
        .import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", &[]))
        .unwrap_err();

    assert_eq!(error.kind(), "INVENTORY_INVALID_BOOTSTRAP");
}

#[test]
fn stock_cannot_move_for_a_product_the_catalog_does_not_know() {
    let core = in_memory_core();
    open_register(&core);

    let error = core
        .inventory_in(InventoryInCommand {
            request_id: None,
            product_ref: ProductRef::new("SKU-GHOST").unwrap(),
            quantity: Quantity::from_units(1).unwrap(),
            reason_code: "RECOUNT".to_owned(),
        })
        .unwrap_err();

    // PI.Edge never invents product master data, not even by moving stock for
    // a reference nobody sent it (ADR 011).
    assert_eq!(error.kind(), "INVENTORY_UNKNOWN_PRODUCT");
}

#[test]
fn stock_cannot_move_without_an_active_catalog() {
    let core = in_memory_core();
    init_device(&core);

    let error = core
        .import_inventory_bootstrap(bootstrap_command("WMS-SNAPSHOT-1", &[(SKU, 1)]))
        .unwrap_err();

    assert_eq!(error.kind(), "INVENTORY_CATALOG_REQUIRED");
}

// ------------------------------------------------------------ manual movements

#[test]
fn a_manual_movement_states_its_direction_and_its_reason() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    manual_in(&core, SKU, 2_500, "RECEIPT");
    manual_out(&core, SKU, 500, "DAMAGE");

    assert_eq!(on_hand(&core, SKU), 12_000);
    let view = position(&core, SKU);
    assert_eq!(view.movements.len(), 3);
    assert_eq!(view.movements[1].movement_type, MovementType::ManualIn);
    assert_eq!(view.movements[1].direction, Direction::In);
    assert_eq!(view.movements[1].reason_code.as_deref(), Some("RECEIPT"));
    assert_eq!(view.movements[2].movement_type, MovementType::ManualOut);
    assert_eq!(view.movements[2].direction, Direction::Out);
    assert_eq!(view.movements[2].reason_code.as_deref(), Some("DAMAGE"));
    assert_eq!(view.on_hand_display, "12.000");
}

#[test]
fn a_manual_movement_without_a_reason_is_refused() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    let error = core
        .inventory_out(InventoryOutCommand {
            request_id: None,
            product_ref: ProductRef::new(SKU).unwrap(),
            quantity: Quantity::from_units(1).unwrap(),
            reason_code: "   ".to_owned(),
        })
        .unwrap_err();

    // Stock that moved for no stated reason is not an audit trail.
    assert_eq!(error.kind(), "INVENTORY_REASON_REQUIRED");
    assert_eq!(on_hand(&core, SKU), 10_000);
}

#[test]
fn a_replayed_manual_movement_moves_stock_once() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    let request_id = uuid::Uuid::new_v4();
    let command = || InventoryOutCommand {
        request_id: Some(request_id),
        product_ref: ProductRef::new(SKU).unwrap(),
        quantity: Quantity::from_units(1).unwrap(),
        reason_code: "DAMAGE".to_owned(),
    };
    let first = core.inventory_out(command()).unwrap();
    let replay = core.inventory_out(command()).unwrap();

    assert_eq!(first.position.on_hand_thousandths, 9_000);
    assert_eq!(replay.position.on_hand_thousandths, 9_000);
    assert_eq!(position(&core, SKU).movements.len(), 2);
}

#[test]
fn a_fractional_quantity_survives_the_round_trip() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 1)]);

    manual_out(&core, SKU, 250, "SAMPLE");

    // 1.000 − 0.250 = 0.750, in thousandths throughout. No float ever touches
    // a stock figure, for the same reason none touches a price.
    assert_eq!(on_hand(&core, SKU), 750);
    assert_eq!(position(&core, SKU).on_hand_display, "0.750");
}

#[test]
fn a_position_below_zero_is_reported_as_it_is() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 1)]);

    manual_out(&core, SKU, 1_750, "SHRINKAGE");

    // −0.750, not clamped to zero: selling or losing what the system did not
    // know it had is a reportable condition (ADR 029).
    let view = position(&core, SKU);
    assert_eq!(view.position.on_hand_thousandths, -750);
    assert!(view.position.is_negative());
    assert_eq!(view.on_hand_display, "-0.750");
}

// ------------------------------------------------------------------ the sale

#[test]
fn stock_moves_when_the_sale_finalizes_and_not_before() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    assert_eq!(on_hand(&core, SKU), 10_000, "a basket is not a commitment");

    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(on_hand(&core, SKU), 10_000, "a total is not a sale");

    pay_cash(&core, 11_600);
    assert_eq!(
        on_hand(&core, SKU),
        10_000,
        "money has moved but the sale is not finished"
    );

    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(on_hand(&core, SKU), 9_000);
}

#[test]
fn a_finalized_sale_writes_one_movement_per_line_naming_its_line() {
    let core = in_memory_core();
    init_device(&core);
    two_product_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    bootstrap_stock(&core, &[(SKU, 10), (SECOND_SKU, 10)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 2)).unwrap();
    core.add_item(item(SECOND_SKU, 3)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 100_000);
    let sale = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    assert_eq!(on_hand(&core, SKU), 8_000);
    assert_eq!(on_hand(&core, SECOND_SKU), 7_000);

    let movement = &position(&core, SKU).movements[1];
    assert_eq!(movement.movement_type, MovementType::Sale);
    assert_eq!(movement.direction, Direction::Out);
    assert_eq!(movement.transaction_id, Some(sale.id()));
    assert_eq!(movement.line_number, Some(1));
    assert_eq!(movement.source_ref, format!("{}:1", sale.id()));
    // Operational context travels with the movement, so a later reconciliation
    // knows which day and which session it belongs to.
    assert!(movement.business_day_id.is_some());
    assert!(movement.session_id.is_some());
    assert!(movement.reason_code.is_none());
}

#[test]
fn a_promotion_moves_the_physical_quantity_not_the_billed_one() {
    let core = in_memory_core();
    init_device(&core);
    CatalogFixture::demo(1)
        .promoting(buy_two_get_one())
        .activate(&core)
        .unwrap();
    open_day_and_session(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    let basket = core.add_item(item(SKU, 3)).unwrap();
    assert!(
        !basket.applied_promotions().is_empty(),
        "the promotion must actually apply, or this test proves nothing"
    );
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 50_000);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // The customer paid for two and three units left the shelf. Inventory
    // follows the goods, not the invoice (ADR 027).
    assert_eq!(on_hand(&core, SKU), 7_000);
    let movement = &position(&core, SKU).movements[1];
    assert_eq!(movement.quantity, Quantity::from_units(3).unwrap());
}

#[test]
fn two_products_keep_separate_ledgers() {
    let core = in_memory_core();
    init_device(&core);
    two_product_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    bootstrap_stock(&core, &[(SKU, 10), (SECOND_SKU, 10)]);

    manual_out(&core, SKU, 4_000, "DAMAGE");

    assert_eq!(on_hand(&core, SKU), 6_000);
    assert_eq!(on_hand(&core, SECOND_SKU), 10_000);
    assert_eq!(position(&core, SECOND_SKU).movements.len(), 1);
}

#[test]
fn two_stores_keep_separate_ledgers() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let store_a =
        core_with_store(edge_storage::SqliteEdgeStore::open(first.path().join("a.db")).unwrap());
    let store_b =
        core_with_store(edge_storage::SqliteEdgeStore::open(second.path().join("b.db")).unwrap());

    for (core, store_id) in [(&store_a, "STORE-A"), (&store_b, "STORE-B")] {
        core.init_device(InitDeviceCommand {
            tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
            store_id: StoreId::new(store_id).unwrap(),
            currency: Currency::Mxn,
            allow_negative_inventory: true,
        })
        .unwrap();
        activate_demo_catalog(core);
        open_day_and_session(core);
        bootstrap_stock(core, &[(SKU, 10)]);
    }

    manual_out(&store_a, SKU, 10_000, "TRANSFER_OUT");

    // Each installation is the durable record of the movements that happened
    // *there*; a position is never a tenant-wide figure. The grain is the
    // installation, not the store id: two POS of one store hold two databases
    // and would be just as independent as these two (ADR 026).
    assert_eq!(on_hand(&store_a, SKU), 0);
    assert_eq!(on_hand(&store_b, SKU), 10_000);
    assert_eq!(
        position(&store_a, SKU).position.store_id.as_str(),
        "STORE-A"
    );
    assert_eq!(
        position(&store_b, SKU).position.store_id.as_str(),
        "STORE-B"
    );
}

/// Two POS of **one** store, as PI.Edge is actually deployed: one installation
/// per register, each with its own database and runtime.
///
/// The point is not to model the store's real stock — neither figure below is
/// it. The point is that a projection's scope is the *installation*, even when
/// `store_id` is identical, which is why PI.Edge may not present `on_hand` as
/// a store-wide count while several POS run without coordination (ADR 026).
#[test]
fn two_installations_of_the_same_store_keep_separate_projections() {
    const STORE: &str = "SAME-STORE";
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    // Disjoint id ranges, because in production each installation mints its
    // own UUIDs; the shared sequential factory of the other tests would make
    // two independent ledgers look alike by accident.
    let pos_a = installation(&dir_a.path().join("a.db"), STORE, "REG-001", 1);
    let pos_b = installation(&dir_b.path().join("b.db"), STORE, "REG-002", 10_000);

    // Both were provisioned from the same upstream baseline.
    bootstrap_stock(&pos_a, &[(SKU, 10)]);
    bootstrap_stock(&pos_b, &[(SKU, 10)]);

    sell(&pos_a, 3);
    sell(&pos_b, 4);

    // Each installation reports what *it* did, and keeps reporting it.
    assert_eq!(on_hand(&pos_a, SKU), 7_000);
    assert_eq!(on_hand(&pos_b, SKU), 6_000);

    // Neither learned anything about the other: two movements each, and the
    // sale each one recorded is its own.
    let ledger_a = position(&pos_a, SKU);
    let ledger_b = position(&pos_b, SKU);
    assert_eq!(ledger_a.movements.len(), 2);
    assert_eq!(ledger_b.movements.len(), 2);
    assert_eq!(
        ledger_a.movements[1].quantity,
        Quantity::from_units(3).unwrap()
    );
    assert_eq!(
        ledger_b.movements[1].quantity,
        Quantity::from_units(4).unwrap()
    );
    let refs_a: Vec<&str> = ledger_a
        .movements
        .iter()
        .map(|movement| movement.source_ref.as_str())
        .collect();
    assert!(
        !refs_a.contains(&ledger_b.movements[1].source_ref.as_str()),
        "installation A must not carry a movement caused by B's sale"
    );

    // And the `store_id` is the same on both — which is exactly why the figure
    // cannot be read as "what the store has".
    assert_eq!(ledger_a.position.store_id.as_str(), STORE);
    assert_eq!(ledger_b.position.store_id.as_str(), STORE);
    assert_ne!(
        ledger_a.position.on_hand_thousandths,
        ledger_b.position.on_hand_thousandths
    );
}

/// One Edge installation: its own database, its own ids, its own register.
fn installation(
    path: &std::path::Path,
    store_id: &str,
    register_id: &str,
    id_offset: u64,
) -> TestCore {
    let core = TestCore::new(
        edge_storage::SqliteEdgeStore::open(path).unwrap(),
        FixedClock::new(start_instant(), 1_000),
        SequentialIdFactory::starting_at(id_offset),
    );
    core.init_device(InitDeviceCommand {
        tenant_id: TenantId::new("TENANT-DEMO").unwrap(),
        store_id: StoreId::new(store_id).unwrap(),
        currency: Currency::Mxn,
        allow_negative_inventory: true,
    })
    .unwrap();
    activate_demo_catalog(&core);
    core.open_business_day(OpenBusinessDayCommand::default())
        .unwrap();
    core.open_register_session(OpenRegisterSessionCommand {
        request_id: None,
        register_id: RegisterId::new(register_id).unwrap(),
        operator_id: OperatorId::new("CASHIER-001").unwrap(),
        opening_cash_minor: 100_000,
    })
    .unwrap();
    core
}

/// A finished cash sale of `units` of the demo product, overpaid so the test
/// states a quantity rather than an amount.
fn sell(core: &TestCore, units: i64) {
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, units)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(core, 100_000);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
}

// ------------------------------------------------------- the negative policy

#[test]
fn a_permissive_installation_sells_what_it_does_not_have() {
    let core = in_memory_core();
    open_register(&core);

    // No bootstrap at all: the ledger says zero and the sale still happens.
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(demo_item()).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 11_600);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // A stale position must not stop the counter, and the shortfall is
    // visible rather than hidden.
    assert_eq!(on_hand(&core, SKU), -1_000);
}

#[test]
fn a_strict_installation_refuses_the_sale_before_any_money_moves() {
    let core = in_memory_core();
    open_strict_register(&core);
    bootstrap_stock(&core, &[(SKU, 1)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 2)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();

    let error = core
        .start_payment(StartPaymentCommand {
            request_id: None,
            tender_type: Tender::Cash,
            amount_minor: 23_200,
        })
        .unwrap_err();

    // The check is at the start of payment, because discovering a shortfall
    // after charging the customer is the one outcome nobody can undo cleanly
    // (ADR 029).
    assert_eq!(error.kind(), "INVENTORY_INSUFFICIENT_STOCK");
    assert_eq!(on_hand(&core, SKU), 1_000);
    let sale = core.get_active_transaction().unwrap().unwrap();
    let (payments, settlement) = core.get_transaction_payments(sale.id()).unwrap();
    assert!(payments.is_empty(), "nothing was charged");
    assert!(settlement.committed_total.is_zero());
}

#[test]
fn a_strict_installation_allows_a_sale_the_ledger_covers_exactly() {
    let core = in_memory_core();
    open_strict_register(&core);
    bootstrap_stock(&core, &[(SKU, 1)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 1)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 11_600);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    assert_eq!(on_hand(&core, SKU), 0);
}

#[test]
fn a_strict_installation_still_records_what_already_left_the_counter() {
    let core = in_memory_core();
    open_strict_register(&core);
    bootstrap_stock(&core, &[(SKU, 1)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 1)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 11_600);

    // Between the availability check and the finalization, someone writes the
    // stock off. The sale is already paid.
    manual_out(&core, SKU, 1_000, "DAMAGE");
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // After money moves, the ledger's job is to record what happened, not to
    // enforce a policy that can no longer change the outcome. The window is
    // documented, not closed: there are no reservations in V1 (ADR 029).
    assert_eq!(on_hand(&core, SKU), -1_000);
    let types: Vec<MovementType> = position(&core, SKU)
        .movements
        .iter()
        .map(|m| m.movement_type)
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

// ------------------------------------------------------------ the ledger itself

/// That the database itself refuses an `UPDATE` or a `DELETE` is pinned in
/// `edge-storage`, where the triggers live. What the runtime has to show is the
/// consequence: the only way to fix a wrong figure is another movement, and the
/// wrong one stays visible.
#[test]
fn a_correction_is_a_new_movement_and_the_mistake_stays_on_the_record() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    manual_out(&core, SKU, 5_000, "DAMAGE");

    // The write-off was wrong: 0.500 was damaged, not 5.000.
    manual_in(&core, SKU, 4_500, "DAMAGE_REVERSAL");

    assert_eq!(on_hand(&core, SKU), 9_500);
    let view = position(&core, SKU);
    assert_eq!(view.movements.len(), 3);
    // The overstated write-off is still there, with its original magnitude.
    assert_eq!(
        view.movements[1].quantity,
        Quantity::from_thousandths(5_000).unwrap()
    );
    assert_eq!(
        view.movements[2].reason_code.as_deref(),
        Some("DAMAGE_REVERSAL")
    );
}

#[test]
fn the_position_is_the_fold_and_is_stored_nowhere() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    manual_in(&core, SKU, 3_500, "RECEIPT");
    manual_out(&core, SKU, 1_250, "DAMAGE");

    let view = position(&core, SKU);
    let folded: i64 = view
        .movements
        .iter()
        .map(|movement| movement.signed_thousandths())
        .sum();

    assert_eq!(view.position.on_hand_thousandths, folded);
    assert_eq!(folded, 12_250);
    assert_eq!(view.position.movement_count, 3);
    assert_eq!(
        view.position.last_movement_at,
        view.movements.last().map(|m| m.occurred_at)
    );
}

#[test]
fn the_movement_list_is_oldest_first_and_pages() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    manual_in(&core, SKU, 1_000, "RECEIPT");
    manual_out(&core, SKU, 2_000, "DAMAGE");

    let product = ProductRef::new(SKU).unwrap();
    let all = core
        .get_inventory_movements(product.clone(), 10, 0)
        .unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].movement_type, MovementType::OpeningBalance);

    let second_page = core.get_inventory_movements(product, 10, 1).unwrap();
    assert_eq!(second_page.len(), 2);
    assert_eq!(second_page[0].movement_id, all[1].movement_id);
}

#[test]
fn the_journal_records_the_bootstrap_and_every_manual_movement() {
    let core = in_memory_core();
    open_register(&core);
    bootstrap_stock(&core, &[(SKU, 10)]);
    manual_in(&core, SKU, 1_000, "RECEIPT");

    let types = journal_types(&core);
    assert_eq!(
        types
            .iter()
            .filter(|t| t.as_str() == "INVENTORY_BOOTSTRAPPED")
            .count(),
        1
    );
    assert_eq!(
        types
            .iter()
            .filter(|t| t.as_str() == "INVENTORY_MOVEMENT_RECORDED")
            .count(),
        1
    );
}

#[test]
fn a_finalized_sale_reports_how_many_movements_it_wrote() {
    let core = in_memory_core();
    init_device(&core);
    two_product_catalog(1).activate(&core).unwrap();
    open_day_and_session(&core);
    bootstrap_stock(&core, &[(SKU, 10), (SECOND_SKU, 10)]);

    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    core.add_item(item(SKU, 1)).unwrap();
    core.add_item(item(SECOND_SKU, 1)).unwrap();
    core.totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    pay_cash(&core, 100_000);
    core.finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();

    // A count, not a list: the journal event says how much stock moved without
    // duplicating the ledger into a payload.
    let finalized = core
        .get_journal(50)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type.as_str() == "TRANSACTION_FINALIZED")
        .unwrap();
    assert_eq!(finalized.payload_json["inventory_movements"], 2);
}
