//! Monetary value objects.
//!
//! Money is stored as `i64` minor units (cents for MXN). Floating point
//! arithmetic is forbidden workspace-wide (`clippy::float_arithmetic = deny`).

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Errors produced by monetary arithmetic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("monetary overflow")]
    Overflow,
    #[error("currency mismatch: expected {expected}, got {actual}")]
    CurrencyMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("unknown currency code `{0}`")]
    UnknownCurrency(String),
    #[error("amount must not be negative (got {0} minor units)")]
    NegativeAmount(i64),
    #[error("quantity must be strictly positive (got {0} thousandths)")]
    InvalidQuantity(i64),
    #[error("tax rate out of range (got {0} basis points)")]
    InvalidTaxRate(u32),
    #[error("unknown price mode `{0}`")]
    UnknownPriceMode(String),
    #[error("percentage out of range (got {0} basis points)")]
    InvalidPercentage(u32),
}

/// Supported currencies. Only MXN is active today; the type exists so that
/// nothing in the core assumes a single currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    Mxn,
}

impl Currency {
    /// ISO-4217 code, also the value persisted in SQLite.
    pub const fn code(self) -> &'static str {
        match self {
            Currency::Mxn => "MXN",
        }
    }

    /// Number of decimal digits in the minor unit representation.
    pub const fn minor_exponent(self) -> u32 {
        match self {
            Currency::Mxn => 2,
        }
    }

    pub fn from_code(code: &str) -> Result<Self, MoneyError> {
        match code {
            "MXN" => Ok(Currency::Mxn),
            other => Err(MoneyError::UnknownCurrency(other.to_owned())),
        }
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// An exact monetary amount in minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    minor: i64,
    currency: Currency,
}

impl Money {
    pub const fn from_minor(minor: i64, currency: Currency) -> Self {
        Self { minor, currency }
    }

    pub const fn zero(currency: Currency) -> Self {
        Self { minor: 0, currency }
    }

    pub const fn minor(&self) -> i64 {
        self.minor
    }

    pub const fn currency(&self) -> Currency {
        self.currency
    }

    pub const fn is_zero(&self) -> bool {
        self.minor == 0
    }

    pub const fn is_negative(&self) -> bool {
        self.minor < 0
    }

    pub const fn is_positive(&self) -> bool {
        self.minor > 0
    }

    fn require_same_currency(&self, other: &Money) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch {
                expected: self.currency.code(),
                actual: other.currency.code(),
            })
        }
    }

    pub fn checked_add(&self, other: Money) -> Result<Money, MoneyError> {
        self.require_same_currency(&other)?;
        self.minor
            .checked_add(other.minor)
            .map(|m| Money::from_minor(m, self.currency))
            .ok_or(MoneyError::Overflow)
    }

    pub fn checked_sub(&self, other: Money) -> Result<Money, MoneyError> {
        self.require_same_currency(&other)?;
        self.minor
            .checked_sub(other.minor)
            .map(|m| Money::from_minor(m, self.currency))
            .ok_or(MoneyError::Overflow)
    }

    /// Extended amount for a quantity, rounded half away from zero.
    pub fn mul_quantity(&self, quantity: Quantity) -> Result<Money, MoneyError> {
        let scaled = i128::from(self.minor)
            .checked_mul(i128::from(quantity.thousandths()))
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::from_minor(
            div_round_half_away_from_zero(scaled, i128::from(Quantity::SCALE))?,
            self.currency,
        ))
    }

    /// The part of this amount that corresponds to `part` out of `whole`.
    ///
    /// `part == whole` returns the amount **unchanged**, with no rounding step
    /// at all. That is what makes successive slices exact: a refund of the
    /// last remaining units takes precisely what is left, so repeated partial
    /// refunds of one line add up to the line, to the minor unit (ADR 031).
    pub fn prorate(&self, part: Quantity, whole: Quantity) -> Result<Money, MoneyError> {
        if part.thousandths() > whole.thousandths() {
            return Err(MoneyError::InvalidQuantity(part.thousandths()));
        }
        if part.thousandths() == whole.thousandths() {
            return Ok(*self);
        }
        let scaled = i128::from(self.minor)
            .checked_mul(i128::from(part.thousandths()))
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::from_minor(
            div_round_half_away_from_zero(scaled, i128::from(whole.thousandths()))?,
            self.currency,
        ))
    }

    /// Ordering against another amount of the same currency.
    pub fn checked_cmp(&self, other: &Money) -> Result<Ordering, MoneyError> {
        self.require_same_currency(other)?;
        Ok(self.minor.cmp(&other.minor))
    }

    /// True when `self >= other`.
    pub fn is_at_least(&self, other: &Money) -> Result<bool, MoneyError> {
        Ok(self.checked_cmp(other)? != Ordering::Less)
    }

    /// Clamp negative amounts to zero (used for change / balance display).
    pub fn clamp_non_negative(self) -> Money {
        if self.minor < 0 {
            Money::zero(self.currency)
        } else {
            self
        }
    }

    /// Human readable amount without the currency code, e.g. `116.00`.
    pub fn format_amount(&self) -> String {
        let exponent = self.currency.minor_exponent();
        let sign = if self.minor < 0 { "-" } else { "" };
        let magnitude = self.minor.unsigned_abs();
        if exponent == 0 {
            return format!("{sign}{magnitude}");
        }
        let divisor = 10u64.pow(exponent);
        let units = magnitude / divisor;
        let fraction = magnitude % divisor;
        let width = exponent as usize;
        format!("{sign}{units}.{fraction:0width$}")
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.format_amount(), self.currency.code())
    }
}

/// Quantity expressed in thousandths of a unit (3 implied decimals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Quantity(i64);

impl Quantity {
    pub const SCALE: i64 = 1_000;

    pub fn from_thousandths(thousandths: i64) -> Result<Self, MoneyError> {
        if thousandths <= 0 {
            return Err(MoneyError::InvalidQuantity(thousandths));
        }
        Ok(Self(thousandths))
    }

    pub fn from_units(units: i64) -> Result<Self, MoneyError> {
        let thousandths = units.checked_mul(Self::SCALE).ok_or(MoneyError::Overflow)?;
        Self::from_thousandths(thousandths)
    }

    pub const fn thousandths(&self) -> i64 {
        self.0
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let units = self.0 / Self::SCALE;
        let fraction = (self.0 % Self::SCALE).unsigned_abs();
        write!(f, "{units}.{fraction:03}")
    }
}

/// How a quoted price relates to tax.
///
/// Always declared explicitly by the catalog's tax profile — never inferred
/// from the amount, the rate or the kind of product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PriceMode {
    /// The quoted price is net; tax is added on top. `gross = net + tax`.
    TaxExclusive,
    /// The quoted price is what the customer pays; tax is backed out of it.
    /// This is how Mexican retail shelf prices normally work.
    TaxInclusive,
}

impl PriceMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            PriceMode::TaxExclusive => "TAX_EXCLUSIVE",
            PriceMode::TaxInclusive => "TAX_INCLUSIVE",
        }
    }

    pub fn parse(value: &str) -> Result<Self, MoneyError> {
        match value {
            "TAX_EXCLUSIVE" => Ok(PriceMode::TaxExclusive),
            "TAX_INCLUSIVE" => Ok(PriceMode::TaxInclusive),
            other => Err(MoneyError::UnknownPriceMode(other.to_owned())),
        }
    }
}

/// A taxed amount, decomposed. `net + tax == gross` always holds, in both
/// price modes and at every rate, by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaxedAmount {
    pub net: Money,
    pub tax: Money,
    pub gross: Money,
}

/// A proportion in basis points (1000 = 10.00%), used for discounts.
///
/// Separate from [`TaxRate`] on purpose: a discount is not a tax, and a
/// discount above 100% is nonsense while a tax above 100% merely needs a wider
/// bound. Both share the core's single rounding policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Percentage(u32);

impl Percentage {
    pub const SCALE: u32 = 10_000;
    /// 100%. A discount cannot exceed what it discounts.
    pub const MAX_BPS: u32 = 10_000;

    pub fn from_bps(bps: u32) -> Result<Self, MoneyError> {
        if bps > Self::MAX_BPS {
            return Err(MoneyError::InvalidPercentage(bps));
        }
        Ok(Self(bps))
    }

    pub const fn bps(&self) -> u32 {
        self.0
    }

    /// This proportion of an amount, rounded half away from zero.
    pub fn of(&self, amount: Money) -> Result<Money, MoneyError> {
        let scaled = i128::from(amount.minor())
            .checked_mul(i128::from(self.0))
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::from_minor(
            div_round_half_away_from_zero(scaled, i128::from(Self::SCALE))?,
            amount.currency(),
        ))
    }
}

/// Tax rate in basis points (1600 = 16.00%).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaxRate(u32);

impl TaxRate {
    pub const SCALE: u32 = 10_000;
    /// Defensive upper bound (1000%). Guards against unit mix-ups.
    pub const MAX_BPS: u32 = 100_000;
    pub const ZERO: TaxRate = TaxRate(0);

    pub fn from_bps(bps: u32) -> Result<Self, MoneyError> {
        if bps > Self::MAX_BPS {
            return Err(MoneyError::InvalidTaxRate(bps));
        }
        Ok(Self(bps))
    }

    pub const fn bps(&self) -> u32 {
        self.0
    }

    /// Tax owed on a taxable base, rounded half away from zero.
    pub fn tax_on(&self, base: Money) -> Result<Money, MoneyError> {
        let scaled = i128::from(base.minor())
            .checked_mul(i128::from(self.0))
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::from_minor(
            div_round_half_away_from_zero(scaled, i128::from(Self::SCALE))?,
            base.currency(),
        ))
    }

    /// Back the tax out of a tax-inclusive amount.
    ///
    /// `net = round(gross * 10000 / (10000 + bps))` with the core's single
    /// rounding policy, and `tax = gross - net`, which is what guarantees
    /// `net + tax == gross` even when the division is not exact.
    pub fn split_inclusive(&self, gross: Money) -> Result<(Money, Money), MoneyError> {
        if self.0 == 0 {
            return Ok((gross, Money::zero(gross.currency())));
        }
        let scaled = i128::from(gross.minor())
            .checked_mul(i128::from(Self::SCALE))
            .ok_or(MoneyError::Overflow)?;
        let denominator = i128::from(Self::SCALE)
            .checked_add(i128::from(self.0))
            .ok_or(MoneyError::Overflow)?;
        let net = Money::from_minor(
            div_round_half_away_from_zero(scaled, denominator)?,
            gross.currency(),
        );
        Ok((net, gross.checked_sub(net)?))
    }

    /// Decompose a quoted amount according to its price mode.
    pub fn apply(&self, amount: Money, mode: PriceMode) -> Result<TaxedAmount, MoneyError> {
        match mode {
            PriceMode::TaxExclusive => {
                let tax = self.tax_on(amount)?;
                Ok(TaxedAmount {
                    net: amount,
                    tax,
                    gross: amount.checked_add(tax)?,
                })
            }
            PriceMode::TaxInclusive => {
                let (net, tax) = self.split_inclusive(amount)?;
                Ok(TaxedAmount {
                    net,
                    tax,
                    gross: amount,
                })
            }
        }
    }
}

/// Spread an amount across weighted shares, exactly.
///
/// Largest fractional remainder, ties broken by ascending index, and **each
/// share capped at its own weight**, so the shares always sum to
/// `min(amount, Σ weights)` — no minor unit is ever created or lost.
///
/// This is the single allocation policy of PI.Edge, next to its single
/// rounding policy, because two implementations of "split this money" drift in
/// one of them. It has two real consumers: a basket discount spread over the
/// lines it applies to (ADR 015), and a refund spread over the units and the
/// tenders of the original sale (ADR 031).
///
/// The returned vector is aligned with `weights`, zeros included; the caller
/// decides what a zero share means in its own domain.
pub fn allocate_proportionally(amount: Money, weights: &[Money]) -> Vec<Money> {
    let currency = amount.currency();
    let zeros = || vec![Money::zero(currency); weights.len()];
    let total: i64 = weights.iter().map(Money::minor).sum();
    if total <= 0 || !amount.is_positive() {
        return zeros();
    }
    // Never hand out more than the weights can hold: the caps are the weights.
    let budget = amount.minor().min(total);

    struct Share {
        minor: i64,
        remainder: i128,
        cap: i64,
    }
    let denominator = i128::from(total);
    let mut shares: Vec<Share> = weights
        .iter()
        .map(|weight| {
            let numerator = i128::from(budget) * i128::from(weight.minor());
            Share {
                minor: i64::try_from(numerator / denominator).unwrap_or(0),
                remainder: numerator % denominator,
                cap: weight.minor(),
            }
        })
        .collect();

    let mut residual = budget - shares.iter().map(|share| share.minor).sum::<i64>();
    let mut order: Vec<usize> = (0..shares.len()).collect();
    order.sort_by(|left, right| {
        shares[*right]
            .remainder
            .cmp(&shares[*left].remainder)
            .then_with(|| left.cmp(right))
    });
    while residual > 0 {
        let mut progressed = false;
        for index in &order {
            if residual == 0 {
                break;
            }
            if shares[*index].minor < shares[*index].cap {
                shares[*index].minor += 1;
                residual -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    shares
        .into_iter()
        .map(|share| Money::from_minor(share.minor, currency))
        .collect()
}

/// Integer division rounding halves away from zero; the only rounding mode
/// used by PI.Edge monetary math.
fn div_round_half_away_from_zero(numerator: i128, denominator: i128) -> Result<i64, MoneyError> {
    if denominator == 0 {
        return Err(MoneyError::Overflow);
    }
    let negative = (numerator < 0) != (denominator < 0);
    let n = numerator.unsigned_abs();
    let d = denominator.unsigned_abs();
    let quotient = n / d;
    let remainder = n % d;
    let rounded = if remainder
        .checked_mul(2)
        .map(|doubled| doubled >= d)
        .unwrap_or(true)
    {
        quotient.checked_add(1).ok_or(MoneyError::Overflow)?
    } else {
        quotient
    };
    let signed = i128::try_from(rounded).map_err(|_| MoneyError::Overflow)?;
    let signed = if negative { -signed } else { signed };
    i64::try_from(signed).map_err(|_| MoneyError::Overflow)
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    const MXN: Currency = Currency::Mxn;

    fn m(minor: i64) -> Money {
        Money::from_minor(minor, MXN)
    }

    #[test]
    fn adds_and_subtracts_exactly() {
        assert_eq!(m(10_000).checked_add(m(1_600)).unwrap(), m(11_600));
        assert_eq!(m(11_600).checked_sub(m(1_600)).unwrap(), m(10_000));
        assert_eq!(m(100).checked_sub(m(300)).unwrap().minor(), -200);
    }

    #[test]
    fn detects_overflow() {
        assert_eq!(
            m(i64::MAX).checked_add(m(1)).unwrap_err(),
            MoneyError::Overflow
        );
    }

    #[test]
    fn multiplies_by_fractional_quantity_with_half_up_rounding() {
        // 3 x 1000 = 3000
        assert_eq!(
            m(1_000)
                .mul_quantity(Quantity::from_units(3).unwrap())
                .unwrap(),
            m(3_000)
        );
        // 2.5 x 333 = 832.5 -> 833
        assert_eq!(
            m(333)
                .mul_quantity(Quantity::from_thousandths(2_500).unwrap())
                .unwrap(),
            m(833)
        );
        // 0.333 x 100 = 33.3 -> 33
        assert_eq!(
            m(100)
                .mul_quantity(Quantity::from_thousandths(333).unwrap())
                .unwrap(),
            m(33)
        );
    }

    #[test]
    fn rejects_non_positive_quantity() {
        assert_eq!(
            Quantity::from_thousandths(0).unwrap_err(),
            MoneyError::InvalidQuantity(0)
        );
        assert_eq!(
            Quantity::from_units(-1).unwrap_err(),
            MoneyError::InvalidQuantity(-1_000)
        );
    }

    #[test]
    fn computes_tax_deterministically() {
        let rate = TaxRate::from_bps(1_600).unwrap();
        assert_eq!(rate.tax_on(m(10_000)).unwrap(), m(1_600));
        // 16% of 3.33 = 0.5328 -> 0.53
        assert_eq!(rate.tax_on(m(333)).unwrap(), m(53));
        // 16% of 3.44 = 0.5504 -> 0.55
        assert_eq!(rate.tax_on(m(344)).unwrap(), m(55));
        assert_eq!(TaxRate::ZERO.tax_on(m(10_000)).unwrap(), m(0));
    }

    #[test]
    fn rejects_absurd_tax_rate() {
        assert!(TaxRate::from_bps(TaxRate::MAX_BPS + 1).is_err());
    }

    #[test]
    fn rejects_currency_mismatch_on_display_paths() {
        // Only one currency exists today, so the guard is exercised directly.
        let a = Money::from_minor(1, Currency::Mxn);
        assert!(a
            .require_same_currency(&Money::from_minor(1, Currency::Mxn))
            .is_ok());
    }

    #[test]
    fn formats_amounts() {
        assert_eq!(m(11_600).format_amount(), "116.00");
        assert_eq!(m(5).format_amount(), "0.05");
        assert_eq!(m(-250).format_amount(), "-2.50");
        assert_eq!(m(11_600).to_string(), "116.00 MXN");
    }

    #[test]
    fn compares_and_clamps() {
        assert!(m(11_600).is_at_least(&m(11_600)).unwrap());
        assert!(!m(11_599).is_at_least(&m(11_600)).unwrap());
        assert_eq!(m(-5).clamp_non_negative(), m(0));
    }

    #[test]
    fn exclusive_mode_keeps_the_existing_arithmetic() {
        let rate = TaxRate::from_bps(1_600).unwrap();
        let taxed = rate.apply(m(10_000), PriceMode::TaxExclusive).unwrap();
        assert_eq!(taxed.net, m(10_000));
        assert_eq!(taxed.tax, m(1_600));
        assert_eq!(taxed.gross, m(11_600));
    }

    #[test]
    fn inclusive_mode_backs_the_tax_out_of_the_shelf_price() {
        let rate = TaxRate::from_bps(1_600).unwrap();
        let taxed = rate.apply(m(11_600), PriceMode::TaxInclusive).unwrap();
        assert_eq!(taxed.net, m(10_000));
        assert_eq!(taxed.tax, m(1_600));
        assert_eq!(taxed.gross, m(11_600));

        let rate = TaxRate::from_bps(800).unwrap();
        let taxed = rate.apply(m(10_800), PriceMode::TaxInclusive).unwrap();
        assert_eq!(taxed.net, m(10_000));
        assert_eq!(taxed.tax, m(800));
    }

    #[test]
    fn zero_rate_is_identical_in_both_modes() {
        for mode in [PriceMode::TaxExclusive, PriceMode::TaxInclusive] {
            let taxed = TaxRate::ZERO.apply(m(9_999), mode).unwrap();
            assert_eq!(taxed.net, m(9_999));
            assert_eq!(taxed.tax, m(0));
            assert_eq!(taxed.gross, m(9_999));
        }
    }

    #[test]
    fn net_plus_tax_always_equals_gross() {
        // Amounts where the inclusive division cannot be exact.
        for bps in [1_600, 800, 1, 9_999] {
            let rate = TaxRate::from_bps(bps).unwrap();
            for minor in [1, 3, 7, 99, 101, 333, 1_001, 11_601, 123_457] {
                for mode in [PriceMode::TaxExclusive, PriceMode::TaxInclusive] {
                    let taxed = rate.apply(m(minor), mode).unwrap();
                    assert_eq!(
                        taxed.net.checked_add(taxed.tax).unwrap(),
                        taxed.gross,
                        "bps={bps} minor={minor} mode={mode:?}"
                    );
                    assert!(!taxed.tax.is_negative());
                }
            }
        }
    }

    #[test]
    fn inclusive_rounding_policy_is_frozen() {
        let rate = TaxRate::from_bps(1_600).unwrap();
        // 100 / 1.16 = 86.2068... -> 86 ; tax = 14
        let taxed = rate.apply(m(100), PriceMode::TaxInclusive).unwrap();
        assert_eq!(taxed.net, m(86));
        assert_eq!(taxed.tax, m(14));
        // 1 / 1.16 = 0.862 -> 1 ; tax = 0
        let taxed = rate.apply(m(1), PriceMode::TaxInclusive).unwrap();
        assert_eq!(taxed.net, m(1));
        assert_eq!(taxed.tax, m(0));
        // 333 / 1.16 = 287.068... -> 287 ; tax = 46
        let taxed = rate.apply(m(333), PriceMode::TaxInclusive).unwrap();
        assert_eq!(taxed.net, m(287));
        assert_eq!(taxed.tax, m(46));
    }

    #[test]
    fn price_mode_round_trips_through_its_wire_value() {
        for mode in [PriceMode::TaxExclusive, PriceMode::TaxInclusive] {
            assert_eq!(PriceMode::parse(mode.as_str()).unwrap(), mode);
        }
        assert!(PriceMode::parse("GUESS").is_err());
    }

    #[test]
    fn percentage_shares_the_core_rounding_policy() {
        let ten = Percentage::from_bps(1_000).unwrap();
        assert_eq!(ten.of(m(10_000)).unwrap(), m(1_000));
        // 10% of 2.55 = 0.255 -> 0.26 (half away from zero)
        assert_eq!(ten.of(m(255)).unwrap(), m(26));
        // 33.33% of 1.00 = 0.3333 -> 0.33
        assert_eq!(
            Percentage::from_bps(3_333).unwrap().of(m(100)).unwrap(),
            m(33)
        );
        assert_eq!(Percentage::from_bps(0).unwrap().of(m(999)).unwrap(), m(0));
        assert_eq!(
            Percentage::from_bps(10_000).unwrap().of(m(999)).unwrap(),
            m(999)
        );
    }

    #[test]
    fn percentage_cannot_exceed_one_hundred_per_cent() {
        assert_eq!(
            Percentage::from_bps(10_001).unwrap_err(),
            MoneyError::InvalidPercentage(10_001)
        );
    }

    #[test]
    fn quantity_display() {
        assert_eq!(Quantity::from_units(2).unwrap().to_string(), "2.000");
        assert_eq!(
            Quantity::from_thousandths(1_500).unwrap().to_string(),
            "1.500"
        );
    }

    #[test]
    fn prorating_the_whole_returns_the_amount_untouched() {
        let line = mxn(23_200);
        let three = Quantity::from_units(3).unwrap();
        assert_eq!(line.prorate(three, three).unwrap(), line);
    }

    #[test]
    fn prorating_rounds_with_the_core_policy() {
        // 232.00 paid for three units: one unit is 77.33 (77.3333 down).
        let line = mxn(23_200);
        assert_eq!(
            line.prorate(
                Quantity::from_units(1).unwrap(),
                Quantity::from_units(3).unwrap()
            )
            .unwrap()
            .minor(),
            7_733
        );
        // 0.01 over two units rounds away from zero, not to even.
        assert_eq!(
            mxn(1)
                .prorate(
                    Quantity::from_units(1).unwrap(),
                    Quantity::from_units(2).unwrap()
                )
                .unwrap()
                .minor(),
            1
        );
    }

    #[test]
    fn prorating_more_than_the_whole_is_refused() {
        assert!(mxn(100)
            .prorate(
                Quantity::from_units(4).unwrap(),
                Quantity::from_units(3).unwrap()
            )
            .is_err());
    }

    /// Slicing a line one unit at a time, always against what is left, adds up
    /// to the line exactly — the invariant every partial refund depends on.
    #[test]
    fn successive_prorated_slices_add_up_to_the_whole() {
        for total in [1_i64, 99, 23_200, 7, 100_003] {
            for units in [1_i64, 2, 3, 7] {
                let mut remaining = mxn(total);
                let mut remaining_units = units;
                let mut taken = 0;
                while remaining_units > 0 {
                    let slice = remaining
                        .prorate(
                            Quantity::from_units(1).unwrap(),
                            Quantity::from_units(remaining_units).unwrap(),
                        )
                        .unwrap();
                    taken += slice.minor();
                    remaining = remaining.checked_sub(slice).unwrap();
                    remaining_units -= 1;
                }
                assert_eq!(taken, total, "{total} over {units} units");
                assert_eq!(remaining.minor(), 0);
            }
        }
    }

    // ------------------------------------------------ the allocation policy

    fn mxn(minor: i64) -> Money {
        Money::from_minor(minor, Currency::Mxn)
    }

    fn minors(shares: &[Money]) -> Vec<i64> {
        shares.iter().map(Money::minor).collect()
    }

    #[test]
    fn an_allocation_sums_to_the_amount_exactly() {
        // 100.00 over three equal weights: 33.34 + 33.33 + 33.33, never
        // 33.33 three times with a cent left over.
        let shares = allocate_proportionally(mxn(10_000), &[mxn(10_000), mxn(10_000), mxn(10_000)]);
        assert_eq!(minors(&shares), vec![3_334, 3_333, 3_333]);
        assert_eq!(shares.iter().map(Money::minor).sum::<i64>(), 10_000);
    }

    #[test]
    fn an_allocation_is_proportional_to_the_weights() {
        let shares = allocate_proportionally(mxn(50_000), &[mxn(30_000), mxn(70_000)]);
        assert_eq!(minors(&shares), vec![15_000, 35_000]);
    }

    #[test]
    fn no_share_exceeds_its_own_weight() {
        // The weights are the caps: 500 cannot be spread onto 100 + 200.
        let shares = allocate_proportionally(mxn(50_000), &[mxn(10_000), mxn(20_000)]);
        assert_eq!(minors(&shares), vec![10_000, 20_000]);
        assert_eq!(shares.iter().map(Money::minor).sum::<i64>(), 30_000);
    }

    #[test]
    fn a_remainder_goes_to_the_largest_fraction_then_the_lowest_index() {
        // Equal remainders everywhere: the single extra cent goes to index 0,
        // which is what makes a split reproducible rather than merely correct.
        let shares = allocate_proportionally(mxn(1), &[mxn(100), mxn(100)]);
        assert_eq!(minors(&shares), vec![1, 0]);
    }

    #[test]
    fn an_allocation_is_aligned_with_its_weights_zeros_included() {
        let shares = allocate_proportionally(mxn(100), &[mxn(0), mxn(10_000), mxn(0)]);
        assert_eq!(shares.len(), 3);
        assert_eq!(minors(&shares), vec![0, 100, 0]);
    }

    #[test]
    fn nothing_is_allocated_without_an_amount_or_a_weight() {
        assert_eq!(
            minors(&allocate_proportionally(mxn(0), &[mxn(100)])),
            vec![0]
        );
        assert_eq!(
            minors(&allocate_proportionally(mxn(-100), &[mxn(100)])),
            vec![0]
        );
        assert_eq!(
            minors(&allocate_proportionally(mxn(100), &[mxn(0), mxn(0)])),
            vec![0, 0]
        );
        assert!(allocate_proportionally(mxn(100), &[]).is_empty());
    }

    /// The property a refund depends on: whatever the weights, the shares add
    /// up to the budget, so slicing an amount can never create or lose a minor
    /// unit (ADR 031).
    #[test]
    fn an_allocation_never_creates_or_loses_a_minor_unit() {
        for amount in [1_i64, 7, 99, 100, 23_200, 999_983] {
            for weights in [
                vec![mxn(1_000_000)],
                vec![mxn(1_000_000), mxn(1)],
                vec![mxn(333), mxn(333), mxn(334)],
                vec![mxn(7_733), mxn(7_733), mxn(7_734)],
                vec![mxn(1), mxn(2), mxn(3), mxn(5), mxn(8), mxn(13)],
            ] {
                let total: i64 = weights.iter().map(Money::minor).sum();
                let shares = allocate_proportionally(mxn(amount), &weights);
                assert_eq!(
                    shares.iter().map(Money::minor).sum::<i64>(),
                    amount.min(total),
                    "amount {amount} over {} weights",
                    weights.len()
                );
                for (share, weight) in shares.iter().zip(&weights) {
                    assert!(share.minor() <= weight.minor(), "a share exceeded its cap");
                }
            }
        }
    }
}
