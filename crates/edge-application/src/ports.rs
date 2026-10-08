//! Ports owned by the application layer.
//!
//! Adapters (SQLite, hardware, sync, UI transport) implement these; the
//! application never depends on an adapter crate.

use edge_domain::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

/// Time source. The domain never reads the clock itself.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// Public identifier factory (UUID).
pub trait IdFactory: Send + Sync {
    fn new_uuid(&self) -> Uuid;
}

/// Identity of this edge device, provisioned by PI.COMMERCE Server.
/// Stored locally so the core can restart without the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub tenant_id: TenantId,
    pub store_id: StoreId,
    pub currency: Currency,
}

/// A single durable unit of work. Every method runs inside one store
/// transaction; dropping the unit of work without committing rolls everything
/// back.
pub trait UnitOfWork {
    fn device_profile(&mut self) -> Result<Option<DeviceProfile>, AppError>;
    fn put_device_profile(
        &mut self,
        profile: &DeviceProfile,
        now: Timestamp,
    ) -> Result<(), AppError>;

    fn find_open_business_day(
        &mut self,
        store_id: &StoreId,
    ) -> Result<Option<BusinessDay>, AppError>;
    fn find_business_day(&mut self, id: BusinessDayId) -> Result<Option<BusinessDay>, AppError>;
    fn insert_business_day(&mut self, day: &BusinessDay, now: Timestamp) -> Result<(), AppError>;
    fn update_business_day(&mut self, day: &BusinessDay, now: Timestamp) -> Result<(), AppError>;

    /// Sessions that are not CLOSED for the given business day.
    fn find_open_sessions(
        &mut self,
        business_day_id: BusinessDayId,
    ) -> Result<Vec<RegisterSession>, AppError>;
    fn find_open_session_for_register(
        &mut self,
        register_id: &RegisterId,
    ) -> Result<Option<RegisterSession>, AppError>;
    fn insert_register_session(
        &mut self,
        session: &RegisterSession,
        now: Timestamp,
    ) -> Result<(), AppError>;
    fn update_register_session(
        &mut self,
        session: &RegisterSession,
        now: Timestamp,
    ) -> Result<(), AppError>;

    /// The non-terminal transaction of a session, if any.
    fn find_active_transaction(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<RetailTransaction>, AppError>;
    fn find_transaction(
        &mut self,
        id: TransactionId,
    ) -> Result<Option<RetailTransaction>, AppError>;
    fn next_register_sequence(&mut self, register_id: &RegisterId) -> Result<i64, AppError>;
    fn insert_transaction(
        &mut self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError>;
    fn update_transaction(
        &mut self,
        transaction: &RetailTransaction,
        now: Timestamp,
    ) -> Result<(), AppError>;

    fn append_journal(&mut self, event: &JournalEvent) -> Result<(), AppError>;
    fn list_journal(&mut self, limit: i64) -> Result<Vec<JournalEvent>, AppError>;

    fn enqueue_outbox(&mut self, event: &OutboxEvent) -> Result<(), AppError>;
    fn count_pending_outbox(&mut self) -> Result<i64, AppError>;
    fn list_pending_outbox(&mut self, limit: i64) -> Result<Vec<OutboxEvent>, AppError>;

    /// Explicit command idempotency: a replayed `request_id` returns the
    /// recorded result instead of applying the command twice.
    fn find_command_result(&mut self, request_id: Uuid) -> Result<Option<String>, AppError>;
    fn record_command_result(
        &mut self,
        request_id: Uuid,
        command_type: &str,
        result_json: &str,
        now: Timestamp,
    ) -> Result<(), AppError>;
}

/// Durable store able to run a closure inside one ACID transaction.
///
/// Intentionally generic (not object safe): there is exactly one store per
/// process and the concrete type is chosen at the composition root.
pub trait EdgeStore {
    fn transact<T, F>(&self, work: F) -> Result<T, AppError>
    where
        F: FnOnce(&mut dyn UnitOfWork) -> Result<T, AppError>;
}
