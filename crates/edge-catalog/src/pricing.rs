//! Price books and price resolution.
//!
//! A product states *what it is*; a price book states *what it costs in a
//! context*. Keeping them apart is what lets a store override a tenant-wide
//! price, and a price change be scheduled, without touching product identity.
//!
//! Resolution is deterministic and explicit. Given a product, a store, a
//! snapshot and an **`effective_at`** instant, exactly one price wins, or the
//! core refuses:
//!
//! 1. candidates are entries of *active* books whose scope applies to the
//!    store, whose currency matches, and whose validity window contains
//!    `effective_at`;
//! 2. no candidate is [`CatalogError::PriceNotFound`];
//! 3. candidates rank by **scope first** (a store book always beats a
//!    tenant book), then by **priority** (higher wins);
//! 4. a tie at the top is [`CatalogError::AmbiguousPrice`] — never a silent
//!    pick.
//!
//! `effective_at` is a parameter, never `now()` read inside the domain, so a
//! sale can pin the instant it prices at (see ADR 013) and tests can place
//! themselves anywhere in time.

use edge_domain::prelude::{
    CatalogSnapshotId, Currency, Money, PriceBookId, PriceMode, ProductRef, Quantity, StoreId,
    Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;

/// Where a price book applies. See [`crate::scope::CatalogScope`].
pub type PriceBookScope = crate::scope::CatalogScope;

/// A named set of prices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBook {
    /// Opaque reference assigned upstream.
    pub price_book_id: PriceBookId,
    /// Short human code, unique within a snapshot. Diagnostics only.
    pub code: String,
    pub name: String,
    pub currency: Currency,
    pub scope: PriceBookScope,
    /// Tiebreaker *within* a scope: higher wins.
    pub priority: i32,
    /// An inactive book contributes no candidates.
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct NewPriceBook {
    pub price_book_id: PriceBookId,
    pub code: String,
    pub name: String,
    pub currency: Currency,
    pub scope: PriceBookScope,
    pub priority: i32,
    pub active: bool,
}

impl PriceBook {
    pub fn import(new: NewPriceBook) -> Result<Self, CatalogError> {
        let code = new.code.trim().to_owned();
        if code.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "price_book_code",
                reason: "must not be empty".to_owned(),
            });
        }
        let name = new.name.trim().to_owned();
        if name.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "price_book_name",
                reason: "must not be empty".to_owned(),
            });
        }
        Ok(Self {
            price_book_id: new.price_book_id,
            code,
            name,
            currency: new.currency,
            scope: new.scope,
            priority: new.priority,
            active: new.active,
        })
    }
}

/// One price for one product in one book, optionally scheduled.
///
/// `valid_from` is inclusive and `valid_until` is exclusive, so a replacement
/// price can start exactly when the previous one ends with neither a gap nor an
/// overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceEntry {
    pub price_book_id: PriceBookId,
    pub product_ref: ProductRef,
    pub amount: Money,
    /// Whether `amount` already includes tax. A property of the price, not of
    /// the tax rate.
    pub price_mode: PriceMode,
    pub valid_from: Option<Timestamp>,
    pub valid_until: Option<Timestamp>,
}

#[derive(Debug, Clone)]
pub struct NewPriceEntry {
    pub price_book_id: PriceBookId,
    pub product_ref: ProductRef,
    pub amount: Money,
    pub price_mode: PriceMode,
    pub valid_from: Option<Timestamp>,
    pub valid_until: Option<Timestamp>,
}

impl PriceEntry {
    pub fn import(new: NewPriceEntry) -> Result<Self, CatalogError> {
        if new.amount.is_negative() {
            return Err(CatalogError::InvalidRecord {
                field: "price_amount",
                reason: "must not be negative".to_owned(),
            });
        }
        if let (Some(from), Some(until)) = (new.valid_from, new.valid_until) {
            if until <= from {
                return Err(CatalogError::InvalidValidity {
                    valid_from: from.to_rfc3339(),
                    valid_until: until.to_rfc3339(),
                });
            }
        }
        Ok(Self {
            price_book_id: new.price_book_id,
            product_ref: new.product_ref,
            amount: new.amount,
            price_mode: new.price_mode,
            valid_from: new.valid_from,
            valid_until: new.valid_until,
        })
    }

    /// Whether this price is in force at an instant.
    /// `valid_from` inclusive, `valid_until` exclusive.
    pub fn is_in_force_at(&self, effective_at: Timestamp) -> bool {
        let started = self.valid_from.is_none_or(|from| effective_at >= from);
        let not_ended = self.valid_until.is_none_or(|until| effective_at < until);
        started && not_ended
    }
}

/// Everything price resolution is allowed to depend on.
#[derive(Debug, Clone, Copy)]
pub struct PricingContext<'a> {
    pub store_id: &'a StoreId,
    pub product_ref: &'a ProductRef,
    /// The instant prices are resolved at. Explicit, never `now()`.
    pub effective_at: Timestamp,
    pub snapshot_id: CatalogSnapshotId,
    pub quantity: Quantity,
    pub currency: Currency,
}

/// A price book together with one of its entries, as offered to the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceCandidate {
    pub book: PriceBook,
    pub entry: PriceEntry,
}

/// The winning price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPrice {
    pub price_book_id: PriceBookId,
    pub price_book_code: String,
    pub unit_price: Money,
    pub price_mode: PriceMode,
    /// Instant the resolution was made at, carried for the journal.
    pub effective_at: Timestamp,
}

/// Resolve the price of a product in a context.
///
/// `candidates` may be over-broad: the resolver filters by activity, scope,
/// currency and validity itself, so the whole rule lives in one place and is
/// testable without a database.
pub fn resolve_price(
    candidates: &[PriceCandidate],
    context: &PricingContext<'_>,
) -> Result<ResolvedPrice, CatalogError> {
    let mut applicable: Vec<&PriceCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.book.active)
        .filter(|candidate| candidate.book.scope.applies_to(context.store_id))
        .filter(|candidate| candidate.book.currency == context.currency)
        .filter(|candidate| candidate.entry.amount.currency() == context.currency)
        .filter(|candidate| candidate.entry.product_ref == *context.product_ref)
        .filter(|candidate| candidate.entry.is_in_force_at(context.effective_at))
        .collect();

    if applicable.is_empty() {
        return Err(CatalogError::PriceNotFound {
            product_ref: context.product_ref.to_string(),
            store_id: context.store_id.to_string(),
            effective_at: context.effective_at.to_rfc3339(),
        });
    }

    // Most specific scope first, then highest priority.
    applicable.sort_by(|left, right| {
        right
            .book
            .scope
            .rank()
            .cmp(&left.book.scope.rank())
            .then(right.book.priority.cmp(&left.book.priority))
    });

    let winner = applicable.first().ok_or(CatalogError::PriceNotFound {
        product_ref: context.product_ref.to_string(),
        store_id: context.store_id.to_string(),
        effective_at: context.effective_at.to_rfc3339(),
    })?;

    // Anything tied with the winner makes the answer arbitrary. Refuse.
    let tied: Vec<&&PriceCandidate> = applicable
        .iter()
        .filter(|candidate| {
            candidate.book.scope.rank() == winner.book.scope.rank()
                && candidate.book.priority == winner.book.priority
        })
        .collect();
    if tied.len() > 1 {
        let mut books: Vec<String> = tied
            .iter()
            .map(|candidate| candidate.book.price_book_id.to_string())
            .collect();
        books.sort();
        books.dedup();
        return Err(CatalogError::AmbiguousPrice {
            product_ref: context.product_ref.to_string(),
            scope: winner.book.scope.as_str(),
            priority: winner.book.priority,
            price_books: books.join(", "),
        });
    }

    Ok(ResolvedPrice {
        price_book_id: winner.book.price_book_id.clone(),
        price_book_code: winner.book.code.clone(),
        unit_price: winner.entry.amount,
        price_mode: winner.entry.price_mode,
        effective_at: context.effective_at,
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ts(day: u32) -> Timestamp {
        Timestamp::parse_rfc3339(&format!("2026-10-{day:02}T00:00:00Z")).unwrap()
    }

    fn store() -> StoreId {
        StoreId::new("STORE-1").unwrap()
    }

    fn product() -> ProductRef {
        ProductRef::new("P-1").unwrap()
    }

    fn book(id: &str, scope: PriceBookScope, priority: i32, active: bool) -> PriceBook {
        PriceBook::import(NewPriceBook {
            price_book_id: PriceBookId::new(id).unwrap(),
            code: id.to_owned(),
            name: format!("book {id}"),
            currency: Currency::Mxn,
            scope,
            priority,
            active,
        })
        .unwrap()
    }

    fn entry(
        book: &PriceBook,
        minor: i64,
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> PriceEntry {
        PriceEntry::import(NewPriceEntry {
            price_book_id: book.price_book_id.clone(),
            product_ref: product(),
            amount: Money::from_minor(minor, Currency::Mxn),
            price_mode: PriceMode::TaxExclusive,
            valid_from,
            valid_until,
        })
        .unwrap()
    }

    fn candidate(book: PriceBook, entry: PriceEntry) -> PriceCandidate {
        PriceCandidate { book, entry }
    }

    fn context(effective_at: Timestamp) -> (StoreId, ProductRef, Timestamp) {
        (store(), product(), effective_at)
    }

    fn resolve(
        candidates: &[PriceCandidate],
        effective_at: Timestamp,
    ) -> Result<ResolvedPrice, CatalogError> {
        let (store_id, product_ref, effective_at) = context(effective_at);
        resolve_price(
            candidates,
            &PricingContext {
                store_id: &store_id,
                product_ref: &product_ref,
                effective_at,
                snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(1)),
                quantity: Quantity::from_units(1).unwrap(),
                currency: Currency::Mxn,
            },
        )
    }

    #[test]
    fn resolves_the_base_price() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let candidates = vec![candidate(base.clone(), entry(&base, 10_000, None, None))];
        let resolved = resolve(&candidates, ts(8)).unwrap();
        assert_eq!(resolved.unit_price.minor(), 10_000);
        assert_eq!(resolved.price_book_code, "BASE");
        assert_eq!(resolved.effective_at, ts(8));
    }

    #[test]
    fn a_store_book_overrides_the_base_regardless_of_priority() {
        let base = book("BASE", PriceBookScope::Tenant, 100, true);
        let local = book(
            "STORE",
            PriceBookScope::Store { store_id: store() },
            0,
            true,
        );
        let candidates = vec![
            candidate(base.clone(), entry(&base, 10_000, None, None)),
            candidate(local.clone(), entry(&local, 8_900, None, None)),
        ];
        let resolved = resolve(&candidates, ts(8)).unwrap();
        assert_eq!(resolved.unit_price.minor(), 8_900);
        assert_eq!(resolved.price_book_code, "STORE");
    }

    #[test]
    fn ignores_a_book_scoped_to_another_store() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let elsewhere = book(
            "OTHER",
            PriceBookScope::Store {
                store_id: StoreId::new("STORE-2").unwrap(),
            },
            50,
            true,
        );
        let candidates = vec![
            candidate(base.clone(), entry(&base, 10_000, None, None)),
            candidate(elsewhere.clone(), entry(&elsewhere, 1, None, None)),
        ];
        assert_eq!(
            resolve(&candidates, ts(8)).unwrap().unit_price.minor(),
            10_000
        );
    }

    #[test]
    fn ignores_an_inactive_book() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let disabled = book(
            "DISABLED",
            PriceBookScope::Store { store_id: store() },
            99,
            false,
        );
        let candidates = vec![
            candidate(base.clone(), entry(&base, 10_000, None, None)),
            candidate(disabled.clone(), entry(&disabled, 1, None, None)),
        ];
        assert_eq!(
            resolve(&candidates, ts(8)).unwrap().unit_price.minor(),
            10_000
        );
    }

    #[test]
    fn a_future_price_is_not_active_yet() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let candidates = vec![
            candidate(base.clone(), entry(&base, 10_000, None, Some(ts(10)))),
            candidate(base.clone(), entry(&base, 12_000, Some(ts(10)), None)),
        ];
        assert_eq!(
            resolve(&candidates, ts(9)).unwrap().unit_price.minor(),
            10_000
        );
    }

    #[test]
    fn an_expired_price_is_ignored() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let candidates = vec![
            candidate(base.clone(), entry(&base, 10_000, None, Some(ts(10)))),
            candidate(base.clone(), entry(&base, 12_000, Some(ts(10)), None)),
        ];
        assert_eq!(
            resolve(&candidates, ts(11)).unwrap().unit_price.minor(),
            12_000
        );
    }

    #[test]
    fn valid_from_is_inclusive_and_valid_until_is_exclusive() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let scheduled = entry(&base, 12_000, Some(ts(10)), Some(ts(12)));
        assert!(!scheduled
            .is_in_force_at(Timestamp::parse_rfc3339("2026-10-09T23:59:59.999999Z").unwrap()));
        // The instant valid_from names is already in force.
        assert!(scheduled.is_in_force_at(ts(10)));
        assert!(scheduled
            .is_in_force_at(Timestamp::parse_rfc3339("2026-10-11T23:59:59.999999Z").unwrap()));
        // The instant valid_until names is already over.
        assert!(!scheduled.is_in_force_at(ts(12)));

        // Back to back schedules leave neither gap nor overlap.
        let previous = entry(&base, 10_000, None, Some(ts(10)));
        assert!(previous.is_in_force_at(ts(9)));
        assert!(!previous.is_in_force_at(ts(10)));
    }

    #[test]
    fn no_applicable_price_is_an_explicit_refusal() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let candidates = vec![candidate(
            base.clone(),
            entry(&base, 10_000, Some(ts(20)), None),
        )];
        let error = resolve(&candidates, ts(8)).unwrap_err();
        assert_eq!(error.code(), "PRICE_NOT_FOUND");
        assert!(matches!(error, CatalogError::PriceNotFound { .. }));
        assert_eq!(resolve(&[], ts(8)).unwrap_err().code(), "PRICE_NOT_FOUND");
    }

    #[test]
    fn two_prices_at_the_same_scope_and_priority_are_ambiguous() {
        let first = book("A", PriceBookScope::Tenant, 5, true);
        let second = book("B", PriceBookScope::Tenant, 5, true);
        let candidates = vec![
            candidate(first.clone(), entry(&first, 10_000, None, None)),
            candidate(second.clone(), entry(&second, 11_000, None, None)),
        ];
        let error = resolve(&candidates, ts(8)).unwrap_err();
        assert_eq!(error.code(), "AMBIGUOUS_PRICE");
        match error {
            CatalogError::AmbiguousPrice { price_books, .. } => {
                assert_eq!(price_books, "A, B");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn overlapping_schedules_in_one_book_are_ambiguous() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let candidates = vec![
            candidate(base.clone(), entry(&base, 10_000, None, Some(ts(12)))),
            candidate(base.clone(), entry(&base, 12_000, Some(ts(10)), None)),
        ];
        // Both are in force on the 11th: the data is wrong and we say so.
        assert_eq!(
            resolve(&candidates, ts(11)).unwrap_err().code(),
            "AMBIGUOUS_PRICE"
        );
    }

    #[test]
    fn priority_breaks_ties_within_a_scope() {
        let low = book("LOW", PriceBookScope::Tenant, 1, true);
        let high = book("HIGH", PriceBookScope::Tenant, 2, true);
        let candidates = vec![
            candidate(low.clone(), entry(&low, 10_000, None, None)),
            candidate(high.clone(), entry(&high, 9_000, None, None)),
        ];
        assert_eq!(resolve(&candidates, ts(8)).unwrap().price_book_code, "HIGH");
    }

    #[test]
    fn carries_the_price_mode_of_the_winning_entry() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let mut inclusive = entry(&base, 11_600, None, None);
        inclusive.price_mode = PriceMode::TaxInclusive;
        let resolved = resolve(&[candidate(base, inclusive)], ts(8)).unwrap();
        assert_eq!(resolved.price_mode, PriceMode::TaxInclusive);
        assert_eq!(resolved.unit_price.minor(), 11_600);
    }

    #[test]
    fn rejects_an_inverted_validity_window() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        let error = PriceEntry::import(NewPriceEntry {
            price_book_id: base.price_book_id.clone(),
            product_ref: product(),
            amount: Money::from_minor(1, Currency::Mxn),
            price_mode: PriceMode::TaxExclusive,
            valid_from: Some(ts(12)),
            valid_until: Some(ts(10)),
        })
        .unwrap_err();
        assert_eq!(error.code(), "CATALOG_INVALID_VALIDITY");
    }

    #[test]
    fn rejects_a_negative_amount_and_an_unnamed_book() {
        let base = book("BASE", PriceBookScope::Tenant, 0, true);
        assert_eq!(
            PriceEntry::import(NewPriceEntry {
                price_book_id: base.price_book_id.clone(),
                product_ref: product(),
                amount: Money::from_minor(-1, Currency::Mxn),
                price_mode: PriceMode::TaxExclusive,
                valid_from: None,
                valid_until: None,
            })
            .unwrap_err()
            .code(),
            "CATALOG_INVALID_RECORD"
        );
        assert!(PriceBook::import(NewPriceBook {
            price_book_id: PriceBookId::new("X").unwrap(),
            code: "  ".to_owned(),
            name: "x".to_owned(),
            currency: Currency::Mxn,
            scope: PriceBookScope::Tenant,
            priority: 0,
            active: true,
        })
        .is_err());
    }
}
