//! A CashGenic that answers as *CashGenic API Specification v1* says, over
//! TLS with a certificate it signed itself — for tests only.
//!
//! The first bench answered the protocol this client had invented — `POST
//! /PayAmount`, a root `state`, a decimal `deposited` — so every test agreed
//! with the bug. This one only speaks the document: an unknown route is a
//! `404` (p. 6), a session command is a `POST /session` whose body names the
//! request, an amount that is not an integer is `REQUEST_FAIL_INVALID_AMOUNT`,
//! a bearer it did not issue is a `401`, and `/status` hands out each scripted
//! batch of events exactly once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::showroom::CashGenicSettingsV1;

const BENCH_CERT: &[u8] = include_bytes!("../../tests/fixtures/bench-cert.der");
const BENCH_KEY: &[u8] = include_bytes!("../../tests/fixtures/bench-key.der");

pub(crate) const USERNAME: &str = "ApiUserOne";
pub(crate) const PASSWORD: &str = "1234";
/// p. 17's example transaction id.
pub(crate) const TRANSACTION_ID: &str = "d3ac57bb-fa6d-467c-aef4-913e85c1defa";

/// What the bench will do. Everything not set answers as the document's
/// examples do.
#[derive(Debug, Default)]
pub(crate) struct Script {
    /// Leave the connection open after a complete answer, until the client
    /// closes it.
    pub(crate) hold_open: bool,
    /// How long every answer takes.
    pub(crate) delay: Duration,
    /// Extra delay for each `/status`, in order.
    pub(crate) status_delays: VecDeque<Duration>,
    /// The `events` of each `/status`, in order; an empty array once spent.
    pub(crate) events: VecDeque<Vec<Value>>,
    /// `responseCode`s to answer per request name, in order; `0` once spent.
    pub(crate) codes: HashMap<&'static str, VecDeque<i64>>,
    /// Replaces the document's `/system` example.
    pub(crate) system: Option<Value>,
    /// Refuse the next authenticated request with a `401` and forget the
    /// token, as a device whose grant has lapsed would.
    pub(crate) expire_token: bool,
    /// Refuse every bearer, always.
    pub(crate) refuse_every_bearer: bool,
}

/// One request, as it arrived.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) bearer: Option<String>,
    pub(crate) body: String,
}

impl Seen {
    /// `POST /session PayAmount`, `GET /system` — what a test compares.
    pub(crate) fn line(&self) -> String {
        let request = serde_json::from_str::<Value>(&self.body)
            .ok()
            .and_then(|body| {
                body.get("request")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        match request {
            Some(request) => format!("{} {} {request}", self.method, self.path),
            None => format!("{} {}", self.method, self.path),
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    script: Mutex<Script>,
    seen: Mutex<Vec<Seen>>,
    valid: Mutex<Option<String>>,
    issued: AtomicUsize,
    in_flight: AtomicUsize,
    most_in_flight: AtomicUsize,
}

/// A running bench. Lives until the test process ends.
pub(crate) struct Bench {
    address: SocketAddr,
    shared: Arc<Shared>,
}

impl Bench {
    pub(crate) fn start(script: Script) -> Self {
        let shared = Arc::new(Shared {
            script: Mutex::new(script),
            ..Shared::default()
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let config = server_config();
        let serving = Arc::clone(&shared);
        std::thread::spawn(move || {
            for socket in listener.incoming().flatten() {
                let shared = Arc::clone(&serving);
                let config = Arc::clone(&config);
                // One thread per connection, so that two requests the client
                // failed to serialise really would overlap here.
                std::thread::spawn(move || serve(socket, config, &shared));
            }
        });
        Self { address, shared }
    }

    pub(crate) fn host(&self) -> String {
        self.address.ip().to_string()
    }

    pub(crate) fn port(&self) -> u16 {
        self.address.port()
    }

    pub(crate) fn address(&self) -> String {
        self.address.to_string()
    }

    pub(crate) fn settings(&self) -> CashGenicSettingsV1 {
        CashGenicSettingsV1 {
            enabled: true,
            host: self.host(),
            port: self.port(),
            username: USERNAME.to_owned(),
            password: PASSWORD.to_owned(),
        }
    }

    pub(crate) fn script(&self, change: impl FnOnce(&mut Script)) {
        change(&mut self.shared.script.lock().unwrap());
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.shared.seen.lock().unwrap().clone()
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        self.seen().iter().map(Seen::line).collect()
    }

    /// How many requests began with `prefix`, e.g. `"POST /token"`.
    pub(crate) fn count(&self, prefix: &str) -> usize {
        self.lines()
            .iter()
            .filter(|line| line.starts_with(prefix))
            .count()
    }

    pub(crate) fn tokens_issued(&self) -> usize {
        self.shared.issued.load(Ordering::SeqCst)
    }

    /// The most requests that were ever being answered at the same time.
    pub(crate) fn most_in_flight(&self) -> usize {
        self.shared.most_in_flight.load(Ordering::SeqCst)
    }
}

/// An event as p. 46 shapes it.
pub(crate) fn event(name: &str, transaction_id: &str, cash_in: i64) -> Value {
    let kind = if transaction_id.is_empty() {
        ""
    } else {
        "Payment"
    };
    json!({
        "errorCondition": "None",
        "event": name,
        "lowFloatLevelWarning": false,
        "timestamp": "2026-11-03 12:00:00",
        "transaction": {
            "cash_in": cash_in,
            "cash_out": 0,
            "request_amount": 12345,
            "transaction_id": transaction_id,
            "transaction_type": kind
        },
        "value": cash_in
    })
}

/// The document's `/system` example (p. 11), completed with the fields of
/// p. 12.
fn spec_system() -> Value {
    json!({
        "systemVersion": "2.1.49",
        "faceDeviceCapable": false,
        "lowFloatWarnPercent": 0,
        "maxPayoutLevel": 20000,
        "paymentDevices": [
            {
                "paymentType": "Bills", "isConnected": true, "firmwareVersion": "4.10",
                "serialNumber": "BNV-0001", "datasetVersion": "MXN01", "countryCode": "MXN",
                "cashboxValue": 50000, "storedValue": 75050,
                "denominations": [], "cashboxDenominations": []
            },
            {
                "paymentType": "Coins", "isConnected": true, "firmwareVersion": "2.03",
                "serialNumber": "CNV-0002", "datasetVersion": "MXN01", "countryCode": "MXN",
                "cashboxValue": 0, "storedValue": 0,
                "denominations": [], "cashboxDenominations": []
            }
        ]
    })
}

fn server_config() -> Arc<rustls::ServerConfig> {
    let certificate = rustls_pki_types::CertificateDer::from(BENCH_CERT.to_vec());
    let key = rustls_pki_types::PrivateKeyDer::Pkcs8(rustls_pki_types::PrivatePkcs8KeyDer::from(
        BENCH_KEY.to_vec(),
    ));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap(),
    )
}

fn serve(socket: TcpStream, config: Arc<rustls::ServerConfig>, shared: &Shared) {
    let Ok(connection) = rustls::ServerConnection::new(config) else {
        return;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
    let mut stream = rustls::StreamOwned::new(connection, socket);
    let Some(seen) = read_request(&mut stream) else {
        return;
    };

    let now = shared.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    shared.most_in_flight.fetch_max(now, Ordering::SeqCst);
    shared.seen.lock().unwrap().push(seen.clone());
    let (status, body) = answer(&seen, shared);
    let hold_open = shared.script.lock().unwrap().hold_open;
    // Released before the answer is written: a client that waits for the
    // answer can never be counted as overlapping with itself.
    shared.in_flight.fetch_sub(1, Ordering::SeqCst);

    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        _ => "Not Found",
    };
    let connection = if hold_open { "keep-alive" } else { "close" };
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.flush();
    if hold_open {
        // Until the client hangs up: a keep-alive device.
        let mut sink = [0_u8; 256];
        while matches!(stream.read(&mut sink), Ok(read) if read > 0) {}
    } else {
        stream.conn.send_close_notify();
        let _ = stream.flush();
    }
}

fn read_request(stream: &mut impl Read) -> Option<Seen> {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        if let Some(at) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < head_end + length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..read]);
    }
    let mut first = head.lines().next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let bearer = head
        .lines()
        .find_map(|line| line.strip_prefix("Authorization: Bearer "))
        .map(|token| token.trim().to_owned());
    // The grant's content type travels with its body, so a test can tell a
    // form from JSON.
    let content_type = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Type: "))
        .unwrap_or_default();
    let body = String::from_utf8_lossy(&data[head_end..head_end + length]).into_owned();
    let body = if path == "/token" {
        format!("{content_type}\n{body}")
    } else {
        body
    };
    Some(Seen {
        method,
        path,
        bearer,
        body,
    })
}

fn answer(seen: &Seen, shared: &Shared) -> (u16, String) {
    let delay = shared.script.lock().unwrap().delay;
    std::thread::sleep(delay);

    if seen.method == "POST" && seen.path == "/token" {
        return grant(seen, shared);
    }
    let known = matches!(
        (seen.method.as_str(), seen.path.as_str()),
        ("GET", "/system") | ("GET", "/status?eventVersion=2") | ("POST", "/session")
    );
    if !known {
        return (404, String::new());
    }

    {
        let mut script = shared.script.lock().unwrap();
        let mut valid = shared.valid.lock().unwrap();
        let accepted = !script.refuse_every_bearer
            && valid.is_some()
            && seen.bearer.as_deref() == valid.as_deref();
        if !accepted || script.expire_token {
            if accepted {
                script.expire_token = false;
                *valid = None;
            }
            return (401, String::new());
        }
    }

    match seen.path.as_str() {
        "/system" => {
            let system = shared.script.lock().unwrap().system.clone();
            (200, system.unwrap_or_else(spec_system).to_string())
        }
        "/session" => session(seen, shared),
        _ => {
            let (events, extra) = {
                let mut script = shared.script.lock().unwrap();
                (
                    script.events.pop_front().unwrap_or_default(),
                    script.status_delays.pop_front().unwrap_or_default(),
                )
            };
            std::thread::sleep(extra);
            (
                200,
                json!({
                    "cashStatus": {
                        "billsCashbox": 50000, "billsStored": 75050, "coinsCashbox": 0,
                        "coinsStored": 0, "collectableBillValue": 0, "collectableCoinValue": 0,
                        "totalCollectableCash": 0, "totalSystemCash": 125050
                    },
                    "events": events,
                    "faceDeviceState": "none"
                })
                .to_string(),
            )
        }
    }
}

/// p. 9: a form-encoded password grant, or `401` with no body.
fn grant(seen: &Seen, shared: &Shared) -> (u16, String) {
    let expected = format!(
        "application/x-www-form-urlencoded\ngrant_type=password&username={USERNAME}&password={PASSWORD}"
    );
    if seen.body != expected {
        return (401, String::new());
    }
    let number = shared.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let token = format!("bench-token-{number}");
    *shared.valid.lock().unwrap() = Some(token.clone());
    (
        200,
        json!({
            "access_token": token,
            "expires_in": 43200000,
            "token_type": "bearer",
            "userName": USERNAME
        })
        .to_string(),
    )
}

/// p. 15: `{"request": …, "value": …}`, answered with a `responseCode`.
fn session(seen: &Seen, shared: &Shared) -> (u16, String) {
    let Ok(body) = serde_json::from_str::<Value>(&seen.body) else {
        return (400, String::new());
    };
    let Some(request) = body.get("request").and_then(Value::as_str) else {
        return (400, String::new());
    };
    let scripted = {
        let mut script = shared.script.lock().unwrap();
        ["PayAmount", "CancelPayment", "CloseSession", "StopPayment"]
            .into_iter()
            .find(|name| *name == request)
            .and_then(|name| script.codes.get_mut(name))
            .and_then(VecDeque::pop_front)
    };
    let (code, data) = match request {
        "PayAmount" => match body.get("value") {
            Some(Value::Number(value)) if value.as_i64().is_some_and(|v| v > 0) => {
                (scripted.unwrap_or(0), TRANSACTION_ID)
            }
            _ => (12, "invalid amount"),
        },
        "CancelPayment" => (scripted.unwrap_or(0), "session cancelled"),
        "StopPayment" => (scripted.unwrap_or(0), "session stopped"),
        "CloseSession" => (scripted.unwrap_or(0), "session closed"),
        _ => (16, "invalid request"),
    };
    let data = if code == 0 { data } else { "refused" };
    (
        200,
        json!({ "responseCode": code, "responseData": data }).to_string(),
    )
}
