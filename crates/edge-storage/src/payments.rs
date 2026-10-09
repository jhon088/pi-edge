//! SQLite adapter for the payment context.
//!
//! Payments and their attempts are written with plain `INSERT` and `UPDATE`:
//! an attempt is append-only and a payment moves through a state machine, so
//! an upsert would hide exactly the mistakes the state machine exists to
//! prevent. Every lookup here is an index hit or a primary key.

use edge_application::ports::PaymentUnitOfWork;
use edge_application::AppError;
use edge_domain::prelude::*;
use edge_payments::{
    AttemptStatus, Payment, PaymentAttempt, PaymentStatus, PaymentView, ProviderKind, TenderType,
};
use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

use crate::error::sqlite_err;
use crate::sqlite::{money, parse_currency, parse_id, parse_ts, SqliteUnitOfWork};

const PAYMENT_COLUMNS: &str = "payment_id, transaction_id, tender_type, provider_kind, \
     requested_minor, authorized_minor, committed_minor, currency, status, created_at, updated_at";

const ATTEMPT_COLUMNS: &str = "attempt_id, payment_id, attempt_number, status, requested_minor, \
     authorized_minor, currency, provider_kind, correlation_id, provider_reference, failure_code, \
     created_at, updated_at";

impl PaymentUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn insert_payment(&mut self, payment: &Payment) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO payments
                     (payment_id, transaction_id, tender_type, provider_kind, requested_minor,
                      authorized_minor, committed_minor, currency, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    payment.payment_id.to_string(),
                    payment.transaction_id.to_string(),
                    payment.tender_type.as_str(),
                    payment.provider_kind.map(ProviderKind::as_str),
                    payment.requested_amount.minor(),
                    payment.authorized_amount.map(|amount| amount.minor()),
                    payment.committed_amount.minor(),
                    payment.requested_amount.currency().code(),
                    payment.status.as_str(),
                    payment.created_at.to_rfc3339(),
                    payment.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn update_payment(&mut self, payment: &Payment) -> Result<(), AppError> {
        let affected = self
            .tx
            .execute(
                "UPDATE payments
                 SET status = ?2, authorized_minor = ?3, committed_minor = ?4, updated_at = ?5
                 WHERE payment_id = ?1",
                params![
                    payment.payment_id.to_string(),
                    payment.status.as_str(),
                    payment.authorized_amount.map(|amount| amount.minor()),
                    payment.committed_amount.minor(),
                    payment.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        if affected != 1 {
            return Err(AppError::Payment(
                edge_payments::PaymentError::PaymentNotFound {
                    payment_id: payment.payment_id.to_string(),
                },
            ));
        }
        Ok(())
    }

    fn find_payment(&mut self, payment_id: PaymentId) -> Result<Option<PaymentView>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!("SELECT {PAYMENT_COLUMNS} FROM payments WHERE payment_id = ?1"),
                params![payment_id.to_string()],
                RawPayment::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        match raw {
            None => Ok(None),
            Some(raw) => {
                let payment = raw.into_domain()?;
                let attempts = self.load_attempts(payment.payment_id)?;
                Ok(Some(PaymentView { payment, attempts }))
            }
        }
    }

    fn list_transaction_payments(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Vec<PaymentView>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {PAYMENT_COLUMNS} FROM payments
                 WHERE transaction_id = ?1
                 ORDER BY created_at ASC, payment_id ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![transaction_id.to_string()], RawPayment::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        drop(statement);

        let mut views = Vec::with_capacity(rows.len());
        for raw in rows {
            let payment = raw.into_domain()?;
            let attempts = self.load_attempts(payment.payment_id)?;
            views.push(PaymentView { payment, attempts });
        }
        Ok(views)
    }

    fn insert_payment_attempt(&mut self, attempt: &PaymentAttempt) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO payment_attempts
                     (attempt_id, payment_id, attempt_number, status, requested_minor,
                      authorized_minor, currency, provider_kind, correlation_id,
                      provider_reference, failure_code, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    attempt.attempt_id.to_string(),
                    attempt.payment_id.to_string(),
                    attempt.attempt_number,
                    attempt.status.as_str(),
                    attempt.requested_amount.minor(),
                    attempt.authorized_amount.map(|amount| amount.minor()),
                    attempt.requested_amount.currency().code(),
                    attempt.provider_kind.as_str(),
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

    fn update_payment_attempt(&mut self, attempt: &PaymentAttempt) -> Result<(), AppError> {
        let affected = self
            .tx
            .execute(
                "UPDATE payment_attempts
                 SET status = ?2, authorized_minor = ?3, provider_reference = ?4,
                     failure_code = ?5, updated_at = ?6
                 WHERE attempt_id = ?1",
                params![
                    attempt.attempt_id.to_string(),
                    attempt.status.as_str(),
                    attempt.authorized_amount.map(|amount| amount.minor()),
                    attempt.provider_reference,
                    attempt.failure_code,
                    attempt.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        if affected != 1 {
            return Err(AppError::Payment(
                edge_payments::PaymentError::AttemptNotFound {
                    attempt_id: attempt.attempt_id.to_string(),
                },
            ));
        }
        Ok(())
    }

    fn find_payment_attempt(
        &mut self,
        attempt_id: Uuid,
    ) -> Result<Option<PaymentAttempt>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!("SELECT {ATTEMPT_COLUMNS} FROM payment_attempts WHERE attempt_id = ?1"),
                params![attempt_id.to_string()],
                RawAttempt::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawAttempt::into_domain).transpose()
    }

    fn next_attempt_number(&mut self, payment_id: PaymentId) -> Result<i64, AppError> {
        self.tx
            .query_row(
                "SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM payment_attempts
                 WHERE payment_id = ?1",
                params![payment_id.to_string()],
                |row| row.get(0),
            )
            .map_err(sqlite_err)
    }
}

impl SqliteUnitOfWork<'_, '_> {
    fn load_attempts(&self, payment_id: PaymentId) -> Result<Vec<PaymentAttempt>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {ATTEMPT_COLUMNS} FROM payment_attempts
                 WHERE payment_id = ?1
                 ORDER BY attempt_number ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![payment_id.to_string()], RawAttempt::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawAttempt::into_domain).collect()
    }
}

// ------------------------------------------------------------ row conversions

struct RawPayment {
    payment_id: String,
    transaction_id: String,
    tender_type: String,
    provider_kind: Option<String>,
    requested_minor: i64,
    authorized_minor: Option<i64>,
    committed_minor: i64,
    currency: String,
    status: String,
    created_at: String,
    updated_at: String,
}

impl RawPayment {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            payment_id: row.get(0)?,
            transaction_id: row.get(1)?,
            tender_type: row.get(2)?,
            provider_kind: row.get(3)?,
            requested_minor: row.get(4)?,
            authorized_minor: row.get(5)?,
            committed_minor: row.get(6)?,
            currency: row.get(7)?,
            status: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    fn into_domain(self) -> Result<Payment, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(Payment::rehydrate(Payment {
            payment_id: parse_id(&self.payment_id)?,
            transaction_id: parse_id(&self.transaction_id)?,
            tender_type: TenderType::parse(&self.tender_type).map_err(AppError::Payment)?,
            provider_kind: self
                .provider_kind
                .as_deref()
                .map(ProviderKind::parse)
                .transpose()
                .map_err(AppError::Payment)?,
            requested_amount: money(self.requested_minor, currency),
            authorized_amount: self.authorized_minor.map(|minor| money(minor, currency)),
            committed_amount: money(self.committed_minor, currency),
            status: PaymentStatus::parse(&self.status).map_err(AppError::Payment)?,
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
        }))
    }
}

struct RawAttempt {
    attempt_id: String,
    payment_id: String,
    attempt_number: i64,
    status: String,
    requested_minor: i64,
    authorized_minor: Option<i64>,
    currency: String,
    provider_kind: String,
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
            payment_id: row.get(1)?,
            attempt_number: row.get(2)?,
            status: row.get(3)?,
            requested_minor: row.get(4)?,
            authorized_minor: row.get(5)?,
            currency: row.get(6)?,
            provider_kind: row.get(7)?,
            correlation_id: row.get(8)?,
            provider_reference: row.get(9)?,
            failure_code: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
        })
    }

    fn into_domain(self) -> Result<PaymentAttempt, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(PaymentAttempt {
            attempt_id: parse_uuid(&self.attempt_id, "attempt_id")?,
            payment_id: parse_id(&self.payment_id)?,
            attempt_number: self.attempt_number,
            status: AttemptStatus::parse(&self.status).map_err(AppError::Payment)?,
            requested_amount: money(self.requested_minor, currency),
            authorized_amount: self.authorized_minor.map(|minor| money(minor, currency)),
            provider_kind: ProviderKind::parse(&self.provider_kind).map_err(AppError::Payment)?,
            correlation_id: parse_uuid(&self.correlation_id, "correlation_id")?,
            provider_reference: self.provider_reference,
            failure_code: self.failure_code,
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
        })
    }
}

fn parse_uuid(value: &str, field: &'static str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| {
        AppError::Payment(edge_payments::PaymentError::InvalidRecord {
            field,
            reason: format!("`{value}` is not a UUID"),
        })
    })
}
