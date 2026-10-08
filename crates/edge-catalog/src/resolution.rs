//! Price and tax resolution.
//!
//! This is where a scanned code becomes a priced line. The authority is here,
//! in Rust, never in a client: a UI sends identity and quantity, and gets back
//! amounts it is only allowed to display.
//!
//! [`PriceResolver`] is the extension point. Today the only implementation
//! returns the product's base store price from the snapshot;
//! promotions, price lists and customer-specific pricing will be further
//! implementations, and the transaction aggregate will not change when they
//! arrive.

use edge_domain::prelude::{
    BusinessDayId, Currency, Money, PriceMode, ProductRef, Quantity, StoreId, TaxRate,
};

use crate::error::CatalogError;
use crate::product::{Barcode, CatalogProduct};
use crate::tax::TaxProfile;

/// Everything pricing is allowed to depend on.
#[derive(Debug, Clone, Copy)]
pub struct PricingContext<'a> {
    pub store_id: &'a StoreId,
    pub business_day_id: BusinessDayId,
    /// Version of the snapshot the sale captured, for auditability.
    pub snapshot_version: i64,
    pub quantity: Quantity,
    pub currency: Currency,
}

/// Resolves the unit price of a product in a context.
pub trait PriceResolver {
    fn unit_price(
        &self,
        product: &CatalogProduct,
        context: &PricingContext<'_>,
    ) -> Result<Money, CatalogError>;
}

/// The snapshot's base price for this store.
#[derive(Debug, Default, Clone, Copy)]
pub struct BasePriceResolver;

impl PriceResolver for BasePriceResolver {
    fn unit_price(
        &self,
        product: &CatalogProduct,
        context: &PricingContext<'_>,
    ) -> Result<Money, CatalogError> {
        if product.unit_price.currency() != context.currency {
            return Err(CatalogError::CurrencyMismatch {
                expected: context.currency.code(),
                actual: product.unit_price.currency().code(),
            });
        }
        Ok(product.unit_price)
    }
}

/// A line the transaction aggregate can price without knowing the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSaleLine {
    pub product_ref: ProductRef,
    pub sku: Option<String>,
    /// The code actually scanned, when the product was reached that way.
    pub barcode: Option<Barcode>,
    /// Product name as of the captured snapshot. Receipts must show what was
    /// sold, not what the product is called today.
    pub description: String,
    pub quantity: Quantity,
    pub unit_price: Money,
    pub tax_rate: TaxRate,
    pub price_mode: PriceMode,
    /// Version that priced this line, recorded in the journal.
    pub snapshot_version: i64,
}

/// Resolve one sale line from catalog data.
pub fn resolve_sale_line(
    product: &CatalogProduct,
    profile: &TaxProfile,
    scanned: Option<&Barcode>,
    resolver: &dyn PriceResolver,
    context: &PricingContext<'_>,
) -> Result<ResolvedSaleLine, CatalogError> {
    product.is_saleable()?;
    if product.tax_profile != profile.code {
        return Err(CatalogError::UnknownTaxProfile {
            product_ref: product.product_ref.to_string(),
            code: product.tax_profile.to_string(),
        });
    }
    let unit_price = resolver.unit_price(product, context)?;
    Ok(ResolvedSaleLine {
        product_ref: product.product_ref.clone(),
        sku: product.sku.clone(),
        barcode: scanned.cloned(),
        description: product.name.clone(),
        quantity: context.quantity,
        unit_price,
        tax_rate: profile.rate,
        price_mode: profile.price_mode,
        snapshot_version: context.snapshot_version,
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::NewCatalogProduct;
    use crate::tax::TaxProfileCode;
    use uuid::Uuid;

    fn profile(bps: u32, mode: PriceMode) -> TaxProfile {
        TaxProfile::new(
            TaxProfileCode::new("IVA16").unwrap(),
            "IVA",
            TaxRate::from_bps(bps).unwrap(),
            mode,
        )
        .unwrap()
    }

    fn product(price: i64, currency: Currency) -> CatalogProduct {
        CatalogProduct::import(NewCatalogProduct {
            product_ref: ProductRef::new("P-1").unwrap(),
            external_ref: None,
            sku: Some("SKU-1".to_owned()),
            name: "Leche 1L".to_owned(),
            active: true,
            sellable: true,
            tax_profile: TaxProfileCode::new("IVA16").unwrap(),
            unit_price: Money::from_minor(price, currency),
        })
        .unwrap()
    }

    fn context(store: &StoreId, quantity: i64) -> PricingContext<'_> {
        PricingContext {
            store_id: store,
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(1)),
            snapshot_version: 4,
            quantity: Quantity::from_units(quantity).unwrap(),
            currency: Currency::Mxn,
        }
    }

    #[test]
    fn resolves_identity_price_and_tax() {
        let store = StoreId::new("S-1").unwrap();
        let barcode = Barcode::new("7501").unwrap();
        let resolved = resolve_sale_line(
            &product(1_999, Currency::Mxn),
            &profile(1_600, PriceMode::TaxInclusive),
            Some(&barcode),
            &BasePriceResolver,
            &context(&store, 2),
        )
        .unwrap();

        assert_eq!(resolved.product_ref.as_str(), "P-1");
        assert_eq!(resolved.description, "Leche 1L");
        assert_eq!(resolved.sku.as_deref(), Some("SKU-1"));
        assert_eq!(resolved.barcode.as_ref(), Some(&barcode));
        assert_eq!(resolved.unit_price.minor(), 1_999);
        assert_eq!(resolved.tax_rate.bps(), 1_600);
        assert_eq!(resolved.price_mode, PriceMode::TaxInclusive);
        assert_eq!(resolved.snapshot_version, 4);
    }

    #[test]
    fn omits_the_barcode_when_the_product_was_not_scanned() {
        let store = StoreId::new("S-1").unwrap();
        let resolved = resolve_sale_line(
            &product(100, Currency::Mxn),
            &profile(0, PriceMode::TaxExclusive),
            None,
            &BasePriceResolver,
            &context(&store, 1),
        )
        .unwrap();
        assert!(resolved.barcode.is_none());
    }

    #[test]
    fn refuses_products_that_cannot_be_sold() {
        let store = StoreId::new("S-1").unwrap();
        let mut inactive = product(100, Currency::Mxn);
        inactive.active = false;
        assert!(matches!(
            resolve_sale_line(
                &inactive,
                &profile(0, PriceMode::TaxExclusive),
                None,
                &BasePriceResolver,
                &context(&store, 1)
            ),
            Err(CatalogError::ProductInactive { .. })
        ));
    }

    #[test]
    fn refuses_a_profile_that_is_not_the_products_own() {
        let store = StoreId::new("S-1").unwrap();
        let other = TaxProfile::new(
            TaxProfileCode::new("IVA8").unwrap(),
            "IVA 8%",
            TaxRate::from_bps(800).unwrap(),
            PriceMode::TaxExclusive,
        )
        .unwrap();
        assert!(matches!(
            resolve_sale_line(
                &product(100, Currency::Mxn),
                &other,
                None,
                &BasePriceResolver,
                &context(&store, 1)
            ),
            Err(CatalogError::UnknownTaxProfile { .. })
        ));
    }
}
