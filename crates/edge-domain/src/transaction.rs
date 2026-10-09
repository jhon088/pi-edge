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
    BusinessDayId, CatalogSnapshotId, LineId, OperatorId, PaymentId, PriceBookId, ProductRef,
    RegisterId, SessionId, StoreId, TransactionId,
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
    /// Price book the unit price came from. `None` only for lines priced
    /// before price books existed.
    pub price_book_id: Option<PriceBookId>,
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
    pub price_book_id: Option<PriceBookId>,
    pub discount_amount: Option<Money>,
}

impl TransactionLine {
    /// Recompute every derived amount from the line's own inputs.
    ///
    /// The single place line arithmetic happens: extend by quantity, take the
    /// discount off the quoted amount, then split by price mode. Promotions
    /// feed only the `discount` term, which is what keeps `net + tax == gross`
    /// true for both modes without any new arithmetic (ADR 015).
    fn recompute(&mut self, discount: Money) -> Result<(), DomainError> {
        if discount.is_negative() {
            return Err(DomainError::Money(MoneyError::NegativeAmount(
                discount.minor(),
            )));
        }
        let extended = self.unit_price.mul_quantity(self.quantity)?;
        let quoted = extended.checked_sub(discount)?;
        if quoted.is_negative() {
            return Err(DomainError::DiscountExceedsLine {
                discount_minor: discount.minor(),
                gross_minor: extended.minor(),
            });
        }
        let taxed = self.tax_rate.apply(quoted, self.price_mode)?;
        self.extended_amount = extended;
        self.discount_amount = discount;
        self.taxable_amount = taxed.net;
        self.tax_amount = taxed.tax;
        self.line_total = taxed.gross;
        Ok(())
    }

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
        let currency = input.unit_price.currency();
        let mut line = Self {
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
            price_book_id: input.price_book_id,
            extended_amount: Money::zero(currency),
            discount_amount: Money::zero(currency),
            taxable_amount: Money::zero(currency),
            tax_amount: Money::zero(currency),
            line_total: Money::zero(currency),
        };
        line.recompute(discount)?;
        Ok(line)
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

/// Total discount a promotion evaluation assigns to one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineDiscount {
    pub line_number: i64,
    pub amount: Money,
}

/// What a promotion applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PromotionTarget {
    Line { line_number: i64 },
    Basket,
}

impl PromotionTarget {
    /// Discriminant, stored on its own column so an audit query can filter on
    /// it without parsing a label.
    pub const fn kind(&self) -> &'static str {
        match self {
            PromotionTarget::Line { .. } => "LINE",
            PromotionTarget::Basket => "BASKET",
        }
    }

    pub const fn line_number(&self) -> Option<i64> {
        match self {
            PromotionTarget::Line { line_number } => Some(*line_number),
            PromotionTarget::Basket => None,
        }
    }

    /// Stable human label: `BASKET`, or `LINE:<line_number>`.
    pub fn label(&self) -> String {
        match self {
            PromotionTarget::Line { line_number } => format!("LINE:{line_number}"),
            PromotionTarget::Basket => "BASKET".to_owned(),
        }
    }

    /// Rebuild from the two stored columns.
    pub fn rehydrate(kind: &str, line_number: Option<i64>) -> Result<Self, DomainError> {
        match (kind, line_number) {
            ("LINE", Some(line_number)) => Ok(PromotionTarget::Line { line_number }),
            ("BASKET", None) => Ok(PromotionTarget::Basket),
            _ => Err(DomainError::InvalidPromotionTarget {
                kind: kind.to_owned(),
            }),
        }
    }
}

/// One promotion as applied to this sale, materialized for audit.
///
/// Self-contained on purpose: a historical sale stays interpretable after old
/// catalog snapshots are purged, so the amount and the reason live here rather
/// than only in the snapshot (ADR 015).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedPromotion {
    /// Deterministic composite of promotion and target, not a minted id.
    pub adjustment_id: String,
    pub promotion_id: String,
    pub code: String,
    pub name: String,
    pub priority: i32,
    pub stackability: String,
    pub target: PromotionTarget,
    pub amount: Money,
    /// Rule kind that produced it, e.g. `BUY_N_GET_M`.
    pub reason: String,
    /// Human detail for the journal and the receipt.
    pub detail: Option<String>,
}

/// The result of one promotion evaluation over the whole basket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionOutcome {
    /// Final discount per line, basket-level allocations included.
    pub line_discounts: Vec<LineDiscount>,
    pub applied: Vec<AppliedPromotion>,
}

impl PromotionOutcome {
    pub fn empty() -> Self {
        Self {
            line_discounts: Vec::new(),
            applied: Vec::new(),
        }
    }
}

/// The pricing context a sale was opened with.
///
/// Captured when the sale starts and never changed afterwards. The snapshot
/// pins *which* catalog prices the sale, and `effective_at` pins *when*, so
/// neither activating a new snapshot nor a scheduled price change can move a
/// price mid-sale (see ADR 013, ADR 014).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingCapture {
    pub snapshot_id: CatalogSnapshotId,
    pub catalog_version: i64,
    /// Instant every line of this sale is priced at.
    pub effective_at: Timestamp,
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
    pub pricing: Option<PricingCapture>,
    pub lines: Vec<TransactionLine>,
    /// Never reused: removing a line leaves a gap so an adjustment id always
    /// points at the same line.
    pub next_line_number: i64,
    pub totals: Totals,
    /// Promotions applied by the last evaluation.
    pub applied_promotions: Vec<AppliedPromotion>,
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
    pub pricing: Option<PricingCapture>,
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
            pricing: new.pricing,
            lines: Vec::new(),
            next_line_number: 1,
            totals: Totals::zero(new.currency),
            applied_promotions: Vec::new(),
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

    /// The snapshot and instant this sale must keep resolving against.
    pub fn pricing(&self) -> Option<PricingCapture> {
        self.0.pricing
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
        self.require_mutable("ITEM_ADDED")?;
        self.require_currency(input.unit_price)?;
        if let Some(discount) = input.discount_amount {
            self.require_currency(discount)?;
        }
        let line_number = self.0.next_line_number;
        let line = TransactionLine::price(input, line_number)?;
        self.0.lines.push(line.clone());
        self.0.next_line_number += 1;
        self.invalidate_totals();
        self.touch(now);
        Ok(line)
    }

    /// Drop a line from the basket.
    ///
    /// The line number is not reused, so promotion adjustments keep pointing at
    /// the same line for the life of the sale.
    pub fn remove_line(
        &mut self,
        line_number: i64,
        now: Timestamp,
    ) -> Result<TransactionLine, DomainError> {
        self.require_mutable("ITEM_REMOVED")?;
        let position = self
            .0
            .lines
            .iter()
            .position(|line| line.line_number == line_number)
            .ok_or(DomainError::LineNotFound { line_number })?;
        let removed = self.0.lines.remove(position);
        self.invalidate_totals();
        self.touch(now);
        Ok(removed)
    }

    /// Change how much of a line is being sold, re-extending its amounts.
    pub fn change_line_quantity(
        &mut self,
        line_number: i64,
        quantity: Quantity,
        now: Timestamp,
    ) -> Result<TransactionLine, DomainError> {
        self.require_mutable("QUANTITY_CHANGED")?;
        let line = self
            .0
            .lines
            .iter_mut()
            .find(|line| line.line_number == line_number)
            .ok_or(DomainError::LineNotFound { line_number })?;
        line.quantity = quantity;
        // Promotions are re-evaluated right after, which sets the real
        // discount; recompute now so the line is never left inconsistent.
        line.recompute(Money::zero(line.unit_price.currency()))?;
        let updated = line.clone();
        self.invalidate_totals();
        self.touch(now);
        Ok(updated)
    }

    /// Replace the promotion result for the whole basket.
    ///
    /// Promotions are recomputed from the current basket on every change, so
    /// this overwrites rather than accumulates: `add → remove → add` lands
    /// exactly where a single `add` would (ADR 015).
    pub fn apply_promotions(
        &mut self,
        outcome: PromotionOutcome,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.require_mutable("PROMOTIONS_APPLIED")?;
        for discount in &outcome.line_discounts {
            if !self
                .0
                .lines
                .iter()
                .any(|line| line.line_number == discount.line_number)
            {
                return Err(DomainError::AdjustmentTargetMissing {
                    line_number: discount.line_number,
                });
            }
        }
        let currency = self.0.currency;
        let discount_for = |line: &TransactionLine| {
            outcome
                .line_discounts
                .iter()
                .find(|candidate| candidate.line_number == line.line_number)
                .map(|candidate| candidate.amount)
                .unwrap_or(Money::zero(currency))
        };
        // Re-evaluating a basket no promotion touches must not look like a
        // change: it would bump the version and invalidate totals for nothing.
        let unchanged = outcome.applied == self.0.applied_promotions
            && self
                .0
                .lines
                .iter()
                .all(|line| discount_for(line) == line.discount_amount);
        if unchanged {
            return Ok(());
        }
        for index in 0..self.0.lines.len() {
            let discount = discount_for(&self.0.lines[index]);
            self.0.lines[index].recompute(discount)?;
        }
        self.0.applied_promotions = outcome.applied;
        self.invalidate_totals();
        self.touch(now);
        Ok(())
    }

    /// Promotions applied by the last evaluation.
    pub fn applied_promotions(&self) -> &[AppliedPromotion] {
        &self.0.applied_promotions
    }

    /// Totals stop being official whenever the basket changes.
    fn invalidate_totals(&mut self) {
        self.0.status = TransactionStatus::Open;
        self.0.totals = Totals::zero(self.0.currency);
    }

    fn require_mutable(&self, attempted: &'static str) -> Result<(), DomainError> {
        match self.0.status {
            TransactionStatus::Open | TransactionStatus::Totalized => Ok(()),
            other => Err(DomainError::InvalidTransition {
                aggregate: AGGREGATE,
                from: other.as_str(),
                attempted,
            }),
        }
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
            pricing: Some(PricingCapture {
                snapshot_id: CatalogSnapshotId::from_uuid(Uuid::from_u128(900)),
                catalog_version: 1,
                effective_at: ts(0),
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
            price_book_id: None,
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
    fn captures_the_pricing_context_at_start() {
        let tx = sale();
        let pricing = tx.pricing().expect("a sale must know how it is priced");
        assert_eq!(pricing.catalog_version, 1);
        assert_eq!(pricing.effective_at, ts(0));
        assert_eq!(tx.state().pricing, Some(pricing));
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

    fn discount(line_number: i64, minor: i64) -> PromotionOutcome {
        PromotionOutcome {
            line_discounts: vec![LineDiscount {
                line_number,
                amount: money(minor),
            }],
            applied: vec![AppliedPromotion {
                adjustment_id: format!("PROMO#line:{line_number}"),
                promotion_id: "PROMO".to_owned(),
                code: "PROMO".to_owned(),
                name: "Test promotion".to_owned(),
                priority: 0,
                stackability: "STACKABLE".to_owned(),
                target: PromotionTarget::Line { line_number },
                amount: money(minor),
                reason: "PERCENTAGE_OFF".to_owned(),
                detail: None,
            }],
        }
    }

    #[test]
    fn removing_a_line_never_reuses_its_number() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.add_line(line(5_000, 1, 1_600, 2), ts(2)).unwrap();
        assert_eq!(tx.state().lines[1].line_number, 2);

        let removed = tx.remove_line(1, ts(3)).unwrap();
        assert_eq!(removed.line_number, 1);
        assert_eq!(tx.state().lines.len(), 1);
        // The survivor keeps its number: nothing is renumbered.
        assert_eq!(tx.state().lines[0].line_number, 2);

        let added = tx.add_line(line(1_000, 1, 1_600, 3), ts(4)).unwrap();
        assert_eq!(added.line_number, 3);
        assert!(matches!(
            tx.remove_line(99, ts(5)).unwrap_err(),
            DomainError::LineNotFound { line_number: 99 }
        ));
    }

    #[test]
    fn changing_a_quantity_re_extends_the_line() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        let updated = tx
            .change_line_quantity(1, Quantity::from_units(3).unwrap(), ts(2))
            .unwrap();
        assert_eq!(updated.extended_amount, money(30_000));
        assert_eq!(updated.tax_amount, money(4_800));
        assert_eq!(updated.line_total, money(34_800));

        let totals = tx.totalize(ts(3)).unwrap();
        assert_eq!(totals.grand_total, money(34_800));
        assert!(tx
            .change_line_quantity(42, Quantity::from_units(1).unwrap(), ts(4))
            .is_err());
    }

    #[test]
    fn applying_promotions_discounts_the_line_and_keeps_net_plus_tax_equal_to_gross() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.apply_promotions(discount(1, 1_000), ts(2)).unwrap();

        let line = &tx.state().lines[0];
        assert_eq!(line.discount_amount, money(1_000));
        assert_eq!(line.extended_amount, money(10_000));
        assert_eq!(line.taxable_amount, money(9_000));
        assert_eq!(line.tax_amount, money(1_440));
        assert_eq!(line.line_total, money(10_440));
        assert_eq!(
            line.taxable_amount.checked_add(line.tax_amount).unwrap(),
            line.line_total
        );
        assert_eq!(tx.applied_promotions().len(), 1);

        let totals = tx.totalize(ts(3)).unwrap();
        assert_eq!(totals.subtotal, money(10_000));
        assert_eq!(totals.discount_total, money(1_000));
        assert_eq!(totals.net_total, money(9_000));
        assert_eq!(totals.tax_total, money(1_440));
        assert_eq!(totals.grand_total, money(10_440));
    }

    #[test]
    fn applying_promotions_overwrites_instead_of_accumulating() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.apply_promotions(discount(1, 1_000), ts(2)).unwrap();
        tx.apply_promotions(discount(1, 2_000), ts(3)).unwrap();
        assert_eq!(tx.state().lines[0].discount_amount, money(2_000));
        assert_eq!(tx.state().lines[0].line_total, money(9_280));

        // An empty outcome clears every discount.
        tx.apply_promotions(PromotionOutcome::empty(), ts(4))
            .unwrap();
        assert_eq!(tx.state().lines[0].discount_amount, money(0));
        assert_eq!(tx.state().lines[0].line_total, money(11_600));
        assert!(tx.applied_promotions().is_empty());
    }

    #[test]
    fn a_discount_can_never_exceed_the_line() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        assert!(matches!(
            tx.apply_promotions(discount(1, 10_001), ts(2)).unwrap_err(),
            DomainError::DiscountExceedsLine { .. }
        ));
        // A full discount is allowed and taxes nothing.
        tx.apply_promotions(discount(1, 10_000), ts(3)).unwrap();
        assert_eq!(tx.state().lines[0].tax_amount, money(0));
        assert_eq!(tx.state().lines[0].line_total, money(0));
    }

    #[test]
    fn an_adjustment_for_a_missing_line_is_refused() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        assert!(matches!(
            tx.apply_promotions(discount(7, 100), ts(2)).unwrap_err(),
            DomainError::AdjustmentTargetMissing { line_number: 7 }
        ));
    }

    #[test]
    fn the_basket_cannot_change_once_payment_has_started() {
        let mut tx = sale();
        tx.add_line(line(10_000, 1, 1_600, 1), ts(1)).unwrap();
        tx.totalize(ts(2)).unwrap();
        tx.commit_cash_payment(
            PaymentId::from_uuid(Uuid::from_u128(50)),
            money(5_000),
            ts(3),
        )
        .unwrap();
        assert!(tx.remove_line(1, ts(4)).is_err());
        assert!(tx
            .change_line_quantity(1, Quantity::from_units(2).unwrap(), ts(4))
            .is_err());
        assert!(tx.apply_promotions(discount(1, 1), ts(4)).is_err());
        assert!(tx.add_line(line(1, 1, 0, 9), ts(4)).is_err());
    }

    #[test]
    fn promotions_work_on_a_tax_inclusive_line_too() {
        let mut tx = sale();
        let mut inclusive = line(11_600, 1, 1_600, 1);
        inclusive.price_mode = PriceMode::TaxInclusive;
        tx.add_line(inclusive, ts(1)).unwrap();
        tx.apply_promotions(discount(1, 1_160), ts(2)).unwrap();

        let line = &tx.state().lines[0];
        // The customer pays 104.40 and the tax is backed out of that.
        assert_eq!(line.line_total, money(10_440));
        assert_eq!(line.taxable_amount, money(9_000));
        assert_eq!(line.tax_amount, money(1_440));
        assert_eq!(
            line.taxable_amount.checked_add(line.tax_amount).unwrap(),
            line.line_total
        );
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
