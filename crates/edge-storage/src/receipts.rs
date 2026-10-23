//! SQLite adapter for receipt obligations and the operational history.
//!
//! Two narrow jobs, and the second one explains why they share a file: the
//! history a cashier reads *is* a sale joined to its receipt state, and
//! splitting that join across two modules would buy nothing.
//!
//! Money is read straight out of `retail_transactions.grand_total_minor`,
//! which the sale already materialised. Nothing here recomputes a total
//! (ADR 043).

use edge_application::ports::{ReceiptUnitOfWork, RecentSale};
use edge_application::AppError;
use edge_domain::prelude::{
    Money, ReceiptAttention, ReceiptAttentionScope, ReceiptStatus, TransactionId,
    TransactionReceipt,
};
use rusqlite::{params, OptionalExtension, Row};

use crate::error::sqlite_err;
use crate::sqlite::{parse_currency, parse_id, parse_opt_ts, parse_ts, SqliteUnitOfWork};

const RECEIPT_COLUMNS: &str = "transaction_id, status, attempt_count, first_printed_at, \
     last_attempt_at, last_error_code, last_error_detail, created_at, updated_at";

impl ReceiptUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn insert_receipt_obligation(&mut self, receipt: &TransactionReceipt) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO transaction_receipts
                     (transaction_id, status, attempt_count, first_printed_at, last_attempt_at,
                      last_error_code, last_error_detail, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                 ON CONFLICT (transaction_id) DO NOTHING",
                params![
                    receipt.transaction_id.to_string(),
                    receipt.status.as_str(),
                    receipt.attempt_count,
                    receipt.first_printed_at.map(|ts| ts.to_rfc3339()),
                    receipt.last_attempt_at.map(|ts| ts.to_rfc3339()),
                    receipt.last_error_code,
                    receipt.last_error_detail,
                    receipt.created_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn find_receipt(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Option<TransactionReceipt>, AppError> {
        let sql =
            format!("SELECT {RECEIPT_COLUMNS} FROM transaction_receipts WHERE transaction_id = ?1");
        let row = self
            .tx
            .query_row(&sql, params![transaction_id.to_string()], raw_receipt)
            .optional()
            .map_err(sqlite_err)?;
        row.map(RawReceipt::into_domain).transpose()
    }

    fn update_receipt(&mut self, receipt: &TransactionReceipt) -> Result<(), AppError> {
        self.tx
            .execute(
                "UPDATE transaction_receipts
                 SET status = ?2,
                     attempt_count = ?3,
                     first_printed_at = ?4,
                     last_attempt_at = ?5,
                     last_error_code = ?6,
                     last_error_detail = ?7,
                     updated_at = ?8
                 WHERE transaction_id = ?1",
                params![
                    receipt.transaction_id.to_string(),
                    receipt.status.as_str(),
                    receipt.attempt_count,
                    receipt.first_printed_at.map(|ts| ts.to_rfc3339()),
                    receipt.last_attempt_at.map(|ts| ts.to_rfc3339()),
                    receipt.last_error_code,
                    receipt.last_error_detail,
                    receipt.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn list_receipts_by_status(
        &mut self,
        status: ReceiptStatus,
        limit: i64,
    ) -> Result<Vec<TransactionReceipt>, AppError> {
        let sql = format!(
            "SELECT {RECEIPT_COLUMNS}
             FROM transaction_receipts
             WHERE status = ?1
             ORDER BY created_at ASC
             LIMIT ?2"
        );
        let mut stmt = self.tx.prepare(&sql).map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![status.as_str(), limit], raw_receipt)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawReceipt::into_domain).collect()
    }

    fn receipt_attention(
        &mut self,
        scope: ReceiptAttentionScope,
    ) -> Result<ReceiptAttention, AppError> {
        let mut attention = ReceiptAttention::default();
        // One statement for all three consumers. The day and the session a
        // sale belongs to are already on the sale, so each scope is the
        // canonical relation and never a guess at timestamps (ADR 043).
        let (day, session) = match scope {
            ReceiptAttentionScope::Global => (None, None),
            ReceiptAttentionScope::BusinessDay(day) => (Some(day.to_string()), None),
            ReceiptAttentionScope::RegisterSession(session) => (None, Some(session.to_string())),
        };
        let mut stmt = self
            .tx
            .prepare(
                "SELECT r.status, COUNT(*)
                 FROM transaction_receipts r
                 JOIN retail_transactions t ON t.transaction_id = r.transaction_id
                 WHERE r.status <> 'PRINTED'
                   AND (?1 IS NULL OR t.business_day_id = ?1)
                   AND (?2 IS NULL OR t.session_id = ?2)
                 GROUP BY r.status",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![day, session], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        for (status, count) in rows {
            match ReceiptStatus::parse(&status).map_err(AppError::from)? {
                ReceiptStatus::Pending => attention.pending = count,
                ReceiptStatus::Failed => attention.failed = count,
                ReceiptStatus::Unknown => attention.unknown = count,
                // Excluded by the query, and not an attention item anyway.
                ReceiptStatus::Printed => {}
            }
        }
        Ok(attention)
    }

    fn list_recent_sales(&mut self, limit: i64, offset: i64) -> Result<Vec<RecentSale>, AppError> {
        // A left join: a sale finalized before milestone 12 has no obligation
        // row. It is still shown — hiding history would be worse — with no
        // receipt state rather than an invented one.
        let mut stmt = self
            .tx
            .prepare(
                "SELECT t.transaction_id, t.register_id, t.register_sequence, t.finalized_at,
                        t.grand_total_minor, t.currency, r.status
                 FROM retail_transactions t
                 LEFT JOIN transaction_receipts r ON r.transaction_id = t.transaction_id
                 WHERE t.finalized_at IS NOT NULL
                 ORDER BY t.finalized_at DESC, t.register_sequence DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![limit, offset], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;

        rows.into_iter()
            .map(
                |(id, register, sequence, finalized_at, total, currency, status)| {
                    Ok(RecentSale {
                        transaction_id: parse_id(&id)?,
                        register_id: parse_id(&register)?,
                        register_sequence: sequence,
                        finalized_at: parse_ts(&finalized_at)?,
                        grand_total: Money::from_minor(total, parse_currency(&currency)?),
                        receipt_status: status
                            .map(|status| ReceiptStatus::parse(&status))
                            .transpose()
                            .map_err(AppError::from)?,
                    })
                },
            )
            .collect()
    }
}

struct RawReceipt {
    transaction_id: String,
    status: String,
    attempt_count: i64,
    first_printed_at: Option<String>,
    last_attempt_at: Option<String>,
    last_error_code: Option<String>,
    last_error_detail: Option<String>,
    created_at: String,
    updated_at: String,
}

fn raw_receipt(row: &Row<'_>) -> rusqlite::Result<RawReceipt> {
    Ok(RawReceipt {
        transaction_id: row.get(0)?,
        status: row.get(1)?,
        attempt_count: row.get(2)?,
        first_printed_at: row.get(3)?,
        last_attempt_at: row.get(4)?,
        last_error_code: row.get(5)?,
        last_error_detail: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

impl RawReceipt {
    fn into_domain(self) -> Result<TransactionReceipt, AppError> {
        Ok(TransactionReceipt {
            transaction_id: parse_id(&self.transaction_id)?,
            status: ReceiptStatus::parse(&self.status).map_err(AppError::from)?,
            attempt_count: self.attempt_count,
            first_printed_at: parse_opt_ts(self.first_printed_at)?,
            last_attempt_at: parse_opt_ts(self.last_attempt_at)?,
            last_error_code: self.last_error_code,
            last_error_detail: self.last_error_detail,
            created_at: parse_ts(&self.created_at)?,
            updated_at: parse_ts(&self.updated_at)?,
        })
    }
}
