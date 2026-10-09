//! SQLite adapter for the cash context.
//!
//! The ledger is append-only, so there is no `UPDATE` or `DELETE` statement
//! against `cash_movements` anywhere in this file — and a trigger in migration
//! 0006 makes it impossible rather than merely unimplemented (ADR 021). Close
//! declarations are evidence and are equally immutable.

use edge_application::ports::CashUnitOfWork;
use edge_application::AppError;
use edge_cash::{
    Approval, ApprovalAction, ApprovalStatus, CashError, CashMovement, CloseDeclaration, Direction,
    MovementType, SafeDrop, SafeDropStatus,
};
use edge_domain::prelude::*;
use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

use crate::error::sqlite_err;
use crate::sqlite::{
    money, parse_currency, parse_id, parse_opt_ts, parse_ts, RawSession, SqliteUnitOfWork,
    SESSION_COLUMNS,
};

const MOVEMENT_COLUMNS: &str = "movement_id, business_day_id, session_id, register_id, \
     operator_id, movement_type, direction, amount_minor, currency, tendered_minor, \
     change_minor, source_ref, reason_code, approval_id, created_at";

const APPROVAL_COLUMNS: &str = "approval_id, action_type, aggregate_ref, requested_by, \
     requested_at, approved_by, decided_at, status, reason";

const SAFE_DROP_COLUMNS: &str = "safe_drop_id, session_id, approval_id, amount_minor, currency, \
     reason_code, status, requested_by, requested_at, approved_by, approved_at";

const DECLARATION_COLUMNS: &str = "declaration_id, session_id, closure_kind, declared_minor, \
     expected_minor, variance_minor, currency, declared_by, declared_at, reason";

impl CashUnitOfWork for SqliteUnitOfWork<'_, '_> {
    fn insert_cash_movement(&mut self, movement: &CashMovement) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO cash_movements
                     (movement_id, business_day_id, session_id, register_id, operator_id,
                      movement_type, direction, amount_minor, currency, tendered_minor,
                      change_minor, source_ref, reason_code, approval_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    movement.movement_id.to_string(),
                    movement.business_day_id.to_string(),
                    movement.session_id.to_string(),
                    movement.register_id.as_str(),
                    movement.operator_id.as_str(),
                    movement.movement_type.as_str(),
                    movement.direction.as_str(),
                    movement.amount.minor(),
                    movement.amount.currency().code(),
                    movement.tendered.map(|amount| amount.minor()),
                    movement.change.map(|amount| amount.minor()),
                    movement.source_ref,
                    movement.reason_code,
                    movement.approval_id.map(|id| id.to_string()),
                    movement.created_at.to_rfc3339(),
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    // The unique index on (session, type, source_ref) is what
                    // makes a replayed payment unable to move the drawer twice.
                    AppError::Cash(CashError::InvalidRecord {
                        field: "source_ref",
                        reason: "this cause has already moved the drawer".to_owned(),
                    })
                } else {
                    sqlite_err(error)
                }
            })?;
        Ok(())
    }

    fn list_cash_movements(
        &mut self,
        session_id: SessionId,
    ) -> Result<Vec<CashMovement>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {MOVEMENT_COLUMNS} FROM cash_movements
                 WHERE session_id = ?1
                 ORDER BY created_at ASC, movement_id ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![session_id.to_string()], RawMovement::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawMovement::into_domain).collect()
    }

    fn find_cash_movement_by_source(
        &mut self,
        session_id: SessionId,
        movement_type: MovementType,
        source_ref: &str,
    ) -> Result<Option<CashMovement>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!(
                    "SELECT {MOVEMENT_COLUMNS} FROM cash_movements
                     WHERE session_id = ?1 AND movement_type = ?2 AND source_ref = ?3"
                ),
                params![session_id.to_string(), movement_type.as_str(), source_ref],
                RawMovement::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawMovement::into_domain).transpose()
    }

    fn insert_approval(&mut self, approval: &Approval) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO approvals
                     (approval_id, action_type, aggregate_ref, requested_by, requested_at,
                      approved_by, decided_at, status, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    approval.approval_id.to_string(),
                    approval.action_type.as_str(),
                    approval.aggregate_ref,
                    approval.requested_by.as_str(),
                    approval.requested_at.to_rfc3339(),
                    approval.approved_by.as_ref().map(OperatorId::as_str),
                    approval.decided_at.map(|ts| ts.to_rfc3339()),
                    approval.status.as_str(),
                    approval.reason,
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn update_approval(&mut self, approval: &Approval) -> Result<(), AppError> {
        let affected = self
            .tx
            .execute(
                "UPDATE approvals
                 SET approved_by = ?2, decided_at = ?3, status = ?4
                 WHERE approval_id = ?1",
                params![
                    approval.approval_id.to_string(),
                    approval.approved_by.as_ref().map(OperatorId::as_str),
                    approval.decided_at.map(|ts| ts.to_rfc3339()),
                    approval.status.as_str(),
                ],
            )
            .map_err(sqlite_err)?;
        if affected != 1 {
            return Err(AppError::Cash(CashError::InvalidRecord {
                field: "approval_id",
                reason: "no such approval".to_owned(),
            }));
        }
        Ok(())
    }

    fn find_approval(&mut self, approval_id: Uuid) -> Result<Option<Approval>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!("SELECT {APPROVAL_COLUMNS} FROM approvals WHERE approval_id = ?1"),
                params![approval_id.to_string()],
                RawApproval::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawApproval::into_domain).transpose()
    }

    fn insert_safe_drop(&mut self, drop: &SafeDrop) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO safe_drops
                     (safe_drop_id, session_id, approval_id, amount_minor, currency,
                      reason_code, status, requested_by, requested_at, approved_by, approved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    drop.safe_drop_id.to_string(),
                    drop.session_id.to_string(),
                    drop.approval_id.to_string(),
                    drop.amount.minor(),
                    drop.amount.currency().code(),
                    drop.reason_code,
                    drop.status.as_str(),
                    drop.requested_by.as_str(),
                    drop.requested_at.to_rfc3339(),
                    drop.approved_by.as_ref().map(OperatorId::as_str),
                    drop.approved_at.map(|ts| ts.to_rfc3339()),
                ],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    fn update_safe_drop(&mut self, drop: &SafeDrop) -> Result<(), AppError> {
        let affected = self
            .tx
            .execute(
                "UPDATE safe_drops
                 SET status = ?2, approved_by = ?3, approved_at = ?4
                 WHERE safe_drop_id = ?1",
                params![
                    drop.safe_drop_id.to_string(),
                    drop.status.as_str(),
                    drop.approved_by.as_ref().map(OperatorId::as_str),
                    drop.approved_at.map(|ts| ts.to_rfc3339()),
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    // The CHECK refuses an approver who is the requester, so
                    // the rule holds even if the domain were bypassed.
                    AppError::Cash(CashError::SelfApproval)
                } else {
                    sqlite_err(error)
                }
            })?;
        if affected != 1 {
            return Err(AppError::Cash(CashError::SafeDropNotFound {
                safe_drop_id: drop.safe_drop_id.to_string(),
            }));
        }
        Ok(())
    }

    fn find_safe_drop(&mut self, safe_drop_id: Uuid) -> Result<Option<SafeDrop>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!("SELECT {SAFE_DROP_COLUMNS} FROM safe_drops WHERE safe_drop_id = ?1"),
                params![safe_drop_id.to_string()],
                RawSafeDrop::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawSafeDrop::into_domain).transpose()
    }

    fn list_pending_safe_drops(
        &mut self,
        session_id: SessionId,
    ) -> Result<Vec<SafeDrop>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {SAFE_DROP_COLUMNS} FROM safe_drops
                 WHERE session_id = ?1 AND status = 'PENDING_APPROVAL'
                 ORDER BY requested_at ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![session_id.to_string()], RawSafeDrop::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawSafeDrop::into_domain).collect()
    }

    fn insert_close_declaration(&mut self, declaration: &CloseDeclaration) -> Result<(), AppError> {
        self.tx
            .execute(
                "INSERT INTO session_close_declarations
                     (declaration_id, session_id, closure_kind, declared_minor, expected_minor,
                      variance_minor, currency, declared_by, declared_at, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    declaration.declaration_id.to_string(),
                    declaration.session_id.to_string(),
                    declaration.closure_kind.as_str(),
                    declaration.declared_cash.map(|amount| amount.minor()),
                    declaration.expected_at_close.minor(),
                    declaration.variance.map(|amount| amount.minor()),
                    declaration.expected_at_close.currency().code(),
                    declaration.declared_by.as_ref().map(OperatorId::as_str),
                    declaration.declared_at.to_rfc3339(),
                    declaration.reason,
                ],
            )
            .map_err(|error| {
                if is_constraint_violation(&error) {
                    AppError::Cash(CashError::AlreadyDeclared)
                } else {
                    sqlite_err(error)
                }
            })?;
        Ok(())
    }

    fn find_close_declaration(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<CloseDeclaration>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!(
                    "SELECT {DECLARATION_COLUMNS} FROM session_close_declarations
                     WHERE session_id = ?1"
                ),
                params![session_id.to_string()],
                RawDeclaration::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawDeclaration::into_domain).transpose()
    }

    fn list_sessions_for_day(
        &mut self,
        business_day_id: BusinessDayId,
    ) -> Result<Vec<RegisterSession>, AppError> {
        let mut statement = self
            .tx
            .prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM register_sessions
                 WHERE business_day_id = ?1
                 ORDER BY opened_at ASC"
            ))
            .map_err(sqlite_err)?;
        let rows = statement
            .query_map(params![business_day_id.to_string()], RawSession::from_row)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        rows.into_iter().map(RawSession::into_domain).collect()
    }

    fn find_register_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<RegisterSession>, AppError> {
        let raw = self
            .tx
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM register_sessions WHERE session_id = ?1"),
                params![session_id.to_string()],
                RawSession::from_row,
            )
            .optional()
            .map_err(sqlite_err)?;
        raw.map(RawSession::into_domain).transpose()
    }
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

// ------------------------------------------------------------ row conversions

struct RawMovement {
    movement_id: String,
    business_day_id: String,
    session_id: String,
    register_id: String,
    operator_id: String,
    movement_type: String,
    direction: String,
    amount_minor: i64,
    currency: String,
    tendered_minor: Option<i64>,
    change_minor: Option<i64>,
    source_ref: Option<String>,
    reason_code: Option<String>,
    approval_id: Option<String>,
    created_at: String,
}

impl RawMovement {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            movement_id: row.get(0)?,
            business_day_id: row.get(1)?,
            session_id: row.get(2)?,
            register_id: row.get(3)?,
            operator_id: row.get(4)?,
            movement_type: row.get(5)?,
            direction: row.get(6)?,
            amount_minor: row.get(7)?,
            currency: row.get(8)?,
            tendered_minor: row.get(9)?,
            change_minor: row.get(10)?,
            source_ref: row.get(11)?,
            reason_code: row.get(12)?,
            approval_id: row.get(13)?,
            created_at: row.get(14)?,
        })
    }

    fn into_domain(self) -> Result<CashMovement, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(CashMovement::rehydrate(CashMovement {
            movement_id: parse_uuid(&self.movement_id, "movement_id")?,
            business_day_id: parse_id(&self.business_day_id)?,
            session_id: parse_id(&self.session_id)?,
            register_id: parse_id(&self.register_id)?,
            operator_id: parse_id(&self.operator_id)?,
            movement_type: MovementType::parse(&self.movement_type).map_err(AppError::Cash)?,
            direction: Direction::parse(&self.direction).map_err(AppError::Cash)?,
            amount: money(self.amount_minor, currency),
            tendered: self.tendered_minor.map(|minor| money(minor, currency)),
            change: self.change_minor.map(|minor| money(minor, currency)),
            source_ref: self.source_ref,
            reason_code: self.reason_code,
            approval_id: self
                .approval_id
                .as_deref()
                .map(|id| parse_uuid(id, "approval_id"))
                .transpose()?,
            created_at: parse_ts(&self.created_at)?,
        }))
    }
}

struct RawApproval {
    approval_id: String,
    action_type: String,
    aggregate_ref: String,
    requested_by: String,
    requested_at: String,
    approved_by: Option<String>,
    decided_at: Option<String>,
    status: String,
    reason: Option<String>,
}

impl RawApproval {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            approval_id: row.get(0)?,
            action_type: row.get(1)?,
            aggregate_ref: row.get(2)?,
            requested_by: row.get(3)?,
            requested_at: row.get(4)?,
            approved_by: row.get(5)?,
            decided_at: row.get(6)?,
            status: row.get(7)?,
            reason: row.get(8)?,
        })
    }

    fn into_domain(self) -> Result<Approval, AppError> {
        Ok(Approval::rehydrate(Approval {
            approval_id: parse_uuid(&self.approval_id, "approval_id")?,
            action_type: ApprovalAction::parse(&self.action_type).map_err(AppError::Cash)?,
            aggregate_ref: self.aggregate_ref,
            requested_by: parse_id(&self.requested_by)?,
            requested_at: parse_ts(&self.requested_at)?,
            approved_by: self.approved_by.as_deref().map(parse_id).transpose()?,
            decided_at: parse_opt_ts(self.decided_at)?,
            status: ApprovalStatus::parse(&self.status).map_err(AppError::Cash)?,
            reason: self.reason,
        }))
    }
}

struct RawSafeDrop {
    safe_drop_id: String,
    session_id: String,
    approval_id: String,
    amount_minor: i64,
    currency: String,
    reason_code: String,
    status: String,
    requested_by: String,
    requested_at: String,
    approved_by: Option<String>,
    approved_at: Option<String>,
}

impl RawSafeDrop {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            safe_drop_id: row.get(0)?,
            session_id: row.get(1)?,
            approval_id: row.get(2)?,
            amount_minor: row.get(3)?,
            currency: row.get(4)?,
            reason_code: row.get(5)?,
            status: row.get(6)?,
            requested_by: row.get(7)?,
            requested_at: row.get(8)?,
            approved_by: row.get(9)?,
            approved_at: row.get(10)?,
        })
    }

    fn into_domain(self) -> Result<SafeDrop, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(SafeDrop::rehydrate(SafeDrop {
            safe_drop_id: parse_uuid(&self.safe_drop_id, "safe_drop_id")?,
            session_id: parse_id(&self.session_id)?,
            approval_id: parse_uuid(&self.approval_id, "approval_id")?,
            amount: money(self.amount_minor, currency),
            reason_code: self.reason_code,
            status: SafeDropStatus::parse(&self.status).map_err(AppError::Cash)?,
            requested_by: parse_id(&self.requested_by)?,
            requested_at: parse_ts(&self.requested_at)?,
            approved_by: self.approved_by.as_deref().map(parse_id).transpose()?,
            approved_at: parse_opt_ts(self.approved_at)?,
        }))
    }
}

struct RawDeclaration {
    declaration_id: String,
    session_id: String,
    closure_kind: String,
    declared_minor: Option<i64>,
    expected_minor: i64,
    variance_minor: Option<i64>,
    currency: String,
    declared_by: Option<String>,
    declared_at: String,
    reason: Option<String>,
}

impl RawDeclaration {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            declaration_id: row.get(0)?,
            session_id: row.get(1)?,
            closure_kind: row.get(2)?,
            declared_minor: row.get(3)?,
            expected_minor: row.get(4)?,
            variance_minor: row.get(5)?,
            currency: row.get(6)?,
            declared_by: row.get(7)?,
            declared_at: row.get(8)?,
            reason: row.get(9)?,
        })
    }

    fn into_domain(self) -> Result<CloseDeclaration, AppError> {
        let currency = parse_currency(&self.currency)?;
        Ok(CloseDeclaration::rehydrate(CloseDeclaration {
            declaration_id: parse_uuid(&self.declaration_id, "declaration_id")?,
            session_id: parse_id(&self.session_id)?,
            closure_kind: ClosureKind::parse(&self.closure_kind).map_err(AppError::from)?,
            declared_cash: self.declared_minor.map(|minor| money(minor, currency)),
            expected_at_close: money(self.expected_minor, currency),
            variance: self.variance_minor.map(|minor| money(minor, currency)),
            declared_by: self.declared_by.as_deref().map(parse_id).transpose()?,
            declared_at: parse_ts(&self.declared_at)?,
            reason: self.reason,
        }))
    }
}

fn parse_uuid(value: &str, field: &'static str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| {
        AppError::Cash(CashError::InvalidRecord {
            field,
            reason: format!("`{value}` is not a UUID"),
        })
    })
}
