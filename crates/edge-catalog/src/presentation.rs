//! How the catalog is shown, and in which language.
//!
//! Presentation is part of the snapshot and part of its hash, for one hard
//! reason: if it travelled outside the hash, two menus differing only in a
//! photograph would share a `content_hash` and the import's idempotency would
//! treat them as the same content and silently keep the old one. The cost —
//! changing an image bumps the catalog version — is the correct cost.
//!
//! Nothing in pricing, tax or promotions reads anything in this file. It is
//! carried, stored and hashed; it never decides a cent (ADR 044).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use edge_domain::prelude::{CategoryRef, ProductRef};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;

/// Longest locale tag accepted. `zh-Hant-HK` is 10 characters; this leaves
/// room without accepting a sentence.
const MAX_LOCALE_LEN: usize = 16;

/// A BCP-47-shaped language tag, kept opaque.
///
/// Validated for shape only: the core never maps a locale to behaviour, it
/// only carries the text a client asked for. Which locales exist is the
/// upstream system's decision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Locale(String);

impl Locale {
    pub fn new(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "locale",
                reason: "must not be blank".to_owned(),
            });
        }
        if trimmed.chars().count() > MAX_LOCALE_LEN {
            return Err(CatalogError::InvalidRecord {
                field: "locale",
                reason: format!("must be at most {MAX_LOCALE_LEN} characters"),
            });
        }
        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(CatalogError::InvalidRecord {
                field: "locale",
                reason: "may only contain letters, digits and `-`".to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Text with a fallback and any number of translations.
///
/// The translations live in a `BTreeMap`, so two payloads carrying the same
/// translations in a different JSON order deserialize to the same value, hash
/// to the same bytes and serialize identically. Determinism is a property of
/// the type, not of the caller's discipline.
///
/// `default` is what a client gets when it asks for a locale nobody translated
/// — never an empty string, and never a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizedText {
    pub default: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translations: BTreeMap<Locale, String>,
}

impl LocalizedText {
    /// Text in one language only. What a retail catalog that never asked for
    /// translations produces.
    pub fn new(default: impl Into<String>) -> Result<Self, CatalogError> {
        Self::with_translations(default, BTreeMap::new())
    }

    pub fn with_translations(
        default: impl Into<String>,
        translations: BTreeMap<Locale, String>,
    ) -> Result<Self, CatalogError> {
        let default = default.into().trim().to_owned();
        if default.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "localized_text.default",
                reason: "must not be blank".to_owned(),
            });
        }
        let mut cleaned = BTreeMap::new();
        for (locale, text) in translations {
            let text = text.trim().to_owned();
            if text.is_empty() {
                return Err(CatalogError::InvalidRecord {
                    field: "localized_text.translation",
                    reason: format!("the `{locale}` translation must not be blank"),
                });
            }
            cleaned.insert(locale, text);
        }
        Ok(Self {
            default,
            translations: cleaned,
        })
    }

    /// The best text for `locale`: the exact translation, else the default.
    ///
    /// No language negotiation, no prefix matching, no fallback chain. A
    /// client that wants `es` and finds only `es-MX` gets the default, which
    /// is a visible, explainable answer rather than a guess.
    pub fn resolve(&self, locale: Option<&Locale>) -> &str {
        locale
            .and_then(|locale| self.translations.get(locale))
            .map(String::as_str)
            .unwrap_or(&self.default)
    }
}

/// A file that belongs with the catalog, named by what it contains.
///
/// The identity **is** the content hash, which is what lets a future asset
/// runtime keep a content-addressed local store, verify what it downloaded,
/// and refuse to activate a snapshot whose assets are not all present. No URL
/// and no path: where the bytes come from is not the catalog's business, and a
/// client must never be handed an address it could fetch itself (ADR 044).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    /// `sha256:<64 hex>` — the identity of the bytes.
    pub content_hash: String,
    /// IANA media type, so a store can serve it and a client can refuse what
    /// it cannot render.
    pub media_type: String,
    /// Size in bytes, so a store can plan its quota before downloading and
    /// detect a truncated file before hashing it.
    pub byte_size: i64,
}

impl AssetRef {
    pub fn import(
        content_hash: impl Into<String>,
        media_type: impl Into<String>,
        byte_size: i64,
    ) -> Result<Self, CatalogError> {
        let content_hash = content_hash.into().trim().to_owned();
        let invalid = |reason: String| CatalogError::InvalidRecord {
            field: "asset_ref.content_hash",
            reason,
        };
        let Some(hex) = content_hash.strip_prefix("sha256:") else {
            return Err(invalid("must be prefixed `sha256:`".to_owned()));
        };
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid("must be 64 hexadecimal characters".to_owned()));
        }
        if hex.chars().any(|c| c.is_ascii_uppercase()) {
            return Err(invalid("must be lowercase hexadecimal".to_owned()));
        }
        let media_type = media_type.into().trim().to_owned();
        if media_type.is_empty() || !media_type.contains('/') {
            return Err(CatalogError::InvalidRecord {
                field: "asset_ref.media_type",
                reason: "must be a media type such as `image/webp`".to_owned(),
            });
        }
        if byte_size <= 0 {
            return Err(CatalogError::InvalidRecord {
                field: "asset_ref.byte_size",
                reason: "must be positive".to_owned(),
            });
        }
        Ok(Self {
            content_hash,
            media_type,
            byte_size,
        })
    }
}

/// A grouping a client can browse.
///
/// Flat on purpose: a tree is one more invariant to enforce and nothing asked
/// for one. `display_order` is the only ordering, with the reference breaking
/// ties so two clients always render the same list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub category_ref: CategoryRef,
    pub name: LocalizedText,
    pub display_order: i32,
    pub image: Option<AssetRef>,
}

impl Category {
    pub fn import(
        category_ref: CategoryRef,
        name: LocalizedText,
        display_order: i32,
        image: Option<AssetRef>,
    ) -> Self {
        Self {
            category_ref,
            name,
            display_order,
            image,
        }
    }
}

/// How one product is shown.
///
/// Separate from `CatalogProduct` because the two answer different questions:
/// the product says what may be sold and how it is taxed, this says what a
/// customer sees. A product with no presentation row is sellable and simply
/// has nothing to show — which is every retail catalog in existence today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductPresentation {
    pub product_ref: ProductRef,
    /// `None` for a product that is reachable by code or barcode but not by
    /// browsing.
    pub category_ref: Option<CategoryRef>,
    /// Shown instead of the commercial name when present. The commercial name
    /// stays the one on the receipt and in the ledger.
    pub display_name: Option<LocalizedText>,
    pub description: Option<LocalizedText>,
    pub display_order: i32,
    pub image: Option<AssetRef>,
    /// What to offer alongside this one, in the order a screen should offer
    /// it. Merchandising, decided by whoever builds the catalog.
    ///
    /// This is presentation, not pricing and not a rule: nothing here changes
    /// a total, and a screen that ignores it still sells correctly. It lives
    /// in the snapshot so the suggestion travels with the menu instead of
    /// being invented client-side, and so the same list reaches every channel
    /// (ADR 046).
    pub recommended_product_refs: Vec<ProductRef>,
}

impl ProductPresentation {
    /// Most products one row may recommend.
    ///
    /// A cap, not a layout: a screen shows three or four. The limit is here so
    /// a bad import cannot put a thousand refs in a snapshot and make every
    /// menu read quadratic.
    pub const MAX_RECOMMENDATIONS: usize = 12;

    /// Reject a recommendation list that cannot mean anything, at import,
    /// where the catalog is still refusable. A product recommending itself and
    /// the same product twice are both merchandising mistakes; whether each
    /// ref resolves is checked at commit, with the rest of the snapshot.
    pub fn validate_recommendations(&self) -> Result<(), CatalogError> {
        if self.recommended_product_refs.len() > Self::MAX_RECOMMENDATIONS {
            return Err(CatalogError::InvalidRecord {
                field: "product_presentation.recommended_product_refs",
                reason: format!(
                    "{} recommendations; at most {} are allowed",
                    self.recommended_product_refs.len(),
                    Self::MAX_RECOMMENDATIONS
                ),
            });
        }
        let mut seen = BTreeSet::new();
        for recommended in &self.recommended_product_refs {
            if recommended == &self.product_ref {
                return Err(CatalogError::InvalidRecord {
                    field: "product_presentation.recommended_product_refs",
                    reason: format!("`{recommended}` recommends itself"),
                });
            }
            if !seen.insert(recommended.clone()) {
                return Err(CatalogError::InvalidRecord {
                    field: "product_presentation.recommended_product_refs",
                    reason: format!("`{recommended}` is recommended twice"),
                });
            }
        }
        Ok(())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn locale(tag: &str) -> Locale {
        Locale::new(tag).expect("a valid tag")
    }

    const HASH: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_locale_is_validated_for_shape_and_nothing_else() {
        assert_eq!(locale("es-MX").as_str(), "es-MX");
        assert_eq!(locale("  en  ").as_str(), "en", "trimmed");
        for bad in ["", "   ", "es_MX", "español", "a-very-long-locale-tag-x"] {
            assert!(Locale::new(bad).is_err(), "`{bad}` is not a locale");
        }
    }

    #[test]
    fn translations_normalize_their_order_so_two_payloads_agree() {
        let one = LocalizedText::with_translations(
            "Hamburguesa",
            BTreeMap::from([
                (locale("en"), "Burger".to_owned()),
                (locale("fr"), "Hamburger".to_owned()),
            ]),
        )
        .unwrap();
        let other = LocalizedText::with_translations(
            "Hamburguesa",
            BTreeMap::from([
                (locale("fr"), "Hamburger".to_owned()),
                (locale("en"), "Burger".to_owned()),
            ]),
        )
        .unwrap();
        assert_eq!(one, other);
        let order = |text: &LocalizedText| {
            text.translations
                .keys()
                .map(|locale| locale.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&one),
            order(&other),
            "iteration order is canonical, so the hash cannot depend on input order"
        );
    }

    #[test]
    fn resolving_a_locale_nobody_translated_answers_the_default() {
        let text = LocalizedText::with_translations(
            "Refresco",
            BTreeMap::from([(locale("en"), "Soda".to_owned())]),
        )
        .unwrap();
        assert_eq!(text.resolve(Some(&locale("en"))), "Soda");
        assert_eq!(text.resolve(Some(&locale("fr"))), "Refresco");
        // No prefix matching: `es` is not `es-MX`, and guessing would be worse
        // than answering the default.
        assert_eq!(text.resolve(Some(&locale("es-MX"))), "Refresco");
        assert_eq!(text.resolve(None), "Refresco");
    }

    #[test]
    fn blank_text_is_refused_at_the_boundary() {
        assert!(LocalizedText::new("   ").is_err());
        assert!(LocalizedText::with_translations(
            "Hamburguesa",
            BTreeMap::from([(locale("en"), "  ".to_owned())])
        )
        .is_err());
    }

    #[test]
    fn an_asset_is_named_by_its_content() {
        let asset = AssetRef::import(HASH, "image/webp", 2_048).expect("a valid asset");
        assert_eq!(asset.content_hash, HASH);
        for (hash, why) in [
            ("0123", "no prefix"),
            ("sha256:abc", "too short"),
            ("md5:0123456789abcdef0123456789abcdef", "wrong algorithm"),
            (
                "sha256:0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
                "uppercase",
            ),
            (
                "sha256:zzzz456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "not hex",
            ),
        ] {
            assert!(
                AssetRef::import(hash, "image/webp", 1).is_err(),
                "{why} must be refused"
            );
        }
        assert!(
            AssetRef::import(HASH, "webp", 1).is_err(),
            "not a media type"
        );
        assert!(AssetRef::import(HASH, "image/webp", 0).is_err(), "no bytes");
    }
}
