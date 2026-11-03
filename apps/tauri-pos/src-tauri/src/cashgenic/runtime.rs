//! The one place PX Kiosk talks to its CashGenic.
//!
//! A CashGenic is a session machine, not a stateless REST service (ADR 051):
//!
//! * `/status` returns "a list of events **since the last Status request**"
//!   (p. 45). Whoever reads it consumes it. So exactly one reader exists —
//!   [`Runtime::poll`], run by one thread — and every screen, the floor
//!   monitor and "Probar conexión" read the [`State`] it keeps instead.
//! * "After the session is complete a CloseSession Request must be issued …
//!   No other Sessions will be allowed until the current session is closed"
//!   (p. 15). So the session is followed here, from the events, and closed
//!   here — only once an event says it is finished, and never blind.
//! * One bearer serves "subsequent API requests" (p. 9). It is kept until the
//!   device answers `401`, and then replaced once.
//!
//! Every request, of every kind, goes through one [`Link`], which is a mutex:
//! the device never sees two requests at once. The lock is held for the
//! network exchange and the bookkeeping that belongs to it, never while a
//! screen waits for a person, and [`State`] is a separate lock so a reader of
//! the snapshot never waits on the network.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;

use super::http::{self, Timeouts};
use super::protocol::{self, Body, ResponseCode, SessionAnswer, SessionRequest, Status, System};
use super::CashGenicStatusV1;
use crate::showroom::CashGenicSettingsV1;
use crate::BridgeError;

/// How often the authority asks for events.
#[derive(Debug, Clone, Copy)]
pub(super) struct Cadence {
    /// With no session: six requests a minute.
    pub(super) idle: Duration,
    /// While a session is open: what a customer feeding notes needs to see.
    pub(super) active: Duration,
}

pub(super) const CADENCE: Cadence = Cadence {
    idle: Duration::from_secs(10),
    active: Duration::from_millis(800),
};

/// Consecutive failed exchanges before the acceptor is reported as not
/// communicating. One timeout on a showroom LAN is not an outage.
const FAILURES_BEFORE_ERROR: u32 = 3;

/// Why a request to the device did not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    Disabled,
    NotConfigured,
    /// No HTTP response: connect, handshake, read, write or framing.
    Transport(String),
    /// `/token` answered `401`.
    Credentials,
    /// An HTTP status other than 2xx.
    Http {
        status: u16,
        detail: String,
    },
    /// A 2xx this client cannot read.
    Malformed(String),
    /// The device answered and said no (`responseCode` ≠ 0). Not the network.
    Refused {
        code: ResponseCode,
        data: String,
    },
}

impl Failure {
    /// Whether this says the acceptor cannot be talked to — as opposed to a
    /// device that answered and refused.
    fn communication(&self) -> bool {
        matches!(
            self,
            Self::NotConfigured
                | Self::Transport(_)
                | Self::Credentials
                | Self::Http { .. }
                | Self::Malformed(_)
        )
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Disabled => "CashGenic is not enabled on this kiosk".to_owned(),
            Self::NotConfigured => "no address for the note acceptor".to_owned(),
            Self::Transport(detail) => detail.clone(),
            Self::Credentials => "the device refused the API username or password (401)".to_owned(),
            Self::Http { status, detail } if detail.is_empty() => {
                format!("the device answered {status}")
            }
            Self::Http { status, detail } => format!("the device answered {status}: {detail}"),
            Self::Malformed(detail) => format!("unreadable answer from the device: {detail}"),
            Self::Refused { code, data } if data.is_empty() => code.constant(),
            Self::Refused { code, data } => format!("{}: {data}", code.constant()),
        }
    }

    pub(crate) fn into_bridge(self) -> BridgeError {
        let code = match &self {
            Self::Disabled => "CASHGENIC_DISABLED",
            Self::NotConfigured => "CASHGENIC_NOT_CONFIGURED",
            Self::Transport(_) => "CASHGENIC_UNREACHABLE",
            Self::Credentials => "CASHGENIC_CREDENTIALS",
            Self::Http { .. } => "CASHGENIC_REFUSED",
            Self::Malformed(_) => "CASHGENIC_BAD_RESPONSE",
            Self::Refused { code, .. } => code.error_code(),
        };
        BridgeError {
            code: code.to_owned(),
            message: self.message(),
        }
    }
}

/// What a grant was issued for. A token is reused only against the same
/// device with the same credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl Endpoint {
    fn of(settings: &CashGenicSettingsV1) -> Self {
        Self {
            host: settings.host.trim().to_owned(),
            port: settings.port,
            username: settings.username.clone(),
            password: settings.password.clone(),
        }
    }
}

#[derive(Debug)]
struct Grant {
    token: String,
    endpoint: Endpoint,
}

/// The single line to the device. Holding a [`Wire`] is holding the line.
#[derive(Debug)]
struct Link {
    grant: Mutex<Option<Grant>>,
    timeouts: Timeouts,
}

impl Link {
    fn open(&self, settings: &CashGenicSettingsV1) -> Wire<'_> {
        Wire {
            grant: self.grant.lock().unwrap_or_else(PoisonError::into_inner),
            settings: settings.clone(),
            endpoint: Endpoint::of(settings),
            timeouts: self.timeouts,
        }
    }
}

/// Exclusive use of the device for one or more requests.
struct Wire<'a> {
    grant: MutexGuard<'a, Option<Grant>>,
    settings: CashGenicSettingsV1,
    endpoint: Endpoint,
    timeouts: Timeouts,
}

impl Wire<'_> {
    fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Body>,
    ) -> Result<http::Response, Failure> {
        let host = &self.endpoint.host;
        if host.is_empty() {
            return Err(Failure::NotConfigured);
        }
        let address = format!("{host}:{}", self.endpoint.port);
        let request = http::request_bytes(method, path, &address, token, body);
        http::exchange(host, self.endpoint.port, &request, self.timeouts)
            .map_err(|error| Failure::Transport(error.to_string()))
    }

    /// The bearer: the one held, or a new one when none is held for this
    /// device and these credentials.
    fn token(&mut self) -> Result<String, Failure> {
        if let Some(grant) = self.grant.as_ref() {
            if grant.endpoint == self.endpoint {
                return Ok(grant.token.clone());
            }
        }
        *self.grant = None;
        let response = self.send(
            "POST",
            protocol::TOKEN_PATH,
            None,
            Some(&protocol::grant(&self.settings)),
        )?;
        if response.status == 401 {
            return Err(Failure::Credentials);
        }
        let answer = json(&response)?;
        let token = protocol::access_token(&answer)
            .ok_or_else(|| Failure::Malformed("/token returned no access_token".to_owned()))?;
        tracing::info!("CASHGENIC_TOKEN obtained");
        *self.grant = Some(Grant {
            token: token.clone(),
            endpoint: self.endpoint.clone(),
        });
        Ok(token)
    }

    /// An authenticated request. A `401` drops the token, asks for a new one
    /// and sends **this** request once more — once. A transport failure is
    /// never repeated here.
    fn authorised(
        &mut self,
        method: &str,
        path: &str,
        body: Option<Body>,
    ) -> Result<Value, Failure> {
        let token = self.token()?;
        let mut response = self.send(method, path, Some(&token), body.as_ref())?;
        if response.status == 401 {
            tracing::info!("CASHGENIC_TOKEN rejected path={path}: asking for a new one, once");
            *self.grant = None;
            let token = self.token()?;
            response = self.send(method, path, Some(&token), body.as_ref())?;
        }
        json(&response)
    }

    fn system(&mut self) -> Result<System, Failure> {
        let answer = self.authorised("GET", protocol::SYSTEM_PATH, None)?;
        serde_json::from_value(answer).map_err(|error| Failure::Malformed(error.to_string()))
    }

    fn status(&mut self) -> Result<Status, Failure> {
        let answer = self.authorised("GET", protocol::STATUS_PATH, None)?;
        serde_json::from_value(answer).map_err(|error| Failure::Malformed(error.to_string()))
    }

    fn session(&mut self, request: SessionRequest) -> Result<SessionAnswer, Failure> {
        let answer = self.authorised("POST", protocol::SESSION_PATH, Some(request.body()))?;
        let answer = SessionAnswer::read(&answer).ok_or_else(|| {
            Failure::Malformed(format!(
                "{} answered without a responseCode",
                request.name()
            ))
        })?;
        let value = match request {
            SessionRequest::PayAmount(value) => value.to_string(),
            _ => "-".to_owned(),
        };
        tracing::info!(
            "CASHGENIC_SESSION request={} value={value} response_code={} data={:?}",
            request.name(),
            answer.code.constant(),
            answer.data
        );
        Ok(answer)
    }
}

/// The JSON of a 2xx, or why there is none.
fn json(response: &http::Response) -> Result<Value, Failure> {
    if !(200..300).contains(&response.status) {
        let said: String = String::from_utf8_lossy(&response.body)
            .trim()
            .chars()
            .take(200)
            .collect();
        return Err(Failure::Http {
            status: response.status,
            detail: said,
        });
    }
    serde_json::from_slice(&response.body).map_err(|error| Failure::Malformed(error.to_string()))
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// `PaymentComplete`: paid in, change given.
    Paid,
    /// `CancelComplete`, or `RefundComplete` after a cancel: given back.
    Cancelled,
    /// `StopComplete`: halted, what was paid kept.
    Stopped,
}

/// The session this kiosk started, as the events tell it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Session {
    #[default]
    None,
    Paying,
    Cancelling,
    Stopping,
    /// A terminal event arrived; `CloseSession` has not been accepted yet.
    Finished(Outcome),
}

impl Session {
    fn open(self) -> bool {
        self != Self::None
    }

    fn running(self) -> bool {
        matches!(self, Self::Paying | Self::Cancelling | Self::Stopping)
    }

    /// The session after `event`. Only a terminal event finishes it, and a
    /// finished session stays finished until it is closed.
    fn after(self, event: &str) -> Self {
        match (self, event) {
            (Self::None, _) | (Self::Finished(_), _) => self,
            (_, "PaymentComplete") => Self::Finished(Outcome::Paid),
            (_, "CancelPayment") => Self::Cancelling,
            (_, "CancelComplete") => Self::Finished(Outcome::Cancelled),
            (Self::Cancelling, "RefundComplete") => Self::Finished(Outcome::Cancelled),
            (_, "StoppingPayment") => Self::Stopping,
            (_, "StopComplete") => Self::Finished(Outcome::Stopped),
            // Could not stop: what was paid already covers the amount, so the
            // payment goes on to `PaymentComplete` and is not closed here.
            (Self::Stopping, "StopCancelled") => Self::Paying,
            _ => self,
        }
    }
}

/// The acceptor's condition, from everything the authority has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Health {
    Disabled,
    /// Enabled, and not yet answered.
    Unknown,
    Ready,
    SessionActive,
    OutOfService,
    CommunicationError,
}

impl Health {
    fn name(self) -> &'static str {
        match self {
            Self::Disabled => "DISABLED",
            Self::Unknown => "UNKNOWN",
            Self::Ready => "READY",
            Self::SessionActive => "SESSION_ACTIVE",
            Self::OutOfService => "OUT_OF_SERVICE",
            Self::CommunicationError => "COMMUNICATION_ERROR",
        }
    }

    fn online(self) -> bool {
        matches!(self, Self::Ready | Self::SessionActive)
    }
}

/// Everything known about the acceptor. Written by the requests, read by the
/// snapshot.
#[derive(Debug, Default)]
struct State {
    /// `None` until the first round reads the preference.
    enabled: Option<bool>,
    /// The device `/system` was last read from; a different one is read again.
    initialised: Option<Endpoint>,
    label: Option<String>,
    disconnected: Option<String>,
    last_event: Option<String>,
    error_condition: Option<String>,
    out_of_service: bool,
    device_in_session: bool,
    manual_payout: Option<String>,
    cash_total: Option<i64>,
    face_device_state: Option<String>,
    session: Session,
    transaction_id: String,
    cash_in: i64,
    outcome: Option<Outcome>,
    failures: u32,
    last_failure: Option<String>,
    answered: bool,
    last_checked_at: Option<String>,
    published: Option<(Health, String, Option<String>)>,
}

impl State {
    fn health(&self) -> Health {
        match self.enabled {
            None => return Health::Unknown,
            Some(false) => return Health::Disabled,
            Some(true) => {}
        }
        if self.failures >= FAILURES_BEFORE_ERROR {
            Health::CommunicationError
        } else if !self.answered {
            Health::Unknown
        } else if self.out_of_service
            || self.error_condition.is_some()
            || self.disconnected.is_some()
        {
            Health::OutOfService
        } else if self.session.open() || self.device_in_session {
            Health::SessionActive
        } else {
            Health::Ready
        }
    }

    /// What `state` says: the device's own last event while it is talking,
    /// as it always has; `OFFLINE`, `DISABLED` or `UNKNOWN` otherwise.
    fn state_name(&self, health: Health) -> String {
        match health {
            Health::Disabled => "DISABLED".to_owned(),
            Health::Unknown => "UNKNOWN".to_owned(),
            Health::CommunicationError => "OFFLINE".to_owned(),
            _ => self
                .last_event
                .clone()
                .unwrap_or_else(|| health.name().to_owned()),
        }
    }

    fn detail(&self, health: Health) -> Option<String> {
        match health {
            Health::Disabled => Some("not enabled on this kiosk".to_owned()),
            Health::Unknown => Some("not checked yet".to_owned()),
            Health::CommunicationError => self.last_failure.clone(),
            Health::OutOfService => self
                .disconnected
                .clone()
                .or_else(|| self.error_condition.clone())
                .or_else(|| Some("fuera de servicio".to_owned())),
            Health::Ready | Health::SessionActive => self.manual_payout.clone(),
        }
    }

    /// `PAYING` … `COMPLETE`: where the payment this kiosk started stands.
    fn phase(&self) -> &'static str {
        match (self.outcome, self.session) {
            (Some(Outcome::Paid), _) => "COMPLETE",
            (Some(Outcome::Cancelled), _) => "CANCELLED",
            (Some(Outcome::Stopped), _) => "STOPPED",
            (None, Session::Paying) => "PAYING",
            (None, Session::Cancelling) => "CANCELLING",
            (None, Session::Stopping) => "STOPPING",
            _ => "NONE",
        }
    }

    fn snapshot(&self, due_minor: i64) -> CashGenicStatusV1 {
        let health = self.health();
        let phase = self.phase();
        let deposited = if phase == "NONE" { 0 } else { self.cash_in };
        CashGenicStatusV1 {
            online: health.online(),
            device: self.label.clone(),
            state: self.state_name(health),
            phase: phase.to_owned(),
            deposited_minor: deposited,
            // Against the core's own total; the device only says what went in.
            outstanding_minor: (due_minor - deposited).max(0),
            change_minor: (deposited - due_minor).max(0),
            cash_total_minor: self.cash_total,
            detail: self.detail(health),
        }
    }

    fn stamp(&mut self) {
        self.last_checked_at = Some(chrono::Utc::now().to_rfc3339());
    }

    fn succeeded(&mut self) {
        self.failures = 0;
        self.last_failure = None;
        self.answered = true;
        self.stamp();
    }

    /// Only a communication failure counts against the device's health; a
    /// refusal is an answer.
    fn failed(&mut self, failure: &Failure) {
        if failure.communication() {
            self.failures = self.failures.saturating_add(1);
            self.last_failure = Some(failure.message());
            tracing::warn!(
                "CASHGENIC_FAILURE consecutive={} detail={:?}",
                self.failures,
                failure.message()
            );
            self.stamp();
        } else {
            self.succeeded();
        }
    }

    fn apply_system(&mut self, system: &System, endpoint: Endpoint) {
        self.label = system.label();
        self.disconnected = system.disconnected();
        self.initialised = Some(endpoint);
        for device in &system.payment_devices {
            tracing::info!(
                "CASHGENIC_SYSTEM version={:?} payment_type={:?} is_connected={:?} \
                 firmware={:?} serial={:?} country={:?} cashbox_value={:?} stored_value={:?}",
                system.system_version,
                device.payment_type,
                device.is_connected,
                device.firmware_version,
                device.serial_number,
                device.country_code,
                device.cashbox_value,
                device.stored_value
            );
        }
    }

    /// Fold one batch of events in, in order. A batch can be empty: nothing
    /// happened since the last read, and everything known stays known.
    fn apply_status(&mut self, status: &Status) {
        for event in &status.events {
            let transaction = event.transaction.clone().unwrap_or_default();
            tracing::info!(
                "CASHGENIC_EVENT event={} timestamp={:?} value={:?} transaction_type={:?} \
                 transaction_id={:?} request_amount={:?} cash_in={:?} cash_out={:?} \
                 error_condition={:?} low_float={:?}",
                event.event,
                event.timestamp,
                event.value,
                transaction.transaction_type,
                transaction.transaction_id,
                transaction.request_amount,
                transaction.cash_in,
                transaction.cash_out,
                event.error_condition,
                event.low_float_level_warning
            );
            self.last_event = Some(event.event.clone());
            if let Some(condition) = &event.error_condition {
                self.error_condition =
                    (!condition.eq_ignore_ascii_case("none")).then(|| condition.clone());
            }
            self.device_in_session = event.in_session();
            match event.event.as_str() {
                "SystemStartup"
                | "BillOutOfService"
                | "CoinOutOfService"
                | "BillCoinOutOfService" => self.out_of_service = true,
                // "ready for session requests" (p. 92).
                "Idle" => {
                    self.out_of_service = false;
                    self.disconnected = None;
                }
                "InsufficientFloat" | "PayoutFailed" | "RestoreTransaction" | "PayOutError" => {
                    let note = format!("{}: se requiere un pago manual", event.event);
                    tracing::warn!("CASHGENIC_MANUAL_PAYOUT event={}", event.event);
                    self.manual_payout = Some(note);
                }
                _ => {}
            }

            // Ours: the transaction PayAmount returned, or — when the event
            // carries no id — the session that is open.
            let id = event.transaction_id();
            let ours = self.session.open()
                && (id.is_empty() || self.transaction_id.is_empty() || id == self.transaction_id);
            if !ours {
                continue;
            }
            if let Some(cash_in) = transaction.cash_in {
                self.cash_in = cash_in;
            }
            let before = self.session;
            self.session = self.session.after(&event.event);
            if let Session::Finished(outcome) = self.session {
                self.outcome = Some(outcome);
            }
            if self.session != before {
                tracing::info!(
                    "CASHGENIC_SESSION_STATE from={before:?} to={:?} by={}",
                    self.session,
                    event.event
                );
            }
        }
        if let Some(total) = status
            .cash_status
            .as_ref()
            .and_then(|cash| cash.total_system_cash)
        {
            self.cash_total = Some(total);
        }
        if status.face_device_state.is_some() && status.face_device_state != self.face_device_state
        {
            tracing::info!("CASHGENIC_FACE_DEVICE state={:?}", status.face_device_state);
            self.face_device_state = status.face_device_state.clone();
        }
    }

    /// Log the health when it changes, and only then.
    fn publish(&mut self) {
        let health = self.health();
        let line = (health, self.state_name(health), self.detail(health));
        if self.published.as_ref() == Some(&line) {
            return;
        }
        tracing::info!(
            "CASHGENIC_PROBE enabled={} health={} online={} state={:?} detail={:?}",
            health != Health::Disabled,
            health.name(),
            health.online(),
            line.1,
            line.2.as_deref().unwrap_or_default()
        );
        self.published = Some(line);
    }
}

/// The CashGenic of this process.
pub(super) struct Runtime {
    settings: Box<dyn Fn() -> CashGenicSettingsV1 + Send + Sync>,
    link: Link,
    state: Mutex<State>,
    woken: Mutex<bool>,
    wake: Condvar,
    cadence: Cadence,
}

impl Runtime {
    pub(super) fn new(
        settings: impl Fn() -> CashGenicSettingsV1 + Send + Sync + 'static,
        timeouts: Timeouts,
        cadence: Cadence,
    ) -> Self {
        Self {
            settings: Box::new(settings),
            link: Link {
                grant: Mutex::new(None),
                timeouts,
            },
            state: Mutex::new(State::default()),
            woken: Mutex::new(false),
            wake: Condvar::new(),
            cadence,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run the authority for the life of the process.
    pub(super) fn start(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("cashgenic-status".to_owned())
            .spawn(move || loop {
                let open = runtime.poll();
                runtime.rest(open);
            });
        if let Err(error) = started {
            tracing::error!("CASHGENIC_RUNTIME_FAILED error={error:?}");
        }
    }

    /// Cut the authority's wait short: a session started, or the settings
    /// changed.
    pub(super) fn wake(&self) {
        *self.woken.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }

    fn rest(&self, session_open: bool) {
        let wait = if session_open {
            self.cadence.active
        } else {
            self.cadence.idle
        };
        let woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut woken, _) = self
            .wake
            .wait_timeout_while(woken, wait, |woken| !*woken)
            .unwrap_or_else(PoisonError::into_inner);
        *woken = false;
    }

    /// One round of the authority — the only caller of `/status`.
    ///
    /// Reads `/system` first when this device has not been read yet, then the
    /// events, then closes a session the events say is finished. Returns
    /// whether a session is open, which sets the pace of the next round.
    pub(super) fn poll(&self) -> bool {
        let settings = (self.settings)();
        if !settings.enabled {
            let mut state = self.state();
            state.enabled = Some(false);
            state.failures = 0;
            state.initialised = None;
            state.publish();
            return false;
        }
        self.state().enabled = Some(true);
        let mut wire = self.link.open(&settings);

        let initialised = self.state().initialised.as_ref() == Some(&wire.endpoint);
        if !initialised {
            if let Err(failure) = self.read_system(&mut wire) {
                let mut state = self.state();
                state.failed(&failure);
                state.publish();
                return state.session.open();
            }
        }

        match wire.status() {
            Ok(status) => {
                let mut state = self.state();
                state.succeeded();
                state.apply_status(&status);
            }
            Err(failure) => self.state().failed(&failure),
        }

        let finished = matches!(self.state().session, Session::Finished(_));
        if finished {
            // Logged inside; the next round tries again if it did not close.
            let _ = self.close_finished(&mut wire);
        }
        let mut state = self.state();
        state.publish();
        state.session.open()
    }

    fn read_system(&self, wire: &mut Wire<'_>) -> Result<(), Failure> {
        let system = wire.system()?;
        let mut state = self.state();
        state.apply_system(&system, wire.endpoint.clone());
        state.succeeded();
        Ok(())
    }

    /// `CloseSession` for a session an event finished. `0` closes it, and so
    /// does `13` — there is no session left to close.
    fn close_finished(&self, wire: &mut Wire<'_>) -> Result<(), Failure> {
        let answer = self.command(wire, SessionRequest::CloseSession)?;
        let mut state = self.state();
        match answer.code {
            ResponseCode::Ok | ResponseCode::SessionNotActive => {
                if answer.code == ResponseCode::SessionNotActive {
                    tracing::info!("CASHGENIC_SESSION_ALREADY_CLOSED");
                }
                state.session = Session::None;
                state.publish();
                Ok(())
            }
            code => {
                tracing::warn!("CASHGENIC_CLOSE_REFUSED code={}", code.constant());
                Err(Failure::Refused {
                    code,
                    data: answer.data,
                })
            }
        }
    }

    /// A session command, with its effect on the health recorded.
    fn command(
        &self,
        wire: &mut Wire<'_>,
        request: SessionRequest,
    ) -> Result<SessionAnswer, Failure> {
        match wire.session(request) {
            Ok(answer) => {
                self.state().succeeded();
                Ok(answer)
            }
            Err(failure) => {
                let mut state = self.state();
                state.failed(&failure);
                state.publish();
                Err(failure)
            }
        }
    }

    /// The acceptor as the screens draw it. No network.
    pub(super) fn snapshot(&self, due_minor: i64) -> CashGenicStatusV1 {
        self.state().snapshot(due_minor)
    }

    pub(super) fn last_checked_at(&self) -> Option<String> {
        self.state().last_checked_at.clone()
    }

    /// "Probar conexión": a grant if none is held, and `/system`. Never
    /// `/status` — that would take events from the authority.
    pub(super) fn probe(&self) -> CashGenicStatusV1 {
        let settings = (self.settings)();
        if !settings.enabled {
            let mut state = self.state();
            state.enabled = Some(false);
            state.publish();
            return state.snapshot(0);
        }
        let mut wire = self.link.open(&settings);
        self.state().enabled = Some(true);
        match self.read_system(&mut wire) {
            Ok(()) => {
                let mut state = self.state();
                state.publish();
                state.snapshot(0)
            }
            Err(failure) => {
                let mut state = self.state();
                state.failed(&failure);
                state.publish();
                // The operator is looking at this test, not at the health:
                // its own failure is the answer.
                CashGenicStatusV1 {
                    online: false,
                    state: "OFFLINE".to_owned(),
                    detail: Some(failure.message()),
                    ..state.snapshot(0)
                }
            }
        }
    }

    /// `PayAmount`, in minor units.
    ///
    /// `10` and `14` say another session is open. It is closed and the
    /// payment asked for once more **only** when it is this kiosk's own
    /// session and an event already finished it; a session nobody here knows
    /// the state of is reported and left alone.
    pub(super) fn pay(&self, amount_minor: i64) -> Result<CashGenicStatusV1, Failure> {
        let settings = (self.settings)();
        if !settings.enabled {
            return Err(Failure::Disabled);
        }
        let mut wire = self.link.open(&settings);
        if self.state().session.running() {
            return Err(Failure::Refused {
                code: ResponseCode::SessionActive,
                data: "a payment is already in progress on this kiosk".to_owned(),
            });
        }
        let first = self.command(&mut wire, SessionRequest::PayAmount(amount_minor))?;
        let answer = if matches!(
            first.code,
            ResponseCode::InSession | ResponseCode::SessionActive
        ) {
            let known_finished = matches!(self.state().session, Session::Finished(_));
            if !known_finished {
                let mut state = self.state();
                state.device_in_session = true;
                state.publish();
                tracing::warn!(
                    "CASHGENIC_SESSION_UNKNOWN code={}: not closed, not started",
                    first.code.constant()
                );
                return Err(Failure::Refused {
                    code: first.code,
                    data: first.data,
                });
            }
            self.close_finished(&mut wire)?;
            self.command(&mut wire, SessionRequest::PayAmount(amount_minor))?
        } else {
            first
        };
        if answer.code != ResponseCode::Ok {
            let mut state = self.state();
            if answer.code == ResponseCode::OutOfService {
                state.out_of_service = true;
            }
            state.publish();
            return Err(Failure::Refused {
                code: answer.code,
                data: answer.data,
            });
        }
        {
            let mut state = self.state();
            state.session = Session::Paying;
            state.transaction_id = answer.data;
            state.cash_in = 0;
            state.outcome = None;
            state.manual_payout = None;
            state.publish();
        }
        drop(wire);
        self.wake();
        Ok(self.snapshot(amount_minor))
    }

    /// `CancelPayment` for the payment this kiosk started. The rest — the
    /// refund, `CancelComplete`, `CloseSession` — is the authority's.
    pub(super) fn cancel(&self) -> Result<(), Failure> {
        let settings = (self.settings)();
        if !settings.enabled {
            return Err(Failure::Disabled);
        }
        let mut wire = self.link.open(&settings);
        let session = self.state().session;
        if !session.running() {
            tracing::info!("CASHGENIC_CANCEL_NOOP session={session:?}");
            return Ok(());
        }
        let answer = self.command(&mut wire, SessionRequest::CancelPayment)?;
        let mut state = self.state();
        match answer.code {
            ResponseCode::Ok => {
                state.session = Session::Cancelling;
                state.publish();
                drop(state);
                drop(wire);
                self.wake();
                Ok(())
            }
            ResponseCode::SessionNotActive => {
                state.session = Session::None;
                state.publish();
                Ok(())
            }
            code => Err(Failure::Refused {
                code,
                data: answer.data,
            }),
        }
    }

    /// Make sure no finished session is left open. A session still running
    /// is not closed: that is never done blind.
    pub(super) fn close(&self) -> Result<(), Failure> {
        let settings = (self.settings)();
        if !settings.enabled {
            return Ok(());
        }
        let mut wire = self.link.open(&settings);
        // Copied out: `close_finished` takes the state lock itself.
        let session = self.state().session;
        match session {
            Session::None => Ok(()),
            Session::Finished(_) => self.close_finished(&mut wire),
            _ => Err(Failure::Refused {
                code: ResponseCode::SessionActive,
                data: "the payment is still in progress; it is not closed blind".to_owned(),
            }),
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::cashgenic::bench::{event, Bench, Script, TRANSACTION_ID};
    use crate::logging::captured;

    fn runtime(bench: &Bench) -> Runtime {
        runtime_with(bench, http::TIMEOUTS)
    }

    fn runtime_with(bench: &Bench, timeouts: Timeouts) -> Runtime {
        let settings = bench.settings();
        Runtime::new(move || settings.clone(), timeouts, CADENCE)
    }

    fn events(batches: Vec<Vec<serde_json::Value>>) -> VecDeque<Vec<serde_json::Value>> {
        batches.into_iter().collect()
    }

    fn payment_session_requests(bench: &Bench) -> Vec<String> {
        bench
            .lines()
            .into_iter()
            .filter(|line| line.starts_with("POST /session") || line.starts_with("GET /status"))
            .collect()
    }

    // ------------------------------------------------------------ requests

    #[test]
    fn a_payment_is_a_post_to_session_with_an_integer_amount() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();

        let started = runtime.pay(12_345).unwrap();
        assert_eq!(started.phase, "PAYING");

        let pay = bench
            .seen()
            .into_iter()
            .find(|seen| seen.line() == "POST /session PayAmount")
            .expect("PayAmount went to /session");
        let body: serde_json::Value = serde_json::from_str(&pay.body).unwrap();
        assert_eq!(body, json!({ "request": "PayAmount", "value": 12345 }));
        assert!(body["value"].is_i64(), "not \"123.45\"");
        assert!(
            !bench.lines().iter().any(|line| line.contains("/PayAmount")),
            "the invented route is gone: {:?}",
            bench.lines()
        );
    }

    #[test]
    fn a_response_code_other_than_zero_is_not_a_payment() {
        let bench = Bench::start(Script::default());
        bench.script(|script| {
            script.codes.insert("PayAmount", VecDeque::from([11, 12]));
        });
        let runtime = runtime(&bench);
        runtime.poll();

        let out = runtime.pay(12_345).unwrap_err();
        assert_eq!(
            out,
            Failure::Refused {
                code: ResponseCode::OutOfService,
                data: "refused".to_owned()
            }
        );
        assert_eq!(out.into_bridge().code, "CASHGENIC_OUT_OF_SERVICE");
        let after = runtime.snapshot(12_345);
        assert_eq!(after.phase, "NONE", "no session was started");
        assert_ne!(after.state, "OFFLINE", "an answer is not the network");
        assert_eq!(runtime.state().health(), Health::OutOfService);
        assert_eq!(runtime.state().failures, 0);

        let invalid = runtime.pay(12_345).unwrap_err();
        assert_eq!(invalid.into_bridge().code, "CASHGENIC_INVALID_AMOUNT");
        assert_eq!(runtime.state().failures, 0);
    }

    // --------------------------------------------------------------- token

    #[test]
    fn one_token_serves_every_request_while_the_device_accepts_it() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);

        runtime.poll();
        let after_start = bench.lines();
        assert_eq!(
            after_start,
            vec!["POST /token", "GET /system", "GET /status?eventVersion=2"],
            "startup: one grant, the system, the events"
        );

        // A minute idle: six rounds at the idle cadence.
        assert_eq!(
            Duration::from_secs(60).as_secs() / CADENCE.idle.as_secs(),
            6
        );
        for _ in 0..6 {
            runtime.poll();
        }
        assert_eq!(bench.count("POST /token"), 1, "{:?}", bench.lines());
        assert_eq!(bench.count("GET /system"), 1);
        assert_eq!(bench.count("GET /status"), 7);

        runtime.pay(12_345).unwrap();
        runtime.poll();
        runtime.cancel().unwrap();
        assert_eq!(bench.tokens_issued(), 1, "the same bearer for the payment");
        let bearers: Vec<_> = bench
            .seen()
            .into_iter()
            .filter_map(|seen| seen.bearer)
            .collect();
        assert!(
            bearers.iter().all(|bearer| bearer == "bench-token-1"),
            "{bearers:?}"
        );
    }

    #[test]
    fn a_401_renews_the_token_and_repeats_the_request_exactly_once() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();

        bench.script(|script| script.expire_token = true);
        runtime.poll();
        assert_eq!(bench.tokens_issued(), 2);
        let tail: Vec<String> = bench.lines().into_iter().skip(3).collect();
        assert_eq!(
            tail,
            vec![
                "GET /status?eventVersion=2",
                "POST /token",
                "GET /status?eventVersion=2"
            ]
        );
        assert_eq!(runtime.state().failures, 0);

        // A device that refuses every bearer: one renewal, one repeat, then
        // the failure — not a loop.
        bench.script(|script| script.refuse_every_bearer = true);
        let before = bench.lines().len();
        runtime.poll();
        let tail: Vec<String> = bench.lines().into_iter().skip(before).collect();
        assert_eq!(
            tail,
            vec![
                "GET /status?eventVersion=2",
                "POST /token",
                "GET /status?eventVersion=2"
            ]
        );
        assert_eq!(runtime.state().failures, 1);
    }

    #[test]
    fn neither_token_nor_password_reaches_the_log() {
        let bench = Bench::start(Script::default());
        bench.script(|script| {
            script.events = events(vec![vec![event("Idle", "", 0)]]);
        });
        let runtime = runtime(&bench);
        let log = captured(|| {
            runtime.poll();
            runtime.pay(12_345).unwrap();
            runtime.probe();
        });
        assert!(log.contains("CASHGENIC_EVENT event=Idle"), "{log}");
        assert!(log.contains("CASHGENIC_SESSION request=PayAmount value=12345"));
        for secret in ["bench-token-1", "password=1234"] {
            assert!(!log.contains(secret), "{secret} leaked: {log}");
        }
    }

    // -------------------------------------------------------------- status

    #[test]
    fn only_the_authority_reads_status() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        let mut rounds = 0;

        runtime.poll();
        rounds += 1;
        for _ in 0..20 {
            runtime.snapshot(12_345);
        }
        runtime.probe();
        runtime.pay(12_345).unwrap();
        for _ in 0..20 {
            runtime.snapshot(12_345);
        }
        runtime.poll();
        rounds += 1;
        runtime.cancel().unwrap();
        runtime.close().unwrap_err();

        assert_eq!(bench.count("GET /status"), rounds, "{:?}", bench.lines());
    }

    #[test]
    fn probar_conexion_reads_system_and_never_status() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);

        let probed = runtime.probe();

        assert_eq!(bench.lines(), vec!["POST /token", "GET /system"]);
        assert!(probed.online, "{probed:?}");
        assert_eq!(probed.device.as_deref(), Some("CashGenic 2.1.49"));
    }

    #[test]
    fn system_reads_is_connected_from_payment_devices() {
        let bench = Bench::start(Script::default());
        bench.script(|script| {
            script.system = Some(json!({
                "systemVersion": "2.1.49",
                "paymentDevices": [
                    { "paymentType": "Bills", "isConnected": false },
                    { "paymentType": "Coins", "isConnected": true }
                ]
            }));
        });
        let runtime = runtime(&bench);

        let probed = runtime.probe();

        assert!(!probed.online);
        assert_eq!(
            probed.detail.as_deref(),
            Some("el módulo de billetes no está conectado")
        );
        assert_eq!(runtime.state().health(), Health::OutOfService);
    }

    #[test]
    fn requests_never_overlap_at_the_device() {
        let bench = Bench::start(Script {
            delay: Duration::from_millis(15),
            ..Script::default()
        });
        let runtime = Arc::new(runtime(&bench));
        runtime.poll();

        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let runtime = Arc::clone(&runtime);
                std::thread::spawn(move || {
                    for _ in 0..4 {
                        match worker {
                            0 => {
                                runtime.poll();
                            }
                            1 => {
                                runtime.probe();
                            }
                            2 => {
                                let _ = runtime.pay(12_345);
                                let _ = runtime.cancel();
                            }
                            _ => {
                                let _ = runtime.close();
                                runtime.snapshot(1);
                            }
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(bench.lines().len() > 12, "{:?}", bench.lines());
        assert_eq!(
            bench.most_in_flight(),
            1,
            "two requests reached the device at once"
        );
    }

    #[test]
    fn a_snapshot_never_waits_for_the_network() {
        let bench = Bench::start(Script {
            delay: Duration::from_millis(400),
            ..Script::default()
        });
        let runtime = Arc::new(runtime(&bench));
        let polling = {
            let runtime = Arc::clone(&runtime);
            std::thread::spawn(move || runtime.poll())
        };
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        runtime.snapshot(0);
        assert!(started.elapsed() < Duration::from_millis(100));
        polling.join().unwrap();
    }

    // ------------------------------------------------------------- session

    #[test]
    fn payment_complete_comes_before_close_session() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();
        bench.script(|script| {
            script.events = events(vec![
                vec![
                    event("NewPayment", TRANSACTION_ID, 0),
                    event("Waiting", TRANSACTION_ID, 0),
                ],
                vec![
                    event("PayInStarted", TRANSACTION_ID, 0),
                    event("Payment", TRANSACTION_ID, 20000),
                ],
                vec![
                    event("PayInComplete", TRANSACTION_ID, 20000),
                    event("Change", TRANSACTION_ID, 20000),
                ],
                vec![event("PaymentComplete", TRANSACTION_ID, 20000)],
                vec![event("Idle", "", 0)],
            ]);
        });
        runtime.pay(12_345).unwrap();

        runtime.poll();
        runtime.poll();
        let covered = runtime.snapshot(12_345);
        assert_eq!(covered.deposited_minor, 20_000);
        assert_eq!(covered.outstanding_minor, 0);
        assert_eq!(covered.phase, "PAYING", "covered is not complete");
        runtime.poll();
        assert_eq!(runtime.snapshot(12_345).phase, "PAYING");
        assert_eq!(
            bench.count("POST /session CloseSession"),
            0,
            "not before PaymentComplete"
        );

        runtime.poll();
        let done = runtime.snapshot(12_345);
        assert_eq!(done.phase, "COMPLETE");
        assert_eq!(done.change_minor, 7_655);
        runtime.poll();

        let order = payment_session_requests(&bench);
        let complete_at = 1 + 4; // startup status, PayAmount, then the fourth status
        assert_eq!(order[complete_at], "GET /status?eventVersion=2");
        assert_eq!(order[complete_at + 1], "POST /session CloseSession");
        assert_eq!(bench.count("POST /session CloseSession"), 1);
        assert_eq!(runtime.state().session, Session::None);
        assert_eq!(runtime.state().health(), Health::Ready);
    }

    #[test]
    fn a_cancel_is_closed_after_cancel_complete_or_refund_complete() {
        for terminal in ["CancelComplete", "RefundComplete"] {
            let bench = Bench::start(Script::default());
            let runtime = runtime(&bench);
            runtime.poll();
            runtime.pay(12_345).unwrap();
            bench.script(|script| {
                script.events = events(vec![
                    vec![event("Payment", TRANSACTION_ID, 5000)],
                    vec![event("CancelPayment", TRANSACTION_ID, 5000)],
                    vec![event("Refund", TRANSACTION_ID, 5000)],
                    vec![event(terminal, TRANSACTION_ID, 5000)],
                ]);
            });
            runtime.poll();
            runtime.cancel().unwrap();
            runtime.poll();
            runtime.poll();
            assert_eq!(bench.count("POST /session CloseSession"), 0, "{terminal}");
            assert_eq!(runtime.snapshot(12_345).phase, "CANCELLING");

            runtime.poll();
            assert_eq!(runtime.snapshot(12_345).phase, "CANCELLED", "{terminal}");
            let lines = payment_session_requests(&bench);
            let close = lines
                .iter()
                .position(|line| line == "POST /session CloseSession")
                .unwrap();
            assert_eq!(lines[close - 1], "GET /status?eventVersion=2");
            assert!(lines[..close].contains(&"POST /session CancelPayment".to_owned()));
            assert_eq!(runtime.state().session, Session::None, "{terminal}");
        }
    }

    #[test]
    fn a_stop_is_closed_after_stop_complete_and_stop_cancelled_is_not_closed() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();
        runtime.pay(12_345).unwrap();
        bench.script(|script| {
            script.events = events(vec![
                vec![event("StoppingPayment", TRANSACTION_ID, 20000)],
                vec![event("StopCancelled", TRANSACTION_ID, 20000)],
                vec![event("StoppingPayment", TRANSACTION_ID, 5000)],
                vec![event("StopComplete", TRANSACTION_ID, 5000)],
            ]);
        });
        runtime.poll();
        assert_eq!(runtime.snapshot(12_345).phase, "STOPPING");
        runtime.poll();
        assert_eq!(
            runtime.state().session,
            Session::Paying,
            "StopCancelled: still paying"
        );
        assert_eq!(bench.count("POST /session CloseSession"), 0);
        runtime.poll();
        runtime.poll();
        assert_eq!(runtime.snapshot(12_345).phase, "STOPPED");
        assert_eq!(bench.count("POST /session CloseSession"), 1);
        let lines = payment_session_requests(&bench);
        assert_eq!(
            lines.last().map(String::as_str),
            Some("POST /session CloseSession")
        );
    }

    #[test]
    fn events_of_another_transaction_do_not_move_this_session() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();
        runtime.pay(12_345).unwrap();
        bench.script(|script| {
            script.events = events(vec![vec![event("PaymentComplete", "an-older-one", 9)]]);
        });
        runtime.poll();
        assert_eq!(runtime.snapshot(12_345).phase, "PAYING");
        assert_eq!(bench.count("POST /session CloseSession"), 0);
    }

    #[test]
    fn a_session_nobody_here_knows_is_neither_closed_nor_replaced() {
        let bench = Bench::start(Script::default());
        bench.script(|script| {
            script.codes.insert("PayAmount", VecDeque::from([14]));
        });
        let runtime = runtime(&bench);
        runtime.poll();

        let refused = runtime.pay(12_345).unwrap_err();
        assert_eq!(refused.into_bridge().code, "CASHGENIC_SESSION_ACTIVE");
        assert_eq!(bench.count("POST /session CloseSession"), 0);
        assert_eq!(bench.count("POST /session PayAmount"), 1);
        assert_eq!(runtime.state().health(), Health::SessionActive);
    }

    #[test]
    fn a_finished_session_of_ours_is_closed_and_the_payment_asked_once_more() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();
        runtime.pay(12_345).unwrap();
        // PaymentComplete arrives, and the authority's close is refused.
        bench.script(|script| {
            script.events = events(vec![vec![event("PaymentComplete", TRANSACTION_ID, 12345)]]);
            script.codes.insert("CloseSession", VecDeque::from([10]));
            script.codes.insert("PayAmount", VecDeque::from([14]));
        });
        runtime.poll();
        assert_eq!(runtime.state().session, Session::Finished(Outcome::Paid));

        let next = runtime.pay(5_000).unwrap();
        assert_eq!(next.phase, "PAYING");
        let tail: Vec<String> = bench.lines().into_iter().rev().take(3).collect();
        assert_eq!(
            tail,
            vec![
                "POST /session PayAmount",
                "POST /session CloseSession",
                "POST /session PayAmount"
            ]
        );
    }

    #[test]
    fn close_is_a_no_op_without_a_session_and_refused_while_one_runs() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();
        let before = bench.lines().len();
        runtime.close().unwrap();
        assert_eq!(
            bench.lines().len(),
            before,
            "nothing to close, nothing sent"
        );

        runtime.pay(12_345).unwrap();
        let before = bench.lines().len();
        let refused = runtime.close().unwrap_err();
        assert_eq!(refused.into_bridge().code, "CASHGENIC_SESSION_ACTIVE");
        assert_eq!(
            bench.lines().len(),
            before,
            "a running session is not closed blind"
        );
    }

    #[test]
    fn a_close_answered_13_means_there_is_no_session_left() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        runtime.poll();
        runtime.pay(12_345).unwrap();
        bench.script(|script| {
            script.events = events(vec![vec![event("PaymentComplete", TRANSACTION_ID, 12345)]]);
            script.codes.insert("CloseSession", VecDeque::from([13]));
        });
        runtime.poll();
        assert_eq!(runtime.state().session, Session::None);
        assert_eq!(runtime.snapshot(12_345).phase, "COMPLETE");
    }

    // -------------------------------------------------------------- health

    #[test]
    fn one_timeout_is_not_offline_and_three_in_a_row_are() {
        let bench = Bench::start(Script::default());
        let quick = Timeouts {
            response: Duration::from_millis(300),
            ..http::TIMEOUTS
        };
        let runtime = runtime_with(&bench, quick);
        bench.script(|script| script.events = events(vec![vec![event("Idle", "", 0)]]));
        runtime.poll();
        assert_eq!(runtime.snapshot(0).state, "Idle");

        bench.script(|script| {
            script.status_delays = VecDeque::from([Duration::from_millis(600)]);
        });
        runtime.poll();
        let after_one = runtime.snapshot(0);
        assert!(after_one.online, "one timeout flipped it: {after_one:?}");
        assert_eq!(after_one.state, "Idle");
        runtime.poll();
        assert!(runtime.snapshot(0).online, "the next answer clears it");
        assert_eq!(runtime.state().failures, 0);

        bench.script(|script| {
            script.status_delays = VecDeque::from([Duration::from_millis(600); 3]);
        });
        runtime.poll();
        runtime.poll();
        assert!(runtime.snapshot(0).online);
        runtime.poll();
        let after_three = runtime.snapshot(0);
        assert!(!after_three.online);
        assert_eq!(after_three.state, "OFFLINE");
        assert!(
            after_three.detail.unwrap_or_default().contains("http_read"),
            "the stage is in the reason"
        );
    }

    #[test]
    fn out_of_service_events_are_not_offline_and_idle_clears_them() {
        let bench = Bench::start(Script::default());
        let runtime = runtime(&bench);
        bench.script(|script| {
            script.events = events(vec![
                vec![event("BillOutOfService", "", 0)],
                vec![event("Idle", "", 0)],
            ]);
        });
        runtime.poll();
        let out = runtime.snapshot(0);
        assert!(!out.online);
        assert_eq!(out.state, "BillOutOfService");
        assert_eq!(runtime.state().health(), Health::OutOfService);
        runtime.poll();
        assert_eq!(runtime.state().health(), Health::Ready);
        assert!(runtime.snapshot(0).online);
    }

    #[test]
    fn a_disabled_acceptor_is_never_dialled() {
        let bench = Bench::start(Script::default());
        let settings = CashGenicSettingsV1 {
            enabled: false,
            ..bench.settings()
        };
        let runtime = Runtime::new(move || settings.clone(), http::TIMEOUTS, CADENCE);
        runtime.poll();
        let probed = runtime.probe();
        assert_eq!(probed.state, "DISABLED");
        assert!(!probed.online);
        assert_eq!(runtime.pay(100).unwrap_err(), Failure::Disabled);
        assert!(bench.lines().is_empty(), "dialled: {:?}", bench.lines());
    }

    #[test]
    fn the_budgets_stay_generous_until_measured() {
        assert!(
            http::TIMEOUTS.handshake > Duration::from_secs(7),
            "stand handshakes took 6-7 s"
        );
        assert!(http::TIMEOUTS.connect >= Duration::from_secs(4));
        assert!(http::TIMEOUTS.response >= Duration::from_secs(4));
    }
}
