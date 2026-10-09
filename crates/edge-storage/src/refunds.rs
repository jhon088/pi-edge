//! SQLite adapter for the refund context.
//!
//! Refund attempts are append-only, so nothing here deletes one and a trigger
//! in migration 0008 makes it impossible rather than merely unimplemented.
//!
//! The refundable totals are folded by the database: what earlier refunds took
//! from each line, and what each original tender already gave back. A long
//! refund history should not travel across the boundary just to be summed, and
//! the sums are what the reservation rule depends on (ADR 030).

use edge_application::ports::{RefundUnitOfWork, RefundedLineTotals, RefundedTenderTotals};
use edge_application::AppError;
use edge_domain::prelude::*;
use edge_payments::TenderType;
use edge_refunds::{
    FiscalState, Refund, RefundAmounts, RefundAttempt, RefundAttemptStatus, RefundAuthority,
    RefundFiscalStatus, RefundLine, RefundPayment, RefundPaymentStatus, RefundPaymentView,
    RefundStatus, TransactionFiscalMetadata,
};
use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

use crate::error::sqlite_err;
use crate::sqlite::{money, parse_currency, parse_id, parse_opt_ts, parse_ts, SqliteUnitOfWork};

const REFUND_COLUMNS: &str = "refund_id, original_transaction_id, store_id, business_day_id, \
     session_id, register_id, operator_id, status, authority, authorization_ref, currency, \
     extended_minor, discount_minor, net_minor, tax_minor, gross_minor, fiscal_status, \
     original_fiscal_state, reason_code, restock, created_at, updated_at, accepted_at, \
     completed_at, version";

/// Statuses that still hold refundable quantity: a refund that is live, or one
/// that completed and actually took the goods.
const RESERVING_STATUSES: &str = "('CREATED', 'ACCEPTED', 'FINANCIAL_PENDING', 'COMPLETED')";

impl RefundUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn insert_refund(&mut self, refund: &Refund) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO refunds
                     (refund_id, original_transaction_id, store_id, business_day_id, session_id,
                      register_id, operator_id, status, authority, authorization_ref, currency,
                      extended_minor, discount_minor, net_minor, tax_minor, gross_minor,
                      fiscal_status, original_fiscal_state, reason_code, restock, created_at,
                      updated_at, accepted_at, completed_at, version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
                params![
                    refund.refund_id.to_string(),
                    refund.original_transaction_id.to_string(),
                    refund.store_id.as_str(),
                    refund.business_day_id.to_string(),
                    refund.session_id.to_string(),
                    refund.register_id.as_str(),
                    refund.operator_id.as_str(),
                    refund.status.as_str(),
                    refund.authority.as_str(),
                    refund.authorization_ref,
                    refund.currency.code(),
                    refund.total.extended.minor(),
                    refund.total.discount.minor(),
                    refund.total.net.minor(),
                    refund.total.tax.minor(),
                    refund.total.gross.minor(),
                    refund.fiscal_status.as_str(),
                    refund.original_fiscal_state.as_str(),
                    refund.reason_code,
                    i64::from(refund.restock),
                    refund.created_at.to_rfc3339(),
                    refund.updated_at.to_rfc3339(),
                    refund.accepted_at.map(|ts| ts.to_rfc3339()),
                    refund.completed_at.map(|ts| ts.to_rfc3339()),
                    refund.version,
                ],
            )
            .map_err(refund_conflict)?;
        for line in &refund.lines {
            self.tx
                .execute(
                    "INSERT INTO refund_lines
                         (refund_line_id, refund_id, original_line_number, product_ref,
                          description, quantity_thousandths, unit_price_minor, extended_minor,
                          discount_minor, net_minor, tax_minor, gross_minor)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        line.refund_line_id.to_string(),
                        refund.refund_id.to_string(),
                        line.original_line_number,
                        line.product_ref,
                        line.description,
                        line.quantity.thousandths(),
                        line.original_unit_price.minor(),
                        line.amounts.extended.minor(),
                        line.amounts.discount.minor(),
                        line.amounts.net.minor(),
                        line.amounts.tax.minor(),
                        line.amounts.gross.minor(),
                    ],
                )
                .map_err(sqlite_err)?;
        }
        Ok(())
    }

    fn update_refund(&mut self, refund: &Refund) -> Result<(), AppError> {
        // Optimistic: the version the caller loaded has to still be there.
        let changed = self
            .tx
            .execute(
                "UPDATE refunds
                    SET status = ?2, fiscal_status = ?3, updated_at = ?4, accepted_at = ?5,
                        completed_at = ?6, version = ?7
                  WHERE refund_id = ?1 AND version < ?7",
                params![
                    refund.refund_id.to_string(),
                    refund.status.as_str(),
                    refund.fiscal_status.as_str(),
                    refund.updated_at.to_rfc3339(),
                    refund.accepted_at.map(|ts| ts.to_rfc3339()),
                    refund.completed_at.map(|ts| ts.to_rfc3339()),
                    refund.version,
                ],
            )
            .map_err(sqlite_err)?;
        if changed == 0 {
            return Err(AppError::ConcurrentModification {
                aggregate: "Refund",
                id: refund.refund_id.to_string(),
            });
        }
        Ok(())
    }

    fn find_refund(&mut self, refund_id: RefundId) -> Result<Option<Refund>, AppError> {
        let raw: Option<RawRefund> = self
            .tx
            .query_row(
                &format!("SELECT {REFUND_COLUMNS} FROM refunds WHERE refund_id = ?1"),
                params![refund_id.to_string()],
                RawRefund::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        match raw {
            Some(raw) => Ok(Some(self.hydrate(raw)?)),
            None => Ok(None),
        }
    }

    fn list_transaction_refunds(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<Refund>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {REFUND_COLUMNS} FROM refunds
                  WHERE original_transaction_id = ?1
                  ORDER BY created_at, refund_id"
            ))
            .map_err(sqlite_err)?;
        let raws = statement
            .query_map(params![transaction_id.to_string()], RawRefund::from_row)
            .map_err(sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        drop(statement);
        raws.into_iter().map(|raw| self.hydrate(raw)).collect()
    }

    fn find_refund_in_progress(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Option<Refund>, AppError> {
        let raw: Option<RawRefund> = self
            .tx
            .query_row(
                &format!(
                    "SELECT {REFUND_COLUMNS} FROM refunds
                      WHERE original_transaction_id = ?1
                        AND status NOT IN ('COMPLETED', 'FAILED')"
                ),
                params![transaction_id.to_string()],
                RawRefund::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        match raw {
            Some(raw) => Ok(Some(self.hydrate(raw)?)),
            None => Ok(None),
        }
    }

    fn refunded_by_line(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<RefundedLineTotals>, AppError> {
        let currency = self.transaction_currency(transaction_id)?;
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT l.original_line_number,
                        SUM(l.quantity_thousandths),
                        SUM(l.extended_minor), SUM(l.discount_minor),
                        SUM(l.net_minor), SUM(l.tax_minor), SUM(l.gross_minor)
                   FROM refund_lines l
                   JOIN refunds r ON r.refund_id = l.refund_id
                  WHERE r.original_transaction_id = ?1
                    AND r.status IN {RESERVING_STATUSES}
                  GROUP BY l.original_line_number"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![transaction_id.to_string()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        Ok(rows
            .into_iter()
            .map(
                |(line_number, quantity, extended, discount, net, tax, gross)| RefundedLineTotals {
                    original_line_number: line_number,
                    quantity_thousandths: quantity,
                    amounts: RefundAmounts {
                        extended: money(extended, currency),
                        discount: money(discount, currency),
                        net: money(net, currency),
                        tax: money(tax, currency),
                        gross: money(gross, currency),
                    },
                },
            )
            .collect())
    }

    fn refunded_by_payment(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<RefundedTenderTotals>, AppError> {
        let currency = self.transaction_currency(transaction_id)?;
        let mut statement = self
            .tx
            .prepare(&format!(
                // What a tender has spoken for: the full request of a live
                // refund, and what a settled one actually returned. A failed
                // refund payment reserves nothing.
                "SELECT p.original_payment_id,
                        SUM(CASE WHEN p.status = 'FAILED' THEN 0
                                 WHEN p.status = 'REFUNDED' THEN p.refunded_minor
                                 ELSE p.requested_minor END)
                   FROM refund_payments p
                   JOIN refunds r ON r.refund_id = p.refund_id
                  WHERE r.original_transaction_id = ?1
                    AND r.status IN {RESERVING_STATUSES}
                  GROUP BY p.original_payment_id"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![transaction_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        rows.into_iter()
            .map(|(payment_id, reserved)| {
                Ok(RefundedTenderTotals {
                    original_payment_id: parse_id(&payment_id)?,
                    reserved: money(reserved, currency),
                })
            })
            .collect()
    }

    fn insert_refund_payment(&mut self, payment: &RefundPayment) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO refund_payments
                     (refund_payment_id, refund_id, original_payment_id, tender_type,
                      requested_minor, refunded_minor, status, currency, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    payment.refund_payment_id.to_string(),
                    payment.refund_id.to_string(),
                    payment.original_payment_id.to_string(),
                    payment.tender_type.as_str(),
                    payment.requested_amount.minor(),
                    payment.refunded_amount.minor(),
                    payment.status.as_str(),
                    payment.requested_amount.currency().code(),
                    payment.created_at.to_rfc3339(),
                    payment.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn update_refund_payment(&mut self, payment: &RefundPayment) -> Result<(), AppError> {
        self.tx
            .execute(
                "UPDATE refund_payments
                    SET refunded_minor = ?2, status = ?3, updated_at = ?4
                  WHERE refund_payment_id = ?1",
                params![
                    payment.refund_payment_id.to_string(),
                    payment.refunded_amount.minor(),
                    payment.status.as_str(),
                    payment.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn list_refund_payments(
        &mut self,
        refund_id: RefundId,
    ) -> Result<Vec<RefundPaymentView>, AppError> {
        let mut statement = self
            .tx
            .prepare(
                "SELECT refund_payment_id, refund_id, original_payment_id, tender_type,
                        requested_minor, refunded_minor, status, currency, created_at, updated_at
                   FROM refund_payments
                  WHERE refund_id = ?1
                  ORDER BY created_at, refund_payment_id",
            )
            .map_err(sqlite_err)?;
        let raws = statement
            .query_map(params![refund_id.to_string()], RawRefundPayment::from_row)
            .map_err(sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        drop(statement);

        let mut views = Vec::with_capacity(raws.len());
        for raw in raws {
            let payment = raw.into_domain()?;
            let attempts = self.attempts_of(payment.refund_payment_id)?;
            views.push(RefundPaymentView { payment, attempts });
        }
        Ok(views)
    }

    fn insert_refund_attempt(&mut self, attempt: &RefundAttempt) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO refund_attempts
                     (attempt_id, refund_payment_id, attempt_number, status, requested_minor,
                      refunded_minor, currency, correlation_id, provider_reference, failure_code,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    attempt.attempt_id.to_string(),
                    attempt.refund_payment_id.to_string(),
                    attempt.attempt_number,
                    attempt.status.as_str(),
                    attempt.requested_amount.minor(),
                    attempt.refunded_amount.map(|a| a.minor()),
                    attempt.requested_amount.currency().code(),
                    attempt.correlation_id.to_string(),
                    attempt.provider_reference,
                    attempt.failure_code,
                    attempt.created_at.to_rfc3339(),
                    attempt.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn update_refund_attempt(&mut self, attempt: &RefundAttempt) -> Result<(), AppError> {
        self.tx
            .execute(
                "UPDATE refund_attempts
                    SET status = ?2, refunded_minor = ?3, provider_reference = ?4,
                        failure_code = ?5, updated_at = ?6
                  WHERE attempt_id = ?1",
                params![
                    attempt.attempt_id.to_string(),
                    attempt.status.as_str(),
                    attempt.refunded_amount.map(|a| a.minor()),
                    attempt.provider_reference,
                    attempt.failure_code,
                    attempt.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn next_refund_attempt_number(&mut self, refund_payment_id: Uuid) -> Result<i64, AppError> {
        let highest: Option<i64> = self
            .tx
            .query_row(
                "SELECT MAX(attempt_number) FROM refund_attempts WHERE refund_payment_id = ?1",
                params![refund_payment_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)?
            .flatten();
        Ok(highest.unwrap_or(0) + 1)
    }

    fn find_transaction_fiscal_metadata(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Option<TransactionFiscalMetadata>, AppError> {
        let row: Option<(String, Option<String>, String)> = self
            .tx
            .query_row(
                "SELECT fiscal_state, invoice_ref, updated_at
                   FROM transaction_fiscal_metadata WHERE transaction_id = ?1",
                params![transaction_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sqlite_err)?;
        match row {
            Some((state, invoice_ref, updated_at)) => Ok(Some(TransactionFiscalMetadata {
                fiscal_state: FiscalState::parse(&state).map_err(AppError::Refund)?,
                invoice_ref,
                updated_at: parse_ts(&updated_at)?,
            })),
            None => Ok(None),
        }
    }

    fn put_transaction_fiscal_metadata(
        &mut self,
        transaction_id: TransactionId,
        metadata: &TransactionFiscalMetadata,
    ) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO transaction_fiscal_metadata
                     (transaction_id, fiscal_state, invoice_ref, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (transaction_id) DO UPDATE
                    SET fiscal_state = excluded.fiscal_state,
                        invoice_ref = excluded.invoice_ref,
                        updated_at = excluded.updated_at",
                params![
                    transaction_id.to_string(),
                    metadata.fiscal_state.as_str(),
                    metadata.invoice_ref,
                    metadata.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }
}

impl SqliteUnitOfWork<'_, '_> {
    fn attempts_of(&self, refund_payment_id: Uuid) -> Result<Vec<RefundAttempt>, AppError> {
        let mut statement = self
            .tx
            .prepare(
                "SELECT attempt_id, refund_payment_id, attempt_number, status, requested_minor,
                        refunded_minor, currency, correlation_id, provider_reference,
                        failure_code, created_at, updated_at
                   FROM refund_attempts
                  WHERE refund_payment_id = ?1
                  ORDER BY attempt_number",
            )
            .map_err(sqlite_err)?;
        let raws = statement
            .query_map(params![refund_payment_id.to_string()], RawAttempt::from_row)
            .map_err(sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        drop(statement);
        raws.into_iter().map(RawAttempt::into_domain).collect()
    }

    fn transaction_currency(&self, transaction_id: TransactionId) -> Result<Currency, AppError> {
        let code: String = self
            .tx
            .query_row(
                "SELECT currency FROM retail_transactions WHERE transaction_id = ?1",
                params![transaction_id.to_string()],
                |row| row.get(0),
            )
            .map_err(sqlite_err)?;
        parse_currency(&code)
    }

    fn hydrate(&self, raw: RawRefund) -> Result<Refund, AppError> {
        let currency = parse_currency(&raw.currency)?;
        let mut statement = self
            .tx
            .prepare(
                "SELECT refund_line_id, original_line_number, product_ref, description,
                        quantity_thousandths, unit_price_minor, extended_minor, discount_minor,
                        net_minor, tax_minor, gross_minor
                   FROM refund_lines WHERE refund_id = ?1 ORDER BY original_line_number",
            )
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![raw.refund_id.clone()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        drop(statement);

        let mut lines = Vec::with_capacity(rows.len());
        for row in rows {
            lines.push(RefundLine {
                refund_line_id: parse_uuid(&row.0)?,
                original_line_number: row.1,
                product_ref: row.2,
                description: row.3,
                quantity: Quantity::from_thousandths(row.4)
                    .map_err(|e| AppError::Domain(DomainError::Money(e)))?,
                original_unit_price: money(row.5, currency),
                amounts: RefundAmounts {
                    extended: money(row.6, currency),
                    discount: money(row.7, currency),
                    net: money(row.8, currency),
                    tax: money(row.9, currency),
                    gross: money(row.10, currency),
                },
            });
        }

        Ok(Refund {
            refund_id: parse_id(&raw.refund_id)?,
            original_transaction_id: parse_id(&raw.original_transaction_id)?,
            store_id: parse_id(&raw.store_id)?,
            business_day_id: parse_id(&raw.business_day_id)?,
            session_id: parse_id(&raw.session_id)?,
            register_id: parse_id(&raw.register_id)?,
            operator_id: parse_id(&raw.operator_id)?,
            status: RefundStatus::parse(&raw.status).map_err(AppError::Refund)?,
            authority: RefundAuthority::parse(&raw.authority).map_err(AppError::Refund)?,
            authorization_ref: raw.authorization_ref,
            lines,
            currency,
            total: RefundAmounts {
                extended: money(raw.extended_minor, currency),
                discount: money(raw.discount_minor, currency),
                net: money(raw.net_minor, currency),
                tax: money(raw.tax_minor, currency),
                gross: money(raw.gross_minor, currency),
            },
            fiscal_status: RefundFiscalStatus::parse(&raw.fiscal_status)
                .map_err(AppError::Refund)?,
            original_fiscal_state: FiscalState::parse(&raw.original_fiscal_state)
                .map_err(AppError::Refund)?,
            reason_code: raw.reason_code,
            restock: raw.restock != 0,
            created_at: parse_ts(&raw.created_at)?,
            updated_at: parse_ts(&raw.updated_at)?,
            accepted_at: parse_opt_ts(raw.accepted_at)?,
            completed_at: parse_opt_ts(raw.completed_at)?,
            version: raw.version,
        })
    }
}

/// A unique-index violation on the live-refund index is the concurrency rule
/// speaking, not a storage failure (ADR 030).
fn refund_conflict(error: rusqlite::Error) -> AppError {
    if let rusqlite::Error::SqliteFailure(inner, _) = &error {
        if inner.code == rusqlite::ErrorCode::ConstraintViolation {
            return AppError::Refund(edge_refunds::RefundError::InvalidRecord {
                field: "refund",
                reason: "a refund of this sale is already in progress".to_owned(),
            });
        }
    }
    sqlite_err(error)
}

fn parse_uuid(value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| {
        AppError::Refund(edge_refunds::RefundError::InvalidRecord {
            field: "uuid",
            reason: format!("`{value}` is not a uuid"),
        })
    })
}

struct RawRefund {
    refund_id: String,
    original_transaction_id: String,
    store_id: String,
    business_day_id: String,
    session_id: String,
    register_id: String,
    operator_id: String,
    status: String,
    authority: String,
    authorization_ref: Option<String>,
    currency: String,
    extended_minor: i64,
    discount_minor: i64,
    net_minor: i64,
    tax_minor: i64,
    gross_minor: i64,
    fiscal_status: String,
    original_fiscal_state: String,
    reason_code: String,
    restock: i64,
    created_at: String,
    updated_at: String,
    accepted_at: Option<String>,
    completed_at: Option<String>,
    version: i64,
}

impl RawRefund {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            refund_id: row.get(0)?,
            original_transaction_id: row.get(1)?,
            store_id: row.get(2)?,
            business_day_id: row.get(3)?,
            session_id: row.get(4)?,
            register_id: row.get(5)?,
            operator_id: row.get(6)?,
            status: row.get(7)?,
            authority: row.get(8)?,
            authorization_ref: row.get(9)?,
            currency: row.get(10)?,
            extended_minor: row.get(11)?,
            discount_minor: row.get(12)?,
            net_minor: row.get(13)?,
            tax_minor: row.get(14)?,
            gross_minor: row.get(15)?,
            fiscal_status: row.get(16)?,
            original_fiscal_state: row.get(17)?,
            reason_code: row.get(18)?,
            restock: row.get(19)?,
            created_at: row.get(20)?,
            updated_at: row.get(21)?,
            accepted_at: row.get(22)?,
            completed_at: row.get(23)?,
            version: row.get(24)?,
        })
    }
}

struct RawRefundPayment {
    refund_payment_id: String,
    refund_id: String,
    original_payment_id: String,
    tender_type: String,
    requested_minor: i64,
    refunded_minor: i64,
    status: String,
    currency: String,
    created_at: String,
    updated_at: String,
}

impl RawRefundPayment {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            refund_payment_id: row.get(0)?,
            refund_id: row.get(1)?,
            original_payment_id: row.get(2)?,
            tender_type: row.get(3)?,
            requested_minor: row.get(4)?,
            refunded_minor: row.get(5)?,
            status: row.get(6)?,
            currency: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    fn into_domain(self) -> Result<RefundPayment, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(RefundPayment {
            refund_payment_id: parse_uuid(&self.refund_payment_id)?,
            refund_id: parse_id(&self.refund_id)?,
            original_payment_id: parse_id(&self.original_payment_id)?,
            tender_type: TenderType::parse(&self.tender_type).map_err(AppError::Payment)?,
            requested_amount: money(self.requested_minor, currency),
            refunded_amount: money(self.refunded_minor, currency),
            status: RefundPaymentStatus::parse(&self.status).map_err(AppError::Refund)?,
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
        })
    }
}

struct RawAttempt {
    attempt_id: String,
    refund_payment_id: String,
    attempt_number: i64,
    status: String,
    requested_minor: i64,
    refunded_minor: Option<i64>,
    currency: String,
    correlation_id: String,
    provider_reference: Option<String>,
    failure_code: Option<String>,
    created_at: String,
    updated_at: String,
}

impl RawAttempt {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            attempt_id: row.get(0)?,
            refund_payment_id: row.get(1)?,
            attempt_number: row.get(2)?,
            status: row.get(3)?,
            requested_minor: row.get(4)?,
            refunded_minor: row.get(5)?,
            currency: row.get(6)?,
            correlation_id: row.get(7)?,
            provider_reference: row.get(8)?,
            failure_code: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    fn into_domain(self) -> Result<RefundAttempt, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(RefundAttempt {
            attempt_id: parse_uuid(&self.attempt_id)?,
            refund_payment_id: parse_uuid(&self.refund_payment_id)?,
            attempt_number: self.attempt_number,
            status: RefundAttemptStatus::parse(&self.status).map_err(AppError::Refund)?,
            requested_amount: money(self.requested_minor, currency),
            refunded_amount: self.refunded_minor.map(|minor| money(minor, currency)),
            correlation_id: parse_uuid(&self.correlation_id)?,
            provider_reference: self.provider_reference,
            failure_code: self.failure_code,
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
        })
    }
}
