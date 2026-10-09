//! What a refund is worth, derived from the sale that already closed.
//!
//! The rule that makes this safe: **the refund consumes the materialised
//! economics of the original line and never re-prices anything** (ADR 031). No
//! current price, no current promotion, no current tax profile, no current
//! snapshot — the sale already decided what the customer paid, and a refund
//! can only give some of that back.

use edge_domain::prelude::{allocate_proportionally, Currency, Money, Quantity, TransactionLine};
use serde::{Deserialize, Serialize};

use crate::error::RefundError;

/// The money of a refund, with its attribution kept apart.
///
/// `net + tax == gross` always, because `gross` is computed from the other two
/// rather than prorated on its own: two independent roundings of the same
/// amount are how a cent goes missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundAmounts {
    /// Quoted amount before discount, in the original line's price mode.
    pub extended: Money,
    /// The share of the line's discount that this quantity carries.
    pub discount: Money,
    /// Tax base after discount.
    pub net: Money,
    pub tax: Money,
    /// What is actually given back: `net + tax`.
    pub gross: Money,
}

impl RefundAmounts {
    pub fn zero(currency: Currency) -> Self {
        let zero = Money::zero(currency);
        Self {
            extended: zero,
            discount: zero,
            net: zero,
            tax: zero,
            gross: zero,
        }
    }

    /// The whole of one original line: everything that line could ever give
    /// back.
    pub fn of_line(line: &TransactionLine) -> Self {
        Self {
            extended: line.extended_amount,
            discount: line.discount_amount,
            net: line.taxable_amount,
            tax: line.tax_amount,
            gross: line.line_total,
        }
    }

    pub fn checked_sub(&self, other: &RefundAmounts) -> Result<Self, RefundError> {
        Ok(Self {
            extended: self.extended.checked_sub(other.extended)?,
            discount: self.discount.checked_sub(other.discount)?,
            net: self.net.checked_sub(other.net)?,
            tax: self.tax.checked_sub(other.tax)?,
            gross: self.gross.checked_sub(other.gross)?,
        })
    }

    pub fn checked_add(&self, other: &RefundAmounts) -> Result<Self, RefundError> {
        Ok(Self {
            extended: self.extended.checked_add(other.extended)?,
            discount: self.discount.checked_add(other.discount)?,
            net: self.net.checked_add(other.net)?,
            tax: self.tax.checked_add(other.tax)?,
            gross: self.gross.checked_add(other.gross)?,
        })
    }

    /// The part of *what is left* that corresponds to `slice` of
    /// `remaining_quantity`.
    ///
    /// Always against the remainder, never against the original: that is what
    /// makes a sequence of partial refunds add up to the line exactly. The
    /// refund that takes the last units takes precisely what is left, so no
    /// minor unit is created or lost however the line is sliced (ADR 031).
    pub fn slice(
        &self,
        slice: Quantity,
        remaining_quantity: Quantity,
    ) -> Result<Self, RefundError> {
        let net = self.net.prorate(slice, remaining_quantity)?;
        let tax = self.tax.prorate(slice, remaining_quantity)?;
        Ok(Self {
            extended: self.extended.prorate(slice, remaining_quantity)?,
            discount: self.discount.prorate(slice, remaining_quantity)?,
            net,
            tax,
            gross: net.checked_add(tax)?,
        })
    }
}

/// What one original line can still give back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRefundable {
    pub line_number: i64,
    pub product_ref: String,
    pub description: String,
    pub sold_quantity: Quantity,
    /// The original line's quoted unit price, carried for display and audit.
    /// Never an input to the refundable arithmetic (ADR 031).
    pub unit_price: Money,
    /// Already refunded by earlier refunds, in thousandths.
    pub refunded_thousandths: i64,
    /// `sold − refunded`, in thousandths. Zero means the line is exhausted.
    pub remaining_thousandths: i64,
    /// The economics still available, after earlier refunds took their share.
    pub remaining: RefundAmounts,
    /// What the whole line was worth, for display and audit.
    pub original: RefundAmounts,
}

impl LineRefundable {
    /// Build from the original line and what earlier refunds already took.
    pub fn new(
        line: &TransactionLine,
        refunded_thousandths: i64,
        refunded: RefundAmounts,
    ) -> Result<Self, RefundError> {
        let original = RefundAmounts::of_line(line);
        let remaining_thousandths = line
            .quantity
            .thousandths()
            .checked_sub(refunded_thousandths)
            .ok_or(RefundError::InvalidRecord {
                field: "refunded_quantity",
                reason: "exceeds the quantity sold".to_owned(),
            })?;
        if remaining_thousandths < 0 {
            return Err(RefundError::InvalidRecord {
                field: "refunded_quantity",
                reason: "exceeds the quantity sold".to_owned(),
            });
        }
        Ok(Self {
            line_number: line.line_number,
            product_ref: line.product_ref.to_string(),
            description: line.description.clone(),
            sold_quantity: line.quantity,
            unit_price: line.unit_price,
            refunded_thousandths,
            remaining_thousandths,
            remaining: original.checked_sub(&refunded)?,
            original,
        })
    }

    pub fn is_exhausted(&self) -> bool {
        self.remaining_thousandths == 0
    }

    /// What refunding `quantity` of this line is worth.
    pub fn quote(&self, quantity: Quantity) -> Result<RefundAmounts, RefundError> {
        let remaining = Quantity::from_thousandths(self.remaining_thousandths).map_err(|_| {
            RefundError::QuantityExceeded {
                line_number: self.line_number,
                remaining: "0.000".to_owned(),
                requested: quantity.to_string(),
            }
        })?;
        if quantity.thousandths() > remaining.thousandths() {
            return Err(RefundError::QuantityExceeded {
                line_number: self.line_number,
                remaining: remaining.to_string(),
                requested: quantity.to_string(),
            });
        }
        self.remaining.slice(quantity, remaining)
    }
}

/// How much of a refund each original tender has to give back.
///
/// Pro-rata over what each tender actually paid, capped at what it has left to
/// give: the one policy of V1, deterministic and with no operator input
/// (ADR 031). A tender never returns more than it took.
pub fn allocate_over_tenders(
    amount: Money,
    tenders: &[TenderRefundable],
) -> Result<Vec<(usize, Money)>, RefundError> {
    let caps: Vec<Money> = tenders.iter().map(|tender| tender.remaining).collect();
    let shares = allocate_proportionally(amount, &caps);
    let allocated: i64 = shares.iter().map(Money::minor).sum();
    if allocated < amount.minor() {
        return Err(RefundError::TenderExceeded {
            tender: "ALL",
            remaining: Money::from_minor(allocated, amount.currency()).format_amount(),
            requested: amount.format_amount(),
        });
    }
    Ok(shares
        .into_iter()
        .enumerate()
        .filter(|(_, share)| share.is_positive())
        .collect())
}

/// One original tender, and what it can still refund.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenderRefundable {
    pub payment_id: String,
    pub tender_type: String,
    /// What this tender applied to the sale.
    pub committed: Money,
    /// `committed` minus what earlier refunds already gave back through it.
    pub remaining: Money,
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::{PriceMode, ProductRef, TaxRate};

    fn mxn(minor: i64) -> Money {
        Money::from_minor(minor, Currency::Mxn)
    }

    fn qty(units: i64) -> Quantity {
        Quantity::from_units(units).unwrap()
    }

    /// A line as the sale materialised it. These are the only numbers a refund
    /// is allowed to look at.
    fn line(
        line_number: i64,
        quantity: Quantity,
        unit_price: i64,
        extended: i64,
        discount: i64,
        net: i64,
        tax: i64,
    ) -> TransactionLine {
        TransactionLine {
            line_id: edge_domain::prelude::LineId::from_uuid(uuid::Uuid::from_u128(u128::from(
                line_number as u64,
            ))),
            line_number,
            product_ref: ProductRef::new("SKU-1").unwrap(),
            sku: None,
            barcode: None,
            description: "Producto".to_owned(),
            quantity,
            unit_price: mxn(unit_price),
            tax_rate: TaxRate::from_bps(1_600).unwrap(),
            price_mode: PriceMode::TaxExclusive,
            price_book_id: None,
            extended_amount: mxn(extended),
            discount_amount: mxn(discount),
            taxable_amount: mxn(net),
            tax_amount: mxn(tax),
            line_total: mxn(net + tax),
        }
    }

    /// Buy 2 get 1 on three units of 100.00 at 16%: the customer paid 232.00
    /// for three units.
    fn bogo_line() -> TransactionLine {
        line(1, qty(3), 10_000, 30_000, 10_000, 20_000, 3_200)
    }

    fn refundable(line: &TransactionLine) -> LineRefundable {
        LineRefundable::new(line, 0, RefundAmounts::zero(Currency::Mxn)).unwrap()
    }

    // ----------------------------------------------------- the whole line

    #[test]
    fn a_full_refund_gives_back_exactly_the_line() {
        let original = bogo_line();
        let quote = refundable(&original).quote(qty(3)).unwrap();

        assert_eq!(quote.net.minor(), 20_000);
        assert_eq!(quote.tax.minor(), 3_200);
        assert_eq!(quote.gross.minor(), 23_200);
        assert_eq!(quote.discount.minor(), 10_000);
        assert_eq!(quote.extended.minor(), 30_000);
        // Never the shelf price of three units: the customer paid 232.00.
        assert_eq!(quote.gross, original.line_total);
    }

    // -------------------------------------------- promotions, the hard part

    #[test]
    fn a_promotional_unit_refunds_what_was_paid_per_unit_not_the_shelf_price() {
        // 232.00 over three units. Net and tax each take their own share and
        // each rounds once — 66.67 + 10.67 — so the unit is 77.34, and
        // `net + tax == gross` holds instead of being approximately true.
        let quote = refundable(&bogo_line()).quote(qty(1)).unwrap();

        assert_eq!(quote.net.minor(), 6_667);
        assert_eq!(quote.tax.minor(), 1_067);
        assert_eq!(quote.gross.minor(), 7_734);
        // Not 116.00 (shelf price plus tax), which would be arbitrage: the
        // customer would get back more than the unit cost them.
        assert!(quote.gross.minor() < 11_600);
        // And not zero either, which is what "the third one was free" would
        // produce for whichever unit the refund happened to name.
        assert!(quote.gross.is_positive());
        // The discount travels with the unit: a third of the promotion.
        assert_eq!(quote.discount.minor(), 3_333);
    }

    #[test]
    fn successive_partial_refunds_of_one_line_sum_to_the_line_exactly() {
        let original = bogo_line();
        let mut refunded_thousandths = 0;
        let mut refunded = RefundAmounts::zero(Currency::Mxn);

        for _ in 0..3 {
            let view = LineRefundable::new(&original, refunded_thousandths, refunded).unwrap();
            let quote = view.quote(qty(1)).unwrap();
            refunded_thousandths += 1_000;
            refunded = refunded.checked_add(&quote).unwrap();
        }

        // 77.33 + 77.33 + 77.34 = 232.00. Not a cent created, not a cent lost.
        assert_eq!(refunded.gross, original.line_total);
        assert_eq!(refunded.net.minor(), 20_000);
        assert_eq!(refunded.tax.minor(), 3_200);
        assert_eq!(refunded.discount.minor(), 10_000);

        let exhausted = LineRefundable::new(&original, refunded_thousandths, refunded).unwrap();
        assert!(exhausted.is_exhausted());
        assert_eq!(exhausted.remaining.gross.minor(), 0);
    }

    #[test]
    fn the_last_refund_absorbs_the_remainder_whatever_the_order() {
        // Odd amounts in every component, so every split has a remainder.
        let original = line(1, qty(7), 1_429, 10_003, 1_001, 9_002, 1_441);
        for first in 1..7 {
            let whole = refundable(&original);
            let head = whole.quote(qty(first)).unwrap();
            let tail_view = LineRefundable::new(&original, first * 1_000, head).unwrap();
            let tail = tail_view.quote(qty(7 - first)).unwrap();
            let total = head.checked_add(&tail).unwrap();
            assert_eq!(
                total.gross,
                original.line_total,
                "splitting 7 units as {first} + {}",
                7 - first
            );
            assert_eq!(total.net.minor(), 9_002);
            assert_eq!(total.tax.minor(), 1_441);
        }
    }

    #[test]
    fn net_and_tax_always_add_up_to_what_is_given_back() {
        let original = line(1, qty(3), 3_333, 9_999, 1_111, 8_888, 1_422);
        for units in 1..=3 {
            let quote = refundable(&original).quote(qty(units)).unwrap();
            assert_eq!(
                quote.net.checked_add(quote.tax).unwrap(),
                quote.gross,
                "{units} units"
            );
        }
    }

    #[test]
    fn a_fractional_quantity_refunds_its_share() {
        // 0.750 kg of a 1.000 kg line worth 100.00 + tax.
        let original = line(
            1,
            Quantity::from_thousandths(1_000).unwrap(),
            10_000,
            10_000,
            0,
            10_000,
            1_600,
        );
        let quote = refundable(&original)
            .quote(Quantity::from_thousandths(750).unwrap())
            .unwrap();
        assert_eq!(quote.net.minor(), 7_500);
        assert_eq!(quote.tax.minor(), 1_200);
        assert_eq!(quote.gross.minor(), 8_700);
    }

    // ---------------------------------------------------- refusing too much

    #[test]
    fn refunding_more_than_remains_is_refused() {
        let original = bogo_line();
        let error = refundable(&original).quote(qty(4)).unwrap_err();
        assert_eq!(error.code(), "REFUND_QUANTITY_EXCEEDED");

        // Two already gone: one left, two refused.
        let two = refundable(&original).quote(qty(2)).unwrap();
        let view = LineRefundable::new(&original, 2_000, two).unwrap();
        assert_eq!(view.remaining_thousandths, 1_000);
        assert!(view.quote(qty(1)).is_ok());
        assert_eq!(
            view.quote(qty(2)).unwrap_err().code(),
            "REFUND_QUANTITY_EXCEEDED"
        );
    }

    #[test]
    fn an_exhausted_line_refunds_nothing_more() {
        let original = bogo_line();
        let all = refundable(&original).quote(qty(3)).unwrap();
        let view = LineRefundable::new(&original, 3_000, all).unwrap();
        assert!(view.is_exhausted());
        assert_eq!(
            view.quote(qty(1)).unwrap_err().code(),
            "REFUND_QUANTITY_EXCEEDED"
        );
    }

    // ------------------------------------------------------- tender spread

    fn tender(id: &str, kind: &str, committed: i64, remaining: i64) -> TenderRefundable {
        TenderRefundable {
            payment_id: id.to_owned(),
            tender_type: kind.to_owned(),
            committed: mxn(committed),
            remaining: mxn(remaining),
        }
    }

    #[test]
    fn a_refund_spreads_over_the_original_tenders_pro_rata() {
        // 300 cash + 700 card, refunding 500.
        let tenders = [
            tender("p1", "CASH", 30_000, 30_000),
            tender("p2", "CARD", 70_000, 70_000),
        ];
        let shares = allocate_over_tenders(mxn(50_000), &tenders).unwrap();
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0], (0, mxn(15_000)));
        assert_eq!(shares[1], (1, mxn(35_000)));
    }

    #[test]
    fn a_tender_never_gives_back_more_than_it_took() {
        // Cash already gave back 250 of its 300: only 50 left there.
        let tenders = [
            tender("p1", "CASH", 30_000, 5_000),
            tender("p2", "CARD", 70_000, 70_000),
        ];
        let shares = allocate_over_tenders(mxn(50_000), &tenders).unwrap();
        let total: i64 = shares.iter().map(|(_, share)| share.minor()).sum();
        assert_eq!(total, 50_000);
        let cash = shares.iter().find(|(index, _)| *index == 0).unwrap();
        assert!(cash.1.minor() <= 5_000);
    }

    #[test]
    fn a_refund_the_tenders_cannot_cover_is_refused() {
        let tenders = [tender("p1", "CASH", 30_000, 1_000)];
        let error = allocate_over_tenders(mxn(50_000), &tenders).unwrap_err();
        assert_eq!(error.code(), "REFUND_TENDER_EXCEEDED");
    }

    #[test]
    fn a_tender_spread_sums_to_the_refund_exactly() {
        for amount in [1_i64, 7, 99, 50_000, 99_999] {
            let tenders = [
                tender("p1", "CASH", 33_333, 33_333),
                tender("p2", "CARD", 33_333, 33_333),
                tender("p3", "CARD", 33_334, 33_334),
            ];
            let shares = allocate_over_tenders(mxn(amount), &tenders).unwrap();
            let total: i64 = shares.iter().map(|(_, share)| share.minor()).sum();
            assert_eq!(total, amount, "refunding {amount}");
        }
    }
}
