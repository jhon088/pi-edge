//! The choices a product offers.
//!
//! One model for what every vertical calls something different: retail's size
//! and colour, restaurant's extras, removals and combo slots, venue's bundle
//! configuration. There is no `RestaurantModifier` here and there should never
//! be one — a group of options with a quantity range is the same structure in
//! all three, and naming it after one industry would force the other two to
//! read code about hamburgers (ADR 044).
//!
//! ```text
//! Product "Hamburguesa"
//!   └─ group "Queso"        1..1   → "americano"  component P-QUESO-AM
//!                                  → "cheddar"    component P-QUESO-CH
//!   └─ group "Extras"       0..5   → "tocino" ×2  component P-TOCINO
//!   └─ group "Quitar"       0..3   → "sin cebolla" (no component, no charge)
//!
//! Product "Combo"
//!   └─ group "Bebida"       1..1   → "Coca Cola"  component P-COCA
//!                                      └─ group "Tamaño" 1..1 → "grande"
//! ```
//!
//! **A chargeable option carries a `component_ref`, not a price.** The price
//! of `P-TOCINO` comes from the price books that already exist, with their
//! scope, priority and validity windows — so "bacon costs $20 here and $25
//! there" is expressible on day one without a second pricing engine. An option
//! with no component is semantic: "sin cebolla" changes what the kitchen does
//! and never a cent.
//!
//! Nothing in this file computes money. Configured line pricing is milestone
//! 13c's work; this milestone's job is to make sure the identity it will need
//! is already there.

use std::collections::{BTreeMap, BTreeSet};

use edge_domain::prelude::{ProductRef, SelectionGroupId, SelectionOptionId};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::presentation::{AssetRef, LocalizedText};

/// What a group hangs from.
///
/// A group on a product is the first question asked about it; a group on an
/// option is a follow-up question, which is how "Bebida → Coca Cola → Tamaño"
/// is expressed with no structure beyond this enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "owner", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SelectionOwner {
    Product { product_ref: ProductRef },
    Option { option_id: SelectionOptionId },
}

impl SelectionOwner {
    pub const fn as_str(&self) -> &'static str {
        match self {
            SelectionOwner::Product { .. } => "PRODUCT",
            SelectionOwner::Option { .. } => "OPTION",
        }
    }

    pub fn product_ref(&self) -> Option<&ProductRef> {
        match self {
            SelectionOwner::Product { product_ref } => Some(product_ref),
            SelectionOwner::Option { .. } => None,
        }
    }

    pub fn option_id(&self) -> Option<&SelectionOptionId> {
        match self {
            SelectionOwner::Option { option_id } => Some(option_id),
            SelectionOwner::Product { .. } => None,
        }
    }
}

/// One question, with how many answers it takes.
///
/// `min_quantity` and `max_quantity` count **units across the whole group**,
/// not distinct options: "up to 5 extras" is `0..5` whether that is five
/// different extras or two of one and three of another. Required is simply
/// `min_quantity >= 1`; there is no separate flag to contradict it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionGroup {
    pub group_id: SelectionGroupId,
    pub owner: SelectionOwner,
    pub name: LocalizedText,
    /// Total units this group must receive. `0` makes the group optional.
    pub min_quantity: i64,
    /// Total units this group accepts. Never below `min_quantity`, never zero.
    pub max_quantity: i64,
    pub display_order: i32,
}

impl SelectionGroup {
    pub fn import(
        group_id: SelectionGroupId,
        owner: SelectionOwner,
        name: LocalizedText,
        min_quantity: i64,
        max_quantity: i64,
        display_order: i32,
    ) -> Result<Self, CatalogError> {
        let invalid =
            |field: &'static str, reason: String| CatalogError::InvalidRecord { field, reason };
        if min_quantity < 0 {
            return Err(invalid(
                "selection_group.min_quantity",
                "must not be negative".to_owned(),
            ));
        }
        if max_quantity < 1 {
            return Err(invalid(
                "selection_group.max_quantity",
                "must be at least 1: a group that accepts nothing is not a question".to_owned(),
            ));
        }
        if min_quantity > max_quantity {
            return Err(invalid(
                "selection_group.min_quantity",
                format!("{min_quantity} is more than the maximum of {max_quantity}"),
            ));
        }
        Ok(Self {
            group_id,
            owner,
            name,
            min_quantity,
            max_quantity,
            display_order,
        })
    }

    /// Whether an answer is obligatory.
    pub const fn is_required(&self) -> bool {
        self.min_quantity >= 1
    }
}

/// One answer.
///
/// `component_ref` is what makes it chargeable, and it names a product the
/// price books already know how to price. `default_quantity` is what a client
/// starts with — which is how "the burger comes with onion and the customer
/// removes it" works without the UI inventing a default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionOption {
    pub option_id: SelectionOptionId,
    pub group_id: SelectionGroupId,
    pub name: LocalizedText,
    /// The product whose price this option adds. `None` means the option
    /// changes preparation and never the total.
    pub component_ref: Option<ProductRef>,
    /// Most units of *this* option one line may take.
    pub max_quantity: i64,
    /// Units selected before the customer touches anything.
    pub default_quantity: i64,
    pub display_order: i32,
    pub image: Option<AssetRef>,
}

impl SelectionOption {
    #[allow(clippy::too_many_arguments)]
    pub fn import(
        option_id: SelectionOptionId,
        group_id: SelectionGroupId,
        name: LocalizedText,
        component_ref: Option<ProductRef>,
        max_quantity: i64,
        default_quantity: i64,
        display_order: i32,
        image: Option<AssetRef>,
    ) -> Result<Self, CatalogError> {
        if max_quantity < 1 {
            return Err(CatalogError::InvalidRecord {
                field: "selection_option.max_quantity",
                reason: "must be at least 1".to_owned(),
            });
        }
        if default_quantity < 0 || default_quantity > max_quantity {
            return Err(CatalogError::InvalidRecord {
                field: "selection_option.default_quantity",
                reason: format!("must be between 0 and the maximum of {max_quantity}"),
            });
        }
        Ok(Self {
            option_id,
            group_id,
            name,
            component_ref,
            max_quantity,
            default_quantity,
            display_order,
            image,
        })
    }

    /// Whether choosing this costs money.
    pub const fn is_chargeable(&self) -> bool {
        self.component_ref.is_some()
    }
}

/// Check a whole snapshot's selection structure.
///
/// Called once, at commit, because these are the invariants no single chunk
/// can see: a group may arrive in one chunk and the option it hangs from in
/// another. What it refuses:
///
/// * duplicate group or option references,
/// * an option whose group does not exist,
/// * a group hanging from an option that does not exist,
/// * a group hanging from a product that does not exist,
/// * a chargeable option naming a product that does not exist,
/// * a cycle in the owner chain,
/// * defaults that do not add up to a configuration the group would accept.
///
/// It is a set of lookups, not a rules engine, and it stays that way.
pub fn validate_selection_structure(
    groups: &[SelectionGroup],
    options: &[SelectionOption],
    products: &[ProductRef],
) -> Result<(), CatalogError> {
    let mut by_group: BTreeMap<&SelectionGroupId, &SelectionGroup> = BTreeMap::new();
    for group in groups {
        if by_group.insert(&group.group_id, group).is_some() {
            return Err(CatalogError::DuplicateSelectionGroup {
                group_id: group.group_id.to_string(),
            });
        }
    }
    let mut by_option: BTreeMap<&SelectionOptionId, &SelectionOption> = BTreeMap::new();
    for option in options {
        if by_option.insert(&option.option_id, option).is_some() {
            return Err(CatalogError::DuplicateSelectionOption {
                option_id: option.option_id.to_string(),
            });
        }
    }
    let known_products: BTreeSet<&ProductRef> = products.iter().collect();

    for option in options {
        if !by_group.contains_key(&option.group_id) {
            return Err(CatalogError::UnknownSelectionGroup {
                group_id: option.group_id.to_string(),
            });
        }
        if let Some(component) = &option.component_ref {
            if !known_products.contains(component) {
                return Err(CatalogError::UnknownSelectionComponent {
                    option_id: option.option_id.to_string(),
                    product_ref: component.to_string(),
                });
            }
        }
    }

    for group in groups {
        match &group.owner {
            SelectionOwner::Product { product_ref } => {
                if !known_products.contains(product_ref) {
                    return Err(CatalogError::UnknownSelectionOwner {
                        group_id: group.group_id.to_string(),
                        owner: product_ref.to_string(),
                    });
                }
            }
            SelectionOwner::Option { option_id } => {
                if !by_option.contains_key(option_id) {
                    return Err(CatalogError::UnknownSelectionOwner {
                        group_id: group.group_id.to_string(),
                        owner: option_id.to_string(),
                    });
                }
            }
        }
    }

    // Walk every group up its owner chain. A product always terminates it, so
    // a chain that revisits a group is a cycle.
    for group in groups {
        let mut seen: BTreeSet<&SelectionGroupId> = BTreeSet::new();
        let mut current = group;
        loop {
            if !seen.insert(&current.group_id) {
                return Err(CatalogError::CyclicSelectionGroup {
                    group_id: group.group_id.to_string(),
                });
            }
            let Some(option_id) = current.owner.option_id() else {
                break;
            };
            // Both lookups are proven present by the checks above.
            let Some(option) = by_option.get(option_id) else {
                break;
            };
            let Some(parent) = by_group.get(&option.group_id) else {
                break;
            };
            current = parent;
        }
    }

    // A default configuration a group would itself refuse is a menu that
    // cannot be ordered without the customer fixing it first.
    let mut defaults: BTreeMap<&SelectionGroupId, i64> = BTreeMap::new();
    for option in options {
        *defaults.entry(&option.group_id).or_insert(0) += option.default_quantity;
    }
    for group in groups {
        let total = defaults.get(&group.group_id).copied().unwrap_or(0);
        if total > group.max_quantity {
            return Err(CatalogError::InvalidSelectionDefaults {
                group_id: group.group_id.to_string(),
                reason: format!(
                    "defaults add up to {total}, above the maximum of {}",
                    group.max_quantity
                ),
            });
        }
        // A required group whose defaults fall short is fine — the customer
        // is meant to choose. One that *exceeds* its own maximum is not.
    }
    Ok(())
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> LocalizedText {
        LocalizedText::new(value).expect("valid text")
    }

    fn product(reference: &str) -> ProductRef {
        ProductRef::new(reference).expect("valid product ref")
    }

    fn group(id: &str, owner: SelectionOwner, min: i64, max: i64) -> SelectionGroup {
        SelectionGroup::import(
            SelectionGroupId::new(id).expect("valid group id"),
            owner,
            text(id),
            min,
            max,
            0,
        )
        .expect("valid group")
    }

    fn on(product_ref: &str) -> SelectionOwner {
        SelectionOwner::Product {
            product_ref: product(product_ref),
        }
    }

    fn under(option_id: &str) -> SelectionOwner {
        SelectionOwner::Option {
            option_id: SelectionOptionId::new(option_id).expect("valid option id"),
        }
    }

    fn option(
        id: &str,
        group_id: &str,
        component: Option<&str>,
        max: i64,
        default: i64,
    ) -> SelectionOption {
        SelectionOption::import(
            SelectionOptionId::new(id).expect("valid option id"),
            SelectionGroupId::new(group_id).expect("valid group id"),
            text(id),
            component.map(product),
            max,
            default,
            0,
            None,
        )
        .expect("valid option")
    }

    #[test]
    fn a_group_that_accepts_nothing_is_not_a_question() {
        assert!(SelectionGroup::import(
            SelectionGroupId::new("G").unwrap(),
            on("P"),
            text("G"),
            0,
            0,
            0
        )
        .is_err());
        assert!(SelectionGroup::import(
            SelectionGroupId::new("G").unwrap(),
            on("P"),
            text("G"),
            -1,
            1,
            0
        )
        .is_err());
        assert!(
            SelectionGroup::import(
                SelectionGroupId::new("G").unwrap(),
                on("P"),
                text("G"),
                3,
                2,
                0
            )
            .is_err(),
            "a minimum above the maximum is impossible to satisfy"
        );
    }

    #[test]
    fn required_is_the_minimum_and_not_a_separate_flag() {
        assert!(group("SIZE", on("P"), 1, 1).is_required());
        assert!(!group("EXTRAS", on("P"), 0, 5).is_required());
    }

    #[test]
    fn an_option_cannot_default_to_more_than_it_allows() {
        assert!(SelectionOption::import(
            SelectionOptionId::new("O").unwrap(),
            SelectionGroupId::new("G").unwrap(),
            text("O"),
            None,
            1,
            2,
            0,
            None
        )
        .is_err());
        assert!(SelectionOption::import(
            SelectionOptionId::new("O").unwrap(),
            SelectionGroupId::new("G").unwrap(),
            text("O"),
            None,
            0,
            0,
            0,
            None
        )
        .is_err());
    }

    #[test]
    fn only_an_option_with_a_component_can_be_charged_for() {
        assert!(option("BACON", "EXTRAS", Some("P-BACON"), 2, 0).is_chargeable());
        assert!(!option("NO_ONION", "REMOVE", None, 1, 0).is_chargeable());
    }

    /// The whole restaurant shape, validated as one snapshot.
    #[test]
    fn a_real_menu_structure_validates() {
        let groups = vec![
            group("SIZE", on("P-BURGER"), 1, 1),
            group("EXTRAS", on("P-BURGER"), 0, 5),
            group("REMOVE", on("P-BURGER"), 0, 3),
            group("DRINK", on("P-COMBO"), 1, 1),
            // The follow-up question: chosen a Coke, now its size.
            group("COKE_SIZE", under("O-COKE"), 1, 1),
        ];
        let options = vec![
            option("O-SMALL", "SIZE", None, 1, 1),
            option("O-LARGE", "SIZE", Some("P-SIZE-UP"), 1, 0),
            option("O-BACON", "EXTRAS", Some("P-BACON"), 2, 0),
            option("O-AVOCADO", "EXTRAS", Some("P-AVOCADO"), 1, 0),
            // Default ingredient the customer may remove.
            option("O-NO-ONION", "REMOVE", None, 1, 0),
            option("O-COKE", "DRINK", Some("P-COKE"), 1, 0),
            option("O-JUICE", "DRINK", Some("P-JUICE"), 1, 0),
            option("O-COKE-L", "COKE_SIZE", Some("P-COKE-L"), 1, 1),
        ];
        let products: Vec<ProductRef> = [
            "P-BURGER",
            "P-COMBO",
            "P-SIZE-UP",
            "P-BACON",
            "P-AVOCADO",
            "P-COKE",
            "P-JUICE",
            "P-COKE-L",
        ]
        .iter()
        .map(|r| product(r))
        .collect();

        validate_selection_structure(&groups, &options, &products).expect("a valid menu");
    }

    #[test]
    fn duplicate_references_are_refused() {
        let products = vec![product("P")];
        let twice = vec![group("G", on("P"), 0, 1), group("G", on("P"), 0, 1)];
        assert_eq!(
            validate_selection_structure(&twice, &[], &products)
                .unwrap_err()
                .code(),
            "CATALOG_DUPLICATE_SELECTION_GROUP"
        );
        let groups = vec![group("G", on("P"), 0, 2)];
        let options = vec![option("O", "G", None, 1, 0), option("O", "G", None, 1, 0)];
        assert_eq!(
            validate_selection_structure(&groups, &options, &products)
                .unwrap_err()
                .code(),
            "CATALOG_DUPLICATE_SELECTION_OPTION"
        );
    }

    #[test]
    fn a_reference_to_something_absent_is_refused() {
        let products = vec![product("P")];
        // An option in no group.
        assert_eq!(
            validate_selection_structure(&[], &[option("O", "MISSING", None, 1, 0)], &products)
                .unwrap_err()
                .code(),
            "CATALOG_UNKNOWN_SELECTION_GROUP"
        );
        // A group hanging from an option that does not exist.
        assert_eq!(
            validate_selection_structure(&[group("G", under("MISSING"), 0, 1)], &[], &products)
                .unwrap_err()
                .code(),
            "CATALOG_UNKNOWN_SELECTION_OWNER"
        );
        // A group hanging from a product that does not exist.
        assert_eq!(
            validate_selection_structure(&[group("G", on("MISSING"), 0, 1)], &[], &products)
                .unwrap_err()
                .code(),
            "CATALOG_UNKNOWN_SELECTION_OWNER"
        );
        // A chargeable option naming a product that does not exist: the price
        // books would have nothing to price in milestone 13c.
        assert_eq!(
            validate_selection_structure(
                &[group("G", on("P"), 0, 1)],
                &[option("O", "G", Some("MISSING"), 1, 0)],
                &products
            )
            .unwrap_err()
            .code(),
            "CATALOG_UNKNOWN_SELECTION_COMPONENT"
        );
    }

    #[test]
    fn a_cycle_in_the_owner_chain_is_refused() {
        // G1 hangs from O2, which lives in G2, which hangs from O1, which
        // lives in G1. Nothing in that menu can ever be rendered.
        let groups = vec![
            group("G1", under("O2"), 0, 1),
            group("G2", under("O1"), 0, 1),
        ];
        let options = vec![
            option("O1", "G1", None, 1, 0),
            option("O2", "G2", None, 1, 0),
        ];
        assert_eq!(
            validate_selection_structure(&groups, &options, &[product("P")])
                .unwrap_err()
                .code(),
            "CATALOG_CYCLIC_SELECTION_GROUP"
        );
    }

    #[test]
    fn defaults_above_the_groups_own_maximum_are_refused() {
        let groups = vec![group("G", on("P"), 0, 1)];
        let options = vec![option("A", "G", None, 1, 1), option("B", "G", None, 1, 1)];
        assert_eq!(
            validate_selection_structure(&groups, &options, &[product("P")])
                .unwrap_err()
                .code(),
            "CATALOG_INVALID_SELECTION_DEFAULTS"
        );
    }

    #[test]
    fn a_required_group_may_leave_the_choice_to_the_customer() {
        // min 1 with no defaults: the client must ask. That is a menu, not an
        // error.
        let groups = vec![group("SIZE", on("P"), 1, 1)];
        let options = vec![
            option("S", "SIZE", None, 1, 0),
            option("L", "SIZE", None, 1, 0),
        ];
        validate_selection_structure(&groups, &options, &[product("P")]).expect("valid");
    }
}

/// One question a product asks, with the answers it accepts.
///
/// Borrowed rather than cloned: both the configuration query and `add_item`
/// build this from rows they already have in hand (ADR 046).
#[derive(Debug, Clone)]
pub struct ConfigurationGroup<'a> {
    pub group: &'a SelectionGroup,
    pub options: Vec<&'a SelectionOption>,
}

/// One answer a client says the customer chose. **Ids and quantities only** —
/// there is no price on it, because a client never states what a choice costs
/// (ADR 044, ADR 046).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedOption {
    pub option_id: SelectionOptionId,
    pub quantity: i64,
}

/// A validated choice: the group it answers, the option, and how many.
#[derive(Debug, Clone)]
pub struct SelectedOption<'a> {
    pub group: &'a SelectionGroup,
    pub option: &'a SelectionOption,
    pub quantity: i64,
}

/// Every question reachable from one product, in display order.
///
/// Walks the owner chain outward: the product's own groups, then the groups
/// that hang from the options of those groups, and so on — which is how a
/// drink's size question appears only under the drink. Cycles cannot occur
/// (the import refuses them), and the walk is bounded by the groups it has
/// not visited yet.
pub fn configuration_for<'a>(
    groups: &'a [SelectionGroup],
    options: &'a [SelectionOption],
    product_ref: &ProductRef,
) -> Vec<ConfigurationGroup<'a>> {
    let mut by_group: BTreeMap<&SelectionGroupId, Vec<&SelectionOption>> = BTreeMap::new();
    for option in options {
        by_group.entry(&option.group_id).or_default().push(option);
    }
    for entries in by_group.values_mut() {
        entries.sort_by(|left, right| {
            left.display_order
                .cmp(&right.display_order)
                .then_with(|| left.option_id.cmp(&right.option_id))
        });
    }

    let mut reached: Vec<&SelectionGroup> = Vec::new();
    let mut owners: Vec<SelectionOwner> = vec![SelectionOwner::Product {
        product_ref: product_ref.clone(),
    }];
    let mut seen: BTreeSet<&SelectionGroupId> = BTreeSet::new();
    while let Some(owner) = owners.pop() {
        let mut found: Vec<&SelectionGroup> = groups
            .iter()
            .filter(|group| group.owner == owner && seen.insert(&group.group_id))
            .collect();
        found.sort_by(|left, right| {
            left.display_order
                .cmp(&right.display_order)
                .then_with(|| left.group_id.cmp(&right.group_id))
        });
        for group in &found {
            for option in by_group.get(&group.group_id).into_iter().flatten() {
                owners.push(SelectionOwner::Option {
                    option_id: option.option_id.clone(),
                });
            }
        }
        reached.extend(found);
    }

    reached
        .into_iter()
        .map(|group| ConfigurationGroup {
            group,
            options: by_group.get(&group.group_id).cloned().unwrap_or_default(),
        })
        .collect()
}

/// Check what a client says the customer chose against what the product
/// actually asks.
///
/// Refuses an option this product does not offer, a quantity above what the
/// option allows, a group total outside its own range, a required question
/// left unanswered, and the same option twice. A nested question only counts
/// when the answer it hangs from was chosen — asking for a drink size with no
/// drink is not a rule to evaluate, it is an option this configuration does
/// not offer.
pub fn validate_selection_request<'a>(
    configuration: &'a [ConfigurationGroup<'a>],
    requested: &[RequestedOption],
) -> Result<Vec<SelectedOption<'a>>, CatalogError> {
    let mut chosen: BTreeMap<&SelectionOptionId, i64> = BTreeMap::new();
    for entry in requested {
        if entry.quantity < 1 {
            return Err(CatalogError::InvalidRecord {
                field: "selection.quantity",
                reason: format!("`{}` must be chosen at least once", entry.option_id),
            });
        }
        let known = configuration.iter().any(|group| {
            group
                .options
                .iter()
                .any(|option| option.option_id == entry.option_id)
        });
        if !known {
            return Err(CatalogError::OptionNotOffered {
                option_id: entry.option_id.to_string(),
            });
        }
        if chosen.insert(&entry.option_id, entry.quantity).is_some() {
            return Err(CatalogError::InvalidRecord {
                field: "selection.option_id",
                reason: format!("`{}` was chosen twice; use one quantity", entry.option_id),
            });
        }
    }

    // Only questions whose owner was answered are asked at all.
    let mut selected = Vec::new();
    for entry in configuration {
        if !is_asked(entry.group, &chosen) {
            for option in &entry.options {
                if chosen.contains_key(&option.option_id) {
                    return Err(CatalogError::OptionNotOffered {
                        option_id: option.option_id.to_string(),
                    });
                }
            }
            continue;
        }

        let mut total = 0_i64;
        for option in &entry.options {
            let quantity = match chosen.get(&option.option_id) {
                Some(quantity) => *quantity,
                None => continue,
            };
            if quantity > option.max_quantity {
                return Err(CatalogError::SelectionQuantityRefused {
                    reference: option.option_id.to_string(),
                    limit: option.max_quantity,
                    requested: quantity,
                });
            }
            total += quantity;
            selected.push(SelectedOption {
                group: entry.group,
                option,
                quantity,
            });
        }
        if total < entry.group.min_quantity {
            return Err(CatalogError::RequiredSelectionMissing {
                group_id: entry.group.group_id.to_string(),
                name: entry.group.name.default.clone(),
                minimum: entry.group.min_quantity,
            });
        }
        if total > entry.group.max_quantity {
            return Err(CatalogError::SelectionQuantityRefused {
                reference: entry.group.group_id.to_string(),
                limit: entry.group.max_quantity,
                requested: total,
            });
        }
    }
    Ok(selected)
}

/// Whether a group is in play: a product's own question always is, and a
/// nested one only once the option it hangs from was chosen.
fn is_asked(group: &SelectionGroup, chosen: &BTreeMap<&SelectionOptionId, i64>) -> bool {
    match &group.owner {
        SelectionOwner::Product { .. } => true,
        SelectionOwner::Option { option_id } => chosen.contains_key(option_id),
    }
}
