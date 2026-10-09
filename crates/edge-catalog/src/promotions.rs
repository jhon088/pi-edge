//! The promotion model.
//!
//! Promotions arrive in the catalog snapshot like everything else: PI.Edge
//! never creates one. A promotion says *when* and *where* it applies, *how* it
//! competes with others, and *what* it does; the engine in [`crate::basket`]
//! decides the rest.
//!
//! Four rule kinds ship in V1. Applicability is "one product" or "any product";
//! a category/group variant is one more enum variant plus one snapshot table,
//! and is deliberately not written until there is a group to point at.

use edge_domain::prelude::{
    Money, Percentage, ProductRef, PromotionId, Quantity, StoreId, Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::scope::CatalogScope;

/// How a promotion competes with the others that also apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Stackability {
    /// Accumulates with other stackable promotions on the same target.
    Stackable,
    /// Applies alone: nothing else may apply to the same target.
    Exclusive,
}

impl Stackability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Stackability::Stackable => "STACKABLE",
            Stackability::Exclusive => "EXCLUSIVE",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CatalogError> {
        match value {
            "STACKABLE" => Ok(Stackability::Stackable),
            "EXCLUSIVE" => Ok(Stackability::Exclusive),
            other => Err(CatalogError::InvalidRecord {
                field: "stackability",
                reason: format!("unknown value `{other}`"),
            }),
        }
    }

    pub const fn is_exclusive(self) -> bool {
        matches!(self, Stackability::Exclusive)
    }
}

/// Which products a rule looks at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "applies_to", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProductApplicability {
    /// Every line in the basket.
    AnyProduct,
    /// One product, by canonical reference.
    Product { product_ref: ProductRef },
}

impl ProductApplicability {
    pub fn matches(&self, product_ref: &ProductRef) -> bool {
        match self {
            ProductApplicability::AnyProduct => true,
            ProductApplicability::Product {
                product_ref: target,
            } => target == product_ref,
        }
    }

    pub fn product_ref(&self) -> Option<&ProductRef> {
        match self {
            ProductApplicability::AnyProduct => None,
            ProductApplicability::Product { product_ref } => Some(product_ref),
        }
    }
}

/// What a promotion does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PromotionRule {
    /// A proportion off each matching line.
    PercentageOff {
        applies_to: ProductApplicability,
        percentage: Percentage,
    },
    /// A flat amount off each matching line, capped at the line.
    FixedAmountOff {
        applies_to: ProductApplicability,
        amount: Money,
    },
    /// Buy `buy`, get `get` free. A line of `buy + get` units earns one
    /// application, and a line twice that earns two.
    BuyNGetM {
        applies_to: ProductApplicability,
        buy: Quantity,
        get: Quantity,
    },
    /// A flat amount off the basket once it reaches `minimum`.
    BasketThreshold { minimum: Money, amount: Money },
}

impl PromotionRule {
    pub const fn kind(&self) -> &'static str {
        match self {
            PromotionRule::PercentageOff { .. } => "PERCENTAGE_OFF",
            PromotionRule::FixedAmountOff { .. } => "FIXED_AMOUNT_OFF",
            PromotionRule::BuyNGetM { .. } => "BUY_N_GET_M",
            PromotionRule::BasketThreshold { .. } => "BASKET_THRESHOLD",
        }
    }

    /// Whether the rule discounts individual lines or the basket as a whole.
    pub const fn is_line_rule(&self) -> bool {
        !matches!(self, PromotionRule::BasketThreshold { .. })
    }

    pub fn applicability(&self) -> Option<&ProductApplicability> {
        match self {
            PromotionRule::PercentageOff { applies_to, .. }
            | PromotionRule::FixedAmountOff { applies_to, .. }
            | PromotionRule::BuyNGetM { applies_to, .. } => Some(applies_to),
            PromotionRule::BasketThreshold { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Promotion {
    /// Opaque reference assigned upstream.
    pub promotion_id: PromotionId,
    /// Short human code, unique within a snapshot.
    pub code: String,
    pub name: String,
    pub active: bool,
    /// Higher wins within the same target. Ties between exclusives are refused.
    pub priority: i32,
    pub stackability: Stackability,
    pub scope: CatalogScope,
    /// Inclusive lower bound.
    pub valid_from: Option<Timestamp>,
    /// Exclusive upper bound.
    pub valid_until: Option<Timestamp>,
    pub rule: PromotionRule,
}

#[derive(Debug, Clone)]
pub struct NewPromotion {
    pub promotion_id: PromotionId,
    pub code: String,
    pub name: String,
    pub active: bool,
    pub priority: i32,
    pub stackability: Stackability,
    pub scope: CatalogScope,
    pub valid_from: Option<Timestamp>,
    pub valid_until: Option<Timestamp>,
    pub rule: PromotionRule,
}

impl Promotion {
    pub fn import(new: NewPromotion) -> Result<Self, CatalogError> {
        let code = new.code.trim().to_owned();
        if code.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "promotion_code",
                reason: "must not be empty".to_owned(),
            });
        }
        let name = new.name.trim().to_owned();
        if name.is_empty() {
            return Err(CatalogError::InvalidRecord {
                field: "promotion_name",
                reason: "must not be empty".to_owned(),
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
        match &new.rule {
            PromotionRule::FixedAmountOff { amount, .. } => {
                if !amount.is_positive() {
                    return Err(CatalogError::InvalidRecord {
                        field: "promotion_amount",
                        reason: "must be positive".to_owned(),
                    });
                }
            }
            PromotionRule::BasketThreshold { minimum, amount } => {
                if !amount.is_positive() {
                    return Err(CatalogError::InvalidRecord {
                        field: "promotion_amount",
                        reason: "must be positive".to_owned(),
                    });
                }
                if !minimum.is_positive() {
                    return Err(CatalogError::InvalidRecord {
                        field: "promotion_minimum",
                        reason: "must be positive".to_owned(),
                    });
                }
            }
            PromotionRule::BuyNGetM { buy, get, .. } => {
                // Fractional "buy 2 get 1" is meaningless: free units are whole.
                if buy.thousandths() % Quantity::SCALE != 0
                    || get.thousandths() % Quantity::SCALE != 0
                {
                    return Err(CatalogError::InvalidRecord {
                        field: "promotion_quantity",
                        reason: "buy and get must be whole units".to_owned(),
                    });
                }
            }
            PromotionRule::PercentageOff { .. } => {}
        }
        Ok(Self {
            promotion_id: new.promotion_id,
            code,
            name,
            active: new.active,
            priority: new.priority,
            stackability: new.stackability,
            scope: new.scope,
            valid_from: new.valid_from,
            valid_until: new.valid_until,
            rule: new.rule,
        })
    }

    /// `valid_from` inclusive, `valid_until` exclusive — the same interval
    /// convention as price entries.
    pub fn is_in_force_at(&self, effective_at: Timestamp) -> bool {
        let started = self.valid_from.is_none_or(|from| effective_at >= from);
        let not_ended = self.valid_until.is_none_or(|until| effective_at < until);
        started && not_ended
    }

    /// Whether this promotion is even a candidate in this store at this
    /// instant. Says nothing about whether it will produce an amount.
    pub fn is_candidate_at(&self, store_id: &StoreId, effective_at: Timestamp) -> bool {
        self.active && self.scope.applies_to(store_id) && self.is_in_force_at(effective_at)
    }

    pub fn matches_product(&self, product_ref: &ProductRef) -> bool {
        self.rule
            .applicability()
            .is_some_and(|applicability| applicability.matches(product_ref))
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::Currency;

    fn ts(day: u32) -> Timestamp {
        Timestamp::parse_rfc3339(&format!("2026-10-{day:02}T00:00:00Z")).unwrap()
    }

    fn promotion(rule: PromotionRule) -> NewPromotion {
        NewPromotion {
            promotion_id: PromotionId::new("PROMO-1").unwrap(),
            code: " PROMO-1 ".to_owned(),
            name: " Diez por ciento ".to_owned(),
            active: true,
            priority: 0,
            stackability: Stackability::Stackable,
            scope: CatalogScope::Tenant,
            valid_from: None,
            valid_until: None,
            rule,
        }
    }

    fn percentage(bps: u32) -> PromotionRule {
        PromotionRule::PercentageOff {
            applies_to: ProductApplicability::AnyProduct,
            percentage: Percentage::from_bps(bps).unwrap(),
        }
    }

    #[test]
    fn trims_and_validates_identity() {
        let imported = Promotion::import(promotion(percentage(1_000))).unwrap();
        assert_eq!(imported.code, "PROMO-1");
        assert_eq!(imported.name, "Diez por ciento");

        let mut blank = promotion(percentage(1_000));
        blank.code = "   ".to_owned();
        assert!(Promotion::import(blank).is_err());
    }

    #[test]
    fn rejects_a_non_positive_amount() {
        let zero = PromotionRule::FixedAmountOff {
            applies_to: ProductApplicability::AnyProduct,
            amount: Money::from_minor(0, Currency::Mxn),
        };
        assert_eq!(
            Promotion::import(promotion(zero)).unwrap_err().code(),
            "CATALOG_INVALID_RECORD"
        );
    }

    #[test]
    fn rejects_a_threshold_without_a_minimum() {
        let bad = PromotionRule::BasketThreshold {
            minimum: Money::from_minor(0, Currency::Mxn),
            amount: Money::from_minor(5_000, Currency::Mxn),
        };
        assert!(Promotion::import(promotion(bad)).is_err());
    }

    #[test]
    fn rejects_fractional_buy_and_get_quantities() {
        let bad = PromotionRule::BuyNGetM {
            applies_to: ProductApplicability::AnyProduct,
            buy: Quantity::from_thousandths(2_500).unwrap(),
            get: Quantity::from_units(1).unwrap(),
        };
        assert!(Promotion::import(promotion(bad)).is_err());

        let good = PromotionRule::BuyNGetM {
            applies_to: ProductApplicability::AnyProduct,
            buy: Quantity::from_units(2).unwrap(),
            get: Quantity::from_units(1).unwrap(),
        };
        assert!(Promotion::import(promotion(good)).is_ok());
    }

    #[test]
    fn rejects_an_inverted_validity_window() {
        let mut inverted = promotion(percentage(1_000));
        inverted.valid_from = Some(ts(12));
        inverted.valid_until = Some(ts(10));
        assert_eq!(
            Promotion::import(inverted).unwrap_err().code(),
            "CATALOG_INVALID_VALIDITY"
        );
    }

    #[test]
    fn validity_uses_the_same_half_open_interval_as_prices() {
        let mut scheduled = promotion(percentage(1_000));
        scheduled.valid_from = Some(ts(10));
        scheduled.valid_until = Some(ts(12));
        let scheduled = Promotion::import(scheduled).unwrap();
        assert!(!scheduled.is_in_force_at(ts(9)));
        assert!(scheduled.is_in_force_at(ts(10)));
        assert!(scheduled.is_in_force_at(ts(11)));
        assert!(!scheduled.is_in_force_at(ts(12)));
    }

    #[test]
    fn candidacy_needs_active_scope_and_validity() {
        let store = StoreId::new("S-1").unwrap();
        let elsewhere = StoreId::new("S-2").unwrap();

        let open = Promotion::import(promotion(percentage(1_000))).unwrap();
        assert!(open.is_candidate_at(&store, ts(10)));

        let mut inactive = promotion(percentage(1_000));
        inactive.active = false;
        assert!(!Promotion::import(inactive)
            .unwrap()
            .is_candidate_at(&store, ts(10)));

        let mut scoped = promotion(percentage(1_000));
        scoped.scope = CatalogScope::Store {
            store_id: store.clone(),
        };
        let scoped = Promotion::import(scoped).unwrap();
        assert!(scoped.is_candidate_at(&store, ts(10)));
        assert!(!scoped.is_candidate_at(&elsewhere, ts(10)));
    }

    #[test]
    fn classifies_line_rules_and_basket_rules() {
        assert!(percentage(1_000).is_line_rule());
        assert!(!PromotionRule::BasketThreshold {
            minimum: Money::from_minor(50_000, Currency::Mxn),
            amount: Money::from_minor(5_000, Currency::Mxn),
        }
        .is_line_rule());
    }

    #[test]
    fn product_applicability_matches_only_its_target() {
        let one = ProductRef::new("P-1").unwrap();
        let two = ProductRef::new("P-2").unwrap();
        let scoped = ProductApplicability::Product {
            product_ref: one.clone(),
        };
        assert!(scoped.matches(&one));
        assert!(!scoped.matches(&two));
        assert!(ProductApplicability::AnyProduct.matches(&two));

        let rule = PromotionRule::PercentageOff {
            applies_to: scoped,
            percentage: Percentage::from_bps(500).unwrap(),
        };
        let promotion = Promotion::import(promotion(rule)).unwrap();
        assert!(promotion.matches_product(&one));
        assert!(!promotion.matches_product(&two));
    }

    #[test]
    fn stackability_round_trips_through_its_wire_value() {
        for value in [Stackability::Stackable, Stackability::Exclusive] {
            assert_eq!(Stackability::parse(value.as_str()).unwrap(), value);
        }
        assert!(Stackability::parse("MAYBE").is_err());
        assert!(Stackability::Exclusive.is_exclusive());
        assert!(!Stackability::Stackable.is_exclusive());
    }
}
