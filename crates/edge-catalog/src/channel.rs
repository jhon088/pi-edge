//! Where something is offered, which is not the same as whether there is any.
//!
//! Two questions that look alike and must never share a model:
//!
//! * **Commercial availability** — "is this offered here, on this channel,
//!   right now?" That is catalog content, decided upstream, and it lives in
//!   this file.
//! * **Inventory availability** — "is there any left?" That is the ledger's
//!   answer, derived from movements this installation recorded (ADR 029).
//!
//! A product with stock can be off the menu for the kiosk, and a product that
//! ran out today is still on tomorrow's menu. Conflating them would make a
//! sold-out item disappear from the catalog and a de-listed item look like a
//! stock problem (ADR 044).
//!
//! A `ChannelRef` is opaque. The core never branches on `"kiosk"` or `"pos"`:
//! it asks the catalog what a channel offers and does what it is told, which
//! is the only way one core serves retail, restaurant and venue without
//! knowing which it is running.

use std::collections::{BTreeMap, BTreeSet};

use edge_domain::prelude::{CategoryRef, ChannelRef, ProductRef, SelectionOptionId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::presentation::LocalizedText;

/// What a channel does about things nobody said anything about.
///
/// Declared per channel, as data, so absence is never ambiguous. An opt-out
/// channel offers everything except what is explicitly withdrawn; an opt-in
/// channel offers only what is explicitly listed — which is what a new
/// self-checkout lane wants on its first day, when "sell everything by
/// default" would be the wrong answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AvailabilityDefault {
    /// Available unless a row withdraws it.
    Offered,
    /// Unavailable unless a row grants it.
    Withheld,
}

impl AvailabilityDefault {
    pub const fn as_str(self) -> &'static str {
        match self {
            AvailabilityDefault::Offered => "OFFERED",
            AvailabilityDefault::Withheld => "WITHHELD",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CatalogError> {
        match value {
            "OFFERED" => Ok(AvailabilityDefault::Offered),
            "WITHHELD" => Ok(AvailabilityDefault::Withheld),
            other => Err(CatalogError::InvalidRecord {
                field: "channel.default_availability",
                reason: format!("`{other}` is not an availability default"),
            }),
        }
    }
}

/// A way of selling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub channel_ref: ChannelRef,
    pub name: LocalizedText,
    pub default_availability: AvailabilityDefault,
}

/// What an availability row talks about.
///
/// Categories are included because withdrawing a whole section is how a menu
/// is actually managed, and options because a sold-out extra must disappear
/// from the question without disappearing from the catalog.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AvailabilityTarget {
    Product { product_ref: ProductRef },
    Category { category_ref: CategoryRef },
    SelectionOption { option_id: SelectionOptionId },
}

impl AvailabilityTarget {
    pub const fn as_str(&self) -> &'static str {
        match self {
            AvailabilityTarget::Product { .. } => "PRODUCT",
            AvailabilityTarget::Category { .. } => "CATEGORY",
            AvailabilityTarget::SelectionOption { .. } => "SELECTION_OPTION",
        }
    }

    /// The reference, for storage and for hashing.
    pub fn reference(&self) -> &str {
        match self {
            AvailabilityTarget::Product { product_ref } => product_ref.as_str(),
            AvailabilityTarget::Category { category_ref } => category_ref.as_str(),
            AvailabilityTarget::SelectionOption { option_id } => option_id.as_str(),
        }
    }
}

/// What a channel says about one target.
///
/// `Snoozed` is a withdrawal with an end: the kitchen ran out of avocado and
/// said so until closing time. It is deliberately a timestamp and not a
/// schedule — a recurring daypart would need a calendar the core has no
/// consumer for yet, and this contract does not stand in its way: a future
/// rule becomes one more variant, and every reader already has to match on
/// this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AvailabilityState {
    Offered,
    Withheld,
    /// Withheld until this instant, then offered again.
    Snoozed {
        until: Timestamp,
    },
}

impl AvailabilityState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            AvailabilityState::Offered => "OFFERED",
            AvailabilityState::Withheld => "WITHHELD",
            AvailabilityState::Snoozed { .. } => "SNOOZED",
        }
    }

    pub const fn snoozed_until(&self) -> Option<Timestamp> {
        match self {
            AvailabilityState::Snoozed { until } => Some(*until),
            _ => None,
        }
    }

    /// Whether this state offers the target at `at`.
    pub fn offers(&self, at: Timestamp) -> bool {
        match self {
            AvailabilityState::Offered => true,
            AvailabilityState::Withheld => false,
            AvailabilityState::Snoozed { until } => at >= *until,
        }
    }
}

/// One channel's answer about one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelAvailability {
    pub channel_ref: ChannelRef,
    pub target: AvailabilityTarget,
    pub state: AvailabilityState,
}

/// Everything a resolution needs, indexed once.
///
/// Built from the rows of one snapshot and then queried per product. A client
/// rendering a menu resolves hundreds of products against one of these, which
/// is why it is a map and not a scan.
#[derive(Debug, Clone, Default)]
pub struct AvailabilityIndex {
    channels: BTreeMap<ChannelRef, AvailabilityDefault>,
    rows: BTreeMap<(ChannelRef, String, String), AvailabilityState>,
}

impl AvailabilityIndex {
    pub fn build(channels: &[Channel], rows: &[ChannelAvailability]) -> Self {
        Self {
            channels: channels
                .iter()
                .map(|channel| (channel.channel_ref.clone(), channel.default_availability))
                .collect(),
            rows: rows
                .iter()
                .map(|row| {
                    (
                        (
                            row.channel_ref.clone(),
                            row.target.as_str().to_owned(),
                            row.target.reference().to_owned(),
                        ),
                        row.state,
                    )
                })
                .collect(),
        }
    }

    /// Whether this snapshot has a channel dimension at all.
    ///
    /// A retail catalog declares no channels, and then nothing is filtered:
    /// the behaviour every existing installation already has.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    fn state_of(&self, channel: &ChannelRef, target: &AvailabilityTarget) -> AvailabilityState {
        self.rows
            .get(&(
                channel.clone(),
                target.as_str().to_owned(),
                target.reference().to_owned(),
            ))
            .copied()
            .unwrap_or(match self.channels.get(channel) {
                Some(AvailabilityDefault::Withheld) => AvailabilityState::Withheld,
                // An unknown channel offers nothing: a device configured for a
                // channel this snapshot never heard of must not sell
                // everything by accident.
                None => AvailabilityState::Withheld,
                Some(AvailabilityDefault::Offered) => AvailabilityState::Offered,
            })
    }

    /// Whether one target is offered on this channel at `at`.
    pub fn offers(&self, channel: &ChannelRef, target: &AvailabilityTarget, at: Timestamp) -> bool {
        self.state_of(channel, target).offers(at)
    }

    /// Whether a product is offered, taking its category with it.
    ///
    /// Withdrawing a section withdraws what is in it: a category nobody
    /// offers cannot be browsed into, so a product inside it is not offered
    /// either, whatever its own row says.
    pub fn offers_product(
        &self,
        channel: &ChannelRef,
        product_ref: &ProductRef,
        category_ref: Option<&CategoryRef>,
        at: Timestamp,
    ) -> bool {
        if self.is_empty() {
            return true;
        }
        let product = AvailabilityTarget::Product {
            product_ref: product_ref.clone(),
        };
        if !self.offers(channel, &product, at) {
            return false;
        }
        match category_ref {
            Some(category_ref) => self.offers(
                channel,
                &AvailabilityTarget::Category {
                    category_ref: category_ref.clone(),
                },
                at,
            ),
            None => true,
        }
    }
}

/// Check a whole snapshot's availability rows.
///
/// Refuses duplicate channels, duplicate rows for the same channel and target,
/// rows naming a channel the snapshot never declared, and a snooze with no
/// end. Targets are *not* checked against products and categories here: that
/// is the import's job, where every chunk has arrived.
pub fn validate_channels(
    channels: &[Channel],
    rows: &[ChannelAvailability],
) -> Result<(), CatalogError> {
    let mut declared: BTreeMap<&ChannelRef, ()> = BTreeMap::new();
    for channel in channels {
        if declared.insert(&channel.channel_ref, ()).is_some() {
            return Err(CatalogError::DuplicateChannel {
                channel_ref: channel.channel_ref.to_string(),
            });
        }
    }
    let mut seen: BTreeMap<(&ChannelRef, &str, &str), ()> = BTreeMap::new();
    for row in rows {
        if !declared.contains_key(&row.channel_ref) {
            return Err(CatalogError::UnknownChannel {
                channel_ref: row.channel_ref.to_string(),
            });
        }
        let key = (
            &row.channel_ref,
            row.target.as_str(),
            row.target.reference(),
        );
        if seen.insert(key, ()).is_some() {
            return Err(CatalogError::DuplicateAvailability {
                channel_ref: row.channel_ref.to_string(),
                target: format!("{}:{}", row.target.as_str(), row.target.reference()),
            });
        }
    }
    Ok(())
}

/// Resolve every availability target against the snapshot.
///
/// Separate from [`validate_channels`] because it needs the whole snapshot:
/// a row may legitimately name a product that arrived in another chunk, so
/// this can only be answered once every chunk is in.
pub fn validate_availability_targets(
    rows: &[ChannelAvailability],
    products: &[ProductRef],
    categories: &[CategoryRef],
    options: &[SelectionOptionId],
) -> Result<(), CatalogError> {
    let products: BTreeSet<&str> = products.iter().map(ProductRef::as_str).collect();
    let categories: BTreeSet<&str> = categories.iter().map(CategoryRef::as_str).collect();
    let options: BTreeSet<&str> = options.iter().map(SelectionOptionId::as_str).collect();
    for row in rows {
        let known = match &row.target {
            AvailabilityTarget::Product { .. } => products.contains(row.target.reference()),
            AvailabilityTarget::Category { .. } => categories.contains(row.target.reference()),
            AvailabilityTarget::SelectionOption { .. } => options.contains(row.target.reference()),
        };
        if !known {
            return Err(CatalogError::UnknownAvailabilityTarget {
                target: format!("{}:{}", row.target.as_str(), row.target.reference()),
            });
        }
    }
    Ok(())
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn channel(reference: &str, default: AvailabilityDefault) -> Channel {
        Channel {
            channel_ref: ChannelRef::new(reference).expect("valid channel"),
            name: LocalizedText::new(reference).expect("valid text"),
            default_availability: default,
        }
    }

    fn reference(value: &str) -> ChannelRef {
        ChannelRef::new(value).expect("valid channel")
    }

    fn product(value: &str) -> ProductRef {
        ProductRef::new(value).expect("valid product")
    }

    fn category(value: &str) -> CategoryRef {
        CategoryRef::new(value).expect("valid category")
    }

    fn at(seconds: i64) -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T12:00:00Z")
            .expect("valid instant")
            .plus_seconds(seconds)
    }

    fn row(channel: &str, product_ref: &str, state: AvailabilityState) -> ChannelAvailability {
        ChannelAvailability {
            channel_ref: reference(channel),
            target: AvailabilityTarget::Product {
                product_ref: product(product_ref),
            },
            state,
        }
    }

    /// The case the whole model exists for.
    #[test]
    fn one_product_is_offered_on_some_channels_and_withheld_on_others() {
        let channels = vec![
            channel("pos", AvailabilityDefault::Offered),
            channel("kiosk", AvailabilityDefault::Offered),
            channel("sco", AvailabilityDefault::Offered),
        ];
        // Only the exception is listed: P is withdrawn from self-checkout.
        let rows = vec![row("sco", "P", AvailabilityState::Withheld)];
        validate_channels(&channels, &rows).expect("valid");
        let index = AvailabilityIndex::build(&channels, &rows);

        assert!(index.offers_product(&reference("pos"), &product("P"), None, at(0)));
        assert!(index.offers_product(&reference("kiosk"), &product("P"), None, at(0)));
        assert!(!index.offers_product(&reference("sco"), &product("P"), None, at(0)));
    }

    #[test]
    fn an_opt_in_channel_offers_only_what_it_was_told_to() {
        let channels = vec![channel("sco", AvailabilityDefault::Withheld)];
        let rows = vec![row("sco", "ALLOWED", AvailabilityState::Offered)];
        let index = AvailabilityIndex::build(&channels, &rows);
        assert!(index.offers_product(&reference("sco"), &product("ALLOWED"), None, at(0)));
        assert!(
            !index.offers_product(&reference("sco"), &product("OTHER"), None, at(0)),
            "absence resolves to the channel's declared default, never to a guess"
        );
    }

    #[test]
    fn a_snooze_ends_by_itself() {
        let channels = vec![channel("kiosk", AvailabilityDefault::Offered)];
        let rows = vec![row(
            "kiosk",
            "AVOCADO",
            AvailabilityState::Snoozed { until: at(100) },
        )];
        let index = AvailabilityIndex::build(&channels, &rows);
        assert!(!index.offers_product(&reference("kiosk"), &product("AVOCADO"), None, at(0)));
        assert!(!index.offers_product(&reference("kiosk"), &product("AVOCADO"), None, at(99)));
        assert!(index.offers_product(&reference("kiosk"), &product("AVOCADO"), None, at(100)));
    }

    #[test]
    fn withdrawing_a_category_withdraws_what_is_in_it() {
        let channels = vec![channel("kiosk", AvailabilityDefault::Offered)];
        let rows = vec![ChannelAvailability {
            channel_ref: reference("kiosk"),
            target: AvailabilityTarget::Category {
                category_ref: category("ALCOHOL"),
            },
            state: AvailabilityState::Withheld,
        }];
        let index = AvailabilityIndex::build(&channels, &rows);
        assert!(!index.offers_product(
            &reference("kiosk"),
            &product("BEER"),
            Some(&category("ALCOHOL")),
            at(0)
        ));
        assert!(index.offers_product(
            &reference("kiosk"),
            &product("BEER"),
            Some(&category("FOOD")),
            at(0)
        ));
    }

    /// Retail, unchanged: a catalog with no channels filters nothing.
    #[test]
    fn a_catalog_with_no_channels_offers_everything() {
        let index = AvailabilityIndex::build(&[], &[]);
        assert!(index.is_empty());
        assert!(index.offers_product(&reference("anything"), &product("P"), None, at(0)));
    }

    #[test]
    fn a_channel_the_snapshot_never_declared_offers_nothing() {
        let channels = vec![channel("pos", AvailabilityDefault::Offered)];
        let index = AvailabilityIndex::build(&channels, &[]);
        assert!(index.offers_product(&reference("pos"), &product("P"), None, at(0)));
        assert!(
            !index.offers_product(&reference("kiosk"), &product("P"), None, at(0)),
            "a device pointed at an unknown channel must not sell the whole catalog"
        );
    }

    #[test]
    fn selection_options_can_be_withdrawn_too() {
        let channels = vec![channel("kiosk", AvailabilityDefault::Offered)];
        let option = AvailabilityTarget::SelectionOption {
            option_id: SelectionOptionId::new("O-AVOCADO").expect("valid option"),
        };
        let rows = vec![ChannelAvailability {
            channel_ref: reference("kiosk"),
            target: option.clone(),
            state: AvailabilityState::Withheld,
        }];
        let index = AvailabilityIndex::build(&channels, &rows);
        assert!(!index.offers(&reference("kiosk"), &option, at(0)));
    }

    #[test]
    fn duplicates_and_unknown_channels_are_refused() {
        let channels = vec![
            channel("pos", AvailabilityDefault::Offered),
            channel("pos", AvailabilityDefault::Withheld),
        ];
        assert_eq!(
            validate_channels(&channels, &[]).unwrap_err().code(),
            "CATALOG_DUPLICATE_CHANNEL"
        );

        let channels = vec![channel("pos", AvailabilityDefault::Offered)];
        assert_eq!(
            validate_channels(&channels, &[row("kiosk", "P", AvailabilityState::Offered)])
                .unwrap_err()
                .code(),
            "CATALOG_UNKNOWN_CHANNEL"
        );
        assert_eq!(
            validate_channels(
                &channels,
                &[
                    row("pos", "P", AvailabilityState::Offered),
                    row("pos", "P", AvailabilityState::Withheld),
                ]
            )
            .unwrap_err()
            .code(),
            "CATALOG_DUPLICATE_AVAILABILITY"
        );
    }

    #[test]
    fn a_target_the_snapshot_does_not_contain_is_refused() {
        let products = vec![product("P-1")];
        let categories = vec![category("C-1")];
        let options = vec![SelectionOptionId::new("O-1").expect("valid option")];
        let known = [
            row("pos", "P-1", AvailabilityState::Offered),
            ChannelAvailability {
                channel_ref: reference("pos"),
                target: AvailabilityTarget::Category {
                    category_ref: category("C-1"),
                },
                state: AvailabilityState::Withheld,
            },
            ChannelAvailability {
                channel_ref: reference("pos"),
                target: AvailabilityTarget::SelectionOption {
                    option_id: SelectionOptionId::new("O-1").expect("valid option"),
                },
                state: AvailabilityState::Withheld,
            },
        ];
        assert!(validate_availability_targets(&known, &products, &categories, &options).is_ok());

        // Each kind is resolved against its own set: a product reference that
        // happens to match a category name is still an unknown product.
        for stray in [
            row("pos", "P-2", AvailabilityState::Offered),
            ChannelAvailability {
                channel_ref: reference("pos"),
                target: AvailabilityTarget::Category {
                    category_ref: category("P-1"),
                },
                state: AvailabilityState::Withheld,
            },
        ] {
            assert_eq!(
                validate_availability_targets(&[stray], &products, &categories, &options)
                    .unwrap_err()
                    .code(),
                "CATALOG_UNKNOWN_AVAILABILITY_TARGET"
            );
        }
    }
}
