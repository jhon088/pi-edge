//! Cash management use cases.
//!
//! Four concerns are kept apart here on purpose (ADR 025): a payment says how
//! a sale was covered, the ledger says what should be in the drawer, a session
//! says who is responsible, and the day says which period it belongs to. This
//! service is the only place allowed to know all four — the same way
//! `service.rs` is the only place that knows both a catalog and a sale.

use edge_cash::{
    position, Approval, ApprovalAction, CashError, CashMovement, CloseDeclaration, Direction,
    MovementType, NewApproval, NewCashMovement, NewCloseDeclaration, NewSafeDrop, SafeDrop,
    SessionCloseRequirements,
};
use edge_domain::prelude::*;
use edge_payments::settle;
use serde_json::json;
use uuid::Uuid;

use crate::commands::{
    ApproveSafeDropCommand, CashInCommand, CashOutCommand, DeclareClosingCashCommand,
    ForceCloseBusinessDayCommand, ReopenSessionCommand, RequestSafeDropCommand,
    RequestSessionCloseCommand,
};
use crate::context::{
    idempotent, journal_event, require_device, require_open_business_day,
    require_single_open_session, EventContext,
};
use crate::error::AppError;
use crate::ports::{Clock, DeviceProfile, EdgeStore, IdFactory, UnitOfWork};
use crate::queries::{BusinessDayCloseResult, CashPositionView, SessionCloseResult};
use crate::EdgeApplication;

impl<S, C, I, P> EdgeApplication<S, C, I, P>
where
    S: EdgeStore,
    C: Clock,
    I: IdFactory,
    P: edge_payments::PaymentProviderPort,
{
    /// What the drawer should contain, and the movements that explain it.
    ///
    /// An operational read, not a cashier's pre-close read: this does expose
    /// the expected figure, which is why the blind close uses a different
    /// operation entirely (ADR 022).
    pub fn get_cash_position(&self) -> Result<CashPositionView, AppError> {
        self.store().transact(|uow| {
            let device = require_device(uow)?;
            let day = require_open_business_day(uow, &device.store_id)?;
            let session = require_single_open_session(uow, day.id())?;
            cash_position(uow, &session, device.currency)
        })
    }

    pub fn cash_in(&self, cmd: CashInCommand) -> Result<CashPositionView, AppError> {
        self.record_manual_movement(
            cmd.request_id,
            "CashIn",
            MovementType::CashIn,
            Direction::In,
            cmd.amount_minor,
            cmd.reason_code,
        )
    }

    pub fn cash_out(&self, cmd: CashOutCommand) -> Result<CashPositionView, AppError> {
        self.record_manual_movement(
            cmd.request_id,
            "CashOut",
            MovementType::CashOut,
            Direction::Out,
            cmd.amount_minor,
            cmd.reason_code,
        )
    }

    /// Ask for a safe drop. **Expected cash does not change**: the money is
    /// still in the drawer until a supervisor says it left (ADR 023).
    pub fn request_safe_drop(&self, cmd: RequestSafeDropCommand) -> Result<SafeDrop, AppError> {
        let now = self.clock.now();
        let safe_drop_id = self.ids.new_uuid();
        let approval_id = self.ids.new_uuid();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "RequestSafeDrop", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;
                let operator = session.state().operator_id.clone();

                let approval = Approval::request(NewApproval {
                    approval_id,
                    action_type: ApprovalAction::SafeDrop,
                    aggregate_ref: safe_drop_id.to_string(),
                    requested_by: operator.clone(),
                    reason: Some(cmd.reason_code.clone()),
                    now,
                });
                let drop = SafeDrop::request(NewSafeDrop {
                    safe_drop_id,
                    session_id: session.id(),
                    approval_id,
                    amount: Money::from_minor(cmd.amount_minor, device.currency),
                    reason_code: cmd.reason_code.clone(),
                    requested_by: operator,
                    now,
                })
                .map_err(AppError::Cash)?;
                uow.insert_approval(&approval)?;
                uow.insert_safe_drop(&drop)?;

                journal(
                    uow,
                    event_id,
                    JournalEventType::SafeDropRequested,
                    &device,
                    &day,
                    &session,
                    now,
                    json!({
                        "safe_drop_id": drop.safe_drop_id.to_string(),
                        "approval_id": approval.approval_id.to_string(),
                        "amount_minor": drop.amount.minor(),
                        "reason_code": drop.reason_code,
                        "requested_by": drop.requested_by.as_str(),
                        "status": drop.status.as_str(),
                    }),
                )?;
                Ok(drop)
            })
        })
    }

    /// Decide a pending drop. An approval is the only thing that moves the
    /// money, and it moves it exactly once.
    pub fn approve_safe_drop(&self, cmd: ApproveSafeDropCommand) -> Result<SafeDrop, AppError> {
        let now = self.clock.now();
        let movement_id = self.ids.new_uuid();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "ApproveSafeDrop", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;
                let mut drop = uow.find_safe_drop(cmd.safe_drop_id)?.ok_or(AppError::Cash(
                    CashError::SafeDropNotFound {
                        safe_drop_id: cmd.safe_drop_id.to_string(),
                    },
                ))?;
                let mut approval = uow.find_approval(drop.approval_id)?.ok_or(AppError::Cash(
                    CashError::SafeDropNotFound {
                        safe_drop_id: cmd.safe_drop_id.to_string(),
                    },
                ))?;

                approval
                    .decide(cmd.approver.clone(), cmd.approve, now)
                    .map_err(AppError::Cash)?;
                if cmd.approve {
                    drop.approve(cmd.approver.clone(), now)
                        .map_err(AppError::Cash)?;
                } else {
                    drop.reject(cmd.approver.clone(), now)
                        .map_err(AppError::Cash)?;
                }
                uow.update_approval(&approval)?;
                uow.update_safe_drop(&drop)?;

                if cmd.approve {
                    // The money leaves the drawer here and nowhere else. The
                    // unique index on (session, type, source_ref) makes a
                    // second approval physically unable to move it again.
                    let already = uow.find_cash_movement_by_source(
                        session.id(),
                        MovementType::SafeDrop,
                        &drop.safe_drop_id.to_string(),
                    )?;
                    if already.is_none() {
                        let movement = CashMovement::record(NewCashMovement {
                            movement_id,
                            business_day_id: day.id(),
                            session_id: session.id(),
                            register_id: session.state().register_id.clone(),
                            operator_id: cmd.approver.clone(),
                            movement_type: MovementType::SafeDrop,
                            direction: Direction::Out,
                            amount: drop.amount,
                            tendered: None,
                            change: None,
                            source_ref: Some(drop.safe_drop_id.to_string()),
                            reason_code: Some(drop.reason_code.clone()),
                            approval_id: Some(drop.approval_id),
                            now,
                        })
                        .map_err(AppError::Cash)?;
                        uow.insert_cash_movement(&movement)?;
                    }
                }

                journal(
                    uow,
                    event_id,
                    JournalEventType::SafeDropApproved,
                    &device,
                    &day,
                    &session,
                    now,
                    json!({
                        "safe_drop_id": drop.safe_drop_id.to_string(),
                        "approval_id": drop.approval_id.to_string(),
                        "amount_minor": drop.amount.minor(),
                        "approved_by": cmd.approver.as_str(),
                        "status": drop.status.as_str(),
                    }),
                )?;
                Ok(drop)
            })
        })
    }

    /// Begin a close.
    ///
    /// The answer names everything standing in the way and **never the
    /// expected cash**: the structural half of the blind close (ADR 022).
    pub fn request_session_close(
        &self,
        cmd: RequestSessionCloseCommand,
    ) -> Result<SessionCloseRequirements, AppError> {
        let now = self.clock.now();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "RequestSessionClose", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let mut session = require_single_open_session(uow, day.id())?;
                let blockers = close_blockers(uow, &session)?;

                if blockers.is_empty() && session.is_open() {
                    session.request_close()?;
                    uow.update_register_session(&session, now)?;
                    journal(
                        uow,
                        event_id,
                        JournalEventType::SessionCloseRequested,
                        &device,
                        &day,
                        &session,
                        now,
                        json!({ "requested_at": now.to_rfc3339() }),
                    )?;
                }

                let movements = uow.list_cash_movements(session.id())?;
                Ok(SessionCloseRequirements {
                    session_id: session.id(),
                    currency: device.currency,
                    movement_count: i64::try_from(movements.len()).unwrap_or(i64::MAX),
                    blockers,
                    requires_declaration: true,
                })
            })
        })
    }

    /// Declare the counted cash and close.
    ///
    /// This is the first moment the expected figure is computed for the
    /// cashier's path, and it is computed *after* the declaration is part of
    /// the same durable write.
    pub fn declare_closing_cash(
        &self,
        cmd: DeclareClosingCashCommand,
    ) -> Result<SessionCloseResult, AppError> {
        let now = self.clock.now();
        let declaration_id = self.ids.new_uuid();
        let declared_event = EventId::from_uuid(self.ids.new_uuid());
        let closed_event = EventId::from_uuid(self.ids.new_uuid());
        let outbox_event = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "DeclareClosingCash", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let mut session = require_single_open_session(uow, day.id())?;

                // A declaration is a reply to a request. Refusing it otherwise
                // is what stops a client from closing in one step and
                // side-stepping the blind-close ordering.
                if !session.is_close_requested() {
                    return Err(AppError::Cash(CashError::DeclarationRequired));
                }
                if uow.find_close_declaration(session.id())?.is_some() {
                    return Err(AppError::Cash(CashError::AlreadyDeclared));
                }
                let blockers = close_blockers(uow, &session)?;
                if !blockers.is_empty() {
                    return Err(AppError::SessionCloseBlocked { blockers });
                }

                let movements = uow.list_cash_movements(session.id())?;
                let position = position(&movements, device.currency).map_err(AppError::Cash)?;
                let declaration = CloseDeclaration::declared(
                    NewCloseDeclaration {
                        declaration_id,
                        session_id: session.id(),
                        expected_at_close: position.expected_cash,
                        now,
                    },
                    Money::from_minor(cmd.counted_minor, device.currency),
                    session.state().operator_id.clone(),
                )
                .map_err(AppError::Cash)?;
                uow.insert_close_declaration(&declaration)?;

                session.close(ClosureKind::Declared, declaration_id, now)?;
                uow.update_register_session(&session, now)?;

                journal(
                    uow,
                    declared_event,
                    JournalEventType::SessionCashDeclared,
                    &device,
                    &day,
                    &session,
                    now,
                    json!({
                        "declaration_id": declaration_id.to_string(),
                        "declared_minor": cmd.counted_minor,
                        "expected_minor": declaration.expected_at_close.minor(),
                        "variance_minor": declaration.variance.map(|v| v.minor()),
                        "declared_by": session.state().operator_id.as_str(),
                    }),
                )?;
                journal(
                    uow,
                    closed_event,
                    JournalEventType::SessionClosed,
                    &device,
                    &day,
                    &session,
                    now,
                    json!({
                        "closed_at": now.to_rfc3339(),
                        "closure_kind": ClosureKind::Declared.as_str(),
                        "declaration_id": declaration_id.to_string(),
                        "variance_minor": declaration.variance.map(|v| v.minor()),
                    }),
                )?;
                enqueue_session_closed(uow, outbox_event, &device, &day, &session, now)?;

                // Scoped to **this session**: closing one till must not report
                // another till's unresolved receipts, even on the same day
                // (ADR 043).
                let receipt_attention =
                    uow.receipt_attention(ReceiptAttentionScope::RegisterSession(session.id()))?;
                Ok(SessionCloseResult {
                    session_id: session.id(),
                    closure_kind: ClosureKind::Declared,
                    declared_cash: declaration.declared_cash,
                    expected_at_close: declaration.expected_at_close,
                    variance: declaration.variance,
                    variance_label: declaration.variance_label().to_owned(),
                    receipt_attention,
                })
            })
        })
    }

    /// Continue a closed session under a new one (ADR 023).
    pub fn reopen_session(&self, cmd: ReopenSessionCommand) -> Result<RegisterSession, AppError> {
        let now = self.clock.now();
        let session_id = SessionId::from_uuid(self.ids.new_uuid());
        let movement_id = self.ids.new_uuid();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            idempotent(uow, cmd.request_id, "ReopenSession", now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let reason = cmd.reason.trim().to_owned();
                if reason.is_empty() {
                    return Err(AppError::Domain(DomainError::MissingReason {
                        field: "reason",
                    }));
                }
                let previous = uow
                    .find_register_session(cmd.session_id)?
                    .ok_or(AppError::NoOpenRegisterSession)?;
                if previous.status() != RegisterSessionStatus::Closed {
                    return Err(AppError::Domain(DomainError::InvalidTransition {
                        aggregate: "RegisterSession",
                        from: previous.status().as_str(),
                        attempted: "REOPENED",
                    }));
                }
                // One live session per register at a time.
                let live = uow.find_open_sessions(day.id())?;
                if let Some(open) = live
                    .iter()
                    .find(|s| s.state().register_id == previous.state().register_id)
                {
                    return Err(AppError::RegisterSessionAlreadyOpen {
                        register_id: open.state().register_id.to_string(),
                        session_id: open.id().to_string(),
                    });
                }

                // The new float is what the operator *counted*, not what the
                // ledger expected: the declaration is the best statement of
                // the money physically there (ADR 023). A forced close
                // declared nothing, so it starts from zero and the difference
                // stays visible as the previous session's unknown variance.
                let opening = uow
                    .find_close_declaration(previous.id())?
                    .and_then(|declaration| declaration.declared_cash)
                    .unwrap_or(Money::zero(device.currency));

                let session = RegisterSession::open(NewRegisterSession {
                    session_id,
                    business_day_id: day.id(),
                    register_id: previous.state().register_id.clone(),
                    operator_id: cmd.operator_id.clone(),
                    opening_cash: opening,
                    opened_at: now,
                    reopened_from_session_id: Some(previous.id()),
                })?;
                uow.insert_register_session(&session, now)?;
                if opening.is_positive() {
                    let movement = CashMovement::record(NewCashMovement {
                        movement_id,
                        business_day_id: day.id(),
                        session_id,
                        register_id: session.state().register_id.clone(),
                        operator_id: cmd.operator_id.clone(),
                        movement_type: MovementType::OpeningFloat,
                        direction: Direction::In,
                        amount: opening,
                        tendered: None,
                        change: None,
                        source_ref: None,
                        reason_code: None,
                        approval_id: None,
                        now,
                    })
                    .map_err(AppError::Cash)?;
                    uow.insert_cash_movement(&movement)?;
                }

                journal(
                    uow,
                    event_id,
                    JournalEventType::SessionReopened,
                    &device,
                    &day,
                    &session,
                    now,
                    json!({
                        "session_id": session_id.to_string(),
                        "reopened_from_session_id": previous.id().to_string(),
                        "reason": reason,
                        "operator_id": cmd.operator_id.as_str(),
                        "opening_cash_minor": opening.minor(),
                    }),
                )?;
                Ok(session)
            })
        })
    }

    /// End the day over open sessions, under an explicit policy (ADR 024).
    ///
    /// Nothing is destroyed and no count is invented: every session still
    /// running is closed as `FORCED` with no declared cash, and its expected
    /// figure is recorded so the unknown variance is auditable.
    /// Answers the same shape as the ordinary close: a day closed over open
    /// sessions is still a day close, and it should report what needs
    /// somebody just as loudly (ADR 043).
    pub fn force_close_business_day(
        &self,
        cmd: ForceCloseBusinessDayCommand,
    ) -> Result<BusinessDayCloseResult, AppError> {
        let now = self.clock.now();
        let day_event = EventId::from_uuid(self.ids.new_uuid());
        let outbox_event = EventId::from_uuid(self.ids.new_uuid());
        // Ids are minted on demand inside the transaction. How many sessions a
        // store has open is the store's business, not a constant of this
        // module, so the loop below asks for an id when it needs one.
        let ids = &self.ids;

        self.store().transact(|uow| {
            idempotent(uow, cmd.request_id, "ForceCloseBusinessDay", now, |uow| {
                let device = require_device(uow)?;
                let mut day = require_open_business_day(uow, &device.store_id)?;
                let reason = cmd.reason.trim().to_owned();
                let actor = cmd.actor.trim().to_owned();

                let open_sessions = uow.find_open_sessions(day.id())?;
                let mut forced = Vec::with_capacity(open_sessions.len());
                for mut session in open_sessions {
                    let declaration_id = ids.new_uuid();
                    let movements = uow.list_cash_movements(session.id())?;
                    let position = position(&movements, device.currency).map_err(AppError::Cash)?;
                    // No counted cash is fabricated: declared stays absent and
                    // the variance stays unknown rather than zero.
                    let declaration = CloseDeclaration::forced(
                        NewCloseDeclaration {
                            declaration_id,
                            session_id: session.id(),
                            expected_at_close: position.expected_cash,
                            now,
                        },
                        reason.clone(),
                    )
                    .map_err(AppError::Cash)?;
                    uow.insert_close_declaration(&declaration)?;
                    session.close(ClosureKind::Forced, declaration_id, now)?;
                    uow.update_register_session(&session, now)?;

                    journal(
                        uow,
                        EventId::from_uuid(ids.new_uuid()),
                        JournalEventType::SessionClosed,
                        &device,
                        &day,
                        &session,
                        now,
                        json!({
                            "closed_at": now.to_rfc3339(),
                            "closure_kind": ClosureKind::Forced.as_str(),
                            "forced": true,
                            "reason": reason,
                            "actor": actor,
                            "declaration_id": declaration_id.to_string(),
                            "expected_minor": declaration.expected_at_close.minor(),
                            "declared_minor": Option::<i64>::None,
                            "variance_minor": Option::<i64>::None,
                        }),
                    )?;
                    enqueue_session_closed(
                        uow,
                        EventId::from_uuid(ids.new_uuid()),
                        &device,
                        &day,
                        &session,
                        now,
                    )?;
                    forced.push(session.id().to_string());
                }

                day.force_close(reason.clone(), actor.clone(), now)?;
                uow.update_business_day(&day, now)?;

                let ctx = EventContext {
                    device: &device,
                    register_id: None,
                    business_day_id: Some(day.id()),
                    session_id: None,
                    transaction_id: None,
                    operator_id: None,
                };
                uow.append_journal(&journal_event(
                    day_event,
                    JournalEventType::BusinessDayForceClosed,
                    day.id().to_string(),
                    &ctx,
                    now,
                    json!({
                        "closed_at": now.to_rfc3339(),
                        "reason": reason,
                        "actor": actor,
                        "forced_sessions": forced,
                    }),
                ))?;
                let payload = serde_json::to_value(day.state())
                    .map_err(|e| AppError::Serialization(e.to_string()))?;
                uow.enqueue_outbox(&OutboxEvent::pending(
                    outbox_event,
                    OutboxEventType::BusinessDayClosed,
                    day.id().to_string(),
                    day.state().version,
                    device.tenant_id.clone(),
                    device.store_id.clone(),
                    None,
                    Some(day.id()),
                    now,
                    payload,
                ))?;
                // A forced close ends the whole day, so it reports the whole
                // day (ADR 043).
                let receipt_attention =
                    uow.receipt_attention(ReceiptAttentionScope::BusinessDay(day.id()))?;
                Ok(BusinessDayCloseResult {
                    day,
                    receipt_attention,
                })
            })
        })
    }

    // ----------------------------------------------------------- internals

    fn record_manual_movement(
        &self,
        request_id: Option<Uuid>,
        command_type: &'static str,
        movement_type: MovementType,
        direction: Direction,
        amount_minor: i64,
        reason_code: String,
    ) -> Result<CashPositionView, AppError> {
        let now = self.clock.now();
        let movement_id = self.ids.new_uuid();
        let event_id = EventId::from_uuid(self.ids.new_uuid());

        self.store().transact(move |uow| {
            idempotent(uow, request_id, command_type, now, |uow| {
                let device = require_device(uow)?;
                let day = require_open_business_day(uow, &device.store_id)?;
                let session = require_single_open_session(uow, day.id())?;
                let movement = CashMovement::record(NewCashMovement {
                    movement_id,
                    business_day_id: day.id(),
                    session_id: session.id(),
                    register_id: session.state().register_id.clone(),
                    operator_id: session.state().operator_id.clone(),
                    movement_type,
                    direction,
                    amount: Money::from_minor(amount_minor, device.currency),
                    tendered: None,
                    change: None,
                    source_ref: None,
                    reason_code: Some(reason_code),
                    approval_id: None,
                    now,
                })
                .map_err(AppError::Cash)?;
                uow.insert_cash_movement(&movement)?;
                journal(
                    uow,
                    event_id,
                    JournalEventType::CashMovementRecorded,
                    &device,
                    &day,
                    &session,
                    now,
                    json!({
                        "movement_id": movement.movement_id.to_string(),
                        "movement_type": movement.movement_type.as_str(),
                        "direction": movement.direction.as_str(),
                        "amount_minor": movement.amount.minor(),
                        "reason_code": movement.reason_code,
                        "operator_id": movement.operator_id.as_str(),
                    }),
                )?;
                cash_position(uow, &session, device.currency)
            })
        })
    }
}

/// Write the session's opening float as the ledger's first movement.
///
/// Called when a session opens, so the ledger alone explains the drawer rather
/// than the ledger plus a column somewhere else (ADR 021).
pub(crate) fn record_opening_float(
    uow: &mut dyn UnitOfWork,
    movement_id: Uuid,
    day: &BusinessDay,
    session: &RegisterSession,
    now: Timestamp,
) -> Result<(), AppError> {
    let opening = session.state().opening_cash;
    if !opening.is_positive() {
        // A session may legitimately open with an empty drawer. A zero
        // movement would be a row that says nothing.
        return Ok(());
    }
    let movement = CashMovement::record(NewCashMovement {
        movement_id,
        business_day_id: day.id(),
        session_id: session.id(),
        register_id: session.state().register_id.clone(),
        operator_id: session.state().operator_id.clone(),
        movement_type: MovementType::OpeningFloat,
        direction: Direction::In,
        amount: opening,
        tendered: None,
        change: None,
        source_ref: None,
        reason_code: None,
        approval_id: None,
        now,
    })
    .map_err(AppError::Cash)?;
    uow.insert_cash_movement(&movement)
}

/// What a committed cash payment did at the counter.
///
/// Grouped because the three amounts only mean anything together: the drawer
/// keeps `committed`, the customer handed over `tendered`, and `change` is the
/// difference (ADR 021).
pub(crate) struct CashSale {
    pub payment_id: PaymentId,
    pub tendered: Money,
    pub committed: Money,
    pub change: Money,
}

/// Write the `CASH_SALE` movement for a committed cash payment.
///
/// `source_ref` is the payment id, so a replayed `StartPayment` cannot move the
/// drawer twice. The amount is the net that stayed in it, with the tender and
/// the change kept alongside for audit (ADR 021).
pub(crate) fn record_cash_sale(
    uow: &mut dyn UnitOfWork,
    movement_id: Uuid,
    day_id: BusinessDayId,
    session: &RegisterSession,
    sale: CashSale,
    now: Timestamp,
) -> Result<(), AppError> {
    let CashSale {
        payment_id,
        tendered,
        committed,
        change,
    } = sale;
    let source_ref = payment_id.to_string();
    if uow
        .find_cash_movement_by_source(session.id(), MovementType::CashSale, &source_ref)?
        .is_some()
    {
        return Ok(());
    }
    let movement = CashMovement::record(NewCashMovement {
        movement_id,
        business_day_id: day_id,
        session_id: session.id(),
        register_id: session.state().register_id.clone(),
        operator_id: session.state().operator_id.clone(),
        movement_type: MovementType::CashSale,
        direction: Direction::In,
        amount: committed,
        tendered: Some(tendered),
        change: Some(change),
        source_ref: Some(source_ref),
        reason_code: None,
        approval_id: None,
        now,
    })
    .map_err(AppError::Cash)?;
    uow.insert_cash_movement(&movement)
}

fn cash_position(
    uow: &mut dyn UnitOfWork,
    session: &RegisterSession,
    currency: Currency,
) -> Result<CashPositionView, AppError> {
    let movements = uow.list_cash_movements(session.id())?;
    let position = position(&movements, currency).map_err(AppError::Cash)?;
    let pending_safe_drops = uow.list_pending_safe_drops(session.id())?;
    Ok(CashPositionView {
        session_id: session.id(),
        register_id: session.state().register_id.clone(),
        operator_id: session.state().operator_id.clone(),
        session_status: session.status(),
        position,
        movements,
        pending_safe_drops,
    })
}

/// Everything standing between a session and a normal close.
///
/// Reported together so an operator sees the whole list instead of discovering
/// them one refusal at a time.
pub(crate) fn close_blockers(
    uow: &mut dyn UnitOfWork,
    session: &RegisterSession,
) -> Result<Vec<String>, AppError> {
    let mut blockers = Vec::new();
    if let Some(active) = uow.find_active_transaction(session.id())? {
        blockers.push(format!("ACTIVE_TRANSACTION:{}", active.id()));
        // An active sale may also hold unresolved payments; naming the sale is
        // the actionable fact.
        let views = uow.list_transaction_payments(active.id())?;
        let settlement =
            settle(&views, active.state().totals.grand_total).map_err(AppError::Payment)?;
        if settlement.requires_resolution {
            blockers.push("UNRESOLVED_PAYMENT".to_owned());
        }
    }
    for drop in uow.list_pending_safe_drops(session.id())? {
        blockers.push(format!("PENDING_SAFE_DROP:{}", drop.safe_drop_id));
    }
    Ok(blockers)
}

#[allow(clippy::too_many_arguments)]
fn journal(
    uow: &mut dyn UnitOfWork,
    event_id: EventId,
    event_type: JournalEventType,
    device: &DeviceProfile,
    day: &BusinessDay,
    session: &RegisterSession,
    now: Timestamp,
    payload: serde_json::Value,
) -> Result<(), AppError> {
    let ctx = EventContext {
        device,
        register_id: Some(&session.state().register_id),
        business_day_id: Some(day.id()),
        session_id: Some(session.id()),
        transaction_id: None,
        operator_id: Some(&session.state().operator_id),
    };
    uow.append_journal(&journal_event(
        event_id,
        event_type,
        session.id().to_string(),
        &ctx,
        now,
        payload,
    ))
}

fn enqueue_session_closed(
    uow: &mut dyn UnitOfWork,
    event_id: EventId,
    device: &DeviceProfile,
    day: &BusinessDay,
    session: &RegisterSession,
    now: Timestamp,
) -> Result<(), AppError> {
    let payload = serde_json::to_value(session.state())
        .map_err(|e| AppError::Serialization(e.to_string()))?;
    uow.enqueue_outbox(&OutboxEvent::pending(
        event_id,
        OutboxEventType::RegisterSessionClosed,
        session.id().to_string(),
        session.state().version,
        device.tenant_id.clone(),
        device.store_id.clone(),
        Some(session.state().register_id.clone()),
        Some(day.id()),
        now,
        payload,
    ))
}
