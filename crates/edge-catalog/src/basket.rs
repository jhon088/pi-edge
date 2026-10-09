//! The promotion engine.
//!
//! A pure, deterministic function of `(basket, promotions, context)`. It never
//! mutates a price and never accumulates state between calls, so a basket that
//! was built by `add → remove → add` evaluates exactly like one built by a
//! single `add`.
//!
//! Everything it produces is a **per-line discount amount** plus an explicit
//! record of why. Basket-level discounts are allocated across lines rather
//! than subtracted from the total, so each line still computes its own tax
//! from its own discounted amount and `net + tax == gross` survives untouched
//! (ADR 015).

use std::collections::BTreeMap;

use edge_domain::prelude::{
    allocate_proportionally, CatalogSnapshotId, Currency, Money, PriceMode, ProductRef,
    PromotionId, Quantity, StoreId, Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::promotions::{Promotion, PromotionRule, Stackability};

/// One line of the basket, already priced by the price books.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasketLine {
    pub line_number: i64,
    pub product_ref: ProductRef,
    pub quantity: Quantity,
    pub unit_price: Money,
    /// Whether `unit_price` includes tax. The engine never touches tax — it is
    /// carried so a rule that one day needs it does not have to re-derive it.
    pub price_mode: PriceMode,
    /// `unit_price × quantity`: the base promotions work on.
    pub extended: Money,
}

#[derive(Debug, Clone)]
pub struct NewBasketLine {
    pub line_number: i64,
    pub product_ref: ProductRef,
    pub quantity: Quantity,
    pub unit_price: Money,
    pub price_mode: PriceMode,
}

impl BasketLine {
    pub fn new(new: NewBasketLine) -> Result<Self, CatalogError> {
        let extended = new.unit_price.mul_quantity(new.quantity)?;
        Ok(Self {
            line_number: new.line_number,
            product_ref: new.product_ref,
            quantity: new.quantity,
            unit_price: new.unit_price,
            price_mode: new.price_mode,
            extended,
        })
    }
}

/// The basket as the engine sees it: lines in a fixed order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricedBasket {
    lines: Vec<BasketLine>,
}

impl PricedBasket {
    /// Lines are sorted by number, so the engine never depends on the order
    /// they happened to arrive in.
    pub fn new(mut lines: Vec<BasketLine>) -> Self {
        lines.sort_by_key(|line| line.line_number);
        Self { lines }
    }

    pub fn lines(&self) -> &[BasketLine] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Everything promotion evaluation is allowed to depend on.
///
/// `customer_context` will join this struct when customer-specific promotions
/// exist; nothing here anticipates it.
#[derive(Debug, Clone, Copy)]
pub struct PromotionContext<'a> {
    pub store_id: &'a StoreId,
    /// Explicit, never `now()`. A sale pins this when it starts.
    pub effective_at: Timestamp,
    pub snapshot_id: CatalogSnapshotId,
    pub currency: Currency,
}

/// What an adjustment applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AdjustmentTarget {
    Line { line_number: i64 },
    Basket,
}

impl AdjustmentTarget {
    /// Stable label stored on the transaction.
    pub fn label(&self) -> String {
        match self {
            AdjustmentTarget::Line { line_number } => format!("LINE:{line_number}"),
            AdjustmentTarget::Basket => "BASKET".to_owned(),
        }
    }

    fn slug(&self) -> String {
        match self {
            AdjustmentTarget::Line { line_number } => format!("line:{line_number}"),
            AdjustmentTarget::Basket => "basket".to_owned(),
        }
    }
}

/// One discount, with everything needed to audit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionAdjustment {
    /// Deterministic composite of promotion and target — not a minted id — so
    /// re-evaluating the same basket produces identical records.
    pub adjustment_id: String,
    pub promotion_id: PromotionId,
    pub promotion_code: String,
    pub promotion_name: String,
    pub priority: i32,
    pub stackability: Stackability,
    pub target: AdjustmentTarget,
    /// Positive discount amount.
    pub amount: Money,
    /// Rule kind that produced it, e.g. `BUY_N_GET_M`.
    pub reason_code: &'static str,
    /// Human detail for the journal, e.g. `2 free units`.
    pub detail: Option<String>,
}

/// Final discount assigned to one line, basket allocations included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineDiscount {
    pub line_number: i64,
    pub amount: Money,
}

/// One promotion and everything it took off this basket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPromotion {
    pub promotion_id: PromotionId,
    pub code: String,
    pub name: String,
    pub priority: i32,
    pub stackability: Stackability,
    pub kind: &'static str,
    pub amount: Money,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionResult {
    pub line_adjustments: Vec<PromotionAdjustment>,
    pub basket_adjustments: Vec<PromotionAdjustment>,
    pub applied_promotions: Vec<AppliedPromotion>,
    /// Per-line totals the transaction applies.
    pub line_discounts: Vec<LineDiscount>,
    pub total_discount: Money,
    /// What a basket threshold was compared against: the sum of line amounts
    /// after line-level discounts.
    pub threshold_basis: Money,
}

impl PromotionResult {
    pub fn empty(currency: Currency) -> Self {
        Self {
            line_adjustments: Vec::new(),
            basket_adjustments: Vec::new(),
            applied_promotions: Vec::new(),
            line_discounts: Vec::new(),
            total_discount: Money::zero(currency),
            threshold_basis: Money::zero(currency),
        }
    }

    pub fn discount_for(&self, line_number: i64) -> Option<Money> {
        self.line_discounts
            .iter()
            .find(|discount| discount.line_number == line_number)
            .map(|discount| discount.amount)
    }

    /// Every adjustment, line ones first, in evaluation order.
    pub fn adjustments(&self) -> impl Iterator<Item = &PromotionAdjustment> {
        self.line_adjustments
            .iter()
            .chain(self.basket_adjustments.iter())
    }
}

/// Evaluate every promotion against the basket.
pub fn evaluate(
    basket: &PricedBasket,
    promotions: &[Promotion],
    context: &PromotionContext<'_>,
) -> Result<PromotionResult, CatalogError> {
    let zero = Money::zero(context.currency);
    if basket.is_empty() {
        return Ok(PromotionResult::empty(context.currency));
    }

    // One deterministic order for every decision: priority first, then id.
    let mut candidates: Vec<&Promotion> = promotions
        .iter()
        .filter(|promotion| promotion.is_candidate_at(context.store_id, context.effective_at))
        .collect();
    candidates.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.promotion_id.cmp(&right.promotion_id))
    });

    let mut discounts: BTreeMap<i64, Money> = basket
        .lines()
        .iter()
        .map(|line| (line.line_number, zero))
        .collect();
    let mut line_adjustments = Vec::new();

    // ---- line rules, line by line ----------------------------------------
    for line in basket.lines() {
        let target = AdjustmentTarget::Line {
            line_number: line.line_number,
        };
        let mut applicable = Vec::new();
        for promotion in &candidates {
            if !promotion.rule.is_line_rule() || !promotion.matches_product(&line.product_ref) {
                continue;
            }
            // Would it do anything at all? Decided on the full line so a
            // zero-value exclusive cannot block a stack.
            if line_amount(promotion, line, line.extended)?.is_positive() {
                applicable.push(*promotion);
            }
        }

        let mut remaining = line.extended;
        for promotion in select(&applicable, &target)? {
            // Stacking is sequential: each promotion works on what is left.
            let amount = line_amount(promotion, line, remaining)?;
            if !amount.is_positive() {
                continue;
            }
            remaining = remaining.checked_sub(amount)?;
            let entry =
                discounts
                    .get_mut(&line.line_number)
                    .ok_or(CatalogError::InvalidRecord {
                        field: "line_number",
                        reason: "unknown basket line".to_owned(),
                    })?;
            *entry = entry.checked_add(amount)?;
            line_adjustments.push(adjustment(
                promotion,
                target,
                amount,
                detail(promotion, line)?,
            ));
        }
    }

    // ---- basket rules, against the post-line-discount basis --------------
    let mut threshold_basis = zero;
    for line in basket.lines() {
        let discount = discounts.get(&line.line_number).copied().unwrap_or(zero);
        threshold_basis = threshold_basis.checked_add(line.extended.checked_sub(discount)?)?;
    }

    let mut applicable = Vec::new();
    for promotion in &candidates {
        if promotion.rule.is_line_rule() {
            continue;
        }
        if basket_amount(promotion, threshold_basis)?.is_positive() {
            applicable.push(*promotion);
        }
    }

    let mut basket_adjustments = Vec::new();
    let mut basket_remaining = threshold_basis;
    for promotion in select(&applicable, &AdjustmentTarget::Basket)? {
        let amount = basket_amount(promotion, basket_remaining)?;
        if !amount.is_positive() {
            continue;
        }
        // Allocate across lines so every line taxes its own discounted amount.
        let weights: Vec<(i64, Money)> = basket
            .lines()
            .iter()
            .map(|line| {
                let discount = discounts.get(&line.line_number).copied().unwrap_or(zero);
                Ok((line.line_number, line.extended.checked_sub(discount)?))
            })
            .collect::<Result<_, CatalogError>>()?;
        let allocation = allocate(amount, &weights);
        if allocation.is_empty() {
            continue;
        }
        for share in &allocation {
            if let Some(entry) = discounts.get_mut(&share.line_number) {
                *entry = entry.checked_add(share.amount)?;
            }
        }
        basket_remaining = basket_remaining.checked_sub(amount)?;
        basket_adjustments.push(adjustment(
            promotion,
            AdjustmentTarget::Basket,
            amount,
            Some(format!(
                "allocated across {} line(s) over a basis of {}",
                allocation.len(),
                threshold_basis.format_amount()
            )),
        ));
    }

    // ---- summaries --------------------------------------------------------
    let line_discounts: Vec<LineDiscount> = discounts
        .iter()
        .filter(|(_, amount)| amount.is_positive())
        .map(|(line_number, amount)| LineDiscount {
            line_number: *line_number,
            amount: *amount,
        })
        .collect();
    let mut total_discount = zero;
    for discount in &line_discounts {
        total_discount = total_discount.checked_add(discount.amount)?;
    }

    let mut applied_promotions = Vec::new();
    for promotion in &candidates {
        let mut amount = zero;
        for adjustment in line_adjustments.iter().chain(basket_adjustments.iter()) {
            if adjustment.promotion_id == promotion.promotion_id {
                amount = amount.checked_add(adjustment.amount)?;
            }
        }
        if amount.is_positive() {
            applied_promotions.push(AppliedPromotion {
                promotion_id: promotion.promotion_id.clone(),
                code: promotion.code.clone(),
                name: promotion.name.clone(),
                priority: promotion.priority,
                stackability: promotion.stackability,
                kind: promotion.rule.kind(),
                amount,
            });
        }
    }

    Ok(PromotionResult {
        line_adjustments,
        basket_adjustments,
        applied_promotions,
        line_discounts,
        total_discount,
        threshold_basis,
    })
}

/// Exclusive/stackable arbitration for one target.
fn select<'a>(
    ordered: &[&'a Promotion],
    target: &AdjustmentTarget,
) -> Result<Vec<&'a Promotion>, CatalogError> {
    let Some(first) = ordered.first() else {
        return Ok(Vec::new());
    };
    if first.stackability.is_exclusive() {
        let tied: Vec<&&Promotion> = ordered
            .iter()
            .filter(|promotion| {
                promotion.priority == first.priority && promotion.stackability.is_exclusive()
            })
            .collect();
        if tied.len() > 1 {
            let mut ids: Vec<String> = tied
                .iter()
                .map(|promotion| promotion.promotion_id.to_string())
                .collect();
            ids.sort();
            return Err(CatalogError::AmbiguousPromotion {
                target: target.label(),
                priority: first.priority,
                promotions: ids.join(", "),
            });
        }
        // Applies alone.
        return Ok(vec![first]);
    }
    // An exclusive promotion cannot join a stack, so it is skipped here.
    Ok(ordered
        .iter()
        .filter(|promotion| !promotion.stackability.is_exclusive())
        .copied()
        .collect())
}

/// What a line rule takes off, capped at what is left of the line.
fn line_amount(
    promotion: &Promotion,
    line: &BasketLine,
    remaining: Money,
) -> Result<Money, CatalogError> {
    if !remaining.is_positive() {
        return Ok(Money::zero(remaining.currency()));
    }
    let amount = match &promotion.rule {
        PromotionRule::PercentageOff { percentage, .. } => percentage.of(remaining)?,
        PromotionRule::FixedAmountOff { amount, .. } => *amount,
        PromotionRule::BuyNGetM { buy, get, .. } => {
            let group = buy
                .thousandths()
                .checked_add(get.thousandths())
                .unwrap_or(i64::MAX);
            let sets = line.quantity.thousandths() / group;
            let free = sets.saturating_mul(get.thousandths());
            match Quantity::from_thousandths(free) {
                Ok(free) => line.unit_price.mul_quantity(free)?,
                // No complete group yet: nothing is free.
                Err(_) => Money::zero(remaining.currency()),
            }
        }
        PromotionRule::BasketThreshold { .. } => Money::zero(remaining.currency()),
    };
    Ok(cap(amount, remaining))
}

/// What a basket rule takes off, capped at the remaining basket basis.
fn basket_amount(promotion: &Promotion, basis: Money) -> Result<Money, CatalogError> {
    let zero = Money::zero(basis.currency());
    if !basis.is_positive() {
        return Ok(zero);
    }
    match &promotion.rule {
        PromotionRule::BasketThreshold { minimum, amount } => {
            if basis.is_at_least(minimum)? {
                Ok(cap(*amount, basis))
            } else {
                Ok(zero)
            }
        }
        _ => Ok(zero),
    }
}

/// A discount never exceeds what it discounts.
fn cap(amount: Money, limit: Money) -> Money {
    if amount.minor() > limit.minor() {
        limit
    } else {
        amount
    }
}

fn detail(promotion: &Promotion, line: &BasketLine) -> Result<Option<String>, CatalogError> {
    Ok(match &promotion.rule {
        PromotionRule::BuyNGetM { buy, get, .. } => {
            let group = buy
                .thousandths()
                .checked_add(get.thousandths())
                .unwrap_or(i64::MAX);
            let sets = line.quantity.thousandths() / group;
            let free = sets.saturating_mul(get.thousandths()) / Quantity::SCALE;
            Some(format!("{sets} application(s), {free} free unit(s)"))
        }
        PromotionRule::PercentageOff { percentage, .. } => {
            Some(format!("{} bps off", percentage.bps()))
        }
        _ => None,
    })
}

fn adjustment(
    promotion: &Promotion,
    target: AdjustmentTarget,
    amount: Money,
    detail: Option<String>,
) -> PromotionAdjustment {
    PromotionAdjustment {
        adjustment_id: format!("{}#{}", promotion.promotion_id, target.slug()),
        promotion_id: promotion.promotion_id.clone(),
        promotion_code: promotion.code.clone(),
        promotion_name: promotion.name.clone(),
        priority: promotion.priority,
        stackability: promotion.stackability,
        target,
        amount,
        reason_code: promotion.rule.kind(),
        detail,
    }
}

/// Spread a basket amount across the lines it applies to.
///
/// A thin adaptation of the core's single allocation policy
/// (`allocate_proportionally`) onto line numbers: weights are passed in
/// ascending line order, so the policy's index tie-break *is* the line-number
/// tie-break, and zero shares are dropped because a line that received nothing
/// carries no discount.
fn allocate(amount: Money, weights: &[(i64, Money)]) -> Vec<LineDiscount> {
    let amounts: Vec<Money> = weights.iter().map(|(_, weight)| *weight).collect();
    allocate_proportionally(amount, &amounts)
        .into_iter()
        .zip(weights)
        .filter(|(share, _)| share.is_positive())
        .map(|(share, (line_number, _))| LineDiscount {
            line_number: *line_number,
            amount: share,
        })
        .collect()
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::promotions::{NewPromotion, ProductApplicability};
    use crate::scope::CatalogScope;
    use edge_domain::prelude::Percentage;
    use uuid::Uuid;

    const MXN: Currency = Currency::Mxn;

    fn money(minor: i64) -> Money {
        Money::from_minor(minor, MXN)
    }

    fn ts(day: u32) -> Timestamp {
        Timestamp::parse_rfc3339(&format!("2026-10-{day:02}T12:00:00Z")).unwrap()
    }

    fn store() -> StoreId {
        StoreId::new("S-1").unwrap()
    }

    fn line(line_number: i64, product: &str, unit_minor: i64, qty_units: i64) -> BasketLine {
        BasketLine::new(NewBasketLine {
            line_number,
            product_ref: ProductRef::new(product).unwrap(),
            quantity: Quantity::from_units(qty_units).unwrap(),
            unit_price: money(unit_minor),
            price_mode: PriceMode::TaxExclusive,
        })
        .unwrap()
    }

    fn promo(
        id: &str,
        priority: i32,
        stackability: Stackability,
        rule: PromotionRule,
    ) -> Promotion {
        Promotion::import(NewPromotion {
            promotion_id: PromotionId::new(id).unwrap(),
            code: id.to_owned(),
            name: format!("promo {id}"),
            active: true,
            priority,
            stackability,
            scope: CatalogScope::Tenant,
            valid_from: None,
            valid_until: None,
            rule,
        })
        .unwrap()
    }

    fn percentage_off(product: Option<&str>, bps: u32) -> PromotionRule {
        PromotionRule::PercentageOff {
            applies_to: applicability(product),
            percentage: Percentage::from_bps(bps).unwrap(),
        }
    }

    fn fixed_off(product: Option<&str>, minor: i64) -> PromotionRule {
        PromotionRule::FixedAmountOff {
            applies_to: applicability(product),
            amount: money(minor),
        }
    }

    fn buy_n_get_m(product: Option<&str>, buy: i64, get: i64) -> PromotionRule {
        PromotionRule::BuyNGetM {
            applies_to: applicability(product),
            buy: Quantity::from_units(buy).unwrap(),
            get: Quantity::from_units(get).unwrap(),
        }
    }

    fn threshold(minimum: i64, amount: i64) -> PromotionRule {
        PromotionRule::BasketThreshold {
            minimum: money(minimum),
            amount: money(amount),
        }
    }

    fn applicability(product: Option<&str>) -> ProductApplicability {
        match product {
            Some(product) => ProductApplicability::Product {
                product_ref: ProductRef::new(product).unwrap(),
            },
            None => ProductApplicability::AnyProduct,
        }
    }

    fn run(
        lines: Vec<BasketLine>,
        promotions: &[Promotion],
    ) -> Result<PromotionResult, CatalogError> {
        run_at(lines, promotions, ts(8))
    }

    fn run_at(
        lines: Vec<BasketLine>,
        promotions: &[Promotion],
        effective_at: Timestamp,
    ) -> Result<PromotionResult, CatalogError> {
        let store = store();
        evaluate(
            &PricedBasket::new(lines),
            promotions,
            &PromotionContext {
                store_id: &store,
                effective_at,
                snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(1)),
                currency: MXN,
            },
        )
    }

    #[test]
    fn an_empty_basket_has_no_promotions() {
        let result = run(
            Vec::new(),
            &[promo(
                "A",
                0,
                Stackability::Stackable,
                percentage_off(None, 1_000),
            )],
        )
        .unwrap();
        assert!(result.applied_promotions.is_empty());
        assert_eq!(result.total_discount, money(0));
    }

    #[test]
    fn applies_a_percentage_discount() {
        let result = run(
            vec![line(1, "P-1", 10_000, 2)],
            &[promo(
                "PCT",
                0,
                Stackability::Stackable,
                percentage_off(Some("P-1"), 1_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(2_000));
        assert_eq!(result.discount_for(1), Some(money(2_000)));
        assert_eq!(result.line_adjustments.len(), 1);
        let adjustment = &result.line_adjustments[0];
        assert_eq!(adjustment.adjustment_id, "PCT#line:1");
        assert_eq!(adjustment.reason_code, "PERCENTAGE_OFF");
        assert_eq!(adjustment.target, AdjustmentTarget::Line { line_number: 1 });
        assert_eq!(result.applied_promotions[0].amount, money(2_000));
    }

    #[test]
    fn applies_a_fixed_discount_capped_at_the_line() {
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[promo(
                "FIX",
                0,
                Stackability::Stackable,
                fixed_off(Some("P-1"), 2_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(2_000));

        // A discount bigger than the line is capped, never negative.
        let result = run(
            vec![line(1, "P-1", 1_500, 1)],
            &[promo(
                "FIX",
                0,
                Stackability::Stackable,
                fixed_off(Some("P-1"), 2_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(1_500));
    }

    #[test]
    fn buy_two_get_one_free() {
        // 3 units at 10.00: one is free.
        let result = run(
            vec![line(1, "P-1", 1_000, 3)],
            &[promo(
                "BNG",
                0,
                Stackability::Stackable,
                buy_n_get_m(Some("P-1"), 2, 1),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(1_000));
        assert_eq!(
            result.line_adjustments[0].detail.as_deref(),
            Some("1 application(s), 1 free unit(s)")
        );
    }

    #[test]
    fn buy_two_get_one_applies_twice_at_quantity_six() {
        let result = run(
            vec![line(1, "P-1", 1_000, 6)],
            &[promo(
                "BNG",
                0,
                Stackability::Stackable,
                buy_n_get_m(Some("P-1"), 2, 1),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(2_000));
        assert_eq!(
            result.line_adjustments[0].detail.as_deref(),
            Some("2 application(s), 2 free unit(s)")
        );
    }

    #[test]
    fn buy_two_get_one_does_nothing_below_a_full_group() {
        for quantity in [1, 2] {
            let result = run(
                vec![line(1, "P-1", 1_000, quantity)],
                &[promo(
                    "BNG",
                    0,
                    Stackability::Stackable,
                    buy_n_get_m(Some("P-1"), 2, 1),
                )],
            )
            .unwrap();
            assert_eq!(result.total_discount, money(0), "quantity {quantity}");
            assert!(result.applied_promotions.is_empty());
        }
    }

    #[test]
    fn a_basket_threshold_below_the_minimum_does_nothing() {
        let result = run(
            vec![line(1, "P-1", 49_900, 1)],
            &[promo(
                "THR",
                0,
                Stackability::Stackable,
                threshold(50_000, 5_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(0));
        assert_eq!(result.threshold_basis, money(49_900));
        assert!(result.basket_adjustments.is_empty());
    }

    #[test]
    fn a_basket_threshold_exactly_at_the_minimum_applies() {
        let result = run(
            vec![line(1, "P-1", 50_000, 1)],
            &[promo(
                "THR",
                0,
                Stackability::Stackable,
                threshold(50_000, 5_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(5_000));
        assert_eq!(result.basket_adjustments.len(), 1);
        assert_eq!(
            result.basket_adjustments[0].target,
            AdjustmentTarget::Basket
        );
        assert_eq!(result.basket_adjustments[0].adjustment_id, "THR#basket");
    }

    #[test]
    fn a_basket_threshold_above_the_minimum_applies_once() {
        let result = run(
            vec![line(1, "P-1", 60_000, 2)],
            &[promo(
                "THR",
                0,
                Stackability::Stackable,
                threshold(50_000, 5_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(5_000));
    }

    #[test]
    fn a_basket_discount_is_allocated_across_lines_and_sums_exactly() {
        // Three lines of 33.33, 33.33 and 33.34: a 10.00 discount cannot split
        // evenly.
        let result = run(
            vec![
                line(1, "P-1", 3_333, 1),
                line(2, "P-2", 3_333, 1),
                line(3, "P-3", 3_334, 1),
            ],
            &[promo(
                "THR",
                0,
                Stackability::Stackable,
                threshold(10_000, 1_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(1_000));
        let allocated: i64 = result
            .line_discounts
            .iter()
            .map(|discount| discount.amount.minor())
            .sum();
        assert_eq!(allocated, 1_000, "allocation must be exact");
        // Largest remainder, ties by line number.
        assert_eq!(result.discount_for(1), Some(money(333)));
        assert_eq!(result.discount_for(2), Some(money(333)));
        assert_eq!(result.discount_for(3), Some(money(334)));
    }

    #[test]
    fn a_basket_threshold_is_measured_after_line_discounts() {
        // 600.00 of goods, half off one line leaves 300.00: below the 500.00
        // threshold, so the basket promotion does not fire.
        let result = run(
            vec![line(1, "P-1", 60_000, 1)],
            &[
                promo(
                    "PCT",
                    10,
                    Stackability::Stackable,
                    percentage_off(Some("P-1"), 5_000),
                ),
                promo("THR", 0, Stackability::Stackable, threshold(50_000, 5_000)),
            ],
        )
        .unwrap();
        assert_eq!(result.threshold_basis, money(30_000));
        assert_eq!(result.total_discount, money(30_000));
        assert!(result.basket_adjustments.is_empty());
    }

    #[test]
    fn an_inactive_promotion_is_ignored() {
        let mut inactive = promo(
            "PCT",
            0,
            Stackability::Stackable,
            percentage_off(None, 1_000),
        );
        inactive.active = false;
        let result = run(vec![line(1, "P-1", 10_000, 1)], &[inactive]).unwrap();
        assert_eq!(result.total_discount, money(0));
    }

    #[test]
    fn a_future_promotion_is_ignored_and_an_expired_one_too() {
        let mut scheduled = promo(
            "PCT",
            0,
            Stackability::Stackable,
            percentage_off(None, 1_000),
        );
        scheduled.valid_from = Some(ts(10));
        scheduled.valid_until = Some(ts(12));
        let scheduled = [scheduled];

        assert_eq!(
            run_at(vec![line(1, "P-1", 10_000, 1)], &scheduled, ts(9))
                .unwrap()
                .total_discount,
            money(0),
            "not started yet"
        );
        assert_eq!(
            run_at(vec![line(1, "P-1", 10_000, 1)], &scheduled, ts(10))
                .unwrap()
                .total_discount,
            money(1_000),
            "valid_from is inclusive"
        );
        assert_eq!(
            run_at(vec![line(1, "P-1", 10_000, 1)], &scheduled, ts(12))
                .unwrap()
                .total_discount,
            money(0),
            "valid_until is exclusive"
        );
        assert_eq!(
            run_at(vec![line(1, "P-1", 10_000, 1)], &scheduled, ts(13))
                .unwrap()
                .total_discount,
            money(0),
            "expired"
        );
    }

    #[test]
    fn a_promotion_scoped_to_another_store_is_ignored() {
        let mut elsewhere = promo(
            "PCT",
            0,
            Stackability::Stackable,
            percentage_off(None, 1_000),
        );
        elsewhere.scope = CatalogScope::Store {
            store_id: StoreId::new("S-2").unwrap(),
        };
        let result = run(vec![line(1, "P-1", 10_000, 1)], &[elsewhere]).unwrap();
        assert_eq!(result.total_discount, money(0));

        let mut here = promo(
            "PCT",
            0,
            Stackability::Stackable,
            percentage_off(None, 1_000),
        );
        here.scope = CatalogScope::Store { store_id: store() };
        let result = run(vec![line(1, "P-1", 10_000, 1)], &[here]).unwrap();
        assert_eq!(result.total_discount, money(1_000));
    }

    #[test]
    fn a_promotion_for_another_product_is_ignored() {
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[promo(
                "PCT",
                0,
                Stackability::Stackable,
                percentage_off(Some("P-2"), 1_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(0));
    }

    #[test]
    fn stackable_promotions_accumulate_sequentially_in_priority_order() {
        // 20.00 off first (priority 10), then 10% of what is left.
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[
                promo(
                    "PCT",
                    5,
                    Stackability::Stackable,
                    percentage_off(Some("P-1"), 1_000),
                ),
                promo(
                    "FIX",
                    10,
                    Stackability::Stackable,
                    fixed_off(Some("P-1"), 2_000),
                ),
            ],
        )
        .unwrap();
        assert_eq!(result.line_adjustments.len(), 2);
        assert_eq!(result.line_adjustments[0].promotion_code, "FIX");
        assert_eq!(result.line_adjustments[0].amount, money(2_000));
        assert_eq!(result.line_adjustments[1].promotion_code, "PCT");
        // 10% of the remaining 80.00, not of the original 100.00.
        assert_eq!(result.line_adjustments[1].amount, money(800));
        assert_eq!(result.total_discount, money(2_800));
    }

    #[test]
    fn an_exclusive_promotion_applies_alone() {
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[
                promo(
                    "EXC",
                    10,
                    Stackability::Exclusive,
                    fixed_off(Some("P-1"), 2_000),
                ),
                promo(
                    "PCT",
                    5,
                    Stackability::Stackable,
                    percentage_off(Some("P-1"), 1_000),
                ),
            ],
        )
        .unwrap();
        assert_eq!(result.line_adjustments.len(), 1);
        assert_eq!(result.line_adjustments[0].promotion_code, "EXC");
        assert_eq!(result.total_discount, money(2_000));
    }

    #[test]
    fn an_exclusive_promotion_cannot_join_a_stack() {
        // The stackable one ranks higher, so the exclusive is skipped.
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[
                promo(
                    "PCT",
                    10,
                    Stackability::Stackable,
                    percentage_off(Some("P-1"), 1_000),
                ),
                promo(
                    "EXC",
                    5,
                    Stackability::Exclusive,
                    fixed_off(Some("P-1"), 2_000),
                ),
            ],
        )
        .unwrap();
        assert_eq!(result.line_adjustments.len(), 1);
        assert_eq!(result.line_adjustments[0].promotion_code, "PCT");
    }

    #[test]
    fn two_exclusive_promotions_at_the_same_priority_are_refused() {
        let error = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[
                promo(
                    "EXC-A",
                    10,
                    Stackability::Exclusive,
                    fixed_off(Some("P-1"), 2_000),
                ),
                promo(
                    "EXC-B",
                    10,
                    Stackability::Exclusive,
                    percentage_off(Some("P-1"), 1_000),
                ),
            ],
        )
        .unwrap_err();
        assert_eq!(error.code(), "AMBIGUOUS_PROMOTION");
        match error {
            CatalogError::AmbiguousPromotion {
                target,
                priority,
                promotions,
            } => {
                assert_eq!(target, "LINE:1");
                assert_eq!(priority, 10);
                assert_eq!(promotions, "EXC-A, EXC-B");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_lower_priority_exclusive_does_not_make_the_winner_ambiguous() {
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[
                promo(
                    "EXC-A",
                    10,
                    Stackability::Exclusive,
                    fixed_off(Some("P-1"), 2_000),
                ),
                promo(
                    "EXC-B",
                    5,
                    Stackability::Exclusive,
                    fixed_off(Some("P-1"), 3_000),
                ),
            ],
        )
        .unwrap();
        assert_eq!(result.line_adjustments[0].promotion_code, "EXC-A");
    }

    #[test]
    fn two_exclusive_basket_promotions_at_the_same_priority_are_refused() {
        let error = run(
            vec![line(1, "P-1", 60_000, 1)],
            &[
                promo(
                    "THR-A",
                    0,
                    Stackability::Exclusive,
                    threshold(50_000, 5_000),
                ),
                promo(
                    "THR-B",
                    0,
                    Stackability::Exclusive,
                    threshold(50_000, 6_000),
                ),
            ],
        )
        .unwrap_err();
        assert_eq!(error.code(), "AMBIGUOUS_PROMOTION");
        match error {
            CatalogError::AmbiguousPromotion { target, .. } => assert_eq!(target, "BASKET"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn stackable_promotions_tied_at_one_priority_are_not_ambiguous() {
        // Both apply, so no choice is being made; id order fixes the sequence.
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[
                promo(
                    "B",
                    0,
                    Stackability::Stackable,
                    fixed_off(Some("P-1"), 1_000),
                ),
                promo(
                    "A",
                    0,
                    Stackability::Stackable,
                    fixed_off(Some("P-1"), 2_000),
                ),
            ],
        )
        .unwrap();
        assert_eq!(result.line_adjustments[0].promotion_code, "A");
        assert_eq!(result.line_adjustments[1].promotion_code, "B");
        assert_eq!(result.total_discount, money(3_000));
    }

    #[test]
    fn stacked_discounts_can_never_drive_a_line_below_zero() {
        let result = run(
            vec![line(1, "P-1", 10_000, 1)],
            &[
                promo(
                    "A",
                    30,
                    Stackability::Stackable,
                    fixed_off(Some("P-1"), 8_000),
                ),
                promo(
                    "B",
                    20,
                    Stackability::Stackable,
                    fixed_off(Some("P-1"), 8_000),
                ),
                promo(
                    "C",
                    10,
                    Stackability::Stackable,
                    percentage_off(Some("P-1"), 10_000),
                ),
            ],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(10_000));
        assert!(result.discount_for(1).unwrap().minor() <= 10_000);
    }

    #[test]
    fn evaluation_does_not_depend_on_the_order_promotions_arrive_in() {
        let lines = vec![line(1, "P-1", 10_000, 1), line(2, "P-2", 5_000, 3)];
        let promotions = vec![
            promo(
                "PCT",
                5,
                Stackability::Stackable,
                percentage_off(None, 1_000),
            ),
            promo(
                "BNG",
                10,
                Stackability::Stackable,
                buy_n_get_m(Some("P-2"), 2, 1),
            ),
            promo("THR", 1, Stackability::Stackable, threshold(10_000, 1_000)),
        ];
        let forward = run(lines.clone(), &promotions).unwrap();
        let reversed: Vec<Promotion> = promotions.iter().rev().cloned().collect();
        let backward = run(lines.clone(), &reversed).unwrap();
        assert_eq!(forward, backward);

        // Nor on the order the lines arrive in.
        let shuffled: Vec<BasketLine> = lines.iter().rev().cloned().collect();
        assert_eq!(run(shuffled, &promotions).unwrap(), forward);
    }

    #[test]
    fn re_evaluating_the_same_basket_is_idempotent() {
        let lines = vec![line(1, "P-1", 10_000, 2)];
        let promotions = [promo(
            "PCT",
            0,
            Stackability::Stackable,
            percentage_off(None, 1_000),
        )];
        let first = run(lines.clone(), &promotions).unwrap();
        let again = run(lines, &promotions).unwrap();
        assert_eq!(first, again);
        assert_eq!(first.line_adjustments[0].adjustment_id, "PCT#line:1");
    }

    #[test]
    fn removing_a_line_removes_its_promotion() {
        let promotions = [promo(
            "BNG",
            0,
            Stackability::Stackable,
            buy_n_get_m(Some("P-2"), 2, 1),
        )];
        let with_both = run(
            vec![line(1, "P-1", 10_000, 1), line(2, "P-2", 1_000, 3)],
            &promotions,
        )
        .unwrap();
        assert_eq!(with_both.total_discount, money(1_000));

        let without = run(vec![line(1, "P-1", 10_000, 1)], &promotions).unwrap();
        assert_eq!(without.total_discount, money(0));
        assert!(without.applied_promotions.is_empty());
    }

    #[test]
    fn changing_a_quantity_recalculates_from_scratch() {
        let promotions = [promo(
            "BNG",
            0,
            Stackability::Stackable,
            buy_n_get_m(Some("P-1"), 2, 1),
        )];
        assert_eq!(
            run(vec![line(1, "P-1", 1_000, 3)], &promotions)
                .unwrap()
                .total_discount,
            money(1_000)
        );
        assert_eq!(
            run(vec![line(1, "P-1", 1_000, 6)], &promotions)
                .unwrap()
                .total_discount,
            money(2_000)
        );
        assert_eq!(
            run(vec![line(1, "P-1", 1_000, 2)], &promotions)
                .unwrap()
                .total_discount,
            money(0)
        );
    }

    #[test]
    fn percentage_rounding_follows_the_core_policy() {
        // 10% of 2.55 = 0.255 -> 0.26, half away from zero.
        let result = run(
            vec![line(1, "P-1", 255, 1)],
            &[promo(
                "PCT",
                0,
                Stackability::Stackable,
                percentage_off(None, 1_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(26));

        // 33.33% of 1.00 = 0.3333 -> 0.33
        let result = run(
            vec![line(1, "P-1", 100, 1)],
            &[promo(
                "PCT",
                0,
                Stackability::Stackable,
                percentage_off(None, 3_333),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(33));
    }

    #[test]
    fn a_basket_discount_never_exceeds_the_basket() {
        let result = run(
            vec![line(1, "P-1", 50_000, 1)],
            &[promo(
                "THR",
                0,
                Stackability::Stackable,
                threshold(10_000, 90_000),
            )],
        )
        .unwrap();
        assert_eq!(result.total_discount, money(50_000));
        assert_eq!(result.discount_for(1), Some(money(50_000)));
    }

    #[test]
    fn a_line_already_fully_discounted_is_skipped_by_a_basket_promotion() {
        let result = run(
            vec![line(1, "P-1", 10_000, 1), line(2, "P-2", 50_000, 1)],
            &[
                promo(
                    "FULL",
                    10,
                    Stackability::Stackable,
                    percentage_off(Some("P-1"), 10_000),
                ),
                promo("THR", 0, Stackability::Stackable, threshold(10_000, 1_000)),
            ],
        )
        .unwrap();
        // Line 1 is already free, so the basket discount lands entirely on 2.
        assert_eq!(result.discount_for(1), Some(money(10_000)));
        assert_eq!(result.discount_for(2), Some(money(1_000)));
        assert_eq!(result.total_discount, money(11_000));
    }
}
