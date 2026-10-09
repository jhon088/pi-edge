//! SQLite adapter for the hardware action log.
//!
//! Append-only, so nothing here deletes a row and a trigger in migration 0009
//! makes it impossible rather than merely unimplemented. An action *is*
//! updated, once, when the device answers: recording the outcome is not
//! rewriting the attempt (ADR 037).
//!
//! The claim is one `INSERT` that a partial unique index may refuse. That
//! refusal is the idempotency rule, not a storage failure: it means this cause
//! already has a physical action taken.

use edge_application::ports::{
    HardwareAction, HardwareActionKind, HardwareActionStatus, HardwareUnitOfWork,
};
use edge_application::AppError;
use edge_domain::prelude::Timestamp;
use edge_hardware::{HardwareDeviceKind, HardwareError};
use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

use crate::error::sqlite_err;
use crate::sqlite::{parse_opt_ts, parse_ts, SqliteUnitOfWork};

const ACTION_COLUMNS: &str = "action_id, kind, device_kind, reason, source_ref, status, \
     error_code, error_message, requested_at, completed_at";

impl HardwareUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn claim_hardware_action(&mut self, action: &HardwareAction) -> Result<Option<Uuid>, AppError> {
        let claimed = self
            .tx
            .execute(
                "INSERT INTO hardware_actions
                     (action_id, kind, device_kind, reason, source_ref, status,
                      error_code, error_message, requested_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'PENDING', NULL, NULL, ?6, NULL)
                 ON CONFLICT DO NOTHING",
                params![
                    action.action_id.to_string(),
                    action.kind.as_str(),
                    action.device_kind.as_str(),
                    action.reason,
                    action.source_ref,
                    action.requested_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        // Zero rows means the partial index refused it: this cause already has
        // an action pending or completed, so nothing physical should happen
        // again.
        Ok((claimed == 1).then_some(action.action_id))
    }

    fn resolve_hardware_action(
        &mut self,
        action_id: Uuid,
        outcome: Result<(), &HardwareError>,
        now: Timestamp,
    ) -> Result<(), AppError> {
        let (status, code, message) = match outcome {
            Ok(()) => (HardwareActionStatus::Completed, None, None),
            Err(error) => (
                HardwareActionStatus::Failed,
                Some(error.code().to_owned()),
                Some(error.to_string()),
            ),
        };
        self.tx
            .execute(
                "UPDATE hardware_actions
                    SET status = ?2, error_code = ?3, error_message = ?4, completed_at = ?5
                  WHERE action_id = ?1 AND status = 'PENDING'",
                params![
                    action_id.to_string(),
                    status.as_str(),
                    code,
                    message,
                    now.to_rfc3339(),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn latest_hardware_action(
        &mut self,
        device_kind: HardwareDeviceKind,
    ) -> Result<Option<HardwareAction>, AppError> {
        let raw: Option<RawAction> = self
            .tx
            .query_row(
                &format!(
                    "SELECT {ACTION_COLUMNS} FROM hardware_actions
                      WHERE device_kind = ?1
                      ORDER BY requested_at DESC, action_id DESC
                      LIMIT 1"
                ),
                params![device_kind.as_str()],
                RawAction::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawAction::into_domain).transpose()
    }

    fn last_hardware_success(
        &mut self,
        device_kind: HardwareDeviceKind,
    ) -> Result<Option<Timestamp>, AppError> {
        let raw: Option<String> = self
            .tx
            .query_row(
                "SELECT completed_at FROM hardware_actions
                  WHERE device_kind = ?1 AND status = 'COMPLETED'
                  ORDER BY completed_at DESC
                  LIMIT 1",
                params![device_kind.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        match raw {
            Some(value) => Ok(Some(parse_ts(&value)?)),
            None => Ok(None),
        }
    }
}

struct RawAction {
    action_id: String,
    kind: String,
    device_kind: String,
    reason: String,
    source_ref: Option<String>,
    status: String,
    error_code: Option<String>,
    error_message: Option<String>,
    requested_at: String,
    completed_at: Option<String>,
}

impl RawAction {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            action_id: row.get(0)?,
            kind: row.get(1)?,
            device_kind: row.get(2)?,
            reason: row.get(3)?,
            source_ref: row.get(4)?,
            status: row.get(5)?,
            error_code: row.get(6)?,
            error_message: row.get(7)?,
            requested_at: row.get(8)?,
            completed_at: row.get(9)?,
        })
    }

    fn into_domain(self) -> Result<HardwareAction, AppError> {
        let kind = match self.kind.as_str() {
            "DRAWER_OPEN" => HardwareActionKind::DrawerOpen,
            "PRINT" => HardwareActionKind::Print,
            other => {
                return Err(AppError::Storage(format!(
                    "`{other}` is not a hardware action kind"
                )))
            }
        };
        let status = match self.status.as_str() {
            "PENDING" => HardwareActionStatus::Pending,
            "COMPLETED" => HardwareActionStatus::Completed,
            "FAILED" => HardwareActionStatus::Failed,
            other => {
                return Err(AppError::Storage(format!(
                    "`{other}` is not a hardware action status"
                )))
            }
        };
        Ok(HardwareAction {
            action_id: Uuid::parse_str(&self.action_id)
                .map_err(|_| AppError::Storage("hardware action id is not a uuid".to_owned()))?,
            kind,
            device_kind: HardwareDeviceKind::parse(&self.device_kind)
                .map_err(|error| AppError::Storage(error.to_string()))?,
            reason: self.reason,
            source_ref: self.source_ref,
            status,
            error_code: self.error_code,
            error_message: self.error_message,
            requested_at: parse_ts(&self.requested_at)?,
            completed_at: parse_opt_ts(self.completed_at)?,
        })
    }
}
