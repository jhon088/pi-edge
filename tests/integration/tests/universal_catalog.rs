//! Universal catalog extensions, end to end against real SQLite (ADR 044).
//!
//! The scenarios here are a restaurant kiosk menu, but nothing in the code
//! under test knows that: the same tables, the same importer and the same
//! hashes serve a retail counter that declares none of it. What is proven:
//!
//! * a menu with required, optional, repeatable, semantic and nested choices
//!   survives a full `begin -> chunk -> commit -> activate` round trip,
//! * a chargeable option is priced by the **existing** price books, through
//!   the product it names — there is no second pricing engine,
//! * availability is commercial, per channel, and keeps a sold-out extra out
//!   of the question without removing it from the catalog,
//! * every structural lie is refused, and a refused import never activates,
//! * the content hash depends on content and not on input order.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use edge_application::commands::*;
use edge_application::ports::EdgeStore;
use edge_application::testing::{CatalogFixture, FixtureProduct};
use edge_application::AppError;
use edge_catalog::{
    AvailabilityDefault, AvailabilityIndex, AvailabilityState, AvailabilityTarget, CatalogError,
    Category, Channel, ChannelAvailability, Locale, LocalizedText, ProductPresentation,
    ProductSelector, SelectionGroup, SelectionOption, SelectionOwner,
};
use edge_domain::prelude::*;
use edge_integration_tests::*;

// ------------------------------------------------------------------ builders

fn text(default: &str) -> LocalizedText {
    LocalizedText::new(default).unwrap()
}

fn bilingual(default: &str, english: &str) -> LocalizedText {
    LocalizedText::with_translations(
        default,
        BTreeMap::from([(Locale::new("en").unwrap(), english.to_owned())]),
    )
    .unwrap()
}

fn product_ref(value: &str) -> ProductRef {
    ProductRef::new(value).unwrap()
}

fn category_ref(value: &str) -> CategoryRef {
    CategoryRef::new(value).unwrap()
}

fn channel_ref(value: &str) -> ChannelRef {
    ChannelRef::new(value).unwrap()
}

fn group_id(value: &str) -> SelectionGroupId {
    SelectionGroupId::new(value).unwrap()
}

fn option_id(value: &str) -> SelectionOptionId {
    SelectionOptionId::new(value).unwrap()
}

fn channel(reference: &str, default: AvailabilityDefault) -> Channel {
    Channel {
        channel_ref: channel_ref(reference),
        name: text(reference),
        default_availability: default,
    }
}

fn category(reference: &str, name: &str, order: i32) -> Category {
    Category::import(category_ref(reference), text(name), order, None)
}

/// A group owned by a product.
fn asks(id: &str, owner: &str, name: &str, min: i64, max: i64, order: i32) -> SelectionGroup {
    SelectionGroup::import(
        group_id(id),
        SelectionOwner::Product {
            product_ref: product_ref(owner),
        },
        text(name),
        min,
        max,
        order,
    )
    .unwrap()
}

/// A group owned by an option: the nested question.
fn asks_after(id: &str, owner: &str, name: &str, min: i64, max: i64) -> SelectionGroup {
    SelectionGroup::import(
        group_id(id),
        SelectionOwner::Option {
            option_id: option_id(owner),
        },
        text(name),
        min,
        max,
        0,
    )
    .unwrap()
}

/// An option, optionally chargeable through the product it names.
fn answer(
    id: &str,
    group: &str,
    name: &str,
    component: Option<&str>,
    max: i64,
    default: i64,
) -> SelectionOption {
    SelectionOption::import(
        option_id(id),
        group_id(group),
        text(name),
        component.map(product_ref),
        max,
        default,
        0,
        None,
    )
    .unwrap()
}

fn withheld(channel: &str, option: &str) -> ChannelAvailability {
    ChannelAvailability {
        channel_ref: channel_ref(channel),
        target: AvailabilityTarget::SelectionOption {
            option_id: option_id(option),
        },
        state: AvailabilityState::Withheld,
    }
}

/// The whole menu: a burger that asks six questions, one of them only after
/// a particular answer. Every chargeable answer names a priced product.
fn menu(version: i64) -> CatalogFixture {
    CatalogFixture::new(version)
        .with(FixtureProduct::new("BURGER", "Hamburguesa", 12_000).barcode("7501000000100"))
        .with(FixtureProduct::new("PATTY", "Carne extra", 2_500))
        .with(FixtureProduct::new("BACON", "Tocino", 1_500))
        .with(FixtureProduct::new("CHEESE", "Queso", 1_000))
        .with(FixtureProduct::new("FRIES", "Papas", 3_000))
        .with(FixtureProduct::new("SALAD", "Ensalada", 3_500))
        .with(FixtureProduct::new("COKE", "Refresco", 2_500))
        .with(FixtureProduct::new("WATER", "Agua", 2_000))
        .with(FixtureProduct::new("SIZE-L", "Tamaño grande", 800))
        .with_channel(channel("COUNTER", AvailabilityDefault::Offered))
        .with_channel(channel("KIOSK", AvailabilityDefault::Offered))
        .with_category(category("C-BURGERS", "Hamburguesas", 0))
        .with_category(category("C-DRINKS", "Bebidas", 1))
        .with_presentation(ProductPresentation {
            product_ref: product_ref("BURGER"),
            category_ref: Some(category_ref("C-BURGERS")),
            display_name: Some(bilingual("Hamburguesa clásica", "Classic burger")),
            description: Some(bilingual("Con papas", "With fries")),
            display_order: 0,
            image: Some(asset_ref_for(&burger_photo())),
            recommended_product_refs: Vec::new(),
        })
        // Exactly one size: required means min_quantity = 1.
        .with_selection(
            asks("G-SIZE", "BURGER", "Tamaño", 1, 1, 0),
            vec![
                answer("O-SINGLE", "G-SIZE", "Sencilla", None, 1, 1),
                answer("O-DOUBLE", "G-SIZE", "Doble", Some("PATTY"), 1, 0),
            ],
        )
        // Up to five extras in total, at most two of the same one.
        .with_selection(
            asks("G-EXTRAS", "BURGER", "Extras", 0, 5, 1),
            vec![
                answer("O-BACON", "G-EXTRAS", "Tocino", Some("BACON"), 2, 0),
                answer("O-CHEESE", "G-EXTRAS", "Queso", Some("CHEESE"), 2, 0),
            ],
        )
        // Removing an ingredient: semantic, free, and on by default.
        .with_selection(
            asks("G-ONION", "BURGER", "Cebolla", 0, 1, 2),
            vec![answer("O-NO-ONION", "G-ONION", "Sin cebolla", None, 1, 1)],
        )
        // The combo: a side and a drink, one each.
        .with_selection(
            asks("G-SIDE", "BURGER", "Acompañamiento", 1, 1, 3),
            vec![
                answer("O-FRIES", "G-SIDE", "Papas", Some("FRIES"), 1, 1),
                answer("O-SALAD", "G-SIDE", "Ensalada", Some("SALAD"), 1, 0),
            ],
        )
        .with_selection(
            asks("G-DRINK", "BURGER", "Bebida", 1, 1, 4),
            vec![
                answer("O-COKE", "G-DRINK", "Refresco", Some("COKE"), 1, 0),
                answer("O-WATER", "G-DRINK", "Agua", Some("WATER"), 1, 1),
            ],
        )
        // Choosing the soft drink asks one more question; choosing water does
        // not. That is the whole of `Bebida -> Coca Cola -> Tamaño`.
        .with_selection(
            asks_after("G-DRINK-SIZE", "O-COKE", "Tamaño bebida", 1, 1),
            vec![
                answer("O-SMALL", "G-DRINK-SIZE", "Chico", None, 1, 1),
                answer("O-LARGE", "G-DRINK-SIZE", "Grande", Some("SIZE-L"), 1, 0),
            ],
        )
        // Sold out at the kiosk, still on the counter's menu.
        .with_availability(withheld("KIOSK", "O-BACON"))
}

/// The bytes of the burger's photo.
///
/// A real, hashable image rather than an invented digest: since milestone 13B
/// a snapshot that references an asset cannot be put in force until the bytes
/// are here, so a menu with a photo has to come with one (ADR 045).
fn burger_photo() -> Vec<u8> {
    png_bytes(1)
}

/// A core that has been provisioned and already holds the menu's photo.
fn menu_core() -> (TestCore, tempfile::TempDir) {
    let (core, dir) = core_with_assets();
    init_device(&core);
    store_asset(&core, &burger_photo(), 64);
    (core, dir)
}

/// Read the stored structure back the way a client eventually will: through
/// the core, from the rows, never from the fixture.
fn stored(
    core: &TestCore,
    snapshot_id: CatalogSnapshotId,
) -> (Vec<SelectionGroup>, Vec<SelectionOption>) {
    core.store()
        .transact(|uow| {
            Ok((
                uow.list_catalog_selection_groups(snapshot_id)?,
                uow.list_catalog_selection_options(snapshot_id)?,
            ))
        })
        .unwrap()
}

fn find_group<'a>(groups: &'a [SelectionGroup], id: &str) -> &'a SelectionGroup {
    groups
        .iter()
        .find(|group| group.group_id.as_str() == id)
        .expect("the group was stored")
}

fn find_option<'a>(options: &'a [SelectionOption], id: &str) -> &'a SelectionOption {
    options
        .iter()
        .find(|option| option.option_id.as_str() == id)
        .expect("the option was stored")
}

// --------------------------------------------------------------- the happy path

#[test]
fn a_menu_imports_commits_and_activates_through_the_existing_pipeline() {
    let (core, _dir) = menu_core();

    let snapshot_id = menu(1).activate(&core).unwrap();
    let status = core.get_catalog_status().unwrap();
    let active = status.active.expect("the menu is in force");
    assert_eq!(active.snapshot.id(), snapshot_id);
    assert_eq!(active.stored.selection_groups, 6);
    assert_eq!(active.stored.selection_options, 11);
    assert_eq!(active.stored.channels, 2);
    assert_eq!(active.stored.categories, 2);
    assert_eq!(active.stored.presentation, 1);
    assert_eq!(active.stored.availability, 1);

    // A product that asks nothing still sells by a bare scan, priced and
    // taxed exactly as before.
    open_day_and_session(&core);
    core.start_transaction(StartTransactionCommand::default())
        .unwrap();
    let sale = core.add_item(item("BACON", 1)).unwrap();
    let line = &sale.state().lines[0];
    assert_eq!(line.unit_price.minor(), 1_500);
    assert_eq!(line.base_unit_price.minor(), 1_500);
    assert!(line.selections.is_empty());
    assert_eq!(line.line_total.minor(), 1_740);

    // The burger asks a required question, so a bare scan of it is refused
    // rather than silently sold without a size (ADR 046).
    assert_eq!(
        core.add_item(scan("7501000000100", 1)).unwrap_err().kind(),
        "CATALOG_SELECTION_REQUIRED"
    );
}

#[test]
fn quantities_say_required_optional_and_repeatable_without_a_single_flag() {
    let (core, _dir) = menu_core();
    let snapshot_id = menu(1).activate(&core).unwrap();
    let (groups, options) = stored(&core, snapshot_id);

    // Exactly one size.
    let size = find_group(&groups, "G-SIZE");
    assert!(size.is_required());
    assert_eq!((size.min_quantity, size.max_quantity), (1, 1));

    // Up to five extras, none required.
    let extras = find_group(&groups, "G-EXTRAS");
    assert!(!extras.is_required());
    assert_eq!((extras.min_quantity, extras.max_quantity), (0, 5));

    // Two of the same extra.
    assert_eq!(find_option(&options, "O-BACON").max_quantity, 2);

    // An ingredient removed by default, and free: no component, no price.
    let no_onion = find_option(&options, "O-NO-ONION");
    assert_eq!(no_onion.default_quantity, 1);
    assert!(!no_onion.is_chargeable());
    assert_eq!(no_onion.component_ref, None);
}

#[test]
fn a_chosen_option_can_ask_one_more_question() {
    let (core, _dir) = menu_core();
    let snapshot_id = menu(1).activate(&core).unwrap();
    let (groups, _) = stored(&core, snapshot_id);

    let nested = find_group(&groups, "G-DRINK-SIZE");
    assert_eq!(
        nested.owner,
        SelectionOwner::Option {
            option_id: option_id("O-COKE")
        },
        "the size question hangs from the soft drink, not from the burger"
    );
    // Water asks nothing more.
    assert!(!groups.iter().any(|group| group.owner
        == SelectionOwner::Option {
            option_id: option_id("O-WATER")
        }));
}

#[test]
fn a_chargeable_option_is_priced_by_the_existing_price_books() {
    let (core, _dir) = menu_core();
    let snapshot_id = menu(1).activate(&core).unwrap();
    let (_, options) = stored(&core, snapshot_id);

    let bacon = find_option(&options, "O-BACON");
    let component = bacon.component_ref.clone().expect("a chargeable extra");

    // No price lives on the option. The amount comes from the product the
    // option names, resolved by the same primitive a scanned barcode uses.
    let lookup = core
        .lookup_product(ProductSelector::ProductRef(component))
        .unwrap();
    assert_eq!(lookup.price.unwrap().unit_price.minor(), 1_500);
    assert_eq!(lookup.tax_profile.rate.bps(), 1_600);
}

#[test]
fn an_option_can_be_withheld_on_one_channel_and_offered_on_another() {
    let (core, _dir) = menu_core();
    let snapshot_id = menu(1).activate(&core).unwrap();

    let (channels, rows) = core
        .store()
        .transact(|uow| {
            Ok((
                uow.list_catalog_channels(snapshot_id)?,
                uow.list_catalog_availability(snapshot_id)?,
            ))
        })
        .unwrap();
    let index = AvailabilityIndex::build(&channels, &rows);
    let now = Timestamp::parse_rfc3339(START).unwrap();
    let bacon = AvailabilityTarget::SelectionOption {
        option_id: option_id("O-BACON"),
    };

    assert!(!index.offers(&channel_ref("KIOSK"), &bacon, now));
    assert!(index.offers(&channel_ref("COUNTER"), &bacon, now));
    // Withholding an extra does not remove it from the catalog: the option is
    // still stored, and the counter still sells it.
    let (_, options) = stored(&core, snapshot_id);
    assert!(options
        .iter()
        .any(|option| option.option_id.as_str() == "O-BACON"));
}

#[test]
fn a_snooze_lifts_by_itself() {
    let (core, _dir) = menu_core();
    let noon = Timestamp::parse_rfc3339("2026-10-08T18:00:00Z").unwrap();
    let snapshot_id = menu(1)
        .with_availability(ChannelAvailability {
            channel_ref: channel_ref("KIOSK"),
            target: AvailabilityTarget::Product {
                product_ref: product_ref("SALAD"),
            },
            state: AvailabilityState::Snoozed { until: noon },
        })
        .activate(&core)
        .unwrap();

    let (channels, rows) = core
        .store()
        .transact(|uow| {
            Ok((
                uow.list_catalog_channels(snapshot_id)?,
                uow.list_catalog_availability(snapshot_id)?,
            ))
        })
        .unwrap();
    let index = AvailabilityIndex::build(&channels, &rows);
    let salad = AvailabilityTarget::Product {
        product_ref: product_ref("SALAD"),
    };
    assert!(!index.offers(
        &channel_ref("KIOSK"),
        &salad,
        Timestamp::parse_rfc3339("2026-10-08T17:59:59Z").unwrap()
    ));
    assert!(index.offers(&channel_ref("KIOSK"), &salad, noon));
}

#[test]
fn localized_text_and_images_survive_the_round_trip() {
    let (core, _dir) = menu_core();
    let snapshot_id = menu(1).activate(&core).unwrap();

    let presentation = core
        .store()
        .transact(|uow| uow.list_catalog_presentation(snapshot_id))
        .unwrap();
    let burger = &presentation[0];
    let display = burger.display_name.as_ref().unwrap();
    assert_eq!(display.resolve(None), "Hamburguesa clásica");
    assert_eq!(
        display.resolve(Some(&Locale::new("en").unwrap())),
        "Classic burger"
    );
    // No prefix guessing: an untranslated locale falls back to the default.
    assert_eq!(
        display.resolve(Some(&Locale::new("en-GB").unwrap())),
        "Hamburguesa clásica"
    );
    let image = burger.image.as_ref().unwrap();
    assert_eq!(image.media_type, "image/png");
    assert_eq!(image.byte_size, burger_photo().len() as i64);
    assert_eq!(image, &asset_ref_for(&burger_photo()));
    // And the bytes behind it are local, because they had to be before this
    // snapshot could be activated at all (ADR 045).
    assert_eq!(read_asset(&core, image), burger_photo());
}

#[test]
fn the_same_menu_hashes_the_same_whatever_order_it_arrives_in() {
    let (forwards, _forwards_dir) = menu_core();
    let first = menu(1).activate(&forwards).unwrap();

    // Same content, every list reversed.
    let mut shuffled = menu(1);
    shuffled.channels.reverse();
    shuffled.categories.reverse();
    shuffled.selection_groups.reverse();
    shuffled.selection_options.reverse();
    shuffled.products.reverse();
    let (backwards, _backwards_dir) = menu_core();
    let second = shuffled.activate(&backwards).unwrap();

    let hash = |core: &TestCore, id| {
        core.store()
            .transact(|uow| {
                Ok(uow
                    .find_catalog_snapshot(id)?
                    .unwrap()
                    .state()
                    .content_hash
                    .clone()
                    .unwrap())
            })
            .unwrap()
    };
    assert_eq!(hash(&forwards, first), hash(&backwards, second));
}

// ------------------------------------------------------- what must be refused

/// Everything a malformed menu can claim, and the import state afterwards.
///
/// The invariant under test is the same in every case: a refused import stays
/// `IMPORTING`, and nothing it carried is ever in force.
fn refuse(mutate: impl FnOnce(&mut CatalogFixture)) -> (AppError, TestCore, tempfile::TempDir) {
    let (core, dir) = menu_core();
    let mut fixture = menu(1);
    mutate(&mut fixture);
    let error = fixture.import(&core).expect_err("the import is refused");
    assert!(
        core.get_catalog_status().unwrap().active.is_none(),
        "a refused import never becomes the catalog that prices sales"
    );
    (error, core, dir)
}

#[test]
fn a_group_hanging_from_an_option_that_does_not_exist_is_refused() {
    let (error, _core, _dir) = refuse(|fixture| {
        fixture
            .selection_groups
            .push(asks_after("G-GHOST", "O-NOBODY", "Fantasma", 1, 1));
        fixture
            .selection_options
            .push(answer("O-GHOST", "G-GHOST", "Nada", None, 1, 0));
    });
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::UnknownSelectionOwner { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn a_chargeable_option_naming_a_product_outside_the_snapshot_is_refused() {
    let (error, _core, _dir) = refuse(|fixture| {
        fixture.selection_options.push(answer(
            "O-TRUFFLE",
            "G-EXTRAS",
            "Trufa",
            Some("TRUFFLE"),
            1,
            0,
        ));
    });
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::UnknownSelectionComponent { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn a_duplicate_option_is_refused() {
    let (error, _core, _dir) = refuse(|fixture| {
        fixture.selection_options.push(answer(
            "O-BACON",
            "G-EXTRAS",
            "Tocino otra vez",
            None,
            1,
            0,
        ));
    });
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::DuplicateSelectionOption { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn a_cycle_of_groups_is_refused() {
    let (error, _core, _dir) = refuse(|fixture| {
        // The size question starts hanging from one of its own answers.
        fixture
            .selection_groups
            .push(asks_after("G-LOOP", "O-LOOP", "Bucle", 1, 1));
        fixture
            .selection_options
            .push(answer("O-LOOP", "G-LOOP", "Se muerde la cola", None, 1, 0));
    });
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::CyclicSelectionGroup { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn availability_naming_something_absent_is_refused() {
    let (error, _core, _dir) = refuse(|fixture| {
        fixture.availability.push(withheld("KIOSK", "O-NOBODY"));
    });
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::UnknownAvailabilityTarget { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn availability_on_a_channel_the_snapshot_never_declared_is_refused() {
    let (error, _core, _dir) = refuse(|fixture| {
        fixture.availability.push(withheld("DRIVE-THRU", "O-BACON"));
    });
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::UnknownChannel { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn a_duplicate_channel_is_refused_before_anything_is_stored() {
    let (core, _dir) = menu_core();
    let mut fixture = menu(1);
    fixture
        .channels
        .push(channel("KIOSK", AvailabilityDefault::Withheld));
    let error = fixture.import(&core).expect_err("refused");
    assert!(
        matches!(
            error,
            AppError::Catalog(CatalogError::DuplicateChannel { .. })
        ),
        "got {error:?}"
    );
    // The header never opened, so there is no import to resume either.
    assert!(core.get_catalog_status().unwrap().importing.is_empty());
}

#[test]
fn a_group_that_accepts_nothing_is_refused_before_it_reaches_the_core() {
    // Built, not imported: the value object refuses itself, so an impossible
    // group cannot be constructed to be sent in the first place.
    assert!(SelectionGroup::import(
        group_id("G-NONE"),
        SelectionOwner::Product {
            product_ref: product_ref("BURGER")
        },
        text("Nada"),
        0,
        0,
        0,
    )
    .is_err());
    assert!(SelectionGroup::import(
        group_id("G-BACKWARDS"),
        SelectionOwner::Product {
            product_ref: product_ref("BURGER")
        },
        text("Al revés"),
        3,
        1,
        0,
    )
    .is_err());
    assert!(SelectionOption::import(
        option_id("O-TOO-MANY"),
        group_id("G-SIZE"),
        text("Demasiados"),
        None,
        1,
        2,
        0,
        None,
    )
    .is_err());
}

#[test]
fn activation_is_still_atomic_when_a_menu_replaces_a_menu() {
    let (core, _dir) = menu_core();
    let first = menu(1).activate(&core).unwrap();

    // Version 2 drops the bacon extra entirely.
    let mut second_menu = menu(2);
    second_menu
        .selection_options
        .retain(|option| option.option_id.as_str() != "O-BACON");
    second_menu.availability.clear();
    let second = second_menu.activate(&core).unwrap();

    let status = core.get_catalog_status().unwrap();
    let active = status.active.expect("one snapshot is in force");
    assert_eq!(active.snapshot.id(), second);
    assert_ne!(active.snapshot.id(), first);
    // Exactly one ACTIVE, and the superseded one keeps its own rows: a sale
    // that captured version 1 still resolves against it.
    let (groups, options) = stored(&core, first);
    assert_eq!(groups.len(), 6);
    assert!(options
        .iter()
        .any(|option| option.option_id.as_str() == "O-BACON"));
}

#[test]
fn a_retail_catalog_declares_no_extensions_and_imports_unchanged() {
    let core = in_memory_core();
    init_device(&core);
    CatalogFixture::demo(1).activate(&core).unwrap();

    let active = core.get_catalog_status().unwrap().active.unwrap();
    assert_eq!(active.stored.channels, 0);
    assert_eq!(active.stored.categories, 0);
    assert_eq!(active.stored.presentation, 0);
    assert_eq!(active.stored.selection_groups, 0);
    assert_eq!(active.stored.selection_options, 0);
    assert_eq!(active.stored.availability, 0);
}
