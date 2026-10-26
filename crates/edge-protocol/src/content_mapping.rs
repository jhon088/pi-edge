//! Conversions for the catalog content model.
//!
//! Enabled by the `catalog-mapping` feature. These convert between the wire
//! records and the pure value objects of `edge-catalog`/`edge-domain`, with no
//! application layer involved, so a *producer* of snapshots can build and hash
//! exactly what PI.Edge will verify.

use std::collections::BTreeMap;

use edge_catalog::channel::{
    AvailabilityDefault, AvailabilityState, AvailabilityTarget, Channel, ChannelAvailability,
};
use edge_catalog::presentation::{AssetRef, Category, Locale, LocalizedText, ProductPresentation};
use edge_catalog::pricing::{NewPriceBook, NewPriceEntry, PriceBookScope};
use edge_catalog::product::NewCatalogProduct;
use edge_catalog::promotions::{
    NewPromotion, ProductApplicability, Promotion, PromotionRule, Stackability,
};
use edge_catalog::scope::CatalogScope;
use edge_catalog::selection::{SelectionGroup, SelectionOption, SelectionOwner};
use edge_catalog::{
    Barcode, CatalogBarcode, CatalogError, CatalogProduct, CatalogSnapshot, ImportProgress,
    PriceBook, PriceEntry, ProductSelector, ResolvedPrice, SnapshotCounts, TaxProfile,
    TaxProfileCode,
};
use edge_domain::prelude::*;

use crate::dto::*;

impl From<&Money> for MoneyV1 {
    fn from(value: &Money) -> Self {
        Self {
            minor: value.minor(),
            currency: value.currency().code().to_owned(),
            formatted: value.format_amount(),
        }
    }
}

impl ProductSelectorV1 {
    pub fn into_domain(self) -> Result<ProductSelector, CatalogError> {
        Ok(match self {
            ProductSelectorV1::Barcode(value) => ProductSelector::Barcode(Barcode::new(value)?),
            ProductSelectorV1::Sku(value) => {
                let trimmed = value.trim().to_owned();
                if trimmed.is_empty() {
                    return Err(CatalogError::InvalidRecord {
                        field: "sku",
                        reason: "must not be empty".to_owned(),
                    });
                }
                ProductSelector::Sku(trimmed)
            }
            ProductSelectorV1::ProductRef(value) => {
                ProductSelector::ProductRef(ProductRef::new(value)?)
            }
        })
    }
}

impl From<&ProductSelector> for ProductSelectorV1 {
    fn from(selector: &ProductSelector) -> Self {
        match selector {
            ProductSelector::Barcode(value) => {
                ProductSelectorV1::Barcode(value.as_str().to_owned())
            }
            ProductSelector::Sku(value) => ProductSelectorV1::Sku(value.clone()),
            ProductSelector::ProductRef(value) => {
                ProductSelectorV1::ProductRef(value.as_str().to_owned())
            }
        }
    }
}

impl From<SnapshotCounts> for SnapshotCountsV1 {
    fn from(counts: SnapshotCounts) -> Self {
        Self {
            tax_profiles: counts.tax_profiles,
            price_books: counts.price_books,
            products: counts.products,
            price_entries: counts.price_entries,
            barcodes: counts.barcodes,
            promotions: counts.promotions,
            channels: counts.channels,
            categories: counts.categories,
            presentation: counts.presentation,
            selection_groups: counts.selection_groups,
            selection_options: counts.selection_options,
            availability: counts.availability,
        }
    }
}

impl From<SnapshotCountsV1> for SnapshotCounts {
    fn from(counts: SnapshotCountsV1) -> Self {
        Self {
            tax_profiles: counts.tax_profiles,
            price_books: counts.price_books,
            products: counts.products,
            price_entries: counts.price_entries,
            barcodes: counts.barcodes,
            promotions: counts.promotions,
            channels: counts.channels,
            categories: counts.categories,
            presentation: counts.presentation,
            selection_groups: counts.selection_groups,
            selection_options: counts.selection_options,
            availability: counts.availability,
        }
    }
}

// ------------------------------------------- universal catalog extensions
//
// Mirror conversions, both directions, so a producer builds exactly what
// PI.Edge verifies (ADR 044). Nothing here accepts a price or a delta: a
// chargeable option names a product and the price books do the rest.

impl From<&LocalizedText> for LocalizedTextV1 {
    fn from(text: &LocalizedText) -> Self {
        Self {
            default: text.default.clone(),
            translations: text
                .translations
                .iter()
                .map(|(locale, value)| (locale.as_str().to_owned(), value.clone()))
                .collect(),
        }
    }
}

impl LocalizedTextV1 {
    pub fn into_domain(self) -> Result<LocalizedText, CatalogError> {
        let mut translations = BTreeMap::new();
        for (locale, value) in self.translations {
            translations.insert(Locale::new(locale)?, value);
        }
        LocalizedText::with_translations(self.default, translations)
    }
}

impl From<&AssetRef> for AssetRefV1 {
    fn from(asset: &AssetRef) -> Self {
        Self {
            content_hash: asset.content_hash.clone(),
            media_type: asset.media_type.clone(),
            byte_size: asset.byte_size,
        }
    }
}

impl AssetRefV1 {
    pub fn into_domain(self) -> Result<AssetRef, CatalogError> {
        AssetRef::import(self.content_hash, self.media_type, self.byte_size)
    }
}

/// `Option<AssetRefV1>` into the domain, since every illustrated record needs
/// the same two lines.
fn asset(image: Option<AssetRefV1>) -> Result<Option<AssetRef>, CatalogError> {
    image.map(AssetRefV1::into_domain).transpose()
}

impl From<&Channel> for ChannelV1 {
    fn from(channel: &Channel) -> Self {
        Self {
            channel_ref: channel.channel_ref.to_string(),
            name: LocalizedTextV1::from(&channel.name),
            default_availability: channel.default_availability.as_str().to_owned(),
        }
    }
}

impl ChannelV1 {
    pub fn into_domain(self) -> Result<Channel, CatalogError> {
        Ok(Channel {
            channel_ref: ChannelRef::new(self.channel_ref)?,
            name: self.name.into_domain()?,
            default_availability: AvailabilityDefault::parse(&self.default_availability)?,
        })
    }
}

impl From<&Category> for CategoryV1 {
    fn from(category: &Category) -> Self {
        Self {
            category_ref: category.category_ref.to_string(),
            name: LocalizedTextV1::from(&category.name),
            display_order: category.display_order,
            image: category.image.as_ref().map(AssetRefV1::from),
        }
    }
}

impl CategoryV1 {
    pub fn into_domain(self) -> Result<Category, CatalogError> {
        Ok(Category::import(
            CategoryRef::new(self.category_ref)?,
            self.name.into_domain()?,
            self.display_order,
            asset(self.image)?,
        ))
    }
}

impl From<&ProductPresentation> for ProductPresentationV1 {
    fn from(presentation: &ProductPresentation) -> Self {
        Self {
            product_ref: presentation.product_ref.to_string(),
            category_ref: presentation.category_ref.as_ref().map(ToString::to_string),
            display_name: presentation
                .display_name
                .as_ref()
                .map(LocalizedTextV1::from),
            description: presentation.description.as_ref().map(LocalizedTextV1::from),
            display_order: presentation.display_order,
            image: presentation.image.as_ref().map(AssetRefV1::from),
            recommended_product_refs: presentation
                .recommended_product_refs
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

impl ProductPresentationV1 {
    pub fn into_domain(self) -> Result<ProductPresentation, CatalogError> {
        Ok(ProductPresentation {
            product_ref: ProductRef::new(self.product_ref)?,
            category_ref: self.category_ref.map(CategoryRef::new).transpose()?,
            display_name: self
                .display_name
                .map(LocalizedTextV1::into_domain)
                .transpose()?,
            description: self
                .description
                .map(LocalizedTextV1::into_domain)
                .transpose()?,
            display_order: self.display_order,
            image: asset(self.image)?,
            recommended_product_refs: self
                .recommended_product_refs
                .into_iter()
                .map(ProductRef::new)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl From<&SelectionGroup> for SelectionGroupV1 {
    fn from(group: &SelectionGroup) -> Self {
        let (owner_kind, owner_ref) = match &group.owner {
            SelectionOwner::Product { product_ref } => ("PRODUCT", product_ref.to_string()),
            SelectionOwner::Option { option_id } => ("OPTION", option_id.to_string()),
        };
        Self {
            group_id: group.group_id.to_string(),
            owner_kind: owner_kind.to_owned(),
            owner_ref,
            name: LocalizedTextV1::from(&group.name),
            min_quantity: group.min_quantity,
            max_quantity: group.max_quantity,
            display_order: group.display_order,
        }
    }
}

impl SelectionGroupV1 {
    pub fn into_domain(self) -> Result<SelectionGroup, CatalogError> {
        let owner = match self.owner_kind.as_str() {
            "PRODUCT" => SelectionOwner::Product {
                product_ref: ProductRef::new(self.owner_ref)?,
            },
            "OPTION" => SelectionOwner::Option {
                option_id: SelectionOptionId::new(self.owner_ref)?,
            },
            other => {
                return Err(CatalogError::InvalidRecord {
                    field: "selection_group.owner_kind",
                    reason: format!("`{other}` is not a selection owner"),
                })
            }
        };
        SelectionGroup::import(
            SelectionGroupId::new(self.group_id)?,
            owner,
            self.name.into_domain()?,
            self.min_quantity,
            self.max_quantity,
            self.display_order,
        )
    }
}

impl From<&SelectionOption> for SelectionOptionV1 {
    fn from(option: &SelectionOption) -> Self {
        Self {
            option_id: option.option_id.to_string(),
            group_id: option.group_id.to_string(),
            name: LocalizedTextV1::from(&option.name),
            component_product_ref: option.component_ref.as_ref().map(ToString::to_string),
            max_quantity: option.max_quantity,
            default_quantity: option.default_quantity,
            display_order: option.display_order,
            image: option.image.as_ref().map(AssetRefV1::from),
        }
    }
}

impl SelectionOptionV1 {
    pub fn into_domain(self) -> Result<SelectionOption, CatalogError> {
        SelectionOption::import(
            SelectionOptionId::new(self.option_id)?,
            SelectionGroupId::new(self.group_id)?,
            self.name.into_domain()?,
            self.component_product_ref
                .map(ProductRef::new)
                .transpose()?,
            self.max_quantity,
            self.default_quantity,
            self.display_order,
            asset(self.image)?,
        )
    }
}

impl From<&ChannelAvailability> for ChannelAvailabilityV1 {
    fn from(row: &ChannelAvailability) -> Self {
        Self {
            channel_ref: row.channel_ref.to_string(),
            target_kind: row.target.as_str().to_owned(),
            target_ref: row.target.reference().to_owned(),
            state: row.state.as_str().to_owned(),
            snoozed_until: row.state.snoozed_until().map(|until| until.to_rfc3339()),
        }
    }
}

impl ChannelAvailabilityV1 {
    pub fn into_domain(self) -> Result<ChannelAvailability, CatalogError> {
        let target = match self.target_kind.as_str() {
            "PRODUCT" => AvailabilityTarget::Product {
                product_ref: ProductRef::new(self.target_ref)?,
            },
            "CATEGORY" => AvailabilityTarget::Category {
                category_ref: CategoryRef::new(self.target_ref)?,
            },
            "SELECTION_OPTION" => AvailabilityTarget::SelectionOption {
                option_id: SelectionOptionId::new(self.target_ref)?,
            },
            other => {
                return Err(CatalogError::InvalidRecord {
                    field: "availability.target_kind",
                    reason: format!("`{other}` is not an availability target"),
                })
            }
        };
        let state = match (self.state.as_str(), self.snoozed_until.as_deref()) {
            ("OFFERED", None) => AvailabilityState::Offered,
            ("WITHHELD", None) => AvailabilityState::Withheld,
            ("SNOOZED", Some(until)) => AvailabilityState::Snoozed {
                until: parse_timestamp(until, "snoozed_until")?,
            },
            ("SNOOZED", None) => {
                return Err(CatalogError::InvalidRecord {
                    field: "availability.snoozed_until",
                    reason: "a snooze must say when it ends".to_owned(),
                })
            }
            (state @ ("OFFERED" | "WITHHELD"), Some(_)) => {
                return Err(CatalogError::InvalidRecord {
                    field: "availability.snoozed_until",
                    reason: format!("`{state}` does not end"),
                })
            }
            (other, _) => {
                return Err(CatalogError::InvalidRecord {
                    field: "availability.state",
                    reason: format!("`{other}` is not an availability state"),
                })
            }
        };
        Ok(ChannelAvailability {
            channel_ref: ChannelRef::new(self.channel_ref)?,
            target,
            state,
        })
    }
}

impl From<&PriceBook> for PriceBookV1 {
    fn from(book: &PriceBook) -> Self {
        Self {
            price_book_id: book.price_book_id.to_string(),
            code: book.code.clone(),
            name: book.name.clone(),
            currency: book.currency.code().to_owned(),
            scope: book.scope.as_str().to_owned(),
            store_id: book.scope.store_id().map(ToString::to_string),
            priority: book.priority,
            active: book.active,
        }
    }
}

impl PriceBookV1 {
    pub fn into_domain(self) -> Result<PriceBook, CatalogError> {
        let scope = match (self.scope.as_str(), self.store_id) {
            ("TENANT", None) => PriceBookScope::Tenant,
            ("TENANT", Some(_)) => {
                return Err(CatalogError::InvalidRecord {
                    field: "price_book_scope",
                    reason: "a tenant book must not name a store".to_owned(),
                })
            }
            ("STORE", Some(store_id)) => PriceBookScope::Store {
                store_id: StoreId::new(store_id)?,
            },
            ("STORE", None) => {
                return Err(CatalogError::InvalidRecord {
                    field: "price_book_scope",
                    reason: "a store book must name its store".to_owned(),
                })
            }
            (other, _) => {
                return Err(CatalogError::InvalidRecord {
                    field: "price_book_scope",
                    reason: format!("unknown scope `{other}`"),
                })
            }
        };
        PriceBook::import(NewPriceBook {
            price_book_id: PriceBookId::new(self.price_book_id)?,
            code: self.code,
            name: self.name,
            currency: Currency::from_code(&self.currency)?,
            scope,
            priority: self.priority,
            active: self.active,
        })
    }
}

impl From<&PriceEntry> for PriceEntryV1 {
    fn from(entry: &PriceEntry) -> Self {
        Self {
            price_book_id: entry.price_book_id.to_string(),
            product_ref: entry.product_ref.to_string(),
            amount: MoneyV1::from(&entry.amount),
            price_mode: entry.price_mode.as_str().to_owned(),
            valid_from: entry.valid_from.map(|ts| ts.to_rfc3339()),
            valid_until: entry.valid_until.map(|ts| ts.to_rfc3339()),
        }
    }
}

impl From<&ResolvedPrice> for ResolvedPriceV1 {
    fn from(price: &ResolvedPrice) -> Self {
        Self {
            price_book_id: price.price_book_id.to_string(),
            price_book_code: price.price_book_code.clone(),
            unit_price: MoneyV1::from(&price.unit_price),
            price_mode: price.price_mode.as_str().to_owned(),
            effective_at: price.effective_at.to_rfc3339(),
        }
    }
}

impl PriceRecordV1 {
    /// Validate one price for a product.
    pub fn into_domain(
        self,
        product_ref: &ProductRef,
        currency: Currency,
    ) -> Result<PriceEntry, CatalogError> {
        let valid_from = match self.valid_from.as_deref() {
            Some(value) => Some(parse_timestamp(value, "valid_from")?),
            None => None,
        };
        let valid_until = match self.valid_until.as_deref() {
            Some(value) => Some(parse_timestamp(value, "valid_until")?),
            None => None,
        };
        PriceEntry::import(NewPriceEntry {
            price_book_id: PriceBookId::new(self.price_book)?,
            product_ref: product_ref.clone(),
            amount: Money::from_minor(self.amount_minor, currency),
            price_mode: PriceMode::parse(&self.price_mode)?,
            valid_from,
            valid_until,
        })
    }
}

impl From<&TaxProfile> for TaxProfileV1 {
    fn from(profile: &TaxProfile) -> Self {
        Self {
            code: profile.code.as_str().to_owned(),
            name: profile.name.clone(),
            rate_bps: profile.rate.bps(),
        }
    }
}

impl From<&CatalogProduct> for CatalogProductV1 {
    fn from(product: &CatalogProduct) -> Self {
        Self {
            product_ref: product.product_ref.to_string(),
            external_ref: product.external_ref.clone(),
            sku: product.sku.clone(),
            name: product.name.clone(),
            active: product.active,
            sellable: product.sellable,
            tax_profile: product.tax_profile.to_string(),
        }
    }
}

impl From<&CatalogSnapshot> for CatalogSnapshotV1 {
    fn from(snapshot: &CatalogSnapshot) -> Self {
        let state = snapshot.state();
        Self {
            snapshot_id: state.snapshot_id.to_string(),
            store_id: state.store_id.to_string(),
            version: state.version,
            status: state.status.as_str().to_owned(),
            declared_content_hash: state.declared_content_hash.clone(),
            content_hash: state.content_hash.clone(),
            generated_at: state.generated_at.to_rfc3339(),
            effective_at: state.effective_at.map(|ts| ts.to_rfc3339()),
            expected: SnapshotCountsV1::from(state.expected),
            imported_at: state.imported_at.to_rfc3339(),
            committed_at: state.committed_at.map(|ts| ts.to_rfc3339()),
            activated_at: state.activated_at.map(|ts| ts.to_rfc3339()),
            superseded_at: state.superseded_at.map(|ts| ts.to_rfc3339()),
        }
    }
}

impl From<&ImportProgress> for ImportProgressV1 {
    fn from(progress: &ImportProgress) -> Self {
        Self {
            snapshot_id: progress.snapshot_id.to_string(),
            status: progress.status.as_str().to_owned(),
            version: progress.version,
            received_chunks: progress.received_chunks.clone(),
            received_records: progress.received_records,
            next_chunk_index: progress.next_chunk_index(),
            expected: SnapshotCountsV1::from(progress.expected),
            stored: SnapshotCountsV1::from(progress.stored),
        }
    }
}

impl TaxProfileV1 {
    pub fn into_domain(self) -> Result<TaxProfile, CatalogError> {
        TaxProfile::new(
            TaxProfileCode::new(self.code)?,
            self.name,
            TaxRate::from_bps(self.rate_bps)?,
        )
    }
}

impl CatalogProductRecordV1 {
    /// Validate one record into a product plus its barcodes.
    ///
    /// Currency comes from the device: a snapshot belongs to one store, and the
    /// store's currency is not a per-record choice.
    pub fn into_domain(self, currency: Currency) -> Result<ProductRecord, CatalogError> {
        let product_ref = ProductRef::new(self.product_ref)?;
        let product = CatalogProduct::import(NewCatalogProduct {
            product_ref: product_ref.clone(),
            external_ref: self.external_ref,
            sku: self.sku,
            name: self.name,
            active: self.active,
            sellable: self.sellable,
            tax_profile: TaxProfileCode::new(self.tax_profile)?,
        })?;
        let barcodes = self
            .barcodes
            .into_iter()
            .map(|code| {
                Ok(CatalogBarcode {
                    barcode: Barcode::new(code)?,
                    product_ref: product_ref.clone(),
                })
            })
            .collect::<Result<Vec<_>, CatalogError>>()?;
        let prices = self
            .prices
            .into_iter()
            .map(|price| price.into_domain(&product_ref, currency))
            .collect::<Result<Vec<_>, CatalogError>>()?;
        Ok(ProductRecord {
            product,
            prices,
            barcodes,
        })
    }
}

pub(crate) fn parse_snapshot_id(value: &str) -> Result<CatalogSnapshotId, CatalogError> {
    value.parse::<CatalogSnapshotId>().map_err(CatalogError::Id)
}

pub(crate) fn parse_timestamp(value: &str, field: &'static str) -> Result<Timestamp, CatalogError> {
    Timestamp::parse_rfc3339(value).map_err(|_| CatalogError::InvalidRecord {
        field,
        reason: format!("`{value}` is not an RFC-3339 UTC timestamp"),
    })
}

impl CatalogSnapshotRequestV1 {
    pub fn snapshot_id(&self) -> Result<CatalogSnapshotId, CatalogError> {
        parse_snapshot_id(&self.snapshot_id)
    }
}

/// One validated product record: the product itself plus the prices and codes
/// that travel with it.
#[derive(Debug, Clone)]
pub struct ProductRecord {
    pub product: CatalogProduct,
    pub prices: Vec<PriceEntry>,
    pub barcodes: Vec<CatalogBarcode>,
}

// ---------------------------------------------------------------- promotions

impl From<&Promotion> for PromotionRecordV1 {
    fn from(promotion: &Promotion) -> Self {
        Self {
            promotion_id: promotion.promotion_id.to_string(),
            code: promotion.code.clone(),
            name: promotion.name.clone(),
            active: promotion.active,
            priority: promotion.priority,
            stackability: promotion.stackability.as_str().to_owned(),
            scope: promotion.scope.as_str().to_owned(),
            store_id: promotion.scope.store_id().map(ToString::to_string),
            valid_from: promotion.valid_from.map(|ts| ts.to_rfc3339()),
            valid_until: promotion.valid_until.map(|ts| ts.to_rfc3339()),
            rule: PromotionRuleV1::from(&promotion.rule),
        }
    }
}

impl From<&PromotionRule> for PromotionRuleV1 {
    fn from(rule: &PromotionRule) -> Self {
        let product_ref = rule
            .applicability()
            .and_then(|applies| applies.product_ref())
            .map(ToString::to_string);
        match rule {
            PromotionRule::PercentageOff { percentage, .. } => PromotionRuleV1::PercentageOff {
                product_ref,
                percentage_bps: percentage.bps(),
            },
            PromotionRule::FixedAmountOff { amount, .. } => PromotionRuleV1::FixedAmountOff {
                product_ref,
                amount_minor: amount.minor(),
            },
            PromotionRule::BuyNGetM { buy, get, .. } => PromotionRuleV1::BuyNGetM {
                product_ref,
                buy_thousandths: buy.thousandths(),
                get_thousandths: get.thousandths(),
            },
            PromotionRule::BasketThreshold { minimum, amount } => {
                PromotionRuleV1::BasketThreshold {
                    minimum_minor: minimum.minor(),
                    amount_minor: amount.minor(),
                }
            }
        }
    }
}

impl PromotionRecordV1 {
    /// Validate and convert. The currency comes from the import header, never
    /// from the record, so a chunk cannot smuggle in a second currency.
    pub fn into_domain(self, currency: Currency) -> Result<Promotion, CatalogError> {
        let scope = match (self.scope.as_str(), self.store_id) {
            ("TENANT", None) => CatalogScope::Tenant,
            ("TENANT", Some(_)) => {
                return Err(CatalogError::InvalidRecord {
                    field: "promotion_scope",
                    reason: "a tenant promotion must not name a store".to_owned(),
                })
            }
            ("STORE", Some(store_id)) => CatalogScope::Store {
                store_id: StoreId::new(store_id)?,
            },
            ("STORE", None) => {
                return Err(CatalogError::InvalidRecord {
                    field: "promotion_scope",
                    reason: "a store promotion must name its store".to_owned(),
                })
            }
            (other, _) => {
                return Err(CatalogError::InvalidRecord {
                    field: "promotion_scope",
                    reason: format!("unknown scope `{other}`"),
                })
            }
        };
        let rule = self.rule.into_domain(currency)?;
        Promotion::import(NewPromotion {
            promotion_id: PromotionId::new(self.promotion_id)?,
            code: self.code,
            name: self.name,
            active: self.active,
            priority: self.priority,
            stackability: Stackability::parse(&self.stackability)?,
            scope,
            valid_from: self
                .valid_from
                .as_deref()
                .map(|value| parse_timestamp(value, "valid_from"))
                .transpose()?,
            valid_until: self
                .valid_until
                .as_deref()
                .map(|value| parse_timestamp(value, "valid_until"))
                .transpose()?,
            rule,
        })
    }
}

impl PromotionRuleV1 {
    pub fn into_domain(self, currency: Currency) -> Result<PromotionRule, CatalogError> {
        let applicability = |product_ref: Option<String>| -> Result<_, CatalogError> {
            Ok(match product_ref {
                Some(value) => ProductApplicability::Product {
                    product_ref: ProductRef::new(value)?,
                },
                None => ProductApplicability::AnyProduct,
            })
        };
        Ok(match self {
            PromotionRuleV1::PercentageOff {
                product_ref,
                percentage_bps,
            } => PromotionRule::PercentageOff {
                applies_to: applicability(product_ref)?,
                percentage: Percentage::from_bps(percentage_bps)?,
            },
            PromotionRuleV1::FixedAmountOff {
                product_ref,
                amount_minor,
            } => PromotionRule::FixedAmountOff {
                applies_to: applicability(product_ref)?,
                amount: Money::from_minor(amount_minor, currency),
            },
            PromotionRuleV1::BuyNGetM {
                product_ref,
                buy_thousandths,
                get_thousandths,
            } => PromotionRule::BuyNGetM {
                applies_to: applicability(product_ref)?,
                buy: Quantity::from_thousandths(buy_thousandths)?,
                get: Quantity::from_thousandths(get_thousandths)?,
            },
            PromotionRuleV1::BasketThreshold {
                minimum_minor,
                amount_minor,
            } => PromotionRule::BasketThreshold {
                minimum: Money::from_minor(minimum_minor, currency),
                amount: Money::from_minor(amount_minor, currency),
            },
        })
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn text(default: &str) -> LocalizedTextV1 {
        LocalizedTextV1 {
            default: default.to_owned(),
            translations: BTreeMap::new(),
        }
    }

    fn availability(state: &str, until: Option<&str>) -> ChannelAvailabilityV1 {
        ChannelAvailabilityV1 {
            channel_ref: "KIOSK".to_owned(),
            target_kind: "PRODUCT".to_owned(),
            target_ref: "P-1".to_owned(),
            state: state.to_owned(),
            snoozed_until: until.map(str::to_owned),
        }
    }

    /// A state and its deadline must agree: a snooze with no end never lifts,
    /// and an end on a permanent state is a producer saying two things.
    #[test]
    fn an_availability_state_and_its_deadline_must_agree() {
        let valid = availability("SNOOZED", Some("2026-10-23T18:00:00Z"))
            .into_domain()
            .unwrap();
        assert!(valid.state.snoozed_until().is_some());
        assert!(availability("OFFERED", None).into_domain().is_ok());
        assert!(availability("WITHHELD", None).into_domain().is_ok());

        for wrong in [
            availability("SNOOZED", None),
            availability("OFFERED", Some("2026-10-23T18:00:00Z")),
            availability("WITHHELD", Some("2026-10-23T18:00:00Z")),
            availability("SOLD_OUT", None),
        ] {
            assert!(wrong.into_domain().is_err());
        }
    }

    /// Every record round-trips: what a producer builds from the domain is
    /// what the core parses back, so the hash the producer declares is the
    /// hash the core computes.
    #[test]
    fn the_extension_records_round_trip() {
        let channel = ChannelV1 {
            channel_ref: "KIOSK".to_owned(),
            name: LocalizedTextV1 {
                default: "Kiosko".to_owned(),
                translations: BTreeMap::from([("en".to_owned(), "Kiosk".to_owned())]),
            },
            default_availability: "WITHHELD".to_owned(),
        };
        assert_eq!(
            ChannelV1::from(&channel.clone().into_domain().unwrap()),
            channel
        );

        let category = CategoryV1 {
            category_ref: "C-1".to_owned(),
            name: text("Hamburguesas"),
            display_order: 3,
            image: Some(AssetRefV1 {
                content_hash: format!("sha256:{}", "a".repeat(64)),
                media_type: "image/webp".to_owned(),
                byte_size: 1_024,
            }),
        };
        assert_eq!(
            CategoryV1::from(&category.clone().into_domain().unwrap()),
            category
        );

        let presentation = ProductPresentationV1 {
            product_ref: "P-1".to_owned(),
            category_ref: Some("C-1".to_owned()),
            display_name: Some(text("Clásica")),
            description: None,
            display_order: 1,
            image: None,
            // Round-tripped in declared order, not sorted: what a screen
            // offers first is a merchandising decision.
            recommended_product_refs: vec!["P-9".to_owned(), "P-3".to_owned()],
        };
        assert_eq!(
            ProductPresentationV1::from(&presentation.clone().into_domain().unwrap()),
            presentation
        );

        let nested = SelectionGroupV1 {
            group_id: "G-2".to_owned(),
            owner_kind: "OPTION".to_owned(),
            owner_ref: "O-1".to_owned(),
            name: text("Tamaño"),
            min_quantity: 1,
            max_quantity: 1,
            display_order: 0,
        };
        assert_eq!(
            SelectionGroupV1::from(&nested.clone().into_domain().unwrap()),
            nested
        );

        let option = SelectionOptionV1 {
            option_id: "O-1".to_owned(),
            group_id: "G-1".to_owned(),
            name: text("Tocino"),
            component_product_ref: Some("BACON".to_owned()),
            max_quantity: 2,
            default_quantity: 1,
            display_order: 4,
            image: None,
        };
        assert_eq!(
            SelectionOptionV1::from(&option.clone().into_domain().unwrap()),
            option
        );

        let row = availability("SNOOZED", Some("2026-10-23T18:00:00.000000Z"));
        assert_eq!(
            ChannelAvailabilityV1::from(&row.clone().into_domain().unwrap()),
            row
        );
    }

    /// An unknown owner or target kind is refused, not guessed.
    #[test]
    fn an_unknown_kind_is_refused() {
        assert!(SelectionGroupV1 {
            group_id: "G-1".to_owned(),
            owner_kind: "MENU".to_owned(),
            owner_ref: "M-1".to_owned(),
            name: text("Qué"),
            min_quantity: 1,
            max_quantity: 1,
            display_order: 0,
        }
        .into_domain()
        .is_err());
        assert!(ChannelAvailabilityV1 {
            target_kind: "PRICE_BOOK".to_owned(),
            ..availability("OFFERED", None)
        }
        .into_domain()
        .is_err());
    }

    /// Localized text keeps no price, no html, nothing but text — and its
    /// translations survive both directions in locale order.
    #[test]
    fn translations_round_trip_in_locale_order() {
        let wire = LocalizedTextV1 {
            default: "Refresco".to_owned(),
            translations: BTreeMap::from([
                ("fr".to_owned(), "Soda".to_owned()),
                ("en".to_owned(), "Soft drink".to_owned()),
            ]),
        };
        let domain = wire.clone().into_domain().unwrap();
        assert_eq!(
            domain
                .translations
                .keys()
                .map(|locale| locale.as_str())
                .collect::<Vec<_>>(),
            vec!["en", "fr"]
        );
        assert_eq!(LocalizedTextV1::from(&domain), wire);
    }
}
