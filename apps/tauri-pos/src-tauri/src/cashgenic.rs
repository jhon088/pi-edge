//! A CashGenic note acceptor, for cash on a showroom kiosk.
//!
//! Six calls and no more: a token, what the machine is, what it is doing, take
//! this much, stop taking it, and close the session. No refill, no collect, no
//! empty, no age verification, no device administration — a demo takes notes
//! and gives change, and everything else is somebody's production problem.
//!
//! The device sits on the local network and speaks plain HTTP, so this is a
//! few lines of `std::net` rather than a dependency: one request, one
//! response, a read timeout, and `serde_json` for the body. Nothing is
//! retried. If the machine does not answer, cash is simply not available and
//! the screen says so.
//!
//! **No cardholder data passes through here, and none can**: the device
//! accepts banknotes, and what comes back is an amount in minor units.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::showroom::CashGenicSettingsV1;
use crate::BridgeError;

/// Long enough for a device on the same switch, short enough that a screen
/// never looks frozen waiting for one that is not there.
const TIMEOUT: Duration = Duration::from_secs(4);

/// What the background health check allows itself.
///
/// Much shorter, because nothing is waiting on its answer: a poll of
/// `/demo/status` reads the last result and never dials. A device that cannot
/// answer in a second on a local switch is a device that is not there.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// How often the acceptor is asked how it is.
const PROBE_EVERY: Duration = Duration::from_secs(10);

/// What the acceptor is doing, as the screen needs to show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CashGenicStatusV1 {
    pub online: bool,
    /// What the device calls itself, when it said.
    pub device: Option<String>,
    /// `IDLE`, `ACCEPTING`, `COMPLETE`, `CANCELLED`, or whatever it reports.
    pub state: String,
    /// Taken so far for the payment in progress, in minor units.
    pub deposited_minor: i64,
    /// What is left to pay. Zero once the amount is covered.
    pub outstanding_minor: i64,
    /// What it will give back. Zero until the amount is covered.
    pub change_minor: i64,
    /// What the device says it is holding, when it says.
    pub cash_total_minor: Option<i64>,
    /// Why it is not usable, in words a person on a stand can act on.
    pub detail: Option<String>,
}

impl CashGenicStatusV1 {
    fn offline(detail: impl Into<String>) -> Self {
        Self {
            online: false,
            device: None,
            state: "OFFLINE".to_owned(),
            deposited_minor: 0,
            outstanding_minor: 0,
            change_minor: 0,
            cash_total_minor: None,
            detail: Some(detail.into()),
        }
    }
}

/// The last thing the acceptor said, and when.
///
/// `/demo/status` is polled every ten seconds by a floor monitor with a three
/// second budget, and a dead acceptor used to spend four of those seconds in
/// `connect`. So the health check moved off the request path: a thread asks
/// the device how it is, and the endpoint reads what it last said. A kiosk is
/// never reported offline because its note acceptor is unplugged.
#[derive(Debug, Clone)]
pub struct CachedHealth {
    pub status: CashGenicStatusV1,
    /// When the answer was taken, RFC 3339. `None` before the first probe.
    pub last_checked_at: Option<String>,
}

impl Default for CachedHealth {
    fn default() -> Self {
        Self {
            status: CashGenicStatusV1 {
                state: "UNKNOWN".to_owned(),
                ..CashGenicStatusV1::offline("not checked yet")
            },
            last_checked_at: None,
        }
    }
}

static HEALTH: OnceLock<RwLock<CachedHealth>> = OnceLock::new();

fn health() -> &'static RwLock<CachedHealth> {
    HEALTH.get_or_init(|| RwLock::new(CachedHealth::default()))
}

/// What the acceptor last said. Never dials; never blocks.
pub fn cached_health() -> CachedHealth {
    health().read().map(|held| held.clone()).unwrap_or_default()
}

fn remember(status: CashGenicStatusV1) {
    if let Ok(mut held) = health().write() {
        *held = CachedHealth {
            status,
            last_checked_at: Some(chrono::Utc::now().to_rfc3339()),
        };
    }
}

/// Ask the acceptor how it is, every ten seconds, for the life of the kiosk.
///
/// Reads the preference each round rather than capturing it, so enabling or
/// disabling the device in Admin takes effect without a restart — and a
/// disabled acceptor is recorded as disabled rather than dialled.
pub fn watch(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        let settings = crate::showroom::data_dir(&app)
            .map(|dir| crate::showroom::stored(&dir).cashgenic)
            .unwrap_or_default();
        remember(if settings.enabled {
            probe_with(&settings, PROBE_TIMEOUT)
        } else {
            CashGenicStatusV1 {
                state: "DISABLED".to_owned(),
                ..CashGenicStatusV1::offline("not enabled on this kiosk")
            }
        });
        std::thread::sleep(PROBE_EVERY);
    });
}

/// One conversation with the device.
pub struct CashGenic<'a> {
    settings: &'a CashGenicSettingsV1,
    timeout: Duration,
}

impl<'a> CashGenic<'a> {
    pub fn new(settings: &'a CashGenicSettingsV1) -> Self {
        Self {
            settings,
            timeout: TIMEOUT,
        }
    }

    /// The same device, on a budget. For the health check, which nobody waits
    /// on and which must never hold up a status poll.
    fn impatient(settings: &'a CashGenicSettingsV1, timeout: Duration) -> Self {
        Self { settings, timeout }
    }

    /// A bearer token. Asked for on every call rather than cached: a demo
    /// makes a handful of requests a minute, and a cache that goes stale
    /// mid-payment is worse than a round trip.
    fn token(&self) -> Result<String, BridgeError> {
        let body = self.call(
            "POST",
            "/token",
            None,
            Some(json!({
                "username": self.settings.username,
                "password": self.settings.password,
            })),
        )?;
        body.get("token")
            .or_else(|| body.get("access_token"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| BridgeError {
                code: "CASHGENIC_NO_TOKEN".to_owned(),
                message: "the device accepted the credentials but returned no token".to_owned(),
            })
    }

    /// What the machine is: model, firmware, whatever it chooses to say.
    pub fn system(&self) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call("GET", "/system", Some(&token), None)
    }

    /// What it is doing right now, including any payment in progress.
    pub fn status(&self) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call("GET", "/status", Some(&token), None)
    }

    /// Start taking notes towards an amount.
    pub fn pay_amount(&self, amount_minor: i64) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call(
            "POST",
            "/PayAmount",
            Some(&token),
            Some(json!({ "amount": minor_to_major(amount_minor) })),
        )
    }

    /// Stop taking notes and give back what was taken.
    pub fn cancel_payment(&self) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call("POST", "/CancelPayment", Some(&token), Some(json!({})))
    }

    /// Finish with the device so the next sale starts clean.
    pub fn close_session(&self) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call("POST", "/CloseSession", Some(&token), Some(json!({})))
    }

    /// One HTTP request, by hand.
    fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, BridgeError> {
        let host = self.settings.host.trim();
        if host.is_empty() {
            return Err(BridgeError {
                code: "CASHGENIC_NOT_CONFIGURED".to_owned(),
                message: "no address for the note acceptor".to_owned(),
            });
        }
        let address = format!("{host}:{}", self.settings.port);
        let resolved = address
            .to_socket_addrs()
            .map_err(|error| offline(&address, error))?
            .next()
            .ok_or_else(|| BridgeError {
                code: "CASHGENIC_UNREACHABLE".to_owned(),
                message: format!("{address} resolves to nothing"),
            })?;

        let mut stream = TcpStream::connect_timeout(&resolved, self.timeout)
            .map_err(|e| offline(&address, e))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.timeout)))
            .map_err(|e| offline(&address, e))?;

        let payload = body.map(|value| value.to_string()).unwrap_or_default();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\
             Accept: application/json\r\n"
        );
        if let Some(token) = token {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        if !payload.is_empty() {
            request.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                payload.len()
            ));
        }
        request.push_str("\r\n");
        request.push_str(&payload);

        stream
            .write_all(request.as_bytes())
            .map_err(|e| offline(&address, e))?;
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|e| offline(&address, e))?;
        parse(&raw)
    }
}

fn offline(address: &str, error: impl std::fmt::Display) -> BridgeError {
    BridgeError {
        code: "CASHGENIC_UNREACHABLE".to_owned(),
        message: format!("{address}: {error}"),
    }
}

/// The JSON out of an HTTP/1.1 response, and the status line if it refused.
fn parse(raw: &[u8]) -> Result<Value, BridgeError> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .ok_or_else(|| BridgeError {
            code: "CASHGENIC_BAD_RESPONSE".to_owned(),
            message: "the device answered with no body".to_owned(),
        })?;

    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(BridgeError {
            code: "CASHGENIC_REFUSED".to_owned(),
            message: format!("the device answered {status}"),
        });
    }

    // Chunked bodies arrive with their sizes interleaved; a demo device sends
    // small JSON, so taking the first balanced object is enough and is far
    // less machinery than a full transfer decoder.
    let body = body.trim();
    let start = body.find(['{', '[']).unwrap_or(0);
    serde_json::from_str(&body[start..])
        .or_else(|_| serde_json::from_str(first_json_object(&body[start..])))
        .map_err(|error| BridgeError {
            code: "CASHGENIC_BAD_RESPONSE".to_owned(),
            message: error.to_string(),
        })
}

/// The first balanced `{...}` in a string, for a chunked body.
fn first_json_object(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (at, byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b'{' if !quoted => depth += 1,
            b'}' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return &text[..=at];
                }
            }
            _ => {}
        }
    }
    text
}

/// Minor units to the major-unit decimal the device's API speaks.
///
/// The core deals in minor units and nothing here changes that: this is the
/// one boundary where an amount leaves for a device that wants pesos, and it
/// is integer arithmetic on the way out.
fn minor_to_major(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// A number the device reported, as minor units.
///
/// Parsed as **text**, never through a float. `187.50` pesos is 18750 minor
/// units and the arithmetic to get there is two integers and a multiply by a
/// hundred; routing money through `f64` on the way in is how a cent goes
/// missing, and this runtime does not do it anywhere (CLAUDE.md).
///
/// Accepts the shapes a device of this kind sends: a decimal, an integer of
/// minor units, or either as a string. Anything else is absent rather than
/// guessed at.
pub fn amount_minor(value: Option<&Value>) -> Option<i64> {
    match value? {
        // An integer is already minor units for this API; anything with a
        // decimal point is pesos, and `to_string` renders it exactly.
        Value::Number(number) => number
            .as_i64()
            .or_else(|| parse_decimal(&number.to_string())),
        Value::String(text) => parse_decimal(text),
        _ => None,
    }
}

/// `"$1,875.00"` -> `187500`. Digits only, no floating point.
fn parse_decimal(text: &str) -> Option<i64> {
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | ' ' | '\u{a0}'))
        .collect();
    let (negative, digits) = match cleaned.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, cleaned.strip_prefix('+').unwrap_or(&cleaned)),
    };
    let (whole, fraction) = match digits.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (digits, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Two places, padded or truncated. A device that sends more precision
    // than a cent is sending something this till cannot charge.
    let mut cents = fraction.to_owned();
    cents.truncate(2);
    while cents.len() < 2 {
        cents.push('0');
    }
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let cents: i64 = cents.parse().ok()?;
    let total = whole.checked_mul(100)?.checked_add(cents)?;
    Some(if negative { -total } else { total })
}

/// Fold whatever the device reported into what the payment screen draws.
pub fn progress(status: &Value, due_minor: i64) -> CashGenicStatusV1 {
    let pick = |names: &[&str]| -> Option<i64> {
        names
            .iter()
            .find_map(|name| amount_minor(status.get(*name)))
    };
    let deposited = pick(&[
        "deposited",
        "Deposited",
        "amountPaid",
        "AmountPaid",
        "inserted",
    ])
    .unwrap_or(0);
    let reported_change = pick(&["change", "Change", "changeAmount"]);
    let state = status
        .get("state")
        .or_else(|| status.get("State"))
        .or_else(|| status.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_owned();

    // The arithmetic the customer is watching. Computed here rather than
    // trusted from the device, because the amount owed is the core's number
    // and this is only what has been put in against it.
    let outstanding = (due_minor - deposited).max(0);
    let change = reported_change.unwrap_or_else(|| (deposited - due_minor).max(0));

    CashGenicStatusV1 {
        online: true,
        device: status
            .get("device")
            .or_else(|| status.get("model"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        state,
        deposited_minor: deposited,
        outstanding_minor: outstanding,
        change_minor: change,
        cash_total_minor: pick(&["cashTotal", "CashTotal", "stored", "cashAvailable"]),
        detail: None,
    }
}

// ----------------------------------------------------------------- commands

#[tauri::command]
pub fn cashgenic_save(
    app: tauri::AppHandle,
    settings: CashGenicSettingsV1,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let dir = crate::showroom::data_dir(&app)?;
    let mut preferences = crate::showroom::stored(&dir);
    preferences.cashgenic = settings;
    crate::showroom::write(&dir, &preferences)?;
    Ok(preferences)
}

/// Ask the device who it is. The "probar conexión" button.
#[tauri::command]
pub fn cashgenic_probe(app: tauri::AppHandle) -> Result<CashGenicStatusV1, BridgeError> {
    let preferences = crate::showroom::stored(&crate::showroom::data_dir(&app)?);
    // A live call, because an operator pressed the button and is watching the
    // screen. Its answer refreshes the cache so the floor monitor sees the
    // same thing without waiting for the next round.
    let status = probe(&preferences.cashgenic);
    remember(status.clone());
    Ok(status)
}

/// What the device is, and what it holds — or why it is not there.
///
/// Never an error: "the acceptor is not answering" is a state a showroom
/// screen has to show, not a failure that stops it.
pub fn probe(settings: &CashGenicSettingsV1) -> CashGenicStatusV1 {
    probe_with(settings, TIMEOUT)
}

fn probe_with(settings: &CashGenicSettingsV1, timeout: Duration) -> CashGenicStatusV1 {
    if !settings.enabled {
        return CashGenicStatusV1 {
            state: "DISABLED".to_owned(),
            ..CashGenicStatusV1::offline("not enabled on this kiosk")
        };
    }
    let device = CashGenic::impatient(settings, timeout);
    match device.system() {
        Ok(system) => {
            let mut status = match device.status() {
                Ok(reported) => progress(&reported, 0),
                Err(error) => return CashGenicStatusV1::offline(error.message),
            };
            status.device = system
                .get("device")
                .or_else(|| system.get("model"))
                .or_else(|| system.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(status.device);
            if status.cash_total_minor.is_none() {
                status.cash_total_minor = amount_minor(system.get("cashTotal"));
            }
            status
        }
        Err(error) => CashGenicStatusV1::offline(error.message),
    }
}

#[tauri::command]
pub fn cashgenic_status(
    app: tauri::AppHandle,
    due_minor: i64,
) -> Result<CashGenicStatusV1, BridgeError> {
    let preferences = crate::showroom::stored(&crate::showroom::data_dir(&app)?);
    if !preferences.cashgenic.enabled {
        return Ok(CashGenicStatusV1 {
            state: "DISABLED".to_owned(),
            ..CashGenicStatusV1::offline("not enabled on this kiosk")
        });
    }
    match CashGenic::new(&preferences.cashgenic).status() {
        Ok(reported) => Ok(progress(&reported, due_minor)),
        Err(error) => Ok(CashGenicStatusV1::offline(error.message)),
    }
}

#[tauri::command]
pub fn cashgenic_pay(
    app: tauri::AppHandle,
    amount_minor: i64,
) -> Result<CashGenicStatusV1, BridgeError> {
    let preferences = crate::showroom::stored(&crate::showroom::data_dir(&app)?);
    let device = CashGenic::new(&preferences.cashgenic);
    device.pay_amount(amount_minor)?;
    match device.status() {
        Ok(reported) => Ok(progress(&reported, amount_minor)),
        Err(error) => Ok(CashGenicStatusV1::offline(error.message)),
    }
}

#[tauri::command]
pub fn cashgenic_cancel(app: tauri::AppHandle) -> Result<(), BridgeError> {
    let preferences = crate::showroom::stored(&crate::showroom::data_dir(&app)?);
    CashGenic::new(&preferences.cashgenic).cancel_payment()?;
    Ok(())
}

#[tauri::command]
pub fn cashgenic_close(app: tauri::AppHandle) -> Result<(), BridgeError> {
    let preferences = crate::showroom::stored(&crate::showroom::data_dir(&app)?);
    CashGenic::new(&preferences.cashgenic).close_session()?;
    Ok(())
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_leave_as_pesos_and_come_back_as_minor_units() {
        assert_eq!(minor_to_major(18_750), "187.50");
        assert_eq!(minor_to_major(20_000), "200.00");
        assert_eq!(minor_to_major(5), "0.05");
        assert_eq!(minor_to_major(0), "0.00");

        assert_eq!(amount_minor(Some(&json!(18750))), Some(18_750));
        assert_eq!(amount_minor(Some(&json!(187.5))), Some(18_750));
        assert_eq!(amount_minor(Some(&json!("187.50"))), Some(18_750));
        assert_eq!(amount_minor(Some(&json!("$1,875.00"))), Some(187_500));
        assert_eq!(amount_minor(Some(&json!("-20.05"))), Some(-2_005));
        assert_eq!(amount_minor(Some(&json!(".5"))), Some(50));
        assert_eq!(amount_minor(Some(&json!("12.349"))), Some(1_234));
        assert_eq!(amount_minor(Some(&json!(null))), None);
        assert_eq!(amount_minor(Some(&json!("not money"))), None);
        assert_eq!(amount_minor(None), None);
    }

    /// The three numbers the customer watches while they feed in notes.
    #[test]
    fn the_screen_shows_what_is_owed_what_went_in_and_what_is_left() {
        let reported = json!({ "state": "ACCEPTING", "deposited": 100.00 });
        let progress = progress(&reported, 18_750);
        assert_eq!(progress.deposited_minor, 10_000);
        assert_eq!(progress.outstanding_minor, 8_750);
        assert_eq!(progress.change_minor, 0, "nothing is owed back yet");
        assert_eq!(progress.state, "ACCEPTING");
    }

    #[test]
    fn once_it_is_covered_the_screen_shows_the_change() {
        let reported = json!({ "state": "COMPLETE", "deposited": 200.00 });
        let progress = progress(&reported, 18_750);
        assert_eq!(progress.deposited_minor, 20_000);
        assert_eq!(progress.outstanding_minor, 0);
        assert_eq!(progress.change_minor, 1_250);
    }

    #[test]
    fn the_device_may_name_the_change_itself() {
        let reported = json!({ "state": "COMPLETE", "deposited": 200.0, "change": 12.5 });
        assert_eq!(progress(&reported, 18_750).change_minor, 1_250);
    }

    #[test]
    fn a_device_that_says_nothing_useful_is_still_readable() {
        let progress = progress(&json!({}), 5_000);
        assert_eq!(progress.state, "UNKNOWN");
        assert_eq!(progress.deposited_minor, 0);
        assert_eq!(progress.outstanding_minor, 5_000);
        assert_eq!(progress.change_minor, 0);
    }

    #[test]
    fn a_disabled_acceptor_is_reported_not_dialled() {
        let status = probe(&CashGenicSettingsV1::default());
        assert!(!status.online);
        assert_eq!(status.state, "DISABLED");
    }

    #[test]
    fn a_body_is_read_out_of_an_ordinary_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"token\":\"abc\"}";
        assert_eq!(parse(raw).unwrap()["token"], "abc");
    }

    #[test]
    fn a_chunked_body_is_read_too() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n11\r\n{\"state\":\"IDLE\"}\r\n0\r\n\r\n";
        assert_eq!(parse(raw).unwrap()["state"], "IDLE");
    }

    #[test]
    fn a_refusal_is_an_error_with_its_status_in_it() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\r\n{}";
        let error = parse(raw).unwrap_err();
        assert_eq!(error.code, "CASHGENIC_REFUSED");
        assert!(error.message.contains("401"), "{}", error.message);
    }

    #[test]
    fn an_unconfigured_device_is_refused_before_a_socket_is_opened() {
        let settings = CashGenicSettingsV1 {
            enabled: true,
            ..CashGenicSettingsV1::default()
        };
        let error = CashGenic::new(&settings).system().unwrap_err();
        assert_eq!(error.code, "CASHGENIC_NOT_CONFIGURED");
    }
}
