//! What every use case needs.
//!
//! Replay protection, journal construction, and the context resolution rule:
//! PI.Edge runs on one terminal, so a command acts on *the* open business day
//! of the device's store, *the* open session of that day and *the* active
//! transaction of that session. Ambiguity is an error, never a guess.
//!
//! This lives apart from the use cases because all four services need it. When
//! it sat inside `service.rs`, every other service imported from a sibling that
//! also imported from it — a cycle in everything but the compiler's eyes.

use edge_domain::prelude::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

use crate::error::AppError;
use crate::ports::{DeviceProfile, UnitOfWork};

/// Replay guard. The lookup, the work and the result record all happen inside
/// the caller's transaction, so a crash can never record a command whose
/// effects were rolled back.
pub(crate) fn idempotent<T, F>(
    uow: &mut dyn UnitOfWork,
    request_id: Option<Uuid>,
    command_type: &'static str,
    now: Timestamp,
    work: F,
) -> Result<T, AppError>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce(&mut dyn UnitOfWork) -> Result<T, AppError>,
{
    let Some(request_id) = request_id else {
        return work(uow);
    };
    if let Some(recorded) = uow.find_command_result(request_id)? {
        tracing::info!(%request_id, command_type, "replaying recorded command result");
        return serde_json::from_str(&recorded).map_err(|e| AppError::Serialization(e.to_string()));
    }
    let outcome = work(&mut *uow)?;
    let serialized =
        serde_json::to_string(&outcome).map_err(|e| AppError::Serialization(e.to_string()))?;
    uow.record_command_result(request_id, command_type, &serialized, now)?;
    Ok(outcome)
}

// -------------------------------------------------------------------- helpers
pub(crate) struct EventContext<'a> {
    pub(crate) device: &'a DeviceProfile,
    pub(crate) register_id: Option<&'a RegisterId>,
    pub(crate) business_day_id: Option<BusinessDayId>,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) transaction_id: Option<TransactionId>,
    pub(crate) operator_id: Option<&'a OperatorId>,
}

pub(crate) fn journal_event(
    event_id: EventId,
    event_type: JournalEventType,
    aggregate_id: String,
    ctx: &EventContext<'_>,
    now: Timestamp,
    payload_json: serde_json::Value,
) -> JournalEvent {
    JournalEvent {
        event_id,
        event_type,
        aggregate_id,
        tenant_id: ctx.device.tenant_id.clone(),
        store_id: ctx.device.store_id.clone(),
        register_id: ctx.register_id.cloned(),
        business_day_id: ctx.business_day_id,
        session_id: ctx.session_id,
        transaction_id: ctx.transaction_id,
        operator_id: ctx.operator_id.cloned(),
        occurred_at: now,
        payload_json,
    }
}

pub(crate) fn require_device(uow: &mut dyn UnitOfWork) -> Result<DeviceProfile, AppError> {
    uow.device_profile()?.ok_or(AppError::DeviceNotInitialized)
}

pub(crate) fn require_open_business_day(
    uow: &mut dyn UnitOfWork,
    store_id: &StoreId,
) -> Result<BusinessDay, AppError> {
    uow.find_open_business_day(store_id)?
        .ok_or(AppError::NoOpenBusinessDay)
}

pub(crate) fn require_single_open_session(
    uow: &mut dyn UnitOfWork,
    business_day_id: BusinessDayId,
) -> Result<RegisterSession, AppError> {
    let mut sessions = uow.find_open_sessions(business_day_id)?;
    match sessions.len() {
        0 => Err(AppError::NoOpenRegisterSession),
        1 => Ok(sessions.remove(0)),
        count => Err(AppError::AmbiguousOpenSession { count }),
    }
}

pub(crate) fn require_active_transaction(
    uow: &mut dyn UnitOfWork,
    session_id: SessionId,
) -> Result<RetailTransaction, AppError> {
    uow.find_active_transaction(session_id)?
        .ok_or(AppError::NoActiveTransaction)
}
