//! A configured sale, end to end: the kiosk MVP's core (ADR 046).
//!
//! What has to be true before any screen is worth drawing:
//!
//! * a product that asks questions cannot be sold without answering them,
//! * a chargeable answer is priced by the **existing** price books, through
//!   the product it names,
//! * a semantic answer — "no onions" — costs nothing and is still recorded,
//! * the line charges base plus the chargeable answers, and the totals,
//!   promotions, tax and receipt all follow from that one number.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use edge_application::commands::*;
use edge_application::order_code;
use edge_application::testing::{CatalogFixture, FixtureProduct};
use edge_catalog::{
    Category, LocalizedText, ProductPresentation, RequestedOption, SelectionGroup, SelectionOption,
    SelectionOwner,
};
use edge_domain::prelude::*;
use edge_integration_tests::*;

fn text(value: &str) -> LocalizedText {
    LocalizedText::new(value).unwrap()
}

fn product_ref(value: &str) -> ProductRef {
    ProductRef::new(value).unwrap()
}

fn group(
    id: &str,
    owner: SelectionOwner,
    name: &str,
    lo: i64,
    hi: i64,
    order: i32,
) -> SelectionGroup {
    SelectionGroup::import(
        SelectionGroupId::new(id).unwrap(),
        owner,
        text(name),
        lo,
        hi,
        order,
    )
    .unwrap()
}

fn owned_by(product: &str) -> SelectionOwner {
    SelectionOwner::Product {
        product_ref: product_ref(product),
    }
}

fn after(option: &str) -> SelectionOwner {
    SelectionOwner::Option {
        option_id: SelectionOptionId::new(option).unwrap(),
    }
}

fn option(
    id: &str,
    group: &str,
    name: &str,
    component: Option<&str>,
    maximum: i64,
) -> SelectionOption {
    // Display order follows the order they are listed in each group below.
    SelectionOption::import(
        SelectionOptionId::new(id).unwrap(),
        SelectionGroupId::new(group).unwrap(),
        text(name),
        component.map(product_ref),
        maximum,
        0,
        ORDER.with(|next| {
            let value = next.get();
            next.set(value + 1);
            value
        }),
        None,
    )
    .unwrap()
}

thread_local! {
    static ORDER: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

fn chose(option: &str, quantity: i64) -> RequestedOption {
    RequestedOption {
        option_id: SelectionOptionId::new(option).unwrap(),
        quantity,
    }
}

/// A burger that asks three questions, a soft drink that asks one, and a combo
/// whose drink answer asks one more.
fn menu(version: i64) -> CatalogFixture {
    CatalogFixture::new(version)
        .with(FixtureProduct::new("BURGER", "Smash Clásica", 12_900).barcode("7500000000011"))
        .with(FixtureProduct::new("COMBO", "Combo Smash", 18_900))
        .with(FixtureProduct::new("SODA", "Refresco", 3_900))
        .with(FixtureProduct::new("CHEESE", "Queso extra", 1_500))
        .with(FixtureProduct::new("BACON", "Tocino", 2_200))
        .with(FixtureProduct::new("PATTY", "Carne extra", 4_500))
        .with(FixtureProduct::new("LARGE", "Tamaño grande", 1_500))
        .with(FixtureProduct::new("UP-SHAKE", "Cambio a malteada", 3_500))
        .with_category(Category::import(
            CategoryRef::new("C-MAIN").unwrap(),
            text("Hamburguesas"),
            0,
            None,
        ))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: Some(CategoryRef::new("C-MAIN").unwrap()),
            display_name: Some(text("Smash Clásica")),
            description: Some(text("Carne smash y queso americano.")),
            display_order: 0,
            image: None,
            // The burger sells the combo. Merchandising declared by the
            // catalog, not worked out by a screen.
            recommended_product_refs: vec![product_ref("COMBO")],
        })
        .with_presentation(ProductPresentation {
            product_ref: product_ref("COMBO"),
            category_ref: Some(CategoryRef::new("C-MAIN").unwrap()),
            display_name: Some(text("Combo Smash")),
            description: None,
            display_order: 1,
            image: None,
            recommended_product_refs: Vec::new(),
        })
        // Required: exactly one size.
        .with_selection(
            group("G-SIZE", owned_by("BURGER"), "¿Cómo la quieres?", 1, 1, 0),
            vec![
                option("O-SINGLE", "G-SIZE", "Sencilla", None, 1),
                option("O-DOUBLE", "G-SIZE", "Doble carne", Some("PATTY"), 1),
            ],
        )
        // Optional: up to four extras, at most two of each.
        .with_selection(
            group("G-EXTRAS", owned_by("BURGER"), "Extras", 0, 4, 1),
            vec![
                option("O-CHEESE", "G-EXTRAS", "Queso extra", Some("CHEESE"), 2),
                option("O-BACON", "G-EXTRAS", "Tocino", Some("BACON"), 2),
            ],
        )
        // Free: ingredients to take out.
        .with_selection(
            group(
                "G-REMOVE",
                owned_by("BURGER"),
                "Quitar ingredientes",
                0,
                2,
                2,
            ),
            vec![
                option("O-NOONION", "G-REMOVE", "Sin cebolla", None, 1),
                option("O-NOSAUCE", "G-REMOVE", "Sin salsa", None, 1),
            ],
        )
        .with_selection(
            group("G-SODA-SIZE", owned_by("SODA"), "Tamaño", 1, 1, 0),
            vec![
                option("O-SODA-REG", "G-SODA-SIZE", "Regular", None, 1),
                option("O-SODA-LARGE", "G-SODA-SIZE", "Grande", Some("LARGE"), 1),
            ],
        )
        // The combo: a drink, and only a soft drink asks for its size.
        .with_selection(
            group("G-CO-DRINK", owned_by("COMBO"), "Elige tu bebida", 1, 1, 0),
            vec![
                option("O-CO-SODA", "G-CO-DRINK", "Refresco", None, 1),
                option("O-CO-SHAKE", "G-CO-DRINK", "Malteada", Some("UP-SHAKE"), 1),
            ],
        )
        .with_selection(
            group(
                "G-CO-SIZE",
                after("O-CO-SODA"),
                "Tamaño de la bebida",
                1,
                1,
                1,
            ),
            vec![
                option("O-CO-REG", "G-CO-SIZE", "Regular", None, 1),
                option("O-CO-LARGE", "G-CO-SIZE", "Grande", Some("LARGE"), 1),
            ],
        )
}

fn kiosk() -> (TestCore, tempfile::TempDir) {
    let (core, dir) = core_with_assets();
    init_device(&core);
    menu(1).activate(&core).unwrap();
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    (core, dir)
}

fn configure(product: &str, selections: Vec<RequestedOption>) -> AddItemCommand {
    AddItemCommand {
        request_id: None,
        selector: edge_catalog::ProductSelector::ProductRef(product_ref(product)),
        quantity: Quantity::from_units(1).unwrap(),
        selections,
        preparation_note: None,
    }
}

// ------------------------------------------------------- the configured line

#[test]
fn a_configured_burger_charges_base_plus_its_chargeable_choices() {
    let (core, _dir) = kiosk();

    // Smash Clásica + queso extra + tocino x2 - cebolla, doble carne.
    let sale = core
        .add_item(configure(
            "BURGER",
            vec![
                chose("O-DOUBLE", 1),
                chose("O-CHEESE", 1),
                chose("O-BACON", 2),
                chose("O-NOONION", 1),
            ],
        ))
        .unwrap();

    let line = &sale.state().lines[0];
    assert_eq!(line.base_unit_price.minor(), 12_900);
    // 129.00 + 45.00 + 15.00 + 2×22.00 = 233.00
    assert_eq!(line.unit_price.minor(), 23_300);
    assert_eq!(line.extended_amount.minor(), 23_300);
    assert_eq!(line.tax_amount.minor(), 3_728);
    assert_eq!(line.line_total.minor(), 27_028);

    // Every choice is materialised with what it was called and what it cost.
    let names: Vec<(&str, i64, i64)> = line
        .selections
        .iter()
        .map(|s| (s.name.as_str(), s.quantity, s.amount.minor()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("Doble carne", 1, 4_500),
            ("Queso extra", 1, 1_500),
            ("Tocino", 2, 4_400),
            ("Sin cebolla", 1, 0),
        ]
    );
    // And a semantic choice is recorded without being a charge.
    let no_onion = line
        .selections
        .iter()
        .find(|s| s.name == "Sin cebolla")
        .unwrap();
    assert!(!no_onion.is_chargeable());
    assert_eq!(no_onion.component_ref, None);
}

#[test]
fn removing_an_ingredient_costs_nothing_and_is_still_on_the_line() {
    let (core, _dir) = kiosk();
    let plain = core
        .add_item(configure("BURGER", vec![chose("O-SINGLE", 1)]))
        .unwrap();
    let plain_total = plain.state().lines[0].line_total;

    let sale = core
        .add_item(configure(
            "BURGER",
            vec![
                chose("O-SINGLE", 1),
                chose("O-NOONION", 1),
                chose("O-NOSAUCE", 1),
            ],
        ))
        .unwrap();
    let stripped = &sale.state().lines[1];
    assert_eq!(stripped.line_total, plain_total, "free is free");
    assert_eq!(stripped.selections.len(), 3);
    assert!(
        stripped
            .selections
            .iter()
            .filter(|s| !s.is_chargeable())
            .count()
            == 3
    );
}

#[test]
fn a_chosen_answer_can_ask_one_more_question() {
    let (core, _dir) = kiosk();

    // The soft drink asks for a size; the shake does not.
    let sale = core
        .add_item(configure(
            "COMBO",
            vec![chose("O-CO-SODA", 1), chose("O-CO-LARGE", 1)],
        ))
        .unwrap();
    assert_eq!(sale.state().lines[0].unit_price.minor(), 18_900 + 1_500);

    let shake = core
        .add_item(configure("COMBO", vec![chose("O-CO-SHAKE", 1)]))
        .unwrap();
    assert_eq!(shake.state().lines[1].unit_price.minor(), 18_900 + 3_500);

    // Asking for the drink size without choosing the soft drink is not a rule
    // to evaluate: that question was never asked.
    assert_eq!(
        core.add_item(configure(
            "COMBO",
            vec![chose("O-CO-SHAKE", 1), chose("O-CO-LARGE", 1)]
        ))
        .unwrap_err()
        .kind(),
        "CATALOG_OPTION_NOT_OFFERED"
    );
}

#[test]
fn the_questions_a_product_asks_are_enforced_by_the_core() {
    let (core, _dir) = kiosk();

    // Required, and unanswered.
    assert_eq!(
        core.add_item(configure("BURGER", vec![]))
            .unwrap_err()
            .kind(),
        "CATALOG_SELECTION_REQUIRED"
    );
    // More of one extra than it allows.
    assert_eq!(
        core.add_item(configure(
            "BURGER",
            vec![chose("O-SINGLE", 1), chose("O-BACON", 3)]
        ))
        .unwrap_err()
        .kind(),
        "CATALOG_SELECTION_QUANTITY_REFUSED"
    );
    // More of the group in total than it allows.
    assert_eq!(
        core.add_item(configure(
            "BURGER",
            vec![
                chose("O-SINGLE", 1),
                chose("O-BACON", 2),
                chose("O-CHEESE", 2)
            ]
        ))
        .unwrap()
        .state()
        .lines
        .len(),
        1,
        "four extras is exactly the maximum, so it is allowed"
    );
    // A choice from another product entirely.
    assert_eq!(
        core.add_item(configure(
            "BURGER",
            vec![chose("O-SINGLE", 1), chose("O-SODA-LARGE", 1)]
        ))
        .unwrap_err()
        .kind(),
        "CATALOG_OPTION_NOT_OFFERED"
    );
}

/// The one thing a client must never be able to do.
#[test]
fn a_client_cannot_state_what_a_choice_costs() {
    let (core, _dir) = kiosk();
    let sale = core
        .add_item(configure(
            "BURGER",
            vec![chose("O-SINGLE", 1), chose("O-BACON", 1)],
        ))
        .unwrap();
    let bacon = &sale.state().lines[0].selections[1];

    // The amount came from the price book entry for the component, not from
    // anything the caller said — the command carries ids and quantities only.
    assert_eq!(bacon.component_ref.as_ref().unwrap().as_str(), "BACON");
    let component = core
        .lookup_product(edge_catalog::ProductSelector::ProductRef(product_ref(
            "BACON",
        )))
        .unwrap();
    assert_eq!(
        bacon.unit_price.minor(),
        component.price.unwrap().unit_price.minor()
    );
}

// ------------------------------------------------------------ the cart and up

#[test]
fn the_cart_totals_the_configured_lines_and_the_promotion_follows() {
    let (core, _dir) = kiosk();
    let mut fixture_total = 0;

    // Two configured burgers and a large soda.
    core.add_item(configure(
        "BURGER",
        vec![chose("O-SINGLE", 1), chose("O-CHEESE", 1)],
    ))
    .unwrap();
    fixture_total += 12_900 + 1_500;
    core.add_item(configure("BURGER", vec![chose("O-DOUBLE", 1)]))
        .unwrap();
    fixture_total += 12_900 + 4_500;
    let sale = core
        .add_item(configure("SODA", vec![chose("O-SODA-LARGE", 1)]))
        .unwrap();
    fixture_total += 3_900 + 1_500;

    // A running total while the customer is still choosing: the core's
    // arithmetic, not a sum the screen did (ADR 046).
    let running = sale.preview_totals().unwrap();
    assert_eq!(running.subtotal.minor(), fixture_total);
    assert_eq!(running.grand_total.minor(), fixture_total * 116 / 100);
    assert_eq!(
        sale.state().totals.grand_total.minor(),
        0,
        "and the official totals stay zero until the sale is totalized"
    );

    // Removing a line removes its choices with it.
    let trimmed = core
        .remove_item(RemoveItemCommand {
            request_id: None,
            line_number: 1,
        })
        .unwrap();
    assert_eq!(trimmed.state().lines.len(), 2);
    assert_eq!(
        trimmed.preview_totals().unwrap().subtotal.minor(),
        fixture_total - (12_900 + 1_500)
    );
}

#[test]
fn a_demo_promotion_discounts_the_configured_price() {
    use edge_catalog::promotions::{
        NewPromotion, ProductApplicability, PromotionRule, Stackability,
    };
    use edge_catalog::scope::CatalogScope;

    let (core, _dir) = core_with_assets();
    init_device(&core);
    menu(1)
        .promoting(
            edge_catalog::Promotion::import(NewPromotion {
                promotion_id: PromotionId::new("PROMO-SMASH25").unwrap(),
                code: "SMASH25".to_owned(),
                name: "25% en Smash Clásica".to_owned(),
                active: true,
                priority: 10,
                stackability: Stackability::Stackable,
                scope: CatalogScope::Tenant,
                valid_from: None,
                valid_until: None,
                rule: PromotionRule::PercentageOff {
                    applies_to: ProductApplicability::Product {
                        product_ref: product_ref("BURGER"),
                    },
                    percentage: Percentage::from_bps(2_500).unwrap(),
                },
            })
            .unwrap(),
        )
        .activate(&core)
        .unwrap();
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();

    // 129.00 + 45.00 = 174.00 configured; 25% of the configured line.
    let sale = core
        .add_item(configure("BURGER", vec![chose("O-DOUBLE", 1)]))
        .unwrap();
    let line = &sale.state().lines[0];
    assert_eq!(line.extended_amount.minor(), 17_400);
    assert_eq!(
        line.discount_amount.minor(),
        4_350,
        "the demo promotion applies to the configured total, which is the \
         simple and predictable MVP semantics"
    );
    assert_eq!(line.taxable_amount.minor(), 13_050);
    assert_eq!(sale.preview_totals().unwrap().grand_total.minor(), 15_138);
    assert_eq!(sale.state().applied_promotions.len(), 1);
    assert_eq!(sale.state().applied_promotions[0].code, "SMASH25");
}

#[test]
fn a_configured_sale_pays_finalizes_and_reads_back_after_a_reload() {
    let (core, dir) = kiosk();
    core.add_item(configure(
        "BURGER",
        vec![
            chose("O-DOUBLE", 1),
            chose("O-BACON", 2),
            chose("O-NOONION", 1),
        ],
    ))
    .unwrap();
    let totalized = core
        .totalize_transaction(TotalizeTransactionCommand::default())
        .unwrap();
    let due = totalized.state().totals.grand_total;

    // Card, through the simulator the composition root chose.
    core.start_payment(StartPaymentCommand {
        request_id: None,
        tender_type: Tender::Card,
        amount_minor: due.minor(),
    })
    .unwrap();
    let finalized = core
        .finalize_transaction(FinalizeTransactionCommand::default())
        .unwrap();
    assert_eq!(finalized.status(), TransactionStatus::Finalized);

    // The order number a kiosk screen shows: derived from the register and
    // this sale's own sequence, so it needs no counter.
    let state = finalized.state();
    assert_eq!(
        order_code(&state.register_id, state.register_sequence),
        "K01-0001"
    );

    // The choices are read back out of SQLite, not out of memory.
    let _ = dir;
    let recent = core.list_recent_sales(10, 0).unwrap();
    assert_eq!(recent.sales.len(), 1);
    let reloaded = core
        .get_transaction(state.transaction_id)
        .unwrap()
        .expect("the sale is durable");
    let line = &reloaded.state().lines[0];
    assert_eq!(line.base_unit_price.minor(), 12_900);
    assert_eq!(line.unit_price.minor(), 12_900 + 4_500 + 4_400);
    assert_eq!(line.selections.len(), 3);
    assert_eq!(line.selections[1].name, "Tocino");
    assert_eq!(line.selections[1].quantity, 2);
}

// ----------------------------------------------------------- what a UI reads

#[test]
fn the_menu_is_everything_a_kiosk_screen_needs_in_one_call() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    menu(1).activate(&core).unwrap();

    let view = core
        .get_menu(Some(ChannelRef::new("KIOSK").unwrap()))
        .unwrap();
    // Only products with presentation are on the menu: a component like
    // `BACON` is priced and sellable, but it is not a menu item.
    let refs: Vec<&str> = view.items.iter().map(|i| i.product_ref.as_str()).collect();
    assert_eq!(refs, vec!["BURGER", "COMBO"]);
    assert_eq!(view.categories.len(), 1);
    assert_eq!(view.items[0].name, "Smash Clásica");
    assert_eq!(view.items[0].from_price.minor(), 12_900);
    assert!(view.items[0].configurable, "it asks a required question");
}

#[test]
fn the_configuration_screen_is_told_the_price_of_every_answer() {
    let (core, _dir) = core_with_assets();
    init_device(&core);
    menu(1).activate(&core).unwrap();

    let view = core
        .get_product_configuration(product_ref("BURGER"), None)
        .unwrap();
    assert_eq!(view.base_price.minor(), 12_900);
    assert_eq!(view.groups.len(), 3);

    let size = &view.groups[0];
    assert_eq!(size.name, "¿Cómo la quieres?");
    assert_eq!((size.min_quantity, size.max_quantity), (1, 1));
    assert_eq!(size.owner_option_id, None);
    assert_eq!(size.options[0].unit_price.minor(), 0, "a plain one is free");
    assert_eq!(size.options[1].unit_price.minor(), 4_500);

    // The combo's nested question says what it hangs from, so a screen renders
    // the nesting it is told about rather than inferring one.
    let combo = core
        .get_product_configuration(product_ref("COMBO"), None)
        .unwrap();
    let nested = combo
        .groups
        .iter()
        .find(|group| group.owner_option_id.is_some())
        .expect("the drink size hangs from the soft drink");
    assert_eq!(
        nested.owner_option_id.as_ref().unwrap().as_str(),
        "O-CO-SODA"
    );
}

#[test]
fn a_note_for_the_kitchen_is_carried_but_never_priced() {
    let (core, _dir) = kiosk();
    let sale = core
        .add_item(AddItemCommand {
            preparation_note: Some("  Sin sal, por favor  ".to_owned()),
            ..configure("BURGER", vec![chose("O-SINGLE", 1)])
        })
        .unwrap();
    let line = &sale.state().lines[0];
    assert_eq!(line.preparation_note.as_deref(), Some("Sin sal, por favor"));
    assert_eq!(line.unit_price.minor(), 12_900);
}

/// The domain refuses a line whose own arithmetic does not add up, so a bug
/// anywhere above it cannot store a sale that lies about its own price.
#[test]
fn a_line_that_does_not_add_up_is_refused_by_the_domain() {
    let configuration = LineConfiguration {
        base_unit_price: Money::from_minor(12_900, Currency::Mxn),
        selections: vec![TransactionSelection {
            option_id: SelectionOptionId::new("O-BACON").unwrap(),
            group_id: SelectionGroupId::new("G-EXTRAS").unwrap(),
            group_name: "Extras".to_owned(),
            name: "Tocino".to_owned(),
            quantity: 1,
            component_ref: Some(product_ref("BACON")),
            unit_price: Money::from_minor(2_200, Currency::Mxn),
            amount: Money::from_minor(2_200, Currency::Mxn),
        }],
        preparation_note: None,
    };
    let (core, _dir) = kiosk();
    let _ = &core;

    let mut transaction = sample_transaction();
    let error = transaction
        .add_line(
            NewLineInput {
                line_id: LineId::from_uuid(uuid::Uuid::from_u128(7)),
                product_ref: product_ref("BURGER"),
                sku: None,
                barcode: None,
                description: "Smash Clásica".to_owned(),
                quantity: Quantity::from_units(1).unwrap(),
                // Base + bacon is 151.00; this line claims 129.00.
                unit_price: Money::from_minor(12_900, Currency::Mxn),
                tax_rate: TaxRate::from_bps(1_600).unwrap(),
                price_mode: PriceMode::TaxExclusive,
                price_book_id: None,
                discount_amount: None,
                configuration: Some(configuration),
            },
            Timestamp::parse_rfc3339(START).unwrap(),
        )
        .unwrap_err();
    assert!(
        matches!(error, DomainError::ConfiguredPriceMismatch { .. }),
        "got {error:?}"
    );
}

fn sample_transaction() -> RetailTransaction {
    RetailTransaction::start(NewRetailTransaction {
        transaction_id: TransactionId::from_uuid(uuid::Uuid::from_u128(1)),
        business_day_id: BusinessDayId::from_uuid(uuid::Uuid::from_u128(2)),
        session_id: SessionId::from_uuid(uuid::Uuid::from_u128(3)),
        store_id: StoreId::new("STORE-DEMO").unwrap(),
        register_id: RegisterId::new("KIOSK-01").unwrap(),
        operator_id: OperatorId::new("KIOSK").unwrap(),
        register_sequence: 1,
        currency: Currency::Mxn,
        pricing: None,
        created_at: Timestamp::parse_rfc3339(START).unwrap(),
    })
}
