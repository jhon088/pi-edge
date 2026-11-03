//! A CashGenic note acceptor, for cash on a showroom kiosk.
//!
//! Six calls and no more: a token, what the machine is, what it is doing, take
//! this much, stop taking it, and close the session. No refill, no collect, no
//! empty, no age verification, no device administration — a demo takes notes
//! and gives change, and everything else is somebody's production problem.
//!
//! The device sits on the local network and serves its API **over TLS** on
//! 44333 with a certificate it signed itself, which is why the first version
//! of this module never worked on the stand: it wrote plain HTTP bytes at a
//! TLS listener and read back a handshake it could not parse. So the request
//! is still written by hand — one request, one response, a read timeout — but
//! it goes through `rustls`, and **only** through `rustls`: the unit on the
//! bench serves HTTPS on 44333, so there is no plain-HTTP path to keep working
//! for a device nobody has.
//!
//! ```text
//! POST /token                    form-urlencoded, OAuth2 password grant
//! GET  /system                   Bearer
//! GET  /status?eventVersion=2    Bearer
//! POST /PayAmount                Bearer, JSON
//! POST /CancelPayment            Bearer, JSON
//! POST /CloseSession             Bearer, JSON
//! ```
//!
//! The certificate is accepted without being verified. That is a real
//! decision and it is confined to [`AcceptTheDeviceOnTheBench`] in this
//! module: the acceptor is a box on a showroom LAN whose certificate names
//! nothing and is signed by nobody, there is no cardholder data on the wire,
//! and the alternative is a demo that cannot take a banknote. Nothing else in
//! this repository opens a TLS connection, and nothing else may use this
//! verifier.
//!
//! **No cardholder data passes through here, and none can**: the device
//! accepts banknotes, and what comes back is an amount in minor units.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::showroom::CashGenicSettingsV1;
use crate::BridgeError;

/// Long enough for a device on the same switch, short enough that a screen
/// never looks frozen waiting for one that is not there.
const TIMEOUT: Duration = Duration::from_secs(4);

/// What the background health check allows itself: the same as "Probar
/// conexión".
///
/// It was one second, on the theory that a device that cannot answer in a
/// second on a local switch is not there. The unit on the stand disproved it:
/// it answered every call, and the watcher still flipped between online and
/// `connection timed out` (os error 10060) from one round to the next. Nothing
/// waits on this answer — a poll of `/demo/status` reads the last result and
/// never dials — so the budget can be the one a person already trusts.
pub(crate) const PROBE_TIMEOUT: Duration = TIMEOUT;

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
    remember_in(health(), status);
}

fn remember_in(cache: &RwLock<CachedHealth>, status: CashGenicStatusV1) {
    if let Ok(mut held) = cache.write() {
        if let Some(line) = probe_change(&held.status, &status) {
            tracing::info!("{line}");
        }
        *held = CachedHealth {
            status,
            last_checked_at: Some(chrono::Utc::now().to_rfc3339()),
        };
    }
}

/// The log line for a probe whose answer differs from the last one, or `None`.
///
/// Only a change is written: the watcher asks every ten seconds, and a log of
/// "still offline" would bury the one line that says when it went. What counts
/// is whether it is enabled, whether it answers, its state and why — not the
/// cash it holds, which moves with every note. `DISABLED` is the state a
/// disabled acceptor and nothing else is given, so it is the enabled flag.
fn probe_change(before: &CashGenicStatusV1, after: &CashGenicStatusV1) -> Option<String> {
    let same = before.online == after.online
        && before.state == after.state
        && before.detail == after.detail;
    if same {
        return None;
    }
    Some(format!(
        "CASHGENIC_PROBE enabled={} online={} state={:?} detail={:?}",
        after.state != "DISABLED",
        after.online,
        after.state,
        after.detail.as_deref().unwrap_or_default()
    ))
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
    /// DIAGNOSTIC, temporary: the health check's rounds record every stage of
    /// every call. The payment path carries none and logs nothing new.
    trace: Option<Trace>,
}

impl<'a> CashGenic<'a> {
    pub fn new(settings: &'a CashGenicSettingsV1) -> Self {
        Self {
            settings,
            timeout: TIMEOUT,
            trace: None,
        }
    }

    /// The same device, on a budget, with every stage of every call timed and
    /// logged as `CASHGENIC_TRACE`. For the health check, which nobody waits
    /// on and which must never hold up a status poll — and for the diagnostic,
    /// which runs exactly this.
    pub(crate) fn traced(settings: &'a CashGenicSettingsV1, timeout: Duration, round: u64) -> Self {
        Self {
            settings,
            timeout,
            trace: Some(Trace {
                round,
                steps: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Every stage recorded so far, in order.
    pub(crate) fn steps(&self) -> Vec<TraceStep> {
        self.trace
            .as_ref()
            .map(|trace| trace.steps.borrow().clone())
            .unwrap_or_default()
    }

    /// A bearer token. Asked for on every call rather than cached: a demo
    /// makes a handful of requests a minute, and a cache that goes stale
    /// mid-payment is worse than a round trip.
    ///
    /// An **OAuth2 password grant**, `application/x-www-form-urlencoded`:
    ///
    /// ```text
    /// POST /token
    /// Content-Type: application/x-www-form-urlencoded
    ///
    /// grant_type=password&username=ApiUserOne&password=1234
    /// ```
    ///
    /// Not JSON. The first version of this module sent
    /// `{"username":…,"password":…}` and the device refused it, which is why
    /// the body shape is pinned by a test rather than left to read like a
    /// detail.
    fn token(&self) -> Result<String, BridgeError> {
        let body = self.call("POST", "/token", None, Some(grant(self.settings)))?;
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
    ///
    /// Takes the token rather than asking for one: its only caller is the
    /// health check, which asks once per round and reads `/status` with the
    /// same grant.
    fn system(&self, token: &str) -> Result<Value, BridgeError> {
        self.call("GET", "/system", Some(token), None)
    }

    /// What it is doing right now, including any payment in progress.
    ///
    /// `eventVersion=2` is the shape this firmware answers with — 2.1.49 on
    /// the unit on the bench — and asking without it is asking for the older
    /// one. The query string is part of the path because that is all this
    /// hand-written client needs it to be.
    pub fn status(&self) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.status_with(&token)
    }

    /// `status`, with a token the caller already holds.
    fn status_with(&self, token: &str) -> Result<Value, BridgeError> {
        self.call("GET", "/status?eventVersion=2", Some(token), None)
    }

    /// Start taking notes towards an amount.
    pub fn pay_amount(&self, amount_minor: i64) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call(
            "POST",
            "/PayAmount",
            Some(&token),
            Some(Body::Json(
                json!({ "amount": minor_to_major(amount_minor) }),
            )),
        )
    }

    /// Stop taking notes and give back what was taken.
    pub fn cancel_payment(&self) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call(
            "POST",
            "/CancelPayment",
            Some(&token),
            Some(Body::Json(json!({}))),
        )
    }

    /// Finish with the device so the next sale starts clean.
    pub fn close_session(&self) -> Result<Value, BridgeError> {
        let token = self.token()?;
        self.call(
            "POST",
            "/CloseSession",
            Some(&token),
            Some(Body::Json(json!({}))),
        )
    }

    /// One request, written by hand, over TLS.
    ///
    /// There is no plain-HTTP path and no scheme probing. The unit on the
    /// bench serves HTTPS on 44333 and that is the only thing this talks to —
    /// a fallback for a configuration nobody has would be a second transport
    /// to keep working for no device.
    ///
    /// `Connection: close` is in every request, so "read to the end" is the
    /// whole response and no chunk decoder or content-length walk is needed.
    fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Body>,
    ) -> Result<Value, BridgeError> {
        let host = self.settings.host.trim();
        if host.is_empty() {
            return Err(BridgeError {
                code: "CASHGENIC_NOT_CONFIGURED".to_owned(),
                message: "no address for the note acceptor".to_owned(),
            });
        }
        let address = format!("{host}:{}", self.settings.port);
        let request = request_bytes(method, path, &address, token, body);

        let mut stages = Stages::new(self.trace.as_ref(), path);

        let started = Instant::now();
        let resolved = match address.to_socket_addrs().map(|mut all| all.next()) {
            Ok(Some(resolved)) => resolved,
            Ok(None) => {
                stages.fail("resolve", started, "resolves to nothing".to_owned());
                return Err(BridgeError {
                    code: "CASHGENIC_UNREACHABLE".to_owned(),
                    message: format!("{address} resolves to nothing"),
                });
            }
            Err(error) => {
                stages.fail("resolve", started, io_detail(&error));
                return Err(offline(&address, error));
            }
        };
        let family = if resolved.is_ipv4() { "ipv4" } else { "ipv6" };
        stages.pass("resolve", started, format!("{resolved} {family}"));

        let started = Instant::now();
        let mut socket = match TcpStream::connect_timeout(&resolved, self.timeout) {
            Ok(socket) => socket,
            Err(error) => {
                stages.fail("tcp_connect", started, io_detail(&error));
                return Err(offline(&address, error));
            }
        };
        let local = socket
            .local_addr()
            .map(|local| local.to_string())
            .unwrap_or_default();
        stages.pass("tcp_connect", started, format!("local={local}"));
        socket
            .set_read_timeout(Some(self.timeout))
            .and_then(|()| socket.set_write_timeout(Some(self.timeout)))
            .map_err(|error| offline(&address, error))?;

        let mut connection = tls_connection(host).map_err(|detail| BridgeError {
            code: "CASHGENIC_UNREACHABLE".to_owned(),
            message: format!("{address}: {detail}"),
        })?;
        // Driven to the end here rather than on the first write, which is all
        // `StreamOwned` would do with it anyway — so the handshake has its own
        // time and its own failure instead of hiding inside "write".
        let started = Instant::now();
        while connection.is_handshaking() {
            if let Err(error) = connection.complete_io(&mut socket) {
                stages.fail("tls_handshake", started, io_detail(&error));
                return Err(offline(&address, error));
            }
        }
        let sni = if host.parse::<std::net::IpAddr>().is_ok() {
            "none(ip-literal)"
        } else {
            host
        };
        stages.pass(
            "tls_handshake",
            started,
            format!(
                "version={:?} suite={:?} sni={sni}",
                connection.protocol_version(),
                connection
                    .negotiated_cipher_suite()
                    .map(|suite| suite.suite()),
            ),
        );

        let raw = round_trip(
            rustls::StreamOwned::new(connection, socket),
            &request,
            &address,
            &mut stages,
        )?;
        let started = Instant::now();
        let parsed = parse(&raw);
        match &parsed {
            Ok(_) => stages.pass("json_parse", started, String::new()),
            Err(error) => stages.fail("json_parse", started, error.code.clone()),
        }
        parsed
    }
}

/// What a request carries, and therefore what its `Content-Type` is.
///
/// Two shapes, because the device wants two. `/token` is an OAuth2 password
/// grant and takes `application/x-www-form-urlencoded`; everything else takes
/// JSON. Sending JSON to `/token` is what the first version of this module
/// did, and the device refuses it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Body {
    /// `application/x-www-form-urlencoded`, in the order given — a form body
    /// is an ordered list, not a map, and the order is part of what a test
    /// can pin.
    Form(Vec<(&'static str, String)>),
    Json(Value),
}

impl Body {
    fn content_type(&self) -> &'static str {
        match self {
            Self::Form(_) => "application/x-www-form-urlencoded",
            Self::Json(_) => "application/json",
        }
    }

    fn encode(&self) -> String {
        match self {
            Self::Form(fields) => fields
                .iter()
                .map(|(name, value)| format!("{name}={}", form_encode(value)))
                .collect::<Vec<_>>()
                .join("&"),
            Self::Json(value) => value.to_string(),
        }
    }
}

/// The password grant `/token` takes, built once for the client and for the
/// diagnostic's curl comparison, so both send the same bytes.
fn grant(settings: &CashGenicSettingsV1) -> Body {
    Body::Form(vec![
        ("grant_type", "password".to_owned()),
        ("username", settings.username.clone()),
        ("password", settings.password.clone()),
    ])
}

/// DIAGNOSTIC, temporary: the grant as the form body curl is handed on stdin.
pub(crate) fn grant_form(settings: &CashGenicSettingsV1) -> String {
    grant(settings).encode()
}

/// Percent-encode one form value.
///
/// Written here rather than pulled in: it is twelve lines and the only place
/// in the repository that needs it. Unreserved characters pass; a space is
/// `+`, as `application/x-www-form-urlencoded` requires; everything else is
/// `%XX`. It matters because the credential is typed by whoever set the kiosk
/// up, and an operator whose password contains `&` must not end up sending
/// two fields.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Write the request and read the whole answer off one stream.
///
/// To the end of the stream, as before — with each stage timed: the write,
/// the headers arriving, and the body to its end, including how it ended and
/// how long the stream stayed open after `Content-Length` said it was done.
fn round_trip(
    mut stream: impl Read + Write,
    request: &[u8],
    address: &str,
    stages: &mut Stages<'_>,
) -> Result<Vec<u8>, BridgeError> {
    let started = Instant::now();
    if let Err(error) = stream.write_all(request).and_then(|()| stream.flush()) {
        stages.fail("http_write", started, io_detail(&error));
        return Err(offline(address, error));
    }
    stages.pass("http_write", started, format!("bytes={}", request.len()));

    let started = Instant::now();
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut head: Option<(usize, Option<usize>)> = None;
    let mut complete_at: Option<Duration> = None;
    let ended: Result<&'static str, std::io::Error> = loop {
        match stream.read(&mut chunk) {
            // rustls says 0 only once the peer's `close_notify` has arrived.
            Ok(0) => break Ok("close_notify"),
            Ok(read) => {
                raw.extend_from_slice(&chunk[..read]);
                if head.is_none() {
                    if let Some(at) = header_end(&raw) {
                        let length = content_length(&raw[..at]);
                        head = Some((at, length));
                        stages.pass("http_headers", started, header_summary(&raw[..at]));
                    }
                }
                if let (None, Some((at, Some(length)))) = (complete_at, head) {
                    if raw.len() >= at + length {
                        complete_at = Some(started.elapsed());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => break Err(error),
        }
    };

    if head.is_none() {
        let detail = match &ended {
            Ok(how) => format!("no header end; ended={how} bytes={}", raw.len()),
            Err(error) => format!("no header end; bytes={} {}", raw.len(), io_detail(error)),
        };
        stages.fail("http_headers", started, detail);
    }
    let how = match &ended {
        Ok(how) => (*how).to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            "tcp_close_without_close_notify".to_owned()
        }
        Err(error) => format!("error {}", io_detail(error)),
    };
    let waited = complete_at.map_or_else(
        || "unknown(no content-length)".to_owned(),
        |at| format!("{}", started.elapsed().saturating_sub(at).as_millis()),
    );
    let body = format!(
        "bytes={} end={how} open_after_complete_ms={waited}",
        raw.len()
    );

    match ended {
        Ok(_) => {
            stages.pass("http_body", started, body);
            Ok(raw)
        }
        // A server that closes without a TLS `close_notify` is ordinary in
        // embedded firmware, and what it already sent is still the answer.
        Err(error) if !raw.is_empty() => {
            tracing::debug!(%error, address, "the device closed the stream abruptly");
            stages.pass("http_body", started, body);
            Ok(raw)
        }
        Err(error) => {
            stages.fail("http_body", started, body);
            Err(offline(address, error))
        }
    }
}

// ------------------------------------------------- DIAGNOSTIC, temporary
//
// Stage timing for the health check and the diagnostic binary. Never a body,
// never a credential, never a token: a stage carries its name, how long it
// took, whether it worked, and a detail built only from socket addresses, I/O
// error kinds, TLS parameters, a status code and an allowlist of headers.

/// The stages of one round, as the diagnostic reads them back.
#[derive(Debug)]
struct Trace {
    round: u64,
    steps: RefCell<Vec<TraceStep>>,
}

/// One stage of one call.
#[derive(Debug, Clone)]
pub(crate) struct TraceStep {
    /// `token`, `system`, `status`, or `other`.
    pub call: &'static str,
    pub step: &'static str,
    pub elapsed: Duration,
    pub ok: bool,
    pub detail: String,
}

static ROUND: AtomicU64 = AtomicU64::new(1);

/// A fresh round id, for the log.
pub(crate) fn next_round() -> u64 {
    ROUND.fetch_add(1, Ordering::Relaxed)
}

/// The recorder one call writes into: nothing at all when untraced.
struct Stages<'t> {
    trace: Option<&'t Trace>,
    call: &'static str,
}

impl<'t> Stages<'t> {
    fn new(trace: Option<&'t Trace>, path: &str) -> Self {
        let call = if path.starts_with("/token") {
            "token"
        } else if path.starts_with("/system") {
            "system"
        } else if path.starts_with("/status") {
            "status"
        } else {
            "other"
        };
        Self { trace, call }
    }

    fn pass(&mut self, step: &'static str, started: Instant, detail: String) {
        self.record(step, started, true, detail);
    }

    fn fail(&mut self, step: &'static str, started: Instant, detail: String) {
        self.record(step, started, false, detail);
    }

    fn record(&mut self, step: &'static str, started: Instant, ok: bool, detail: String) {
        let Some(trace) = self.trace else {
            return;
        };
        let elapsed = started.elapsed();
        tracing::info!(
            "CASHGENIC_TRACE round_id={} call={} step={step} elapsed_ms={} result={} detail={:?}",
            trace.round,
            self.call,
            elapsed.as_millis(),
            if ok { "OK" } else { "ERROR" },
            detail
        );
        trace.steps.borrow_mut().push(TraceStep {
            call: self.call,
            step,
            elapsed,
            ok,
            detail,
        });
    }
}

/// An I/O error as its kind, the operating system's code and its words —
/// the three that tell a connect timeout from a read timeout from a reset.
fn io_detail(error: &std::io::Error) -> String {
    let code = error
        .raw_os_error()
        .map_or_else(|| "none".to_owned(), |code| code.to_string());
    format!(
        "kind={:?} os_error={code} error={:?}",
        error.kind(),
        error.to_string()
    )
}

/// Where the headers end, counting the blank line.
fn header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

fn content_length(head: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(head).lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

/// The status code and the headers that decide where a body ends, plus the
/// names (never the values) of every other header the device sent.
fn header_summary(head: &[u8]) -> String {
    const SHOWN: [&str; 6] = [
        "content-length",
        "transfer-encoding",
        "connection",
        "keep-alive",
        "content-type",
        "server",
    ];
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("?")
        .to_owned();
    let mut shown = Vec::new();
    let mut others = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if SHOWN.contains(&name.as_str()) {
            shown.push(format!("{name}={}", value.trim()));
        } else {
            others.push(name);
        }
    }
    format!(
        "http_status={status} {} other_headers=[{}]",
        shown.join(" "),
        others.join(",")
    )
}

/// An HTTP/1.1 request as bytes.
fn request_bytes(
    method: &str,
    path: &str,
    address: &str,
    token: Option<&str>,
    body: Option<Body>,
) -> Vec<u8> {
    let encoded = body.as_ref().map(|body| body.encode());
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\
         Accept: application/json\r\n"
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let (Some(body), Some(encoded)) = (body.as_ref(), encoded.as_ref()) {
        request.push_str(&format!(
            "Content-Type: {}\r\nContent-Length: {}\r\n",
            body.content_type(),
            encoded.len()
        ));
    }
    request.push_str("\r\n");
    if let Some(encoded) = encoded {
        request.push_str(&encoded);
    }
    request.into_bytes()
}

/// A TLS client for one connection to the acceptor.
fn tls_connection(host: &str) -> Result<rustls::ClientConnection, String> {
    // An IP address is the normal case here and `ServerName` handles it: the
    // name is only used for SNI, which is omitted for a literal address.
    let name = rustls_pki_types::ServerName::try_from(host.to_owned())
        .map_err(|error| format!("`{host}` is not a usable server name: {error}"))?;
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AcceptTheDeviceOnTheBench))
        .with_no_client_auth();
    rustls::ClientConnection::new(std::sync::Arc::new(config), name)
        .map_err(|error| error.to_string())
}

/// Accepts the acceptor's self-signed certificate without checking it.
///
/// **Read the module header before reusing this.** It verifies nothing: not
/// the name, not the chain, not the expiry. It exists because a CashGenic
/// presents a certificate it signed itself, naming nothing, on a showroom
/// LAN, to carry an amount of pesos and no cardholder data. On anything
/// carrying a customer's money or identity this would be a vulnerability, and
/// the reason it is a named type rather than a closure is so that it has to
/// be read before it is used again.
#[derive(Debug)]
struct AcceptTheDeviceOnTheBench;

impl rustls::client::danger::ServerCertVerifier for AcceptTheDeviceOnTheBench {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
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
        // The body too, trimmed: on a stand "the device answered 401" sends
        // somebody to read a manual, and "401: bad credentials" does not.
        // Safe to echo — this API carries amounts, never cardholder data.
        let said: String = body.trim().chars().take(200).collect();
        return Err(BridgeError {
            code: "CASHGENIC_REFUSED".to_owned(),
            message: if said.is_empty() {
                format!("the device answered {status}")
            } else {
                format!("the device answered {status}: {said}")
            },
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
        .or_else(|| status.get("Status"))
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_owned();
    // The firmware reports `errorCondition: "None"` when all is well. Only
    // anything else is worth putting in front of a person.
    let fault = status
        .get("errorCondition")
        .or_else(|| status.get("ErrorCondition"))
        .and_then(Value::as_str)
        .filter(|condition| !condition.eq_ignore_ascii_case("none"))
        .map(str::to_owned);

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
        detail: fault,
    }
}

// ----------------------------------------------------------------- commands

#[tauri::command]
pub fn cashgenic_save(
    app: tauri::AppHandle,
    settings: CashGenicSettingsV1,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    save(&crate::showroom::data_dir(&app)?, settings)
}

fn save(
    dir: &std::path::Path,
    settings: CashGenicSettingsV1,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let mut preferences = crate::showroom::stored(dir);
    preferences.cashgenic = settings;
    crate::showroom::write(dir, &preferences)?;
    Ok(preferences)
}

/// The "Habilitar CashGenic" switch, saved the moment it is flipped.
///
/// Only the flag: the address and the credentials are whatever is stored, so a
/// switch never carries half-typed fields into the file, and Admin can never
/// show a device on that the runtime reads as off.
#[tauri::command]
pub fn cashgenic_set_enabled(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    set_enabled(&crate::showroom::data_dir(&app)?, enabled)
}

fn set_enabled(
    dir: &std::path::Path,
    enabled: bool,
) -> Result<crate::showroom::KioskPreferencesV1, BridgeError> {
    let mut preferences = crate::showroom::stored(dir);
    preferences.cashgenic.enabled = enabled;
    crate::showroom::write(dir, &preferences)?;
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
    let device = CashGenic::traced(settings, timeout, next_round());
    probe_round(&device)
}

/// One health-check round on a device: a grant, `/system`, `/status`. What
/// the watcher and the diagnostic both run.
pub(crate) fn probe_round(device: &CashGenic<'_>) -> CashGenicStatusV1 {
    // One grant per round: three connections, not four. A token outlives a
    // round by a long way, and a refused grant is the same "offline" either
    // way.
    let token = match device.token() {
        Ok(token) => token,
        Err(error) => return CashGenicStatusV1::offline(error.message),
    };
    match device.system(&token) {
        Ok(system) => {
            let mut status = match device.status_with(&token) {
                Ok(reported) => progress(&reported, 0),
                Err(error) => return CashGenicStatusV1::offline(error.message),
            };
            status.device = system
                .get("device")
                .or_else(|| system.get("model"))
                .or_else(|| system.get("name"))
                // What the unit on the bench answers with: `systemVersion`,
                // "2.1.49". A version is not a model, so it is prefixed
                // rather than printed as if it were a name.
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    system
                        .get("systemVersion")
                        .or_else(|| system.get("SystemVersion"))
                        .and_then(Value::as_str)
                        .map(|version| format!("CashGenic {version}"))
                })
                .or(status.device);
            if status.cash_total_minor.is_none() {
                status.cash_total_minor = amount_minor(system.get("cashTotal"));
            }
            // A unit whose bill or coin module is disconnected answers every
            // call and still cannot take money, so it is reported rather than
            // left looking healthy.
            if let Some(disconnected) = disconnected_modules(&system) {
                status.detail = Some(disconnected);
            }
            status
        }
        Err(error) => CashGenicStatusV1::offline(error.message),
    }
}

/// Which of the device's own modules say they are not connected.
///
/// `/system` reports a `connected` flag per module. Absent means the firmware
/// did not say, which is not the same as "not connected" and is left alone.
fn disconnected_modules(system: &Value) -> Option<String> {
    let mut out = Vec::new();
    for (field, label) in [("bills", "billetes"), ("coins", "monedas")] {
        let module = system
            .get(field)
            .or_else(|| system.get(capitalised(field).as_str()))?;
        let connected = module
            .get("connected")
            .or_else(|| module.get("Connected"))
            .and_then(Value::as_bool);
        if connected == Some(false) {
            out.push(label);
        }
    }
    (!out.is_empty()).then(|| format!("el módulo de {} no está conectado", out.join(" y ")))
}

fn capitalised(field: &str) -> String {
    let mut chars = field.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
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

/// Start the acceptor taking notes towards an amount the core computed.
///
/// Gated on the printer for the same reason `kiosk_pay` is, and separately:
/// this is the point at which a customer physically puts a banknote into a
/// machine, and getting it back out is a conversation with a member of staff.
/// Cash is the one tender where letting the payment start and failing
/// afterwards costs somebody their afternoon.
#[tauri::command]
pub fn cashgenic_pay(
    app: tauri::AppHandle,
    bridge: tauri::State<'_, crate::EdgeBridge>,
    amount_minor: i64,
) -> Result<CashGenicStatusV1, BridgeError> {
    crate::kiosk::require_a_printer_that_can_follow_through(&bridge)?;
    crate::kiosk::require_an_authorization_if_the_basket_needs_one(
        &crate::showroom::data_dir(&app)?,
        &bridge,
    )?;
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
    use std::sync::{Arc, Mutex};

    use crate::logging::captured;
    use crate::showroom::{stored, write, KioskPreferencesV1, Vertical};

    fn configured(enabled: bool) -> CashGenicSettingsV1 {
        CashGenicSettingsV1 {
            enabled,
            host: "192.168.88.240".to_owned(),
            port: 44_333,
            username: "ApiUserOne".to_owned(),
            password: "s3cr3t-pw".to_owned(),
        }
    }

    /// A stored kiosk with an identity and a PIN, so a save that dropped any
    /// other field would show.
    fn a_kiosk_with(dir: &std::path::Path, cashgenic: CashGenicSettingsV1) {
        let preferences = KioskPreferencesV1 {
            vertical: Vertical::Retail,
            branch_id: "001".to_owned(),
            kiosk_id: "K01".to_owned(),
            admin_pin: "4321".to_owned(),
            remote_control: true,
            cashgenic,
            ..KioskPreferencesV1::default()
        };
        write(dir, &preferences).unwrap();
    }

    #[test]
    fn switching_the_acceptor_on_is_saved_at_once_and_keeps_its_configuration() {
        let dir = tempfile::tempdir().unwrap();
        a_kiosk_with(dir.path(), configured(false));
        let before = stored(dir.path());

        let answered = set_enabled(dir.path(), true).unwrap();

        let after = stored(dir.path());
        assert!(
            after.cashgenic.enabled,
            "ON is in the file, not only on screen"
        );
        assert_eq!(after.cashgenic, configured(true));
        assert_eq!(answered, after, "Admin is handed what was written");
        // Nothing else moved.
        assert_eq!(
            KioskPreferencesV1 {
                cashgenic: before.cashgenic.clone(),
                ..after
            },
            before
        );
    }

    #[test]
    fn switching_the_acceptor_off_is_saved_at_once_and_forgets_nothing() {
        let dir = tempfile::tempdir().unwrap();
        a_kiosk_with(dir.path(), configured(true));
        let before = stored(dir.path());

        set_enabled(dir.path(), false).unwrap();

        let after = stored(dir.path());
        assert!(!after.cashgenic.enabled);
        assert_eq!(
            after.cashgenic,
            configured(false),
            "address and credentials kept"
        );
        assert_eq!(after.admin_pin, before.admin_pin);
        assert_eq!(after.branch_id, before.branch_id);
        assert_eq!(after.vertical, before.vertical);
        assert!(after.remote_control);
    }

    #[test]
    fn a_saved_configuration_is_the_one_read_back() {
        let dir = tempfile::tempdir().unwrap();
        a_kiosk_with(dir.path(), CashGenicSettingsV1::default());

        save(dir.path(), configured(true)).unwrap();

        let back = stored(dir.path()).cashgenic;
        assert!(back.enabled);
        assert_eq!(back.host, "192.168.88.240");
        assert_eq!(back.port, 44_333);
        assert_eq!(back.username, "ApiUserOne");
        assert_eq!(back.password, "s3cr3t-pw");
        assert_eq!(stored(dir.path()).branch_id, "001");
    }

    fn answering(state: &str, cash_total_minor: i64) -> CashGenicStatusV1 {
        CashGenicStatusV1 {
            online: true,
            device: Some("CashGenic 2.1.49".to_owned()),
            state: state.to_owned(),
            deposited_minor: 0,
            outstanding_minor: 0,
            change_minor: 0,
            cash_total_minor: Some(cash_total_minor),
            detail: None,
        }
    }

    #[test]
    fn a_probe_is_logged_when_its_answer_changes_and_only_then() {
        let cache = RwLock::new(CachedHealth::default());
        let disabled = CashGenicStatusV1 {
            state: "DISABLED".to_owned(),
            ..CashGenicStatusV1::offline("not enabled on this kiosk")
        };
        let log = captured(|| {
            remember_in(&cache, disabled.clone());
            let start = "the first answer replaces UNKNOWN";
            tracing::info!("{start}");
            remember_in(&cache, disabled.clone());
            remember_in(&cache, answering("Idle", 100));
            // The cash it holds moves with every note; that is not a change.
            remember_in(&cache, answering("Idle", 200));
            remember_in(&cache, CashGenicStatusV1::offline("timed out"));
            remember_in(&cache, CashGenicStatusV1::offline("timed out"));
        });
        let after_start = log
            .split("the first answer replaces UNKNOWN")
            .nth(1)
            .unwrap();
        let lines: Vec<&str> = after_start
            .lines()
            .filter(|line| line.contains("CASHGENIC_PROBE"))
            .collect();
        assert_eq!(
            lines.len(),
            2,
            "DISABLED->ONLINE and ONLINE->OFFLINE: {log}"
        );
        assert!(
            lines[0].contains(r#"CASHGENIC_PROBE enabled=true online=true state="Idle" detail="""#)
        );
        assert!(
            lines[1].contains(r#"enabled=true online=false state="OFFLINE" detail="timed out""#)
        );
    }

    #[test]
    fn a_probe_line_never_carries_a_secret() {
        let cache = RwLock::new(CachedHealth::default());
        let log = captured(|| {
            remember_in(
                &cache,
                CashGenicStatusV1::offline(
                    "refused: {\"access_token\":\"eyJabc\"} password=s3cr3t-pw Bearer tok-123",
                ),
            );
        });
        assert!(log.contains("CASHGENIC_PROBE"), "{log}");
        for secret in ["eyJabc", "s3cr3t-pw", "tok-123"] {
            assert!(!log.contains(secret), "{secret} leaked: {log}");
        }
    }

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

        // And what it said, so a stand does not have to read a manual.
        let told = b"HTTP/1.1 401 Unauthorized\r\n\r\n{\"error\":\"bad credentials\"}";
        let error = parse(told).unwrap_err();
        assert!(
            error.message.contains("bad credentials"),
            "{}",
            error.message
        );
    }

    /// The `/token` request, byte for byte.
    ///
    /// Pinned because getting it wrong is what kept cash unavailable on the
    /// stand: the device wants an OAuth2 password grant as a form body, and
    /// answers JSON with a refusal. The assertions are deliberately exact —
    /// the header, the order of the fields, and the length — because "nearly
    /// right" here looks identical to a dead device.
    #[test]
    fn the_token_request_is_a_form_encoded_password_grant() {
        let settings = CashGenicSettingsV1 {
            enabled: true,
            host: "192.168.88.240".to_owned(),
            port: 44_333,
            username: "ApiUserOne".to_owned(),
            password: "1234".to_owned(),
        };
        // Built through the very call the client makes, so the test cannot
        // drift from it.
        let raw = request_bytes(
            "POST",
            "/token",
            "192.168.88.240:44333",
            None,
            Some(Body::Form(vec![
                ("grant_type", "password".to_owned()),
                ("username", settings.username.clone()),
                ("password", settings.password.clone()),
            ])),
        );
        let text = String::from_utf8(raw).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();

        assert!(head.starts_with("POST /token HTTP/1.1\r\n"), "{head}");
        assert!(
            head.contains("Content-Type: application/x-www-form-urlencoded"),
            "the device refuses anything but a form body here: {head}"
        );
        // `Accept: application/json` stays — the *answer* is JSON. What must
        // not appear is a JSON `Content-Type`, which is what was sent before.
        assert!(
            !head.contains("Content-Type: application/json"),
            "`/token` was sent as JSON: {head}"
        );
        assert_eq!(
            body,
            "grant_type=password&username=ApiUserOne&password=1234"
        );
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
        // No bearer on the call that asks for the bearer.
        assert!(!head.contains("Authorization"), "{head}");
    }

    /// A credential with punctuation in it still arrives as one field.
    #[test]
    fn a_password_with_punctuation_cannot_become_a_second_field() {
        let body = Body::Form(vec![
            ("grant_type", "password".to_owned()),
            ("username", "api user".to_owned()),
            ("password", "p&a=s s/w?rd+1".to_owned()),
        ]);
        assert_eq!(
            body.encode(),
            "grant_type=password&username=api+user&password=p%26a%3Ds+s%2Fw%3Frd%2B1"
        );
        // Unreserved characters are left alone, so an ordinary credential is
        // still readable in a packet capture.
        assert_eq!(form_encode("ApiUserOne-1_2.3~4"), "ApiUserOne-1_2.3~4");
    }

    /// Everything that is not `/token` is JSON with a bearer.
    #[test]
    fn every_other_call_is_json_with_the_bearer() {
        let raw = request_bytes(
            "POST",
            "/PayAmount",
            "192.168.88.240:44333",
            Some("tok"),
            Some(Body::Json(json!({ "amount": "187.50" }))),
        );
        let text = String::from_utf8(raw).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /PayAmount HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Host: 192.168.88.240:44333"));
        assert!(head.contains("Authorization: Bearer tok"));
        assert!(head.contains("Content-Type: application/json"));
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
        assert!(head.contains("Connection: close"));
        assert_eq!(body, r#"{"amount":"187.50"}"#);

        // A GET carries no body and therefore no length and no type.
        let get = String::from_utf8(request_bytes(
            "GET",
            "/status?eventVersion=2",
            "host:1",
            Some("tok"),
            None,
        ))
        .unwrap();
        assert!(get.starts_with("GET /status?eventVersion=2 HTTP/1.1"));
        assert!(!get.contains("Content-Length"), "{get}");
        assert!(!get.contains("Content-Type"), "{get}");
    }

    // ------------------------------------------------- against a real listener
    //
    // A TLS server, with a certificate it signed itself, standing in for the
    // acceptor. It is here because the bug that shipped was not in the logic
    // — it was in the transport, and a unit test of the request bytes would
    // not have caught it. This proves the three things that failed: that
    // `AcceptTheDeviceOnTheBench` lets a self-signed certificate through,
    // that the handshake completes, and that the whole `/token` → `/system`
    // → `/status` conversation comes back parsed.

    /// The bench certificate, generated once and checked in. Valid until
    /// 2126; it secures nothing and never leaves a test.
    const BENCH_CERT: &[u8] = include_bytes!("../tests/fixtures/bench-cert.der");
    const BENCH_KEY: &[u8] = include_bytes!("../tests/fixtures/bench-key.der");

    /// What each path answered, so a test can read the conversation back.
    #[derive(Debug, Default)]
    struct Heard {
        token_request: Mutex<String>,
        paths: Mutex<Vec<String>>,
        bearers: Mutex<Vec<String>>,
        /// How long `GET /status` takes to answer. Zero unless a test is
        /// standing in for a slow unit.
        status_delay: Mutex<Duration>,
        /// How long the connection is kept open after the answer, as a
        /// device that ignores `Connection: close` would. One at a time, like
        /// the rest of this bench: while one is held, the next waits.
        hold_open: Mutex<Duration>,
    }

    /// Serve TLS on loopback until the test drops it.
    fn bench_acceptor(heard: Arc<Heard>) -> std::net::SocketAddr {
        let certificate = rustls_pki_types::CertificateDer::from(BENCH_CERT.to_vec());
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(
            rustls_pki_types::PrivatePkcs8KeyDer::from(BENCH_KEY.to_vec()),
        );
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate], key)
                .unwrap(),
        );

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(socket) = incoming else { continue };
                let Ok(connection) = rustls::ServerConnection::new(Arc::clone(&config)) else {
                    continue;
                };
                let mut stream = rustls::StreamOwned::new(connection, socket);
                let mut buffer = [0_u8; 2048];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();

                let first = request.lines().next().unwrap_or_default().to_owned();
                if let Ok(mut paths) = heard.paths.lock() {
                    paths.push(first.clone());
                }
                if let Some(bearer) = request
                    .lines()
                    .find_map(|line| line.strip_prefix("Authorization: Bearer "))
                {
                    if let Ok(mut bearers) = heard.bearers.lock() {
                        bearers.push(bearer.trim().to_owned());
                    }
                }

                let body = if first.contains("/token") {
                    if let Ok(mut seen) = heard.token_request.lock() {
                        *seen = request.clone();
                    }
                    // Only a correctly-shaped grant gets a token, exactly as
                    // the device behaves.
                    let correct = request
                        .contains("Content-Type: application/x-www-form-urlencoded")
                        && request.contains("grant_type=password")
                        && request.contains("username=ApiUserOne")
                        && request.contains("password=1234");
                    if !correct {
                        let refusal = r#"{"error":"unsupported_grant_type"}"#;
                        let _ = stream.write_all(
                            format!(
                                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{refusal}",
                                refusal.len()
                            )
                            .as_bytes(),
                        );
                        let _ = stream.flush();
                        continue;
                    }
                    r#"{"access_token":"bench-token","token_type":"bearer"}"#.to_owned()
                } else if first.contains("/system") {
                    r#"{"systemVersion":"2.1.49","currency":"MXN","bills":{"connected":true},"coins":{"connected":true}}"#.to_owned()
                } else {
                    let delay = heard.status_delay.lock().map(|d| *d).unwrap_or_default();
                    std::thread::sleep(delay);
                    r#"{"state":"Idle","errorCondition":"None","deposited":0}"#.to_owned()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
                // Held the way a keep-alive server holds it: until the client
                // closes or the server's own idle limit, whichever is first.
                let hold = heard.hold_open.lock().map(|d| *d).unwrap_or_default();
                if !hold.is_zero() {
                    let _ = stream.sock.set_read_timeout(Some(hold));
                    let mut rest = [0_u8; 64];
                    let _ = stream.read(&mut rest);
                }
            }
        });
        address
    }

    fn bench_settings(address: std::net::SocketAddr, enabled: bool) -> CashGenicSettingsV1 {
        CashGenicSettingsV1 {
            enabled,
            host: address.ip().to_string(),
            port: address.port(),
            username: "ApiUserOne".to_owned(),
            password: "1234".to_owned(),
        }
    }

    /// The unit on the stand: it answers, but not inside a second. The
    /// watcher's own budget — the constant `watch` passes — has to call that
    /// online, and what it caches is what `/demo/status` publishes.
    #[test]
    fn a_unit_slower_than_a_second_is_online_to_the_watcher() {
        let heard = Arc::new(Heard {
            status_delay: Mutex::new(Duration::from_millis(1_500)),
            ..Heard::default()
        });
        let address = bench_acceptor(Arc::clone(&heard));

        let status = probe_with(&bench_settings(address, true), PROBE_TIMEOUT);

        assert!(
            status.online,
            "a 1.5 s answer was called offline: {:?}",
            status.detail
        );
        assert_eq!(status.state, "Idle");
        let cache = RwLock::new(CachedHealth::default());
        remember_in(&cache, status);
        assert!(
            cache.read().unwrap().status.online,
            "the cache holds what the probe saw"
        );
    }

    /// And one that does not answer inside the budget is offline, with why.
    #[test]
    fn a_unit_slower_than_the_budget_is_offline_with_a_reason() {
        let heard = Arc::new(Heard {
            status_delay: Mutex::new(PROBE_TIMEOUT + Duration::from_millis(1_500)),
            ..Heard::default()
        });
        let address = bench_acceptor(Arc::clone(&heard));

        let status = probe_with(&bench_settings(address, true), PROBE_TIMEOUT);

        assert!(!status.online);
        assert_eq!(status.state, "OFFLINE");
        let detail = status.detail.unwrap_or_default();
        assert!(detail.contains(&address.to_string()), "{detail}");
    }

    #[test]
    fn the_watcher_budget_is_the_probe_buttons() {
        assert_eq!(PROBE_TIMEOUT, TIMEOUT);
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(4));
    }

    /// Disabled means not dialled — even with a device right there to answer.
    #[test]
    fn a_disabled_acceptor_is_not_dialled_even_when_it_is_there() {
        let heard = Arc::new(Heard::default());
        let address = bench_acceptor(Arc::clone(&heard));

        let status = probe_with(&bench_settings(address, false), PROBE_TIMEOUT);

        assert_eq!(status.state, "DISABLED");
        assert!(!status.online);
        assert!(
            heard.paths.lock().unwrap().is_empty(),
            "a disabled acceptor was dialled"
        );
    }

    /// DIAGNOSTIC: every stage of a round, in order, and nothing secret in
    /// the log it leaves.
    #[test]
    fn a_traced_round_times_every_stage_and_logs_no_secret() {
        let heard = Arc::new(Heard::default());
        let address = bench_acceptor(Arc::clone(&heard));
        let settings = bench_settings(address, true);
        let mut steps = Vec::new();
        let log = captured(|| {
            let device = CashGenic::traced(&settings, PROBE_TIMEOUT, 77);
            assert!(probe_round(&device).online);
            steps = device.steps();
        });

        let stages = [
            "resolve",
            "tcp_connect",
            "tls_handshake",
            "http_write",
            "http_headers",
            "http_body",
            "json_parse",
        ];
        let expected: Vec<String> = ["token", "system", "status"]
            .iter()
            .flat_map(|call| stages.iter().map(move |step| format!("{call}.{step}")))
            .collect();
        let seen: Vec<String> = steps
            .iter()
            .map(|step| format!("{}.{}", step.call, step.step))
            .collect();
        assert_eq!(seen, expected);
        assert!(steps.iter().all(|step| step.ok));

        let headers = &steps[4].detail;
        assert!(headers.contains("http_status=200"), "{headers}");
        assert!(headers.contains("content-length="), "{headers}");
        assert!(headers.contains("connection=close"), "{headers}");
        assert!(
            steps[2].detail.contains("sni=none(ip-literal)"),
            "{}",
            steps[2].detail
        );

        assert!(
            log.contains("CASHGENIC_TRACE round_id=77 call=token step=tcp_connect"),
            "{log}"
        );
        for secret in ["1234", "bench-token", "ApiUserOne"] {
            assert!(!log.contains(secret), "{secret} reached the log: {log}");
        }
    }

    /// DIAGNOSTIC: what this client does against a device that answers in
    /// full and then keeps the connection open, as a keep-alive server does
    /// until the client closes. The answer is complete by `Content-Length`,
    /// and this client still waits for the close until its read budget runs
    /// out — on every call. Measured, not assumed.
    #[test]
    fn a_connection_held_open_after_the_answer_costs_the_read_budget() {
        let heard = Arc::new(Heard {
            hold_open: Mutex::new(Duration::from_millis(600)),
            ..Heard::default()
        });
        let address = bench_acceptor(Arc::clone(&heard));
        let settings = bench_settings(address, true);
        let budget = Duration::from_millis(300);

        let device = CashGenic::traced(&settings, budget, 78);
        let status = probe_round(&device);
        let steps = device.steps();

        assert!(status.online, "the answer arrived, so the round passes");
        let body = steps
            .iter()
            .find(|step| step.call == "token" && step.step == "http_body")
            .unwrap();
        assert!(body.ok);
        assert!(body.detail.contains("end=error"), "{}", body.detail);
        assert!(body.elapsed >= budget, "{:?}", body.elapsed);
        assert!(
            body.detail.contains("open_after_complete_ms="),
            "{}",
            body.detail
        );
        // Every call pays it: the round costs three read budgets.
        let waited: Vec<&TraceStep> = steps
            .iter()
            .filter(|step| step.step == "http_body")
            .collect();
        assert_eq!(waited.len(), 3);
        assert!(waited.iter().all(|step| step.elapsed >= budget));
    }

    /// The whole conversation, over TLS, against a self-signed certificate.
    #[test]
    fn the_acceptor_is_reached_over_tls_with_a_certificate_it_signed_itself() {
        let heard = Arc::new(Heard::default());
        let address = bench_acceptor(Arc::clone(&heard));
        let settings = CashGenicSettingsV1 {
            enabled: true,
            host: address.ip().to_string(),
            port: address.port(),
            username: "ApiUserOne".to_owned(),
            password: "1234".to_owned(),
        };

        let status = probe(&settings);
        assert!(
            status.online,
            "the handshake or the grant failed: {:?}",
            status.detail
        );
        assert_eq!(status.state, "Idle");
        assert_eq!(status.device.as_deref(), Some("CashGenic 2.1.49"));
        assert_eq!(status.detail, None);

        // The grant the device accepted, read back off the wire.
        let token_request = heard.token_request.lock().unwrap().clone();
        assert!(
            token_request.contains("Content-Type: application/x-www-form-urlencoded"),
            "{token_request}"
        );
        assert!(
            token_request.ends_with("grant_type=password&username=ApiUserOne&password=1234"),
            "{token_request}"
        );

        // And the order the client asks in: a grant, then the two reads, each
        // carrying the token the grant returned.
        let paths = heard.paths.lock().unwrap().clone();
        assert_eq!(
            paths
                .iter()
                .map(|line| line
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" "))
                .collect::<Vec<_>>(),
            vec!["POST /token", "GET /system", "GET /status?eventVersion=2"],
            "one grant, then the two reads it covers"
        );
        let bearers = heard.bearers.lock().unwrap().clone();
        assert_eq!(
            bearers,
            vec!["bench-token", "bench-token"],
            "the same bearer, twice"
        );
    }

    /// And a payment goes the same way, with the amount in pesos.
    #[test]
    fn a_payment_starts_over_tls_with_the_amount_in_pesos() {
        let heard = Arc::new(Heard::default());
        let address = bench_acceptor(Arc::clone(&heard));
        let settings = CashGenicSettingsV1 {
            enabled: true,
            host: address.ip().to_string(),
            port: address.port(),
            username: "ApiUserOne".to_owned(),
            password: "1234".to_owned(),
        };
        let device = CashGenic::new(&settings);
        device
            .pay_amount(18_750)
            .expect("the device took the amount");
        let paths = heard.paths.lock().unwrap().clone();
        assert!(
            paths.iter().any(|line| line.contains("POST /PayAmount")),
            "{paths:?}"
        );
    }

    /// What the unit on the bench reports when a module is unplugged.
    #[test]
    fn a_disconnected_module_is_reported_rather_than_left_looking_healthy() {
        assert_eq!(
            disconnected_modules(&json!({
                "bills": { "connected": false },
                "coins": { "connected": true },
            }))
            .as_deref(),
            Some("el módulo de billetes no está conectado")
        );
        assert_eq!(
            disconnected_modules(&json!({
                "bills": { "connected": false },
                "coins": { "connected": false },
            }))
            .as_deref(),
            Some("el módulo de billetes y monedas no está conectado")
        );
        // Healthy, and firmware that does not say, are both "nothing to
        // report" — never "not connected".
        assert_eq!(
            disconnected_modules(&json!({
                "bills": { "connected": true },
                "coins": { "connected": true },
            })),
            None
        );
        assert_eq!(
            disconnected_modules(&json!({ "systemVersion": "2.1.49" })),
            None
        );
    }

    /// `errorCondition` is the field that says the device cannot take money.
    #[test]
    fn an_error_condition_reaches_the_screen_and_none_does_not() {
        let jammed = progress(
            &json!({ "state": "Idle", "errorCondition": "BillJam" }),
            5_000,
        );
        assert_eq!(jammed.detail.as_deref(), Some("BillJam"));
        let healthy = progress(&json!({ "state": "Idle", "errorCondition": "None" }), 5_000);
        assert_eq!(healthy.detail, None);
    }

    #[test]
    fn an_unconfigured_device_is_refused_before_a_socket_is_opened() {
        let settings = CashGenicSettingsV1 {
            enabled: true,
            ..CashGenicSettingsV1::default()
        };
        let error = CashGenic::new(&settings).system("t").unwrap_err();
        assert_eq!(error.code, "CASHGENIC_NOT_CONFIGURED");
    }
}
