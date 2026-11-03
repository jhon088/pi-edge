//! What an operator changed on this kiosk: a price, and whether a product
//! needs somebody's permission before it can be paid for.
//!
//! One file beside the preferences, keyed by `product_ref`:
//!
//! ```json
//! {
//!   "T-LATTE":     { "price_minor": 5900 },
//!   "R-ARRACHERA": { "price_minor": 28900, "authorization_required": true }
//! }
//! ```
//!
//! The two fields look alike and are **not** the same kind of thing, which is
//! the only subtle part of this module:
//!
//! * `price_minor` is **catalog data**. A sale is priced by the active
//!   snapshot and nothing else, so a price kept only here would be cosmetic —
//!   which is exactly what this must not be. It reaches the core by being
//!   applied to the dataset and re-imported through the same `demo-seed`
//!   pipeline a release uses, producing a new active catalog. The number the
//!   till charges *is* this number (ADR 050).
//! * `authorization_required` is **not** catalog data and never reaches the
//!   core. "Somebody has to come over before this is paid for" is a showroom
//!   policy about this screen's customers, like the closed sign and the
//!   printer gate, and the core has no concept it belongs to. Putting it in
//!   the catalog would be inventing a commercial attribute for a demo.
//!
//! They share a file because they share an editor: one Admin screen, one list
//! of products, one place an operator's changes live and survive a restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::BridgeError;

/// What was changed about one product.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductOverrideV1 {
    /// The price in minor units, when an operator set one. `None` means the
    /// dataset's own price stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_minor: Option<i64>,
    /// Whether this product cannot be paid for until somebody authorises it.
    /// Retail only; the counter lanes ignore it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub authorization_required: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ProductOverrideV1 {
    /// Whether this entry says anything at all. One that does not is dropped
    /// on write rather than kept as an empty object nobody can read.
    fn is_empty(&self) -> bool {
        self.price_minor.is_none() && !self.authorization_required
    }
}

/// Every override on this installation, by `product_ref`.
pub type Overrides = BTreeMap<String, ProductOverrideV1>;

pub fn path(dir: &Path) -> PathBuf {
    dir.join("overrides.json")
}

/// Read what is stored.
///
/// Anything unreadable or unrecognised is **no overrides** rather than an
/// error: a corrupt file must not stop a kiosk opening, and the dataset's own
/// prices are a working catalog. Said plainly because the failure is quiet —
/// a kiosk that comes up charging the shipped prices after somebody hand-edited
/// this file into nonsense is behaving correctly and looks like a bug.
pub fn stored(dir: &Path) -> Overrides {
    match std::fs::read_to_string(path(dir)) {
        Ok(text) => match serde_json::from_str::<Overrides>(&text) {
            Ok(overrides) => overrides,
            Err(error) => {
                tracing::warn!(%error, "the stored overrides are unreadable; using the dataset's own prices");
                Overrides::new()
            }
        },
        Err(_) => Overrides::new(),
    }
}

/// Write them, dropping entries that say nothing.
pub fn write(dir: &Path, overrides: &Overrides) -> Result<(), BridgeError> {
    let tidy: Overrides = overrides
        .iter()
        .filter(|(_, entry)| !entry.is_empty())
        .map(|(product_ref, entry)| (product_ref.clone(), entry.clone()))
        .collect();
    let body = serde_json::to_string_pretty(&tidy).map_err(|error| BridgeError {
        code: "OVERRIDES_ENCODE_FAILED".to_owned(),
        message: error.to_string(),
    })?;
    std::fs::write(path(dir), body).map_err(|error| BridgeError {
        code: "OVERRIDES_WRITE_FAILED".to_owned(),
        message: error.to_string(),
    })
}

/// Whether this product needs somebody's permission before it is paid for.
pub fn needs_authorization(overrides: &Overrides, product_ref: &str) -> bool {
    overrides
        .get(product_ref)
        .is_some_and(|entry| entry.authorization_required)
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_installation_has_overridden_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(stored(dir.path()).is_empty());
        assert!(!needs_authorization(&stored(dir.path()), "R-ARRACHERA"));
    }

    #[test]
    fn a_price_and_a_flag_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut overrides = Overrides::new();
        overrides.insert(
            "T-LATTE".to_owned(),
            ProductOverrideV1 {
                price_minor: Some(5_900),
                authorization_required: false,
            },
        );
        overrides.insert(
            "R-ARRACHERA".to_owned(),
            ProductOverrideV1 {
                price_minor: Some(28_900),
                authorization_required: true,
            },
        );
        write(dir.path(), &overrides).unwrap();

        let back = stored(dir.path());
        assert_eq!(back.get("T-LATTE").unwrap().price_minor, Some(5_900));
        assert!(!back.get("T-LATTE").unwrap().authorization_required);
        assert_eq!(back.get("R-ARRACHERA").unwrap().price_minor, Some(28_900));
        assert!(needs_authorization(&back, "R-ARRACHERA"));
        assert!(!needs_authorization(&back, "T-LATTE"));
    }

    /// A flag with no price is a complete entry: a product can need
    /// authorization at the price it already had.
    #[test]
    fn a_flag_without_a_price_is_a_whole_entry() {
        let dir = tempfile::tempdir().unwrap();
        let overrides = Overrides::from([(
            "R-CLOROX".to_owned(),
            ProductOverrideV1 {
                price_minor: None,
                authorization_required: true,
            },
        )]);
        write(dir.path(), &overrides).unwrap();
        let back = stored(dir.path());
        assert!(needs_authorization(&back, "R-CLOROX"));
        assert_eq!(back.get("R-CLOROX").unwrap().price_minor, None);
    }

    /// An entry that says nothing is not kept. Turning a flag off and the
    /// price back to the dataset's leaves the file as it started.
    #[test]
    fn an_entry_that_says_nothing_is_dropped_rather_than_stored_empty() {
        let dir = tempfile::tempdir().unwrap();
        let overrides = Overrides::from([
            ("R-CLOROX".to_owned(), ProductOverrideV1::default()),
            (
                "T-LATTE".to_owned(),
                ProductOverrideV1 {
                    price_minor: Some(5_900),
                    authorization_required: false,
                },
            ),
        ]);
        write(dir.path(), &overrides).unwrap();
        let back = stored(dir.path());
        assert!(!back.contains_key("R-CLOROX"), "an empty entry was kept");
        assert_eq!(back.len(), 1);
    }

    /// Zero is a price. A showroom gives things away, and `Some(0)` must not
    /// read as "no override".
    #[test]
    fn a_price_of_zero_is_an_override() {
        let dir = tempfile::tempdir().unwrap();
        let overrides = Overrides::from([(
            "T-LATTE".to_owned(),
            ProductOverrideV1 {
                price_minor: Some(0),
                authorization_required: false,
            },
        )]);
        write(dir.path(), &overrides).unwrap();
        assert_eq!(
            stored(dir.path()).get("T-LATTE").unwrap().price_minor,
            Some(0)
        );
    }

    /// A file somebody hand-edited into nonsense leaves a working kiosk on
    /// the dataset's own prices.
    #[test]
    fn an_unreadable_file_is_no_overrides_rather_than_a_dead_kiosk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "{ not json").unwrap();
        assert!(stored(dir.path()).is_empty());
    }

    /// The shape the CLI reads on the other side of this file.
    #[test]
    fn the_file_is_the_shape_demo_seed_expects() {
        let dir = tempfile::tempdir().unwrap();
        let overrides = Overrides::from([(
            "T-LATTE".to_owned(),
            ProductOverrideV1 {
                price_minor: Some(5_900),
                authorization_required: true,
            },
        )]);
        write(dir.path(), &overrides).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path(dir.path())).unwrap()).unwrap();
        assert_eq!(raw["T-LATTE"]["price_minor"], 5_900);
        assert_eq!(raw["T-LATTE"]["authorization_required"], true);
    }
}
