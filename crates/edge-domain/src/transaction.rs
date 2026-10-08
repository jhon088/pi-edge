//! RetailTransaction aggregate: the heart of the POS.
//!
//! Lifecycle:
//! ```text
//! OPEN -> TOTALIZED -> PAYMENT_IN_PROGRESS -> COMMITTED -> RECEIPT_COMPLETED -> FINALIZED
//!   \                                                                      /
//!    `-> CANCELLED                                      COMMITTED -> FINALIZED
//! ```
//! Adding an item to a `TOTALIZED` transaction invalidates the official totals
//! and returns the aggregate to `OPEN`: the UI must never display totals that
//! the core has not computed.

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{
    BusinessDayId, CatalogSnapshotId, LineId, OperatorId, PaymentId, ProductRef, RegisterId,
    SessionId, StoreId, TransactionId,
};
use crate::money::{Currency, Money, MoneyError, PriceMode, Quantity, TaxRate};
use crate::payment::Payment;
use crate::time::Timestamp;

const AGGREGATE: &str = "RetailTransaction";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransactionStatus {
    Open,
    Totalized,
    PaymentInProgress,
    Committed,
    ReceiptCompleted,
    Finalized,
    Cancelled,
}

impl TransactionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            TransactionStatus::Open => "OPEN",
            TransactionStatus::Totalized => "TOTALIZED",
            TransactionStatus::PaymentInProgress => "PAYMENT_IN_PROGRESS",
            TransactionStatus::Committed => "COMMITTED",
            TransactionStatus::ReceiptCompleted => "RECEIPT_COMPLETED",
            TransactionStatus::Finalized => "FINALIZED",
            TransactionStatus::Cancelled => "CANCELLED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "OPEN" => Ok(TransactionStatus::Open),
            "TOTALIZED" => Ok(TransactionStatus::Totalized),
            "PAYMENT_IN_PROGRESS" => Ok(TransactionStatus::PaymentInProgress),
            "COMMITTED" => Ok(TransactionStatus::Committed),
            "RECEIPT_COMPLETED" => Ok(TransactionStatus::ReceiptCompleted),
            "FINALIZED" => Ok(TransactionStatus::Finalized),
            "CANCELLED" => Ok(TransactionStatus::Cancelled),
            other => Err(DomainError::UnknownEnumValue {
                kind: "TransactionStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// Terminal states: the transaction can no longer be mutated.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            TransactionStatus::Finalized | TransactionStatus::Cancelled
        )
    }

    /// A transaction that still occupies the register.
    pub const fn is_active(self) -> bool {
        !self.is_terminal()
    }
}

/// A priced sale line.
///
/// `product_ref` is the canonical catalog reference received from the server;
/// PI.Edge never creates products. Pricing inputs (`unit_price`, `tax_rate`,
/// `discount_amount`) arrive from the caller in this milestone and will later
/// come from the Catalog Snapshot + Pricing/Tax/Promotions engines without the
/// aggregate changing shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionLine {
    pub line_id: LineId,
    pub line_number: i64,
    pub product_ref: ProductRef,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub description: String,
    pub quantity: Quantity,
    /// Price as quoted by the catalog, in the line's [`PriceMode`].
    pub unit_price: Money,
    pub tax_rate: TaxRate,
    /// Whether `unit_price` already includes tax.
    pub price_mode: PriceMode,
    /// `unit_price * quantity`, before discount, still in the quoted mode.
    pub extended_amount: Money,
    pub discount_amount: Money,
    /// Net tax base after discount. Equals `extended - discount` when the
    /// price is tax-exclusive, and the backed-out net when it is inclusive.
    pub taxable_amount: Money,
    pub tax_amount: Money,
    /// `taxable_amount + tax_amount`; what this line adds to the sale.
    pub line_total: Money,
}

/// Input accepted by [`RetailTransaction::add_line`].
#[derive(Debug, Clone)]
pub struct NewLineInput {
    pub line_id: LineId,
    pub product_ref: ProductRef,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub description: String,
    pub quantity: Quantity,
    pub unit_price: Money,
    pub tax_rate: TaxRate,
    pub price_mode: PriceMode,
    pub discount_amount: Option<Money>,
}

impl TransactionLine {
    fn price(input: NewLineInput, line_number: i64) -> Result<Self, DomainError> {
        if input.description.trim().is_empty() {
            return Err(DomainError::MissingField("description"));
        }
        if input.unit_price.is_negative() {
            return Err(DomainError::Money(MoneyError::NegativeAmount(
                input.unit_price.minor(),
            )));
        }
        let currency = input.unit_price.currency();
        let discount = input.discount_amount.unwrap_or(Money::zero(currency));
        if discount.is_negative() {
            return Err(DomainError::Money(MoneyError::NegativeAmount(
                discount.minor(),
            )));
        }
        // Extend first, then split the tax: the customer pays the quoted price
        // times the quantity, exactly.
        let extended = input.unit_price.mul_quantity(input.quantity)?;
        let quoted = extended.checked_sub(discount)?;
        if quoted.is_negative() {
            return Err(DomainError::DiscountExceedsLine {
                discount_minor: discount.minor(),
                gross_minor: extended.minor(),
            });
        }
        let taxed = input.tax_rate.apply(quoted, input.price_mode)?;
        let taxable = taxed.net;
        let tax_amount = taxed.tax;
        let line_total = taxed.gross;
        Ok(Self {
            line_id: input.line_id,
            line_number,
            product_ref: input.product_ref,
            sku: input.sku,
            barcode: input.barcode,
            description: input.description.trim().to_owned(),
            quantity: input.quantity,
            unit_price: input.unit_price,
            tax_rate: input.tax_rate,
            price_mode: input.price_mode,
            extended_amount: extended,
            discount_amount: discount,
            taxable_amount: taxable,
            tax_amount,
            line_total,
        })
    }
}

/// Official transaction totals. Only the core computes these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// Sum of the quoted extended amounts, before discount. "Gross sales".
    pub subtotal: Money,
    pub discount_total: Money,
    /// Sum of the net tax bases. This is the subtotal a receipt prints next to
    /// the tax line, and it is mode-independent.
    pub net_total: Money,
    pub tax_total: Money,
    /// `net_total + tax_total`, and equal to the sum of the line totals.
    pub grand_total: Money,
}

impl Totals {
    pub fn zero(currency: Currency) -> Self {
        Self {
            subtotal: Money::zero(currency),
            discount_total: Money::zero(currency),
            net_total: Money::zero(currency),
            tax_total: Money::zero(currency),
            grand_total: Money::zero(currency),
        }
    }

    fn compute(lines: &[TransactionLine], currency: Currency) -> Result<Self, DomainError> {
        let mut subtotal = Money::zero(currency);
        let mut discount_total = Money::zero(currency);
        let mut net_total = Money::zero(currency);
        let mut tax_total = Money::zero(currency);
        let mut line_total_sum = Money::zero(currency);
        for line in lines {
            subtotal = subtotal.checked_add(line.extended_amount)?;
            discount_total = discount_total.checked_add(line.discount_amount)?;
            net_total = net_total.checked_add(line.taxable_amount)?;
            tax_total = tax_total.checked_add(line.tax_amount)?;
            line_total_sum = line_total_sum.checked_add(line.line_total)?;
        }
        let grand_total = net_total.checked_add(tax_total)?;
        if grand_total != line_total_sum {
            return Err(DomainError::InconsistentTotals);
        }
        Ok(Self {
            subtotal,
            discount_total,
            net_total,
            tax_total,
            grand_total,
        })
    }
}

/// The catalog snapshot a sale was priced against.
///
/// Captured when the sale starts and never changed afterwards, so activating a
/// new snapshot mid-sale cannot move a price (see ADR 013).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRef {
    pub snapshot_id: CatalogSnapshotId,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetailTransactionState {
    pub transaction_id: TransactionId,
    pub business_day_id: BusinessDayId,
    pub session_id: SessionId,
    pub store_id: StoreId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    /// Per-register monotonic sequence; unique together with `register_id`.
    pub register_sequence: i64,
    pub status: TransactionStatus,
    pub currency: Currency,
    /// `None` only for sales created before the catalog existed.
    pub catalog: Option<CatalogRef>,
    pub lines: Vec<TransactionLine>,
    pub totals: Totals,
    pub payments: Vec<Payment>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub finalized_at: Option<Timestamp>,
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct NewRetailTransaction {
    pub transaction_id: TransactionId,
    pub business_day_id: BusinessDayId,
    pub session_id: SessionId,
    pub store_id: StoreId,
    pub register_id: RegisterId,
    pub operator_id: OperatorId,
    pub register_sequence: i64,
    pub currency: Currency,
    pub catalog: Option<CatalogRef>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RetailTransaction(RetailTransactionState);

impl RetailTransaction {
    pub fn start(new: NewRetailTransaction) -> Self {
        Self(RetailTransactionState {
            transaction_id: new.transaction_id,
            business_day_id: new.business_day_id,
            session_id: new.session_id,
            store_id: new.store_id,
            register_id: new.register_id,
            operator_id: new.operator_id,
            register_sequence: new.register_sequence,
            status: TransactionStatus::Open,
            currency: new.currency,
            catalog: new.catalog,
            lines: Vec::new(),
            totals: Totals::zero(new.currency),
            payments: Vec::new(),
            created_at: new.created_at,
            updated_at: new.created_at,
            finalized_at: None,
            version: 1,
        })
    }

    /// Rebuild from persisted state (storage adapters only).
    pub fn rehydrate(state: RetailTransactionState) -> Self {
        Self(state)
    }

    pub fn state(&self) -> &RetailTransactionState {
        &self.0
    }

    pub fn into_state(self) -> RetailTransactionState {
        self.0
    }

    pub fn id(&self) -> TransactionId {
        self.0.transaction_id
    }

    pub fn status(&self) -> TransactionStatus {
        self.0.status
    }

    pub fn is_active(&self) -> bool {
        self.0.status.is_active()
    }

    /// The snapshot this sale must keep resolving against.
    pub fn catalog(&self) -> Option<CatalogRef> {
        self.0.catalog
    }

    fn touch(&mut self, now: Timestamp) {
        self.0.updated_at = now;
        self.0.version += 1;
    }

    fn require_currency(&self, amount: Money) -> Result<(), DomainError> {
        if amount.currency() == self.0.currency {
            Ok(())
        } else {
            Err(DomainError::Money(MoneyError::CurrencyMismatch {
                expected: self.0.currency.code(),
                actual: amount.currency().code(),
            }))
        }
    }

    /// Append a priced line. Invalidates existing totals.
    pub fn add_line(
        &mut self,
        input: NewLineInput,
        now: Timestamp,
    ) -> Result<TransactionLine, DomainError> {
        match self.0.status {
            TransactionStatus::Open | TransactionStatus::Totalized => {}
            other => {
                return Err(DomainError::InvalidTransition {
                    aggregate: AGGREGATE,
                    from: other.as_str(),
                    attempted: "ITEM_ADDED",
                })
            }
        }
        self.require_currency(input.unit_price)?;
        if let Some(discount) = input.discount_amount {
            self.require_currency(discount)?;
        }
        let line_number = self.0.lines.len() as i64 + 1;
        let line = TransactionLine::price(input, line_number)?;
        self.0.lines.push(line.clone());
        // Totals are no longer official until the core recomputes them.
        self.0.status = TransactionStatus::Open;
        self.0.totals = Totals::zero(self.0.currency);
        self.touch(now);
        Ok(line)
    }

    /// Compute the official totals.
    pub fn totalize(&mut self, now: Timestamp) -> Result<Totals, DomainError> {
        match self.0.status {
            TransactionStatus::Open | TransactionStatus::Totalized => {}
            other => {
                return Err(DomainError::InvalidTransition {
                    aggregate: AGGREGATE,
                    from: other.as_str(),
                    attempted: "TOTALIZED",
                })
            }
        }
        if self.0.lines.is_empty() {
            return Err(DomainError::EmptyTransaction);
        }
        let totals = Totals::compute(&self.0.lines, self.0.currency)?;
        self.0.totals = totals.clone();
        self.0.status = TransactionStatus::Totalized;
        self.touch(now);
        Ok(totals)
    }

    /// Sum of committed payments.
    pub fn paid_total(&self) -> Result<Money, DomainError> {
        let mut total = Money::zero(self.0.currency);
        for payment in self.0.payments.iter().filter(|p| p.is_committed()) {
            total = total.checked_add(payment.amount)?;
        }
        Ok(total)
    }

    /// Remaining amount owed (never negative).
    pub fn balance_due(&self) -> Result<Money, DomainError> {
        Ok(self
            .0
            .totals
            .grand_total
            .checked_sub(self.paid_total()?)?
            .clamp_non_negative())
    }

    /// Cash change owed to the customer (never negative).
    pub fn change_due(&self) -> Result<Money, DomainError> {
        Ok(self
            .paid_total()?
            .checked_sub(self.0.totals.grand_total)?
            .clamp_non_negative())
    }

    /// Commit a cash tender. Moves to `COMMITTED` once fully paid.
    pub fn commit_cash_payment(
        &mut self,
        payment_id: PaymentId,
        amount: Money,
        now: Timestamp,
    ) -> Result<Payment, DomainError> {
        match self.0.status {
            TransactionStatus::Totalized | TransactionStatus::PaymentInProgress => {}
            other => {
                return Err(DomainError::InvalidTransition {
                    aggregate: AGGREGATE,
                    from: other.as_str(),
                    attempted: "CASH_PAYMENT_COMMITTED",
                })
            }
        }
        self.require_currency(amount)?;
        let payment = Payment::committed_cash(payment_id, self.0.transaction_id, amount, now)?;
        self.0.payments.push(payment.clone());
        let paid = self.paid_total()?;
        self.0.status = if paid.is_at_least(&self.0.totals.grand_total)? {
            TransactionStatus::Committed
        } else {
            TransactionStatus::PaymentInProgress
        };
        self.touch(now);
        Ok(payment)
    }

    /// Receipt printing/delivery finished. Optional in this milestone.
    pub fn mark_receipt_completed(&mut self, now: Timestamp) -> Result<(), DomainError> {
        match self.0.status {
            TransactionStatus::Committed => {
                self.0.status = TransactionStatus::ReceiptCompleted;
                self.touch(now);
                Ok(())
            }
            other => Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: other.as_str(),
                attempted: "RECEIPT_COMPLETED",
            }),
        }
    }

    pub fn finalize(&mut self, now: Timestamp) -> Result<(), DomainError> {
        match self.0.status {
            TransactionStatus::Committed | TransactionStatus::ReceiptCompleted => {}
            other => {
                return Err(DomainError::InvalidTransition {
                    aggregate: AGGREGATE,
                    from: other.as_str(),
                    attempted: "FINALIZED",
                })
            }
        }
        let paid = self.paid_total()?;
        if !paid.is_at_least(&self.0.totals.grand_total)? {
            return Err(DomainError::InsufficientPayment {
                required_minor: self.0.totals.grand_total.minor(),
                paid_minor: paid.minor(),
            });
        }
        self.0.status = TransactionStatus::Finalized;
        self.0.finalized_at = Some(now);
        self.touch(now);
        Ok(())
    }

    pub fn cancel(&mut self, now: Timestamp) -> Result<(), DomainError> {
        match self.0.status {
            TransactionStatus::Open | TransactionStatus::Totalized => {
                self.0.status = TransactionStatus::Cancelled;
                self.touch(now);
                Ok(())
            }
            other => Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: other.as_str(),
                attempted: "CANCELLED",
            }),
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const MXN: Currency = Currency::Mxn;

    fn ts(seconds: u32) -> Timestamp {
        Timestamp::parse_rfc3339(&format!("2026-10-08T10:00:{seconds:02}Z")).unwrap()
    }

    fn money(minor: i64) -> Money {
        Money::from_minor(minor, MXN)
    }

    fn sale() -> RetailTransaction {
        RetailTransaction::start(NewRetailTransaction {
            transaction_id: TransactionId::from_uuid(Uuid::from_u128(10)),
            business_day_id: BusinessDayId::from_uuid(Uuid::from_u128(1)),
            session_id: SessionId::from_uuid(Uuid::from_u128(2)),
            store_id: StoreId::new("STORE-1").unwrap(),
            register_id: RegisterId::new("REG-001").unwrap(),
            operator_id: OperatorId::new("CASHIER-001").unwrap(),
            register_sequence: 1,
            currency: MXN,
            catalog: Some(CatalogRef {
                snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(900)),
                version: 1,
            }),
            created_at: ts(0),
        })
    }

    fn line(price_minor: i64, qty_units: i64, tax_bps: u32, seq: u128) -> NewLineInput {
        NewLineInput {
            line_id: LineId::from_uuid(Uuid::from_u128(100 + seq)),
            product_ref: ProductRef::new("SKU-001").unwrap(),
            sku: Some("SKU-001".to_owned()),
            barcode: None,
            description: "Producto prueba".to_owned(),
            quantity: Quantity::from_units(qty_units).unwrap(),
            unit_price: money(price_minor),
            tax_rate: TaxRate::from_bps(tax_bps).unwrap(),
            price_mode: PriceMode::TaxExclusive,
            discount_amount: None,
        }
    }

    #[test]
    fn starts_open_with_zero_totals() {
        let tx = sale();
        assert_eq!(tx.status(), TransactionStatus::Open);
        assert!(tx.state().lines.is_empty());
        assert_eq!(tx.state().totals.grand_total, money(0));
        assert_eq!(tx.state().version, 1);
    }

    #[test]
    fn prices_line_and_totalizes() {
        let mut tx = sale();
        let l = tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        assert_eq!(l.extended_amount, money(10_000));
        assert_eq!(l.tax_amount, money(1_600));
        assert_eq!(l.line_total, money(11_600));
        let totals = tx.totalize(ts(2)).unwrap();
        assert_eq!(totals.subtotal, money(10_000));
        assert_eq!(totals.tax_total, money(1_600));
        assert_eq!(totals.grand_total, money(11_600));
        assert_eq!(tx.status(), TransactionStatus::Totalized);
    }

    #[test]
    fn sums_multiple_lines_and_discounts() {
        let mut tx = sale();
        tx.add_line(line(10_000, 2, 1_600, 1), ts(1)).unwrap();
        let mut discounted = line(5_000, 1, 1_600, 2);
        discounted.discount_amount = Some(money(500));
        tx.add_line(discounted, ts(2)).unwrap();
        let totals = tx.totalize(ts(3)).unwrap();
        assert_eq!(totals.subtotal, money(25_000));
        assert_eq!(totals.discount_total, money(500));
        // 20000 * 16% = 3200 ; (5000-500) * 16% = 720
        assert_eq!(totals.tax_total, money(3_920));
        assert_eq!(totals.grand_total, money(28_420));
    }

    #[test]
    fn prices_an_inclusive_line_by_backing_the_tax_out() {
        let mut tx = sale();
        let mut inclusive = line(11_600, 1, 1_600, 1);
        inclusive.price_mode = PriceMode::TaxInclusive;
        let l = tx.add_line(inclusive, ts(1)).unwrap();
        assert_eq!(l.extended_amount, money(11_600));
        assert_eq!(l.taxable_amount, money(10_000));
        assert_eq!(l.tax_amount, money(1_600));
        // The customer pays the shelf price, not shelf price plus tax.
        assert_eq!(l.line_total, money(11_600));

        let totals = tx.totalize(ts(2)).unwrap();
        assert_eq!(totals.subtotal, money(11_600));
        assert_eq!(totals.net_total, money(10_000));
        assert_eq!(totals.tax_total, money(1_600));
        assert_eq!(totals.grand_total, money(11_600));
    }

    #[test]
    fn mixes_inclusive_and_exclusive_lines_in_one_sale() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        let mut inclusive = line(10_800, 2, 800, 2);
        inclusive.price_mode = PriceMode::TaxInclusive;
        tx.add_line(inclusive, ts(2)).unwrap();

        let totals = tx.totalize(ts(3)).unwrap();
        // exclusive: net 10000, tax 1600 ; inclusive: gross 21600 -> net 20000, tax 1600
        assert_eq!(totals.net_total, money(30_000));
        assert_eq!(totals.tax_total, money(3_200));
        assert_eq!(totals.grand_total, money(33_200));
        assert_eq!(
            totals.grand_total,
            tx.state()
                .lines
                .iter()
                .fold(money(0), |acc, l| acc.checked_add(l.line_total).unwrap())
        );
    }

    #[test]
    fn extended_amount_multiplies_before_splitting_the_tax() {
        // 3 x 0.99 inclusive at 16%: the customer pays 2.97, never 2.98.
        let mut tx = sale();
        let mut inclusive = line(99, 3, 1_600, 1);
        inclusive.price_mode = PriceMode::TaxInclusive;
        let l = tx.add_line(inclusive, ts(1)).unwrap();
        assert_eq!(l.extended_amount, money(297));
        assert_eq!(l.line_total, money(297));
        assert_eq!(
            l.taxable_amount.checked_add(l.tax_amount).unwrap(),
            money(297)
        );
    }

    #[test]
    fn captures_the_catalog_snapshot_at_start() {
        let tx = sale();
        let catalog = tx.catalog().expect("a sale must know its snapshot");
        assert_eq!(catalog.version, 1);
        assert_eq!(tx.state().catalog, Some(catalog));
    }

    #[test]
    fn rejects_discount_bigger_than_line() {
        let mut tx = sale();
        let mut bad = line(1_000, 1, 1_600, 1);
        bad.discount_amount = Some(money(1_001));
        assert!(matches!(
            tx.add_line(bad, ts(1)).unwrap_err(),
            DomainError::DiscountExceedsLine { .. }
        ));
    }

    #[test]
    fn rejects_totalize_without_lines() {
        let mut tx = sale();
        assert_eq!(
            tx.totalize(ts(1)).unwrap_err(),
            DomainError::EmptyTransaction
        );
    }

    #[test]
    fn adding_item_after_totalize_invalidates_totals() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.totalize(ts(2)).unwrap();
        tx.add_line(line(1_000, 1, 1_600, 2), ts(3)).unwrap();
        assert_eq!(tx.status(), TransactionStatus::Open);
        assert_eq!(tx.state().totals.grand_total, money(0));
    }

    #[test]
    fn rejects_payment_before_totalize() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        let err = tx
            .commit_cash_payment(
                PaymentId::from_uuid(Uuid::from_u128(50)),
                money(11_600),
                ts(2),
            )
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidTransition { .. }));
    }

    #[test]
    fn partial_payment_keeps_transaction_in_payment_in_progress() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.totalize(ts(2)).unwrap();
        tx.commit_cash_payment(
            PaymentId::from_uuid(Uuid::from_u128(50)),
            money(5_000),
            ts(3),
        )
        .unwrap();
        assert_eq!(tx.status(), TransactionStatus::PaymentInProgress);
        assert_eq!(tx.balance_due().unwrap(), money(6_600));
        assert_eq!(tx.change_due().unwrap(), money(0));
        assert!(matches!(
            tx.finalize(ts(4)).unwrap_err(),
            DomainError::InvalidTransition { .. }
        ));
        tx.commit_cash_payment(
            PaymentId::from_uuid(Uuid::from_u128(51)),
            money(6_600),
            ts(5),
        )
        .unwrap();
        assert_eq!(tx.status(), TransactionStatus::Committed);
        assert_eq!(tx.balance_due().unwrap(), money(0));
    }

    #[test]
    fn overpayment_yields_change_and_finalizes() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.totalize(ts(2)).unwrap();
        tx.commit_cash_payment(
            PaymentId::from_uuid(Uuid::from_u128(50)),
            money(20_000),
            ts(3),
        )
        .unwrap();
        assert_eq!(tx.change_due().unwrap(), money(8_400));
        tx.finalize(ts(4)).unwrap();
        assert_eq!(tx.status(), TransactionStatus::Finalized);
        assert_eq!(tx.state().finalized_at, Some(ts(4)));
    }

    #[test]
    fn rejects_non_positive_payment() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.totalize(ts(2)).unwrap();
        assert_eq!(
            tx.commit_cash_payment(PaymentId::from_uuid(Uuid::from_u128(50)), money(0), ts(3))
                .unwrap_err(),
            DomainError::NonPositivePayment
        );
    }

    #[test]
    fn rejects_mutation_after_finalize() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.totalize(ts(2)).unwrap();
        tx.commit_cash_payment(
            PaymentId::from_uuid(Uuid::from_u128(50)),
            money(11_600),
            ts(3),
        )
        .unwrap();
        tx.finalize(ts(4)).unwrap();
        assert!(tx.add_line(line(1, 1, 0, 9), ts(5)).is_err());
        assert!(tx.totalize(ts(5)).is_err());
        assert!(tx.finalize(ts(5)).is_err());
        assert!(tx.cancel(ts(5)).is_err());
        assert!(!tx.is_active());
    }

    #[test]
    fn supports_receipt_completed_before_finalize() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.totalize(ts(2)).unwrap();
        tx.commit_cash_payment(
            PaymentId::from_uuid(Uuid::from_u128(50)),
            money(11_600),
            ts(3),
        )
        .unwrap();
        tx.mark_receipt_completed(ts(4)).unwrap();
        assert_eq!(tx.status(), TransactionStatus::ReceiptCompleted);
        tx.finalize(ts(5)).unwrap();
        assert_eq!(tx.status(), TransactionStatus::Finalized);
    }

    #[test]
    fn cancels_only_before_payment() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.cancel(ts(2)).unwrap();
        assert_eq!(tx.status(), TransactionStatus::Cancelled);
    }

    #[test]
    fn version_increments_on_every_mutation() {
        let mut tx = sale();
        assert_eq!(tx.state().version, 1);
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        assert_eq!(tx.state().version, 2);
        tx.totalize(ts(2)).unwrap();
        assert_eq!(tx.state().version, 3);
    }
}
