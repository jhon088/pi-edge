//! Where a piece of snapshot content applies.
//!
//! Shared by price books and promotions, because both answer the same
//! question the same way: tenant-wide, or one store. Two levels on purpose —
//! arbitrary hierarchies are not modelled until a real requirement asks for
//! one.

use edge_domain::prelude::StoreId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CatalogScope {
    /// Applies to every store of the tenant.
    Tenant,
    /// Applies to one store only.
    Store { store_id: StoreId },
}

impl CatalogScope {
    pub const fn as_str(&self) -> &'static str {
        match self {
            CatalogScope::Tenant => "TENANT",
            CatalogScope::Store { .. } => "STORE",
        }
    }

    pub fn store_id(&self) -> Option<&StoreId> {
        match self {
            CatalogScope::Tenant => None,
            CatalogScope::Store { store_id } => Some(store_id),
        }
    }

    /// Whether this scope covers the given store.
    pub fn applies_to(&self, store_id: &StoreId) -> bool {
        match self {
            CatalogScope::Tenant => true,
            CatalogScope::Store { store_id: scoped } => scoped == store_id,
        }
    }

    /// Specificity rank. A more specific scope always wins, regardless of
    /// priority, so "the store's own entry overrides the tenant-wide one" is
    /// structural rather than a property of the data.
    pub const fn rank(&self) -> u8 {
        match self {
            CatalogScope::Tenant => 0,
            CatalogScope::Store { .. } => 1,
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_scope_only_covers_its_own_store() {
        let here = StoreId::new("S-1").unwrap();
        let there = StoreId::new("S-2").unwrap();
        let scoped = CatalogScope::Store {
            store_id: here.clone(),
        };
        assert!(scoped.applies_to(&here));
        assert!(!scoped.applies_to(&there));
        assert!(CatalogScope::Tenant.applies_to(&here));
        assert!(CatalogScope::Tenant.applies_to(&there));
    }

    #[test]
    fn a_store_scope_is_more_specific_than_tenant() {
        let scoped = CatalogScope::Store {
            store_id: StoreId::new("S-1").unwrap(),
        };
        assert!(scoped.rank() > CatalogScope::Tenant.rank());
        assert_eq!(scoped.as_str(), "STORE");
        assert_eq!(CatalogScope::Tenant.as_str(), "TENANT");
        assert_eq!(scoped.store_id().map(StoreId::as_str), Some("S-1"));
        assert!(CatalogScope::Tenant.store_id().is_none());
    }
}
