//! `GET /demo/status` — what a floor-monitoring app asks this kiosk.
//!
//! A showroom has a row of these machines and somebody wants one screen that
//! says which are alive. So: one endpoint, polled, answering what this kiosk
//! knows about itself. **Answering at all is the heartbeat** — there is no
//! separate ping, no WebSocket, no registration and no central server in this
//! repository.
//!
//! It lives in the host rather than in `edge-daemon` because the daemon is the
//! process that owns the money, and an HTTP listener is not something to put
//! in front of it: PI.Edge has no HTTP transport and this does not introduce
//! one. The host already holds the identity, the open sign and the day's
//! tally, and it can ask the core for the rest over the protocol it already
//! speaks.
//!
//! Four routes. Two read — the status a floor polls and the products it
//! sells — and two write: open or close the kiosk, and approve an
//! authorization a lane is waiting on.
//!
//! The two writes are the **first path from the network into this kiosk**, and
//! they are gated accordingly: off by default, a PIN an operator set on the
//! machine, and a caller on the local network. A showroom kiosk in a
//! customer's building must not be closable, or its payments releasable, by
//! whoever can reach the port and guess four digits. The reads stay open, as
//! they always were — they only ever told you how a kiosk is.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{PrinterConfigurationV1, PrinterStatusV1};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::showroom;

/// Fixed, because a monitoring app has to find it without being told.
pub const PORT: u16 = 8_777;

/// Every interface, so a floor app on the LAN reaches it as well as loopback.
const BIND: &str = "0.0.0.0";

/// A poller that has stopped reading still gets a complete answer or none.
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

/// What the core last said about the printer and the day.
///
/// Measured: three IPC reads cost about 45 ms each, so building this on the
/// request path put a poll at ~137 ms. Nothing here changes faster than a
/// person can act, and the monitor polls every ten seconds anyway — so the
/// reads moved to a thread and the endpoint answers from memory. With the
/// acceptor cache beside it, a poll touches two small files and nothing else.
#[derive(Debug, Clone)]
struct CoreHealth {
    printer: Value,
    business_day: &'static str,
}

impl Default for CoreHealth {
    fn default() -> Self {
        Self {
            // Before the first round of the watcher, through the very
            // function a real answer goes through — so a poller arriving in
            // the first five seconds cannot see a different shape.
            printer: printer_payload(None, None),
            business_day: "CLOSED",
        }
    }
}

static CORE: OnceLock<RwLock<CoreHealth>> = OnceLock::new();

fn core_health() -> &'static RwLock<CoreHealth> {
    CORE.get_or_init(|| RwLock::new(CoreHealth::default()))
}

/// Ask the core how things are, on its own thread.
fn watch_core(app: tauri::AppHandle, client: Arc<IpcEdgeClient>) {
    std::thread::spawn(move || loop {
        let day = showroom::data_dir(&app)
            .map(|dir| showroom::stored_day(&dir))
            .unwrap_or_default();
        let fresh = CoreHealth {
            printer: printer(&client),
            business_day: business_day(&client, &day),
        };
        if let Ok(mut held) = core_health().write() {
            *held = fresh;
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// Serve until the process ends.
///
/// Spawned at startup and never joined: the kiosk's life is the listener's.
/// A port already in use is reported once and then let go — a second instance
/// on one machine is a demo being set up, not a failure worth a dialog.
pub fn serve(app: tauri::AppHandle, client: Arc<IpcEdgeClient>) {
    watch_core(app.clone(), Arc::clone(&client));
    std::thread::spawn(move || {
        tracing::info!("STATUS_ENDPOINT_START bind={BIND}:{PORT}");
        let listener = match TcpListener::bind((BIND, PORT)) {
            Ok(listener) => listener,
            Err(error) => {
                // An error, not a warning: the floor cannot see this kiosk,
                // and this line is the only place that says why.
                tracing::error!("{}", bind_failure(&error));
                return;
            }
        };
        tracing::info!("STATUS_ENDPOINT_LISTENING bind={BIND}:{PORT}");
        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    // One at a time. A poll every ten seconds from a handful
                    // of machines does not need a thread pool.
                    if let Err(error) = answer(&app, stream) {
                        tracing::debug!(%error, "status request ended early");
                    }
                }
                Err(error) => tracing::debug!(%error, "status listener hiccup"),
            }
        }
    });
}

/// The bind failure as one line: the address, the operating system's own code
/// (`10048` in use, `10013` reserved or forbidden on Windows) and its message.
fn bind_failure(error: &std::io::Error) -> String {
    let os_error = error
        .raw_os_error()
        .map_or_else(|| "none".to_owned(), |code| code.to_string());
    format!(
        "STATUS_ENDPOINT_BIND_FAILED bind={BIND}:{PORT} os_error={os_error} error={:?}",
        error.to_string()
    )
}

fn answer(app: &tauri::AppHandle, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(Some(WRITE_TIMEOUT))?;
    // The peer, before anything is read: the control routes answer an address
    // on the local network and nothing else.
    let from = stream
        .peer_addr()
        .map(|peer| peer.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));

    let request = read_request(&mut stream, from)?;
    let (code, body) = match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/demo/status") => (200, snapshot(app)),
        ("GET", "/demo/products") => products(app),
        ("POST", "/demo/control/kiosk") => control_kiosk(app, &request),
        ("POST", "/demo/control/authorization") => control_authorization(app, &request),
        (method, path) => unrouted(method, path),
    };
    stream.write_all(http(code, &body).as_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

/// Every method and path this endpoint does **not** answer.
///
/// Kept as its own function so the shape of "no" is one decision and is
/// testable without a socket: an unknown path is 404 whatever the method, and
/// a method this endpoint has no route for at all is 405.
fn unrouted(method: &str, path: &str) -> (u16, Value) {
    const READ: [&str; 2] = ["/demo/status", "/demo/products"];
    const WRITE: [&str; 2] = ["/demo/control/kiosk", "/demo/control/authorization"];
    let known = READ.contains(&path) || WRITE.contains(&path);
    match method {
        "GET" | "POST" if !known => (404, json!({ "error": "not found" })),
        // A known path asked with the wrong verb: reading a control route, or
        // posting to a status route.
        "GET" | "POST" => (405, json!({ "error": "method not allowed" })),
        _ => (405, json!({ "error": "method not allowed" })),
    }
}

/// An HTTP/1.1 response a poller can read without a library.
fn http(code: u16, body: &Value) -> String {
    let payload = body.to_string();
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        _ => "Method Not Allowed",
    };
    format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )
}

/// One request, read off the socket.
#[derive(Debug, Clone)]
struct Request {
    method: String,
    path: String,
    body: String,
    /// Who sent it. The control routes answer a private address only.
    from: IpAddr,
}

/// How much of a body this endpoint will read.
///
/// A control POST carries a request id and a PIN. Anything larger is not one,
/// and reading it would be letting a stranger choose how much memory a kiosk
/// spends.
const MAX_BODY: usize = 4_096;

fn read_request(stream: &mut TcpStream, from: IpAddr) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    // Headers, for the one thing a body needs: its length.
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>().unwrap_or(0).min(MAX_BODY);
            }
        }
    }

    let mut body = vec![0_u8; length];
    if length > 0 {
        // Exactly what was declared. A client that promised more than it sent
        // times out on the read rather than being waited on forever, because
        // the socket carries a read timeout.
        reader.read_exact(&mut body)?;
    }
    Ok(Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
        from,
    })
}

/// Whether a network address is one a showroom's own equipment would have.
///
/// Loopback, the three private IPv4 ranges, IPv4 link-local, and IPv6
/// loopback and unique-local. Everything else is refused by the control
/// routes: a kiosk in a customer's building must not be closable, or its
/// payments releasable, from a routed address.
fn is_on_the_local_network(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(four) => four.is_loopback() || four.is_private() || four.is_link_local(),
        // `is_unique_local` is not stable, so fc00::/7 is checked directly,
        // alongside loopback and link-local.
        IpAddr::V6(six) => {
            six.is_loopback()
                || (six.segments()[0] & 0xfe00) == 0xfc00
                || (six.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// A control answer: the shape every POST on this endpoint returns.
///
/// `success`, `error` and `request_id`, always all three and never a null, so
/// a caller branches on `success` and prints `error` without checking for a
/// missing key.
fn control_answer(code: u16, success: bool, error: &str, request_id: &str) -> (u16, Value) {
    (
        code,
        json!({
            "success": success,
            "error": error,
            "request_id": request_id,
        }),
    )
}

/// Whether a control POST may be acted on at all.
///
/// Three gates, in the order that leaks least: whether this kiosk accepts
/// remote control, whether the caller is on the local network, and only then
/// the PIN. A kiosk with the feature off says so without revealing whether a
/// PIN would have been right.
fn may_control(
    preferences: &showroom::KioskPreferencesV1,
    from: IpAddr,
    pin: &str,
    request_id: &str,
) -> Result<(), (u16, Value)> {
    if !preferences.remote_control {
        return Err(control_answer(
            403,
            false,
            "remote control is off on this kiosk; turn it on in Admin",
            request_id,
        ));
    }
    if !is_on_the_local_network(from) {
        return Err(control_answer(
            403,
            false,
            "this endpoint only answers an address on the local network",
            request_id,
        ));
    }
    if preferences.admin_pin.trim().is_empty() {
        return Err(control_answer(
            403,
            false,
            "this kiosk has no PIN set; set one in Admin",
            request_id,
        ));
    }
    if !preferences.pin_matches(pin) {
        return Err(control_answer(
            401,
            false,
            "that PIN is not this kiosk's",
            request_id,
        ));
    }
    Ok(())
}

/// What a control POST carried.
#[derive(Debug, Clone, Default, Deserialize)]
struct ControlBody {
    #[serde(default)]
    action: String,
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    pin: String,
}

fn control_body(raw: &str) -> Result<ControlBody, (u16, Value)> {
    serde_json::from_str::<ControlBody>(raw).map_err(|error| {
        control_answer(
            400,
            false,
            &format!("the body is not the JSON this endpoint takes: {error}"),
            "",
        )
    })
}

/// `POST /demo/control/kiosk` — open or close the kiosk from the floor.
///
/// Exactly what the Admin button does, and nothing more: it is a sign on the
/// door. The application keeps running, Admin stays reachable, the business
/// day is untouched, and a sale already in progress is never interrupted —
/// the shell only draws the closed sign where a *new* order would begin.
fn control_kiosk(app: &tauri::AppHandle, request: &Request) -> (u16, Value) {
    match showroom::data_dir(app) {
        Ok(dir) => kiosk_control(&dir, request),
        Err(_) => control_answer(503, false, "this kiosk has no writable data directory", ""),
    }
}

/// The same, against a directory rather than an app handle.
///
/// Split out so the whole decision — the three gates, the action, and the
/// file that comes back changed — is testable without a window. The handle
/// above resolves one thing and one thing only.
fn kiosk_control(dir: &std::path::Path, request: &Request) -> (u16, Value) {
    let body = match control_body(&request.body) {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let mut preferences = showroom::stored(dir);
    if let Err(refusal) = may_control(&preferences, request.from, &body.pin, "") {
        return refusal;
    }

    let open = match kiosk_action(&body.action) {
        Ok(open) => open,
        Err(refusal) => return refusal,
    };
    preferences.kiosk_open = open;
    if let Err(error) = showroom::write(dir, &preferences) {
        return control_answer(500, false, &error.message, "");
    }
    tracing::info!(open, from = %request.from, "the kiosk was opened or closed remotely");
    (
        200,
        json!({
            "success": true,
            "error": "",
            "request_id": "",
            "kiosk_open": open,
        }),
    )
}

/// `OPEN` or `CLOSE`, and nothing else.
///
/// Case and whitespace are forgiven because a caller types this by hand;
/// anything else is refused rather than guessed at. There is deliberately no
/// `TOGGLE`: a floor app that does not know whether a kiosk is open should
/// read `/demo/status`, not ask the kiosk to flip.
fn kiosk_action(action: &str) -> Result<bool, (u16, Value)> {
    match action.trim().to_ascii_uppercase().as_str() {
        "OPEN" => Ok(true),
        "CLOSE" => Ok(false),
        other => Err(control_answer(
            400,
            false,
            &format!("`{other}` is not an action; send OPEN or CLOSE"),
            "",
        )),
    }
}

/// `POST /demo/control/authorization` — approve a waiting authorization.
///
/// The remote half of the same gate the screen's PIN keypad releases, through
/// the same state machine: one request id, one sale, one set of products. A
/// request id the basket has already replaced is refused as such rather than
/// silently releasing the newer one.
fn control_authorization(app: &tauri::AppHandle, request: &Request) -> (u16, Value) {
    match showroom::data_dir(app) {
        Ok(dir) => authorization_control(&dir, request),
        Err(_) => control_answer(503, false, "this kiosk has no writable data directory", ""),
    }
}

/// The same, against a directory rather than an app handle.
fn authorization_control(dir: &std::path::Path, request: &Request) -> (u16, Value) {
    let body = match control_body(&request.body) {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let request_id = body.request_id.trim().to_owned();
    let preferences = showroom::stored(dir);
    if let Err(refusal) = may_control(&preferences, request.from, &body.pin, &request_id) {
        return refusal;
    }

    match crate::authorization::approve(&request_id, preferences.pin_matches(&body.pin)) {
        Ok(approved) => {
            tracing::info!(
                request_id = %approved.request_id,
                sale_id = %approved.sale_id,
                from = %request.from,
                "an authorization was approved remotely"
            );
            (
                200,
                json!({
                    "success": true,
                    "error": "",
                    "request_id": approved.request_id,
                    "sale_id": approved.sale_id,
                }),
            )
        }
        Err(refusal) => control_answer(409, false, refusal.detail(), &request_id),
    }
}

/// `GET /demo/products` — what the active experience sells.
///
/// Read-only, and the **same** list Admin draws, through the same function:
/// a floor app and the operator standing at the machine must not be able to
/// disagree about what is on the shelf or what it costs.
fn products(app: &tauri::AppHandle) -> (u16, Value) {
    use tauri::Manager;
    let Ok(dir) = showroom::data_dir(app) else {
        return (
            503,
            json!({ "error": "this kiosk has no writable data directory" }),
        );
    };
    let preferences = showroom::stored(&dir);
    let overrides = crate::overrides::stored(&dir);
    let bridge = app.state::<crate::EdgeBridge>();

    match crate::kiosk::products_of(&bridge, Some(preferences.channel_ref.clone()), &overrides) {
        Ok(products) => (200, products_payload(&preferences, &products)),
        Err(error) => (503, json!({ "error": error.message })),
    }
}

/// The shape `GET /demo/products` answers.
///
/// `price` is the decimal a store app prints and `price_minor` the integer it
/// should compare — both, because publishing only the decimal invites
/// somebody to do arithmetic on it. No nulls: `sku` and `category` are empty
/// strings where the catalog has none.
fn products_payload(
    preferences: &showroom::KioskPreferencesV1,
    products: &[crate::kiosk::AdminProductV1],
) -> Value {
    json!({
        "vertical": preferences.vertical,
        "channel_ref": preferences.channel_ref,
        "products": products
            .iter()
            .map(|product| json!({
                "product_ref": product.product_ref,
                "sku": product.sku,
                "name": product.name,
                "category": product.category_name,
                "category_ref": product.category_ref,
                "price": major(product.price_minor),
                "price_minor": product.price_minor,
                "available": product.available,
                "authorization_required": product.authorization_required,
            }))
            .collect::<Vec<Value>>(),
    })
}

/// Everything this kiosk can honestly say about itself, right now.
fn snapshot(app: &tauri::AppHandle) -> Value {
    let dir = showroom::data_dir(app).ok();
    let preferences = dir
        .as_ref()
        .map(|dir| showroom::stored(dir))
        .unwrap_or_default();
    let day = dir
        .as_ref()
        .map(|dir| showroom::stored_day(dir))
        .unwrap_or_default();

    let core = core_health()
        .read()
        .map(|held| held.clone())
        .unwrap_or_default();
    payload(
        &preferences,
        &day,
        core.printer,
        cashgenic(&preferences),
        core.business_day,
        crate::authorization::status(),
    )
}

/// The shape a floor monitor reads, from parts that are already decided.
fn payload(
    preferences: &showroom::KioskPreferencesV1,
    day: &showroom::DayTallyV1,
    printer: Value,
    cashgenic: Value,
    business_day: &str,
    authorization: Value,
) -> Value {
    // Computed from the very blocks published below, so an alert and the
    // flags beside it can never disagree.
    let alerts = crate::alerts::raise(&printer, &cashgenic, &authorization);
    json!({
        "branch_id": preferences.branch_id,
        "kiosk_id": preferences.kiosk_id,
        "cashier_id": preferences.cashier_id,
        "vertical": preferences.vertical,
        "kiosk_open": preferences.kiosk_open,
        // The process answering this request is the process running the
        // kiosk, so both of these are true by construction. They are in the
        // payload because the poller's contract asks for them, and because a
        // monitor that gets no answer at all is how "off" is reported.
        "kiosk_on": true,
        "app_running": true,
        "printer": printer,
        "cashgenic": cashgenic,
        "sales": {
            "transaction_count": day.transaction_count,
            "day_state": day.state,
            "day_closed_at": day.closed_at,
            "sales_total": major(day.sales_total_minor),
            "cash_total": major(day.cash_total_minor),
            "card_total": major(day.card_total_minor),
        },
        "business_day": business_day,
        // What, if anything, this lane is waiting for somebody to come over
        // and release. `required: false` with an empty product list when
        // nothing is — never a null, so a floor app branches on `required`
        // and reads the rest without checking for absence.
        "authorization": authorization,
        // What is wrong here, and nothing else — the list a console by
        // exception renders verbatim. Empty when this kiosk is well, never
        // null, and ordered most urgent first so the first entry is the one
        // to act on. `KIOSK_OFFLINE` is not in it and cannot be: a kiosk that
        // is off does not answer this request, which is how "off" is
        // reported (see the module header).
        "alerts": alerts,
        // Whether this kiosk would accept a control POST at all. Published so
        // a floor app can tell "it refused my PIN" from "it is not listening
        // for me" without guessing from a 403.
        "remote_control": preferences.remote_control,
    })
}

/// What the printer is, from the core's own status read and its model.
///
/// The cached answer, not a fresh one: the poller has a three second budget
/// and a status read can wait on a socket. The core refreshes it every
/// fifteen seconds, before every payment and after every document, so what is
/// here is at most fifteen seconds old.
fn printer(client: &IpcEdgeClient) -> Value {
    printer_payload(
        client.printer_settings().ok().as_ref(),
        client.printer_status(false).ok().as_ref(),
    )
}

/// The printer block a store app polls, from parts that are already decided.
///
/// Every field carries a value and **none of them is null**: a floor app has
/// to be able to branch without checking for absence, and `UNKNOWN` / `""`
/// say "nothing answered" more clearly than a missing key. What is published
/// is what somebody answered — the device's firmware over `DLE EOT`, or the
/// host spooler's own flags — never an inference from a print having
/// succeeded, which is not a sensor (ADR 035).
///
/// `status_source` says which of the two can answer for this printer, and
/// `status_checked_at` whether it has yet. The two together are how a floor
/// tells "this printer cannot say" from "it has not said".
fn printer_payload(
    settings: Option<&PrinterConfigurationV1>,
    status: Option<&PrinterStatusV1>,
) -> Value {
    let model = settings.and_then(|configuration| {
        let chosen = &configuration.settings.model;
        configuration
            .models
            .iter()
            .find(|entry| &entry.id == chosen)
    });

    json!({
        // What the device is.
        "configured": settings.is_some_and(|c| c.settings.enabled),
        "queue": settings.map(|c| c.settings.queue.clone()).unwrap_or_default(),
        "model": model.map(|entry| entry.id.clone()).unwrap_or_default(),
        "model_label": model.map(|entry| entry.label.clone()).unwrap_or_default(),
        "driver": settings.map(|c| c.driver.clone()).unwrap_or_default(),
        // What this model *could* say, which is not what it has said.
        "reports_paper": model.is_some_and(|entry| entry.reports_paper),
        "reports_jam": model.is_some_and(|entry| entry.reports_jam),
        "paper_states": model
            .map(|entry| entry.paper_states.clone())
            .unwrap_or_else(|| vec!["UNKNOWN".to_owned()]),

        // What it says. The contract a store app polls.
        "online": status.is_some_and(|status| status.online),
        "state": status
            .map(|status| status.state.clone())
            .unwrap_or_else(|| "UNKNOWN".to_owned()),
        "cover_open": status.is_some_and(|status| status.cover_open),
        "paper": status
            .map(|status| status.paper.clone())
            .unwrap_or_else(|| "UNKNOWN".to_owned()),
        "cutter_error": status.is_some_and(|status| status.cutter_error),
        "jammed": status.is_some_and(|status| status.jammed),
        "unrecoverable_error": status.is_some_and(|status| status.unrecoverable_error),
        "status_source": status
            .map(|status| status.status_source.clone())
            .unwrap_or_else(|| "NONE".to_owned()),
        // Empty rather than null, for both of these: a poller branches on
        // truthiness and never on a missing key.
        "status_checked_at": status
            .and_then(|status| status.status_checked_at.clone())
            .unwrap_or_default(),
        "status_error": status
            .and_then(|status| status.status_error.clone())
            .unwrap_or_default(),
        // Whether this printer is currently stopping sales, as the core
        // decided it — published so a floor sees the same answer the kiosk
        // acted on rather than re-deriving it from the flags.
        "blocks_payment": status.is_some_and(|status| status.blocks_payment),
    })
}

/// The note recycler: whether it is on, whether it answers, and what it holds.
///
/// Not probed unless it is enabled — a kiosk with no acceptor must not spend
/// a poll's budget dialling one. `cash_total` is `null` when the device does
/// not report it rather than zero, because zero is a number a floor would act
/// on.
fn cashgenic(preferences: &showroom::KioskPreferencesV1) -> Value {
    published_cashgenic(preferences, crate::cashgenic::cached_health)
}

/// The entry itself, given where the last answer comes from — which is only
/// asked when the acceptor is enabled.
fn published_cashgenic(
    preferences: &showroom::KioskPreferencesV1,
    last_answer: impl FnOnce() -> crate::cashgenic::CachedHealth,
) -> Value {
    if !preferences.cashgenic.enabled {
        return json!({
            "enabled": false,
            "online": false,
            "cash_total": Value::Null,
            "last_checked_at": Value::Null,
        });
    }
    // The last answer, not a fresh one. Dialling here is what used to make a
    // poll take four seconds when the acceptor was unplugged, and a floor
    // monitor with a three second budget would then call the whole kiosk
    // offline over a peripheral.
    let health = last_answer();
    json!({
        "enabled": true,
        "online": health.status.online,
        "state": health.status.state,
        "device": health.status.device,
        "cash_total": health.status.cash_total_minor.map(major),
        "last_checked_at": health.last_checked_at,
    })
}

/// `OPEN` or `CLOSED`.
///
/// The core's business day is the authority when it has one — it is what
/// decides whether a sale may be made at all. The showroom's own day only
/// answers when the core has nothing to say.
fn business_day(client: &IpcEdgeClient, day: &showroom::DayTallyV1) -> &'static str {
    match client.get_edge_state() {
        Ok(state) => match state.business_day {
            Some(open) if open.status == "OPEN" && day.is_open() => "OPEN",
            Some(_) => "CLOSED",
            None => "CLOSED",
        },
        Err(_) if day.is_open() => "OPEN",
        Err(_) => "CLOSED",
    }
}

/// Minor units as the decimal a monitoring screen prints.
///
/// Rendered as text and handed to the JSON parser, so the number in the
/// payload is exactly the amount the till holds. Money does not travel
/// through a float here any more than it does anywhere else (CLAUDE.md).
fn major(minor: i64) -> Value {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.abs();
    serde_json::from_str(&format!("{sign}{}.{:02}", abs / 100, abs % 100))
        .unwrap_or_else(|_| json!(0))
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::showroom::{CashGenicSettingsV1, DayTallyV1, KioskPreferencesV1, Vertical};

    fn kiosk() -> KioskPreferencesV1 {
        KioskPreferencesV1 {
            vertical: Vertical::TimHortons,
            channel_ref: "KIOSK-TIM".to_owned(),
            camera_preview: false,
            branch_id: "001".to_owned(),
            kiosk_id: "K01".to_owned(),
            cashier_id: "CAJERO01".to_owned(),
            kiosk_open: true,
            cashgenic: CashGenicSettingsV1::default(),
            // A kiosk nobody has set up for remote control, which is what
            // one ships as: no PIN, and the control routes not answering.
            // `controllable()` below is the one that was set up.
            admin_pin: String::new(),
            remote_control: false,
        }
    }

    fn day() -> DayTallyV1 {
        DayTallyV1 {
            state: "OPEN".to_owned(),
            transaction_count: 42,
            sales_total_minor: 853_050,
            cash_total_minor: 453_050,
            card_total_minor: 400_000,
            opened_at: None,
            closed_at: None,
        }
    }

    fn sample() -> Value {
        payload(
            &kiosk(),
            &day(),
            json!({ "online": true, "paper": "UNKNOWN" }),
            json!({ "enabled": true, "online": true, "cash_total": 125.0 }),
            "OPEN",
            crate::authorization::AuthorizationV1::none().into_wire(),
        )
    }

    /// The payload a floor monitor is promised, field by field.
    #[test]
    fn the_status_carries_everything_the_contract_asks_for() {
        let body = sample();
        assert_eq!(body["branch_id"], "001");
        assert_eq!(body["kiosk_id"], "K01");
        assert_eq!(body["cashier_id"], "CAJERO01");
        assert_eq!(body["kiosk_open"], true);
        assert_eq!(body["app_running"], true);
        assert_eq!(body["business_day"], "OPEN");
        assert_eq!(body["printer"]["online"], true);
        assert_eq!(body["printer"]["paper"], "UNKNOWN");
        assert_eq!(body["cashgenic"]["enabled"], true);
        assert_eq!(body["cashgenic"]["online"], true);
        assert_eq!(body["sales"]["transaction_count"], 42);
        assert_eq!(body["sales"]["sales_total"], json!(8530.5));
        assert_eq!(body["sales"]["cash_total"], json!(4530.5));
        assert_eq!(body["sales"]["card_total"], json!(4000.0));
        // Which brand the screen is wearing, so a floor can see it at a glance.
        assert_eq!(body["vertical"], "TIM_HORTONS");
        // A well kiosk carries the key with nothing in it. Never a null, and
        // never absent: a console branches on the array, not on the key.
        assert_eq!(body["alerts"], json!([]));
    }

    /// The whole point of the block: what is wrong travels decided, beside
    /// the flags it was decided from.
    #[test]
    fn a_kiosk_with_something_wrong_carries_it_as_an_alert() {
        let body = payload(
            &kiosk(),
            &day(),
            json!({
                "configured": true,
                "online": false,
                "state": "NO_RESPONSE",
                "paper": "UNKNOWN",
                "cover_open": false,
            }),
            json!({ "enabled": true, "online": false }),
            "OPEN",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(
            body["alerts"],
            json!([
                {
                    "code": "PRINTER_OFFLINE",
                    "severity": "BLOCKING",
                    "message": "Impresora desconectada",
                },
                {
                    "code": "CASHGENIC_OFFLINE",
                    "severity": "ACTION",
                    "message": "CashGenic desconectado",
                },
            ])
        );
    }

    /// A closed kiosk still reports its faults. Low paper at seven in the
    /// morning is worth knowing *before* the lane opens, so the list does not
    /// depend on `kiosk_open`.
    #[test]
    fn a_closed_kiosk_still_reports_what_is_wrong_with_it() {
        let mut closed = kiosk();
        closed.kiosk_open = false;
        let body = payload(
            &closed,
            &day(),
            json!({
                "configured": true,
                "online": true,
                "state": "ONLINE",
                "paper": "NEAR_END",
                "cover_open": false,
            }),
            json!({ "enabled": false, "online": false }),
            "CLOSED",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["kiosk_open"], false);
        assert_eq!(body["alerts"][0]["code"], "PRINTER_PAPER_LOW");
        assert_eq!(body["alerts"][0]["severity"], "WARN");
    }

    #[test]
    fn a_closed_kiosk_says_so_without_going_quiet() {
        let mut closed = kiosk();
        closed.kiosk_open = false;
        let body = payload(
            &closed,
            &day(),
            json!({}),
            json!({}),
            "OPEN",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["kiosk_open"], false);
        // Still answering: a closed kiosk is not an offline one.
        assert_eq!(body["app_running"], true);
    }

    #[test]
    fn a_closed_day_reports_closed_and_keeps_its_figures() {
        let mut ended = day();
        ended.state = "CLOSED".to_owned();
        let body = payload(
            &kiosk(),
            &ended,
            json!({}),
            json!({}),
            "CLOSED",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["business_day"], "CLOSED");
        assert_eq!(body["sales"]["transaction_count"], 42);
    }

    /// Four routes, and an honest answer for everything else.
    #[test]
    fn the_endpoint_answers_its_four_paths_and_refuses_the_rest() {
        // A path nobody serves is 404 whichever verb asked.
        for method in ["GET", "POST"] {
            assert_eq!(
                unrouted(method, "/demo/other").0,
                404,
                "{method} /demo/other"
            );
            assert_eq!(unrouted(method, "/").0, 404, "{method} /");
        }
        // A real path asked the wrong way is 405: posting to a status route,
        // or reading a control route.
        for (method, path) in [
            ("POST", "/demo/status"),
            ("POST", "/demo/products"),
            ("GET", "/demo/control/kiosk"),
            ("GET", "/demo/control/authorization"),
        ] {
            assert_eq!(unrouted(method, path).0, 405, "{method} {path}");
        }
        // And a verb this endpoint has no business with at all.
        for method in ["PUT", "DELETE", "PATCH", "OPTIONS"] {
            assert_eq!(unrouted(method, "/demo/status").0, 405, "{method}");
        }
    }

    #[test]
    fn an_enabled_acceptor_that_answers_is_published_enabled_and_online() {
        let mut preferences = kiosk();
        preferences.cashgenic.enabled = true;
        let health = crate::cashgenic::CachedHealth {
            status: crate::cashgenic::CashGenicStatusV1 {
                online: true,
                device: Some("CashGenic 2.1.49".to_owned()),
                state: "Idle".to_owned(),
                deposited_minor: 0,
                outstanding_minor: 0,
                change_minor: 0,
                cash_total_minor: Some(12_550),
                detail: None,
            },
            last_checked_at: Some("2026-11-03T12:00:00+00:00".to_owned()),
        };
        let body = payload(
            &preferences,
            &day(),
            json!({ "online": true, "paper": "UNKNOWN" }),
            published_cashgenic(&preferences, || health),
            "OPEN",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["cashgenic"]["enabled"], true);
        assert_eq!(body["cashgenic"]["online"], true);
        assert_eq!(body["cashgenic"]["state"], "Idle");
        assert_eq!(body["cashgenic"]["cash_total"], json!(125.5));
        assert_eq!(
            body["cashgenic"]["last_checked_at"],
            "2026-11-03T12:00:00+00:00"
        );
    }

    #[test]
    fn a_disabled_acceptor_is_published_without_reading_the_cache() {
        let body = published_cashgenic(&kiosk(), || panic!("the cache was read"));
        assert_eq!(body["enabled"], false);
        assert_eq!(body["online"], false);
    }

    #[test]
    fn a_bind_failure_says_where_and_what_the_os_said() {
        // A real refusal, not a constructed one: a port somebody already holds.
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let error = TcpListener::bind(held.local_addr().unwrap()).unwrap_err();
        let line = bind_failure(&error);
        assert!(
            line.starts_with("STATUS_ENDPOINT_BIND_FAILED bind=0.0.0.0:8777 os_error="),
            "{line}"
        );
        let code = error.raw_os_error().unwrap();
        assert!(line.contains(&format!("os_error={code} ")), "{line}");
        assert!(
            line.contains(&format!("error={:?}", error.to_string())),
            "{line}"
        );
    }

    #[test]
    fn the_response_is_http_a_poller_can_read() {
        let body = sample();
        let response = http(200, &body);
        let (head, payload) = response.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("Content-Type: application/json"));
        assert!(head.contains(&format!("Content-Length: {}", payload.len())));
        // A browser-based monitor on another machine has to be able to read it.
        assert!(head.contains("Access-Control-Allow-Origin: *"));
        assert!(head.contains("Connection: close"));
        assert_eq!(
            serde_json::from_str::<Value>(payload).unwrap()["kiosk_id"],
            "K01"
        );
    }

    /// The payload a kiosk nobody has configured still answers with.
    #[test]
    fn an_unnamed_kiosk_still_answers() {
        let body = payload(
            &KioskPreferencesV1::default(),
            &DayTallyV1::default(),
            json!({ "online": false }),
            json!({ "enabled": false, "online": false }),
            "CLOSED",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["branch_id"], "");
        assert_eq!(body["sales"]["transaction_count"], 0);
        assert_eq!(body["sales"]["sales_total"], json!(0.0));
        assert_eq!(body["cashgenic"]["enabled"], false);
    }

    /// The payload a kiosk nobody has configured still answers with.
    #[test]
    fn amounts_are_carried_as_pesos_to_the_cent() {
        assert_eq!(major(853_050), json!(8530.5));
        assert_eq!(major(0), json!(0.0));
        assert_eq!(major(1_250), json!(12.5));
        assert_eq!(major(5), json!(0.05));
        assert_eq!(major(-1_250), json!(-12.5));
        assert_eq!(major(1), json!(0.01));
        assert_eq!(major(99), json!(0.99));
    }

    // ----------------------------------------------------- the printer block
    //
    // The contract a store app polls, and the one the brief is strictest
    // about: every field present, every field a value.

    /// Every field the contract names, and not one of them null.
    #[test]
    fn the_printer_block_carries_every_field_with_a_value() {
        const PROMISED: [&str; 11] = [
            "online",
            "state",
            "cover_open",
            "paper",
            "cutter_error",
            "jammed",
            "unrecoverable_error",
            "status_source",
            "status_checked_at",
            "status_error",
            "blocks_payment",
        ];
        // Both ends of the range: a kiosk nobody has configured, and one with
        // a printer in trouble.
        for (what, block) in [
            ("an unconfigured kiosk", printer_payload(None, None)),
            (
                "a printer with its cover open",
                printer_payload(
                    None,
                    Some(&PrinterStatusV1 {
                        online: false,
                        state: "OFFLINE".to_owned(),
                        cover_open: true,
                        paper: "NEAR_END".to_owned(),
                        cutter_error: false,
                        jammed: false,
                        unrecoverable_error: false,
                        status_source: "DLE_EOT".to_owned(),
                        status_checked_at: Some("2026-11-02T10:00:00Z".to_owned()),
                        status_error: Some("the cover is open".to_owned()),
                        blocks_payment: true,
                    }),
                ),
            ),
        ] {
            for field in PROMISED {
                let value = block.get(field);
                assert!(value.is_some(), "{what} omits `{field}`");
                assert!(
                    !value.is_some_and(Value::is_null),
                    "{what} publishes `{field}` as null"
                );
            }
        }
    }

    /// A kiosk with nothing configured says so without claiming a sensor.
    #[test]
    fn an_unconfigured_printer_claims_nothing_and_stops_nothing() {
        let block = printer_payload(None, None);
        assert_eq!(block["state"], "UNKNOWN");
        assert_eq!(block["paper"], "UNKNOWN");
        assert_eq!(block["status_source"], "NONE");
        assert_eq!(block["online"], false);
        assert_eq!(block["configured"], false);
        // Never asked, and said so as an empty string rather than a null.
        assert_eq!(block["status_checked_at"], "");
        assert_eq!(block["status_error"], "");
        assert_eq!(
            block["blocks_payment"], false,
            "a kiosk that cannot ask closed the lane"
        );
    }

    /// The three conditions, as a store app would read them off the wire.
    #[test]
    fn the_three_conditions_that_stop_a_payment_are_visible_on_the_wire() {
        let status = |state: &str, paper: &str, cover: bool| PrinterStatusV1 {
            online: false,
            state: state.to_owned(),
            cover_open: cover,
            paper: paper.to_owned(),
            cutter_error: false,
            jammed: false,
            unrecoverable_error: false,
            status_source: "DLE_EOT".to_owned(),
            status_checked_at: Some("2026-11-02T10:00:00Z".to_owned()),
            status_error: Some("something".to_owned()),
            // The core's answer, carried verbatim — this block never
            // re-derives it from the flags.
            blocks_payment: true,
        };
        for (what, reported) in [
            ("the cover", status("OFFLINE", "OK", true)),
            ("no paper", status("OFFLINE", "END", false)),
            ("no answer", status("NO_RESPONSE", "UNKNOWN", false)),
        ] {
            let block = printer_payload(None, Some(&reported));
            assert_eq!(block["blocks_payment"], true, "{what}");
            assert_ne!(block["status_error"], "", "{what} said nothing useful");
        }

        // And the one that does not: a low roll still sells.
        let low = PrinterStatusV1 {
            online: true,
            state: "ONLINE".to_owned(),
            paper: "NEAR_END".to_owned(),
            blocks_payment: false,
            status_error: None,
            ..status("ONLINE", "NEAR_END", false)
        };
        let block = printer_payload(None, Some(&low));
        assert_eq!(block["paper"], "NEAR_END");
        assert_eq!(block["blocks_payment"], false);
        assert_eq!(block["status_error"], "");
    }

    // ------------------------------------------------- the control endpoints
    //
    // The first write path from the network into this kiosk, so the gate is
    // what these pin down rather than the happy path.

    fn controllable() -> KioskPreferencesV1 {
        KioskPreferencesV1 {
            admin_pin: "4821".to_owned(),
            remote_control: true,
            ..kiosk()
        }
    }

    const FROM_LAN: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 88, 20));

    /// A showroom's own equipment, and everything else.
    #[test]
    fn only_an_address_on_the_local_network_may_control_a_kiosk() {
        for allowed in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.4.1",
            "172.31.255.254",
            "192.168.88.20",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(
                is_on_the_local_network(allowed.parse().unwrap()),
                "`{allowed}` is a local address and was refused"
            );
        }
        for refused in [
            "8.8.8.8",
            "172.32.0.1",
            "11.0.0.1",
            "203.0.113.7",
            "2001:4860:4860::8888",
        ] {
            assert!(
                !is_on_the_local_network(refused.parse().unwrap()),
                "`{refused}` is a routed address and was allowed to control a kiosk"
            );
        }
    }

    /// Off by default, and off means off — whatever PIN is offered.
    #[test]
    fn a_kiosk_that_was_not_asked_to_be_controlled_refuses_before_the_pin() {
        let preferences = KioskPreferencesV1 {
            admin_pin: "4821".to_owned(),
            remote_control: false,
            ..kiosk()
        };
        let (code, body) = may_control(&preferences, FROM_LAN, "4821", "r-1").unwrap_err();
        assert_eq!(code, 403);
        assert_eq!(body["success"], false);
        assert!(
            body["error"].as_str().unwrap_or_default().contains("off"),
            "{body}"
        );
        // And it does not reveal that the PIN was right.
        assert_eq!(body["request_id"], "r-1");
    }

    /// A kiosk with no PIN set authorises nothing, remotely or otherwise.
    #[test]
    fn a_kiosk_with_no_pin_cannot_be_controlled_at_all() {
        let preferences = KioskPreferencesV1 {
            admin_pin: String::new(),
            remote_control: true,
            ..kiosk()
        };
        // Including by offering the empty PIN it does not have.
        for offered in ["", "0000"] {
            let (code, body) = may_control(&preferences, FROM_LAN, offered, "").unwrap_err();
            assert_eq!(code, 403, "`{offered}`");
            assert!(
                body["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("no PIN"),
                "{body}"
            );
        }
    }

    #[test]
    fn the_wrong_pin_is_refused_and_the_right_one_is_not() {
        let preferences = controllable();
        let (code, body) = may_control(&preferences, FROM_LAN, "0000", "r-1").unwrap_err();
        assert_eq!(code, 401);
        assert_eq!(body["success"], false);
        assert_eq!(body["request_id"], "r-1");

        assert!(may_control(&preferences, FROM_LAN, "4821", "r-1").is_ok());
        // Typed with a stray space, as a keypad sends it.
        assert!(may_control(&preferences, FROM_LAN, " 4821 ", "r-1").is_ok());
    }

    /// Even with the feature on and the right PIN, not from the internet.
    #[test]
    fn a_routed_address_is_refused_even_with_the_right_pin() {
        let (code, body) =
            may_control(&controllable(), "203.0.113.7".parse().unwrap(), "4821", "").unwrap_err();
        assert_eq!(code, 403);
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("local network"),
            "{body}"
        );
    }

    /// Every control answer is the same three fields, and never a null.
    #[test]
    fn a_control_answer_always_carries_success_error_and_request_id() {
        for (code, body) in [
            control_answer(200, true, "", "r-1"),
            control_answer(401, false, "that PIN is not this kiosk's", "r-1"),
            may_control(&kiosk(), FROM_LAN, "x", "r-1").unwrap_err(),
        ] {
            assert!((200..=503).contains(&code));
            for field in ["success", "error", "request_id"] {
                assert!(!body[field].is_null(), "`{field}` is null in {body}");
            }
            assert!(body["success"].is_boolean());
            assert!(body["error"].is_string());
            assert!(body["request_id"].is_string());
        }
    }

    /// A body that is not the JSON this endpoint takes says so.
    #[test]
    fn a_malformed_control_body_is_refused_with_its_reason() {
        let (code, body) = control_body("{ not json").unwrap_err();
        assert_eq!(code, 400);
        assert_eq!(body["success"], false);
        assert!(!body["error"].as_str().unwrap_or_default().is_empty());

        // The two real shapes parse.
        let kiosk = control_body(r#"{"action":"CLOSE","pin":"4821"}"#).unwrap();
        assert_eq!(kiosk.action, "CLOSE");
        assert_eq!(kiosk.pin, "4821");
        let authorization = control_body(r#"{"request_id":"r-1","pin":"4821"}"#).unwrap();
        assert_eq!(authorization.request_id, "r-1");
        // A field the other shape has is simply absent, not an error.
        assert_eq!(authorization.action, "");
    }

    /// Two actions, forgiving about how they are typed and nothing else.
    #[test]
    fn the_kiosk_is_opened_or_closed_and_never_toggled() {
        for (typed, expected) in [
            ("OPEN", true),
            ("open", true),
            (" Open ", true),
            ("CLOSE", false),
            ("close", false),
            ("\tCLOSE\n", false),
        ] {
            assert_eq!(kiosk_action(typed), Ok(expected), "`{typed}`");
        }
        for refused in ["TOGGLE", "", "OPENN", "SHUTDOWN", "1", "true"] {
            let (code, body) = kiosk_action(refused).unwrap_err();
            assert_eq!(code, 400, "`{refused}`");
            assert_eq!(body["success"], false);
            assert!(
                body["error"].as_str().unwrap_or_default().contains("OPEN"),
                "{body}"
            );
        }
    }

    /// A request read off a socket, headers and body.
    #[test]
    fn a_body_is_read_to_the_length_the_client_declared() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let payload = r#"{"request_id":"r-1","pin":"4821"}"#;
        std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .write_all(
                    format!(
                        "POST /demo/control/authorization HTTP/1.1\r\nHost: x\r\n\
                         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                        payload.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            client.flush().unwrap();
        });
        let (mut accepted, _) = listener.accept().unwrap();
        accepted
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let request = read_request(&mut accepted, FROM_LAN).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/demo/control/authorization");
        assert_eq!(request.body, payload);
        assert_eq!(control_body(&request.body).unwrap().pin, "4821");
    }

    /// A GET carries no body, and the reader does not wait for one.
    #[test]
    fn a_get_is_read_without_waiting_for_a_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            let _ = client.write_all(b"GET /demo/status HTTP/1.1\r\nHost: x\r\n\r\n");
            let _ = client.flush();
        });
        let (mut accepted, _) = listener.accept().unwrap();
        accepted
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let request = read_request(&mut accepted, FROM_LAN).unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/demo/status");
        assert_eq!(request.body, "");
    }

    // ------------------------------------ the control routes, end to end
    //
    // Everything but the socket: the gates, the action, the state machine,
    // and the file that comes back changed.

    /// A directory holding a kiosk that was set up for remote control.
    fn controllable_kiosk() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        showroom::write(dir.path(), &controllable()).unwrap();
        dir
    }

    fn post(path: &str, body: &str, from: IpAddr) -> Request {
        Request {
            method: "POST".to_owned(),
            path: path.to_owned(),
            body: body.to_owned(),
            from,
        }
    }

    /// Closing and opening a kiosk from the floor.
    #[test]
    fn a_kiosk_is_closed_and_opened_remotely_and_the_file_says_so() {
        let dir = controllable_kiosk();
        assert!(
            showroom::stored(dir.path()).kiosk_open,
            "a kiosk opens open"
        );

        let (code, body) = kiosk_control(
            dir.path(),
            &post(
                "/demo/control/kiosk",
                r#"{"action":"CLOSE","pin":"4821"}"#,
                FROM_LAN,
            ),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(body["success"], true);
        assert_eq!(body["error"], "");
        assert_eq!(body["kiosk_open"], false);
        // The stored sign, which is what the shell reads.
        assert!(!showroom::stored(dir.path()).kiosk_open);

        let (code, body) = kiosk_control(
            dir.path(),
            &post(
                "/demo/control/kiosk",
                r#"{"action":"OPEN","pin":"4821"}"#,
                FROM_LAN,
            ),
        );
        assert_eq!(code, 200);
        assert_eq!(body["kiosk_open"], true);
        assert!(showroom::stored(dir.path()).kiosk_open);
    }

    /// One sign, written by both hands.
    ///
    /// Admin on the machine and a floor app over the control route are the
    /// same field in the same file, and `kiosk_preferences` — the command the
    /// shell polls — answers out of that file. If these ever came apart, a
    /// kiosk closed from the floor would keep taking orders, which is the bug
    /// this pins down.
    #[test]
    fn the_sign_admin_writes_and_the_sign_a_floor_app_writes_are_one_field() {
        let dir = controllable_kiosk();

        // What Admin does: read, flip, write. `set_kiosk_open` is this plus
        // an app handle to find the directory with.
        let mut local = showroom::stored(dir.path());
        local.kiosk_open = false;
        showroom::write(dir.path(), &local).unwrap();
        assert!(!showroom::stored(dir.path()).kiosk_open);

        // And the floor app opens the same one again.
        let (code, _) = kiosk_control(
            dir.path(),
            &post(
                "/demo/control/kiosk",
                r#"{"action":"OPEN","pin":"4821"}"#,
                FROM_LAN,
            ),
        );
        assert_eq!(code, 200);
        assert!(
            showroom::stored(dir.path()).kiosk_open,
            "the floor and Admin are writing different signs"
        );

        // Closing remotely changes the sign and nothing else about the lane:
        // the vertical the shell is mounted on is untouched, which is why a
        // poll may adopt the sign and must not adopt the rest.
        let before = showroom::stored(dir.path());
        kiosk_control(
            dir.path(),
            &post(
                "/demo/control/kiosk",
                r#"{"action":"CLOSE","pin":"4821"}"#,
                FROM_LAN,
            ),
        );
        let after = showroom::stored(dir.path());
        assert!(!after.kiosk_open);
        assert_eq!(after.vertical, before.vertical);
        assert_eq!(after.channel_ref, before.channel_ref);
        assert_eq!(after.admin_pin, before.admin_pin);
        assert_eq!(after.remote_control, before.remote_control);
    }

    /// The sign and the accounting day are different states.
    ///
    /// A kiosk can be closed to customers on an open business day — that is
    /// exactly what closing one on a showroom floor means. The shell reads
    /// `kiosk_open`; anything reading `business_day` for this would draw the
    /// wrong screen.
    #[test]
    fn the_sign_and_the_business_day_are_published_apart() {
        let mut closed = kiosk();
        closed.kiosk_open = false;
        let body = payload(
            &closed,
            &day(),
            json!({}),
            json!({}),
            // The core's day is open. The door is not.
            "OPEN",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["kiosk_open"], false);
        assert_eq!(body["business_day"], "OPEN");

        // And the other way round, which is the end of a day on a lane
        // somebody left open.
        let mut open = kiosk();
        open.kiosk_open = true;
        let body = payload(
            &open,
            &day(),
            json!({}),
            json!({}),
            "CLOSED",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["kiosk_open"], true);
        assert_eq!(body["business_day"], "CLOSED");
    }

    /// **Closing changes the sign and nothing else.** Not the business day,
    /// not the identity, not the printer, not the acceptor — a remote close
    /// is the same sign on the door the Admin button hangs.
    #[test]
    fn closing_remotely_touches_nothing_but_the_sign() {
        let dir = controllable_kiosk();
        let before = showroom::stored(dir.path());
        kiosk_control(
            dir.path(),
            &post(
                "/demo/control/kiosk",
                r#"{"action":"CLOSE","pin":"4821"}"#,
                FROM_LAN,
            ),
        );
        let after = showroom::stored(dir.path());
        assert!(!after.kiosk_open);
        assert_eq!(after.vertical, before.vertical);
        assert_eq!(after.channel_ref, before.channel_ref);
        assert_eq!(after.branch_id, before.branch_id);
        assert_eq!(after.kiosk_id, before.kiosk_id);
        assert_eq!(after.cashier_id, before.cashier_id);
        assert_eq!(after.cashgenic, before.cashgenic);
        assert_eq!(after.admin_pin, before.admin_pin);
        assert_eq!(after.remote_control, before.remote_control);
    }

    /// A refused call changes nothing at all.
    #[test]
    fn a_refused_close_leaves_the_kiosk_open() {
        for (what, dir, pin, from) in [
            ("a wrong PIN", controllable_kiosk(), "0000", FROM_LAN),
            (
                "a routed address",
                controllable_kiosk(),
                "4821",
                "203.0.113.7".parse().unwrap(),
            ),
            (
                "remote control off",
                tempfile::tempdir().unwrap(),
                "4821",
                FROM_LAN,
            ),
        ] {
            let (code, body) = kiosk_control(
                dir.path(),
                &post(
                    "/demo/control/kiosk",
                    &format!(r#"{{"action":"CLOSE","pin":"{pin}"}}"#),
                    from,
                ),
            );
            assert!((401..=403).contains(&code), "{what}: {code}");
            assert_eq!(body["success"], false, "{what}");
            assert!(
                showroom::stored(dir.path()).kiosk_open,
                "{what} closed the kiosk anyway"
            );
        }
    }

    /// Approving a waiting authorization from the floor.
    #[test]
    fn an_authorization_is_approved_remotely_and_releases_the_payment() {
        let _turn = crate::authorization::turn();
        crate::authorization::clear();
        let dir = controllable_kiosk();

        let waiting = crate::authorization::require(
            "sale-9",
            vec![crate::authorization::AuthorizationProductV1 {
                product_ref: "R-ARRACHERA".to_owned(),
                name: "Arrachera".to_owned(),
            }],
        );
        let restricted = std::collections::BTreeSet::from(["R-ARRACHERA".to_owned()]);
        assert!(!crate::authorization::allows("sale-9", &restricted));

        let (code, body) = authorization_control(
            dir.path(),
            &post(
                "/demo/control/authorization",
                &format!(r#"{{"request_id":"{}","pin":"4821"}}"#, waiting.request_id),
                FROM_LAN,
            ),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(body["success"], true);
        assert_eq!(body["error"], "");
        assert_eq!(body["request_id"], waiting.request_id);
        assert_eq!(body["sale_id"], "sale-9");
        // And the payment is released, through the same state machine the
        // screen's own keypad uses.
        assert!(crate::authorization::allows("sale-9", &restricted));
        crate::authorization::clear();
    }

    /// A refused approval releases nothing.
    #[test]
    fn a_refused_approval_leaves_the_payment_blocked() {
        let _turn = crate::authorization::turn();
        crate::authorization::clear();
        let dir = controllable_kiosk();
        let waiting = crate::authorization::require(
            "sale-9",
            vec![crate::authorization::AuthorizationProductV1 {
                product_ref: "R-ARRACHERA".to_owned(),
                name: "Arrachera".to_owned(),
            }],
        );
        let restricted = std::collections::BTreeSet::from(["R-ARRACHERA".to_owned()]);

        for (what, pin, request_id, from) in [
            ("a wrong PIN", "0000", waiting.request_id.clone(), FROM_LAN),
            (
                "a routed address",
                "4821",
                waiting.request_id.clone(),
                "8.8.8.8".parse().unwrap(),
            ),
            (
                "a request id the basket replaced",
                "4821",
                "not-the-one".to_owned(),
                FROM_LAN,
            ),
        ] {
            let (code, body) = authorization_control(
                dir.path(),
                &post(
                    "/demo/control/authorization",
                    &format!(r#"{{"request_id":"{request_id}","pin":"{pin}"}}"#),
                    from,
                ),
            );
            assert_ne!(code, 200, "{what} was accepted");
            assert_eq!(body["success"], false, "{what}");
            assert_eq!(body["request_id"], request_id, "{what}");
            assert!(
                !crate::authorization::allows("sale-9", &restricted),
                "{what} released the payment"
            );
        }
        crate::authorization::clear();
    }

    /// `GET /demo/products` publishes what the brief asks for, with no nulls.
    #[test]
    fn the_products_endpoint_publishes_every_field_the_contract_names() {
        let products = vec![
            crate::kiosk::AdminProductV1 {
                product_ref: "R-ARRACHERA".to_owned(),
                sku: "SKU-RT-3001".to_owned(),
                name: "Arrachera".to_owned(),
                category_ref: "R-CARNES".to_owned(),
                category_name: "Carnes".to_owned(),
                price_minor: 28_900,
                price_formatted: "$289.00".to_owned(),
                available: true,
                authorization_required: true,
                price_overridden: true,
                image: None,
            },
            // A counter brand's product, which names itself once and has no
            // separate code: the field is empty, never absent.
            crate::kiosk::AdminProductV1 {
                product_ref: "T-LATTE".to_owned(),
                sku: String::new(),
                name: "Latte".to_owned(),
                category_ref: "T-CAFE".to_owned(),
                category_name: String::new(),
                price_minor: 5_900,
                price_formatted: "$59.00".to_owned(),
                available: true,
                authorization_required: false,
                price_overridden: false,
                image: None,
            },
        ];
        let body = products_payload(&kiosk(), &products);
        assert_eq!(body["vertical"], "TIM_HORTONS");
        assert_eq!(body["channel_ref"], "KIOSK-TIM");

        let rows = body["products"].as_array().expect("an array");
        assert_eq!(rows.len(), 2);
        for row in rows {
            for field in [
                "product_ref",
                "sku",
                "name",
                "category",
                "price",
                "price_minor",
                "available",
                "authorization_required",
            ] {
                assert!(!row[field].is_null(), "`{field}` is null in {row}");
            }
        }
        assert_eq!(rows[0]["authorization_required"], true);
        assert_eq!(rows[0]["sku"], "SKU-RT-3001");
        // Both the decimal a screen prints and the integer to compare.
        assert_eq!(rows[0]["price"], json!(289.0));
        assert_eq!(rows[0]["price_minor"], 28_900);
        assert_eq!(rows[1]["sku"], "", "an absent code became a null");
        assert_eq!(rows[1]["category"], "");
        assert_eq!(rows[1]["price"], json!(59.0));
    }

    // -------------------------------------------------- the published shape

    /// The authorization block, when nothing is waiting.
    #[test]
    fn the_status_says_no_authorization_is_waiting_without_a_single_null() {
        let body = sample();
        let waiting = &body["authorization"];
        assert_eq!(waiting["required"], false);
        assert_eq!(waiting["approved"], false);
        assert_eq!(waiting["products"], json!([]));
        assert_eq!(waiting["request_id"], "");
        assert_eq!(waiting["sale_id"], "");
        assert_eq!(waiting["requested_at"], "");
        for field in [
            "required",
            "request_id",
            "sale_id",
            "approved",
            "requested_at",
            "products",
        ] {
            assert!(!waiting[field].is_null(), "authorization.{field} is null");
        }
    }

    /// And when one is.
    #[test]
    fn a_waiting_authorization_names_the_sale_and_the_products() {
        let waiting = crate::authorization::AuthorizationV1 {
            required: true,
            request_id: "r-7".to_owned(),
            sale_id: "sale-3".to_owned(),
            products: vec![crate::authorization::AuthorizationProductV1 {
                product_ref: "R-ARRACHERA".to_owned(),
                name: "Arrachera".to_owned(),
            }],
            approved: false,
            requested_at: "2026-11-02T10:00:00Z".to_owned(),
        };
        let body = payload(
            &kiosk(),
            &day(),
            json!({}),
            json!({}),
            "OPEN",
            waiting.into_wire(),
        );
        let block = &body["authorization"];
        assert_eq!(block["required"], true);
        assert_eq!(block["approved"], false);
        assert_eq!(block["request_id"], "r-7");
        assert_eq!(block["sale_id"], "sale-3");
        assert_eq!(block["products"][0]["product_ref"], "R-ARRACHERA");
        assert_eq!(block["products"][0]["name"], "Arrachera");
        assert_eq!(block["requested_at"], "2026-11-02T10:00:00Z");
    }

    /// Everything a future store app is promised, in one place.
    #[test]
    fn the_status_carries_the_whole_contract_a_store_app_polls() {
        let body = sample();
        for field in [
            "branch_id",
            "kiosk_id",
            "cashier_id",
            "vertical",
            "kiosk_open",
            "kiosk_on",
            "app_running",
            "printer",
            "cashgenic",
            "sales",
            "business_day",
            "authorization",
            "alerts",
            "remote_control",
        ] {
            assert!(!body[field].is_null(), "`{field}` is missing or null");
        }
        // An empty list is a value. A null here would make every console
        // check for absence before it could read the card.
        assert!(body["alerts"].is_array(), "`alerts` is not an array");
        for figure in [
            "transaction_count",
            "sales_total",
            "cash_total",
            "card_total",
        ] {
            assert!(!body["sales"][figure].is_null(), "sales.{figure}");
        }
    }

    /// Whether this kiosk is listening for a control POST is published, so a
    /// floor app can tell a refused PIN from a kiosk that is not listening.
    #[test]
    fn whether_remote_control_is_on_is_published() {
        assert_eq!(sample()["remote_control"], false);
        let body = payload(
            &controllable(),
            &day(),
            json!({}),
            json!({}),
            "OPEN",
            crate::authorization::AuthorizationV1::none().into_wire(),
        );
        assert_eq!(body["remote_control"], true);
    }

    #[test]
    fn the_port_is_fixed_so_a_monitor_can_find_it() {
        assert_eq!(PORT, 8_777);
    }

    /// Print what this endpoint actually answers, against a running core.
    ///
    /// Ignored by default because it needs a daemon and a data directory —
    /// the things a kiosk has and a test runner does not. Run it by hand when
    /// you want to see the real thing rather than a fixture:
    ///
    /// ```text
    /// PI_EDGE_DB=./data/pi-edge.db PI_EDGE_DATA_DIR=~/Library/.../PX\ Kiosk \
    ///   cargo test -- --ignored --nocapture live_status
    /// ```
    #[test]
    #[ignore = "needs a running edge-daemon and a kiosk data directory"]
    fn live_status() {
        let database = std::env::var("PI_EDGE_DB").expect("PI_EDGE_DB");
        let dir =
            std::path::PathBuf::from(std::env::var("PI_EDGE_DATA_DIR").expect("PI_EDGE_DATA_DIR"));
        let client =
            edge_client::IpcEdgeClient::for_database(std::path::Path::new(&database), "px-kiosk")
                .expect("a client");

        let preferences = showroom::stored(&dir);
        let day = showroom::stored_day(&dir);
        let body = payload(
            &preferences,
            &day,
            printer(&client),
            cashgenic(&preferences),
            business_day(&client, &day),
            crate::authorization::status(),
        );
        println!("{}", serde_json::to_string_pretty(&body).unwrap());
    }
}
