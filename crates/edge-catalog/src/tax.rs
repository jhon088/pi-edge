//! Tax profiles.
//!
//! A profile is the catalog's statement of the rate a product is taxed at. The
//! POS never derives a rate from an amount or a product kind.
//!
//! Whether a quoted amount *includes* that tax is a property of the price, not
//! of the rate, so `price_mode` lives on the price entry (see
//! [`crate::pricing`]). The same 16% profile can serve a tax-inclusive shelf
//! price and a tax-exclusive wholesale price.
//!
//! Mexico needs 16%, 8% (border region) and 0% today; nothing here is specific
//! to those values. Fiscal document semantics (CFDI) are out of scope.

use edge_domain::prelude::TaxRate;
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;

/// Maximum length of a server-assigned tax profile code.
const MAX_CODE_LEN: usize = 64;

/// Opaque tax profile code assigned upstream, e.g. `IVA16`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaxProfileCode(String);

impl TaxProfileCode {
    pub fn new(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "tax_profile_code",
                reason: "must not be empty".to_owned(),
            });
        }
        if trimmed.chars().count() > MAX_CODE_LEN {
            return Err(CatalogError::InvalidRecord {
                field: "tax_profile_code",
                reason: format!("exceeds {MAX_CODE_LEN} characters"),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaxProfileCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxProfile {
    pub code: TaxProfileCode,
    pub name: String,
    pub rate: TaxRate,
}

impl TaxProfile {
    pub fn new(
        code: TaxProfileCode,
        name: impl Into<String>,
        rate: TaxRate,
    ) -> Result<Self, CatalogError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "tax_profile_name",
                reason: "must not be empty".to_owned(),
            });
        }
        Ok(Self {
            code,
            name: name.trim().to_owned(),
            rate,
        })
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_codes() {
        assert_eq!(TaxProfileCode::new(" IVA16 ").unwrap().as_str(), "IVA16");
        assert!(TaxProfileCode::new("  ").is_err());
        assert!(TaxProfileCode::new("x".repeat(MAX_CODE_LEN + 1)).is_err());
    }

    #[test]
    fn requires_a_name() {
        let code = TaxProfileCode::new("IVA16").unwrap();
        assert!(TaxProfile::new(code.clone(), "  ", TaxRate::from_bps(1_600).unwrap()).is_err());
        let profile = TaxProfile::new(code, "IVA 16%", TaxRate::from_bps(1_600).unwrap()).unwrap();
        assert_eq!(profile.rate.bps(), 1_600);
    }
}
