//! Kiosk use cases: the menu, one product's questions, and the choices a
//! customer made (ADR 046).
//!
//! All of it is *reads of the active snapshot* plus the pricing primitives
//! that already exist. There is no kiosk pricing engine, no kiosk promotion
//! engine and no kiosk transaction: a configured line is an ordinary line
//! whose unit price happens to include the choices, and everything downstream
//! — totals, promotions, payments, receipts, refunds — is untouched.

use std::collections::BTreeSet;

use edge_catalog::{
    configuration_for, resolve_price, validate_selection_request, AvailabilityIndex,
    AvailabilityTarget, CatalogSnapshot, ConfigurationGroup, PricingContext, RequestedOption,
};
use edge_domain::prelude::*;
use edge_payments::PaymentProviderPort;

use crate::error::AppError;
use crate::ports::{Clock, DeviceProfile, EdgeStore, IdFactory, UnitOfWork};
use crate::queries::{
    MenuCategoryView, MenuItemView, MenuPromotionView, MenuView, ProductConfigurationView,
    SelectionGroupView, SelectionOptionView,
};
use crate::EdgeApplication;

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: PaymentProviderPort,
{
    /// Everything a kiosk screen needs to draw the menu, from the snapshot in
    /// force: categories in display order, their items with a "from" price
    /// and a photo reference, and the promotions a basket may earn.
    ///
    /// One call rather than twenty: a kiosk draws its whole home screen at
    /// once, and a round trip per product would make a touch feel slow for no
    /// architectural gain.
    pub fn get_menu(&self, channel_ref: Option<ChannelRef>) -> Result<MenuView, AppError> {
        let now = self.clock.now();
        self.store().transact(|uow| {
            let device = crate::context::require_device(uow)?;
            let snapshot =
                uow.find_active_catalog_snapshot(&device.store_id)?
                    .ok_or(AppError::Catalog(
                        edge_catalog::CatalogError::NoActiveSnapshot,
                    ))?;
            let snapshot_id = snapshot.id();

            let channels = uow.list_catalog_channels(snapshot_id)?;
            let availability = uow.list_catalog_availability(snapshot_id)?;
            let index = AvailabilityIndex::build(&channels, &availability);
            // A snapshot that declares no channel filters nothing — that is
            // every retail catalog, and asking it about a channel it never
            // heard of must not empty the menu (ADR 044).
            let channel_ref = channel_ref.filter(|_| !index.is_empty());

            let categories = uow.list_catalog_categories(snapshot_id)?;
            let presentation = uow.list_catalog_presentation(snapshot_id)?;
            let groups = uow.list_catalog_selection_groups(snapshot_id)?;
            let options = uow.list_catalog_selection_options(snapshot_id)?;

            let mut items = Vec::with_capacity(presentation.len());
            for shown in &presentation {
                let product = match uow.lookup_catalog_product(
                    snapshot_id,
                    &edge_catalog::ProductSelector::ProductRef(shown.product_ref.clone()),
                )? {
                    Some(product) if product.is_saleable().is_ok() => product,
                    // A product with presentation that cannot be sold is not a
                    // menu item. Nothing is invented to fill the gap.
                    _ => continue,
                };
                // Commercial availability, per channel. A product withheld for
                // this channel is simply not on this channel's menu (ADR 044).
                if let Some(channel) = channel_ref.as_ref() {
                    if !index.offers_product(
                        channel,
                        &shown.product_ref,
                        shown.category_ref.as_ref(),
                        now,
                    ) {
                        continue;
                    }
                }

                let price = self.price_of(uow, &device, snapshot_id, &shown.product_ref, now)?;
                // "From" means: this is what it costs before any choice. A
                // required choice that costs money makes the real price
                // higher, and the configuration screen says so.
                let configurable =
                    !configuration_for(&groups, &options, &shown.product_ref).is_empty();
                items.push(MenuItemView {
                    product_ref: shown.product_ref.clone(),
                    category_ref: shown.category_ref.clone(),
                    name: shown
                        .display_name
                        .as_ref()
                        .map(|text| text.default.clone())
                        .unwrap_or_else(|| product.name.clone()),
                    description: shown.description.as_ref().map(|text| text.default.clone()),
                    display_order: shown.display_order,
                    image: shown.image.clone(),
                    from_price: price,
                    configurable,
                    recommended_product_refs: shown.recommended_product_refs.clone(),
                });
            }
            // A suggestion the same menu cannot render is not a suggestion.
            // Withheld for this channel, unsaleable, or simply not browsable:
            // whichever it is, the item never made it into `items`, so drop
            // it here rather than leave every client to discover it.
            let shown_refs: BTreeSet<ProductRef> =
                items.iter().map(|item| item.product_ref.clone()).collect();
            for item in &mut items {
                item.recommended_product_refs
                    .retain(|recommended| shown_refs.contains(recommended));
            }
            items.sort_by(|left, right| {
                left.display_order
                    .cmp(&right.display_order)
                    .then_with(|| left.product_ref.cmp(&right.product_ref))
            });

            let mut menu_categories: Vec<MenuCategoryView> = categories
                .into_iter()
                .filter(|category| match channel_ref.as_ref() {
                    Some(channel) => index.offers(
                        channel,
                        &AvailabilityTarget::Category {
                            category_ref: category.category_ref.clone(),
                        },
                        now,
                    ),
                    None => true,
                })
                .map(|category| MenuCategoryView {
                    category_ref: category.category_ref,
                    name: category.name.default,
                    display_order: category.display_order,
                    image: category.image,
                })
                .collect();
            menu_categories.sort_by(|left, right| {
                left.display_order
                    .cmp(&right.display_order)
                    .then_with(|| left.category_ref.cmp(&right.category_ref))
            });

            // What a basket can earn, so the screen can say so before the
            // customer has chosen anything. The amounts stay the engine's.
            let promotions = uow
                .promotion_candidates(snapshot_id, &device.store_id)?
                .into_iter()
                .filter(|promotion| promotion.is_in_force_at(now))
                .map(|promotion| MenuPromotionView {
                    promotion_id: promotion.promotion_id,
                    code: promotion.code,
                    name: promotion.name,
                })
                .collect();

            Ok(MenuView {
                snapshot_id,
                currency: device.currency,
                categories: menu_categories,
                items,
                promotions,
            })
        })
    }

    /// The questions one product asks, with each answer's own price.
    ///
    /// Nested questions are returned flat, each carrying the option it hangs
    /// from, so a client renders the nesting it is told about rather than
    /// inferring one.
    pub fn get_product_configuration(
        &self,
        product_ref: ProductRef,
        channel_ref: Option<ChannelRef>,
    ) -> Result<ProductConfigurationView, AppError> {
        let now = self.clock.now();
        self.store().transact(|uow| {
            let device = crate::context::require_device(uow)?;
            let snapshot =
                uow.find_active_catalog_snapshot(&device.store_id)?
                    .ok_or(AppError::Catalog(
                        edge_catalog::CatalogError::NoActiveSnapshot,
                    ))?;
            let snapshot_id = snapshot.id();
            let product = uow
                .lookup_catalog_product(
                    snapshot_id,
                    &edge_catalog::ProductSelector::ProductRef(product_ref.clone()),
                )?
                .ok_or_else(|| {
                    AppError::Catalog(edge_catalog::CatalogError::ProductNotFound {
                        selector: product_ref.to_string(),
                    })
                })?;

            let channels = uow.list_catalog_channels(snapshot_id)?;
            let availability = uow.list_catalog_availability(snapshot_id)?;
            let index = AvailabilityIndex::build(&channels, &availability);
            let channel_ref = channel_ref.filter(|_| !index.is_empty());
            let groups = uow.list_catalog_selection_groups(snapshot_id)?;
            let options = uow.list_catalog_selection_options(snapshot_id)?;
            let configuration = configuration_for(&groups, &options, &product_ref);

            let base_price = self.price_of(uow, &device, snapshot_id, &product_ref, now)?;
            let mut views = Vec::with_capacity(configuration.len());
            for entry in &configuration {
                let mut option_views = Vec::with_capacity(entry.options.len());
                for option in &entry.options {
                    if let Some(channel) = channel_ref.as_ref() {
                        if !index.offers(
                            channel,
                            &AvailabilityTarget::SelectionOption {
                                option_id: option.option_id.clone(),
                            },
                            now,
                        ) {
                            continue;
                        }
                    }
                    let unit_price = match &option.component_ref {
                        Some(component) => {
                            self.price_of(uow, &device, snapshot_id, component, now)?
                        }
                        // Not a product, so not a price. A semantic choice is
                        // free by construction, not by a zero somebody typed.
                        None => Money::zero(device.currency),
                    };
                    option_views.push(SelectionOptionView {
                        option_id: option.option_id.clone(),
                        name: option.name.default.clone(),
                        component_ref: option.component_ref.clone(),
                        unit_price,
                        max_quantity: option.max_quantity,
                        default_quantity: option.default_quantity,
                        display_order: option.display_order,
                        image: option.image.clone(),
                    });
                }
                views.push(SelectionGroupView {
                    group_id: entry.group.group_id.clone(),
                    owner_option_id: match &entry.group.owner {
                        edge_catalog::SelectionOwner::Option { option_id } => {
                            Some(option_id.clone())
                        }
                        edge_catalog::SelectionOwner::Product { .. } => None,
                    },
                    name: entry.group.name.default.clone(),
                    min_quantity: entry.group.min_quantity,
                    max_quantity: entry.group.max_quantity,
                    display_order: entry.group.display_order,
                    options: option_views,
                });
            }

            Ok(ProductConfigurationView {
                product_ref,
                name: product.name,
                base_price,
                groups: views,
            })
        })
    }

    /// Turn the ids a client sent into materialised, priced selections.
    ///
    /// Everything a client may say is here: which option, how many, and a
    /// note. Everything it may *not* say — the price of a choice, the total,
    /// the discount — is resolved here from the snapshot this sale captured
    /// (ADR 046).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_configuration(
        &self,
        uow: &mut dyn UnitOfWork,
        device: &DeviceProfile,
        snapshot: &CatalogSnapshot,
        capture: &PricingCapture,
        product_ref: &ProductRef,
        base_unit_price: Money,
        requested: &[RequestedOption],
        preparation_note: Option<String>,
    ) -> Result<Option<LineConfiguration>, AppError> {
        let groups = uow.list_catalog_selection_groups(snapshot.id())?;
        let options = uow.list_catalog_selection_options(snapshot.id())?;
        let configuration: Vec<ConfigurationGroup<'_>> =
            configuration_for(&groups, &options, product_ref);

        if configuration.is_empty() {
            if let Some(first) = requested.first() {
                return Err(AppError::Catalog(
                    edge_catalog::CatalogError::OptionNotOffered {
                        option_id: first.option_id.to_string(),
                    },
                ));
            }
            // A note on a product that asks nothing is still a note.
            return Ok(preparation_note.map(|note| LineConfiguration {
                base_unit_price,
                selections: Vec::new(),
                preparation_note: Some(note),
            }));
        }

        let selected =
            validate_selection_request(&configuration, requested).map_err(AppError::Catalog)?;
        let mut selections = Vec::with_capacity(selected.len());
        for choice in selected {
            let unit_price = match &choice.option.component_ref {
                Some(component) => self.price_at(uow, device, snapshot, capture, component)?,
                None => Money::zero(device.currency),
            };
            let quantity = Quantity::from_units(choice.quantity).map_err(DomainError::from)?;
            selections.push(TransactionSelection {
                option_id: choice.option.option_id.clone(),
                group_id: choice.group.group_id.clone(),
                group_name: choice.group.name.default.clone(),
                name: choice.option.name.default.clone(),
                quantity: choice.quantity,
                component_ref: choice.option.component_ref.clone(),
                unit_price,
                amount: unit_price
                    .mul_quantity(quantity)
                    .map_err(DomainError::from)?,
            });
        }
        Ok(Some(LineConfiguration {
            base_unit_price,
            selections,
            preparation_note,
        }))
    }

    /// One product's price, at an instant, through the price books.
    fn price_of(
        &self,
        uow: &mut dyn UnitOfWork,
        device: &DeviceProfile,
        snapshot_id: CatalogSnapshotId,
        product_ref: &ProductRef,
        effective_at: Timestamp,
    ) -> Result<Money, AppError> {
        let candidates = uow.price_candidates(snapshot_id, &device.store_id, product_ref)?;
        let context = PricingContext {
            store_id: &device.store_id,
            product_ref,
            effective_at,
            snapshot_id,
            quantity: Quantity::from_units(1).map_err(DomainError::from)?,
            currency: device.currency,
        };
        Ok(resolve_price(&candidates, &context)?.unit_price)
    }

    /// Same, pinned to the instant the sale captured, so a price change
    /// cannot move a choice between two taps (ADR 013).
    fn price_at(
        &self,
        uow: &mut dyn UnitOfWork,
        device: &DeviceProfile,
        snapshot: &CatalogSnapshot,
        capture: &PricingCapture,
        product_ref: &ProductRef,
    ) -> Result<Money, AppError> {
        self.price_of(
            uow,
            device,
            snapshot.id(),
            product_ref,
            capture.effective_at,
        )
    }
}

/// A short order number a customer can read out: `K01-0042`.
///
/// Derived, never stored. The register and the sale's own sequence on it are
/// already durable, so there is no counter to keep, no gap to explain and
/// nothing to recover after a crash. Simple on purpose — a demo-grade order
/// number, not a fiscal folio (ADR 046).
pub fn order_code(register_id: &RegisterId, register_sequence: i64) -> String {
    let digits: String = register_id
        .as_str()
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let lane = digits
        .parse::<u32>()
        .map(|lane| lane % 100)
        .unwrap_or(1)
        .max(1);
    format!("K{lane:02}-{:04}", register_sequence % 10_000)
}
