//! Products and barcodes as projected locally.
//!
//! Identity is `product_ref`: an opaque reference assigned upstream. PI.Edge
//! never mints one. `external_ref` exists only to carry the source system's own
//! code for traceability; it is metadata, never operational identity.

use edge_domain::prelude::{Money, ProductRef};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::tax::TaxProfileCode;

/// Longest barcode we accept. EAN/UPC/GS1 are far shorter; the bound exists to
/// keep a malformed import from poisoning an index.
const MAX_BARCODE_LEN: usize = 64;
const MAX_NAME_LEN: usize = 512;
const MAX_SKU_LEN: usize = 128;

/// A scannable code. Compared byte for byte: no normalisation, no check-digit
/// arithmetic, no guessing about symbologies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Barcode(String);

impl Barcode {
    pub fn new(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "barcode",
                reason: "must not be empty".to_owned(),
            });
        }
        if trimmed.chars().count() > MAX_BARCODE_LEN {
            return Err(CatalogError::InvalidRecord {
                field: "barcode",
                reason: format!("exceeds {MAX_BARCODE_LEN} characters"),
            });
        }
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(CatalogError::InvalidRecord {
                field: "barcode",
                reason: "must not contain control characters or whitespace".to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Barcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One product, as of one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogProduct {
    /// Canonical identity, assigned upstream.
    pub product_ref: ProductRef,
    /// Source-system code. Metadata for traceability only.
    pub external_ref: Option<String>,
    pub sku: Option<String>,
    pub name: String,
    /// Inactive products stay in the snapshot for historical lookups but
    /// cannot be sold.
    pub active: bool,
    /// Active but non-sellable products exist (services, components).
    pub sellable: bool,
    pub tax_profile: TaxProfileCode,
    /// Base price for this store, in the profile's price mode.
    pub unit_price: Money,
}

/// Input shape for an imported product record, validated into
/// [`CatalogProduct`].
#[derive(Debug, Clone)]
pub struct NewCatalogProduct {
    pub product_ref: ProductRef,
    pub external_ref: Option<String>,
    pub sku: Option<String>,
    pub name: String,
    pub active: bool,
    pub sellable: bool,
    pub tax_profile: TaxProfileCode,
    pub unit_price: Money,
}

impl CatalogProduct {
    pub fn import(new: NewCatalogProduct) -> Result<Self, CatalogError> {
        let name = new.name.trim().to_owned();
        if name.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "name",
                reason: "must not be empty".to_owned(),
            });
        }
        if name.chars().count() > MAX_NAME_LEN {
            return Err(CatalogError::InvalidRecord {
                field: "name",
                reason: format!("exceeds {MAX_NAME_LEN} characters"),
            });
        }
        if new.unit_price.is_negative() {
            return Err(CatalogError::InvalidRecord {
                field: "unit_price",
                reason: "must not be negative".to_owned(),
            });
        }
        let sku = match new.sku {
            Some(sku) => {
                let trimmed = sku.trim().to_owned();
                if trimmed.is_empty() {
                    None
                } else if trimmed.chars().count() > MAX_SKU_LEN {
                    return Err(CatalogError::InvalidRecord {
                        field: "sku",
                        reason: format!("exceeds {MAX_SKU_LEN} characters"),
                    });
                } else {
                    Some(trimmed)
                }
            }
            None => None,
        };
        let external_ref = new
            .external_ref
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        Ok(Self {
            product_ref: new.product_ref,
            external_ref,
            sku,
            name,
            active: new.active,
            sellable: new.sellable,
            tax_profile: new.tax_profile,
            unit_price: new.unit_price,
        })
    }

    /// Whether this product may be added to a sale.
    pub fn is_saleable(&self) -> Result<(), CatalogError> {
        if !self.active {
            return Err(CatalogError::ProductInactive {
                product_ref: self.product_ref.to_string(),
            });
        }
        if !self.sellable {
            return Err(CatalogError::ProductNotSellable {
                product_ref: self.product_ref.to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogBarcode {
    pub barcode: Barcode,
    pub product_ref: ProductRef,
}

/// How a client asks for a product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "value", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProductSelector {
    Barcode(Barcode),
    Sku(String),
    ProductRef(ProductRef),
}

impl std::fmt::Display for ProductSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProductSelector::Barcode(value) => write!(f, "barcode `{value}`"),
            ProductSelector::Sku(value) => write!(f, "sku `{value}`"),
            ProductSelector::ProductRef(value) => write!(f, "product `{value}`"),
        }
    }
}

impl ProductSelector {
    /// The barcode the operator actually scanned, when that is how we got here.
    /// Recorded on the sale line for the receipt and the journal.
    pub fn scanned_barcode(&self) -> Option<&Barcode> {
        match self {
            ProductSelector::Barcode(barcode) => Some(barcode),
            _ => None,
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::Currency;

    fn profile() -> TaxProfileCode {
        TaxProfileCode::new("IVA16").unwrap()
    }

    fn product(name: &str, price: i64) -> NewCatalogProduct {
        NewCatalogProduct {
            product_ref: ProductRef::new("P-1").unwrap(),
            external_ref: Some("  ERP-9  ".to_owned()),
            sku: Some(" SKU-1 ".to_owned()),
            name: name.to_owned(),
            active: true,
            sellable: true,
            tax_profile: profile(),
            unit_price: Money::from_minor(price, Currency::Mxn),
        }
    }

    #[test]
    fn validates_barcodes() {
        assert_eq!(
            Barcode::new(" 7501234567890 ").unwrap().as_str(),
            "7501234567890"
        );
        assert!(Barcode::new("").is_err());
        assert!(Barcode::new("with space").is_err());
        assert!(Barcode::new("with\tcontrol").is_err());
        assert!(Barcode::new("9".repeat(MAX_BARCODE_LEN + 1)).is_err());
    }

    #[test]
    fn trims_and_keeps_metadata() {
        let imported = CatalogProduct::import(product("  Leche 1L ", 1_999)).unwrap();
        assert_eq!(imported.name, "Leche 1L");
        assert_eq!(imported.sku.as_deref(), Some("SKU-1"));
        assert_eq!(imported.external_ref.as_deref(), Some("ERP-9"));
    }

    #[test]
    fn rejects_invalid_records() {
        assert!(CatalogProduct::import(product("   ", 100)).is_err());
        assert!(CatalogProduct::import(product("ok", -1)).is_err());
        let mut long = product("ok", 1);
        long.name = "x".repeat(MAX_NAME_LEN + 1);
        assert!(CatalogProduct::import(long).is_err());
    }

    #[test]
    fn blocks_inactive_and_non_sellable_products() {
        let mut inactive = product("ok", 100);
        inactive.active = false;
        assert!(matches!(
            CatalogProduct::import(inactive).unwrap().is_saleable(),
            Err(CatalogError::ProductInactive { .. })
        ));

        let mut not_sellable = product("ok", 100);
        not_sellable.sellable = false;
        assert!(matches!(
            CatalogProduct::import(not_sellable).unwrap().is_saleable(),
            Err(CatalogError::ProductNotSellable { .. })
        ));

        assert!(CatalogProduct::import(product("ok", 100))
            .unwrap()
            .is_saleable()
            .is_ok());
    }

    #[test]
    fn selector_reports_the_scanned_barcode_only_when_scanned() {
        let barcode = Barcode::new("750123").unwrap();
        assert_eq!(
            ProductSelector::Barcode(barcode.clone()).scanned_barcode(),
            Some(&barcode)
        );
        assert!(ProductSelector::Sku("S".to_owned())
            .scanned_barcode()
            .is_none());
    }
}
