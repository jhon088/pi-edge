//! Turning a scanned code into a priced line.
//!
//! The authority is here, in Rust, never in a client: a UI sends identity and
//! quantity, and gets back amounts it is only allowed to display.
//!
//! Price resolution itself lives in [`crate::pricing`]; this module composes a
//! product, its tax profile and the resolved price into the single input the
//! transaction aggregate needs. Promotions will compose over
//! [`crate::pricing::ResolvedPrice`] without this shape changing.

use edge_domain::prelude::{
    Money, PriceBookId, PriceMode, ProductRef, Quantity, TaxRate, Timestamp,
};

use crate::error::CatalogError;
use crate::pricing::{PricingContext, ResolvedPrice};
use crate::product::{Barcode, CatalogProduct};
use crate::tax::TaxProfile;

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
    /// Book that produced the price, recorded in the journal.
    pub price_book_id: PriceBookId,
    /// Instant the price was resolved at.
    pub priced_at: Timestamp,
}

/// Compose one sale line from catalog data and a resolved price.
pub fn resolve_sale_line(
    product: &CatalogProduct,
    profile: &TaxProfile,
    price: &ResolvedPrice,
    scanned: Option<&Barcode>,
    context: &PricingContext<'_>,
) -> Result<ResolvedSaleLine, CatalogError> {
    product.is_saleable()?;
    if product.tax_profile != profile.code {
        return Err(CatalogError::UnknownTaxProfile {
            product_ref: product.product_ref.to_string(),
            code: product.tax_profile.to_string(),
        });
    }
    if price.unit_price.currency() != context.currency {
        return Err(CatalogError::CurrencyMismatch {
            expected: context.currency.code(),
            actual: price.unit_price.currency().code(),
        });
    }
    Ok(ResolvedSaleLine {
        product_ref: product.product_ref.clone(),
        sku: product.sku.clone(),
        barcode: scanned.cloned(),
        description: product.name.clone(),
        quantity: context.quantity,
        unit_price: price.unit_price,
        tax_rate: profile.rate,
        price_mode: price.price_mode,
        price_book_id: price.price_book_id.clone(),
        priced_at: price.effective_at,
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{
        resolve_price, NewPriceBook, NewPriceEntry, PriceBook, PriceBookScope, PriceCandidate,
        PriceEntry,
    };
    use crate::product::NewCatalogProduct;
    use crate::tax::TaxProfileCode;
    use edge_domain::prelude::{CatalogSnapshotId, Currency, StoreId};
    use uuid::Uuid;

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T12:00:00Z").unwrap()
    }

    fn profile(bps: u32) -> TaxProfile {
        TaxProfile::new(
            TaxProfileCode::new("IVA16").unwrap(),
            "IVA",
            TaxRate::from_bps(bps).unwrap(),
        )
        .unwrap()
    }

    fn product() -> CatalogProduct {
        CatalogProduct::import(NewCatalogProduct {
            product_ref: ProductRef::new("P-1").unwrap(),
            external_ref: None,
            sku: Some("SKU-1".to_owned()),
            name: "Leche 1L".to_owned(),
            active: true,
            sellable: true,
            tax_profile: TaxProfileCode::new("IVA16").unwrap(),
        })
        .unwrap()
    }

    fn candidates(minor: i64, mode: PriceMode) -> (PriceBook, Vec<PriceCandidate>) {
        let book = PriceBook::import(NewPriceBook {
            price_book_id: PriceBookId::new("BASE").unwrap(),
            code: "BASE".to_owned(),
            name: "Base".to_owned(),
            currency: Currency::Mxn,
            scope: PriceBookScope::Tenant,
            priority: 0,
            active: true,
        })
        .unwrap();
        let entry = PriceEntry::import(NewPriceEntry {
            price_book_id: book.price_book_id.clone(),
            product_ref: ProductRef::new("P-1").unwrap(),
            amount: Money::from_minor(minor, Currency::Mxn),
            price_mode: mode,
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
        (book.clone(), vec![PriceCandidate { book, entry }])
    }

    #[test]
    fn composes_identity_price_and_tax() {
        let store = StoreId::new("S-1").unwrap();
        let reference = ProductRef::new("P-1").unwrap();
        let context = PricingContext {
            store_id: &store,
            product_ref: &reference,
            effective_at: ts(),
            snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(1)),
            quantity: Quantity::from_units(2).unwrap(),
            currency: Currency::Mxn,
        };
        let (_, candidates) = candidates(1_999, PriceMode::TaxInclusive);
        let price = resolve_price(&candidates, &context).unwrap();
        let barcode = Barcode::new("7501").unwrap();

        let line = resolve_sale_line(
            &product(),
            &profile(1_600),
            &price,
            Some(&barcode),
            &context,
        )
        .unwrap();

        assert_eq!(line.description, "Leche 1L");
        assert_eq!(line.unit_price.minor(), 1_999);
        assert_eq!(line.price_mode, PriceMode::TaxInclusive);
        assert_eq!(line.tax_rate.bps(), 1_600);
        assert_eq!(line.price_book_id.as_str(), "BASE");
        assert_eq!(line.priced_at, ts());
        assert_eq!(line.barcode.as_ref(), Some(&barcode));
        assert_eq!(line.quantity.thousandths(), 2_000);
    }

    #[test]
    fn refuses_products_that_cannot_be_sold() {
        let store = StoreId::new("S-1").unwrap();
        let reference = ProductRef::new("P-1").unwrap();
        let context = PricingContext {
            store_id: &store,
            product_ref: &reference,
            effective_at: ts(),
            snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(1)),
            quantity: Quantity::from_units(1).unwrap(),
            currency: Currency::Mxn,
        };
        let (_, candidates) = candidates(100, PriceMode::TaxExclusive);
        let price = resolve_price(&candidates, &context).unwrap();

        let mut inactive = product();
        inactive.active = false;
        assert!(matches!(
            resolve_sale_line(&inactive, &profile(0), &price, None, &context),
            Err(CatalogError::ProductInactive { .. })
        ));

        let mut not_sellable = product();
        not_sellable.sellable = false;
        assert!(matches!(
            resolve_sale_line(&not_sellable, &profile(0), &price, None, &context),
            Err(CatalogError::ProductNotSellable { .. })
        ));
    }

    #[test]
    fn refuses_a_profile_that_is_not_the_products_own() {
        let store = StoreId::new("S-1").unwrap();
        let reference = ProductRef::new("P-1").unwrap();
        let context = PricingContext {
            store_id: &store,
            product_ref: &reference,
            effective_at: ts(),
            snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(1)),
            quantity: Quantity::from_units(1).unwrap(),
            currency: Currency::Mxn,
        };
        let (_, candidates) = candidates(100, PriceMode::TaxExclusive);
        let price = resolve_price(&candidates, &context).unwrap();
        let other = TaxProfile::new(
            TaxProfileCode::new("IVA8").unwrap(),
            "IVA 8%",
            TaxRate::from_bps(800).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            resolve_sale_line(&product(), &other, &price, None, &context),
            Err(CatalogError::UnknownTaxProfile { .. })
        ));
    }
}
