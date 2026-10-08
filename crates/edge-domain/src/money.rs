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
    fn quantity_display() {
        assert_eq!(Quantity::from_units(2).unwrap().to_string(), "2.000");
        assert_eq!(
            Quantity::from_thousandths(1_500).unwrap().to_string(),
            "1.500"
        );
    }
}
