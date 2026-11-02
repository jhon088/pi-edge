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
//! Read-only. There is no route that changes anything, and no route but this
//! one.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use edge_client::{EdgeClient, IpcEdgeClient};
use edge_protocol::{PrinterConfigurationV1, PrinterStatusV1};
use serde_json::{json, Value};

use crate::showroom;

/// Fixed, because a monitoring app has to find it without being told.
pub const PORT: u16 = 8_777;

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
        let listener = match TcpListener::bind(("0.0.0.0", PORT)) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, port = PORT, "the status endpoint is not listening");
                return;
            }
        };
        tracing::info!(port = PORT, "status endpoint ready on /demo/status");
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

fn answer(app: &tauri::AppHandle, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(Some(WRITE_TIMEOUT))?;

    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let (code, body) = route(&method, &path, || snapshot(app));
    stream.write_all(http(code, &body).as_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

/// One route, and an honest answer for everything else.
///
/// The snapshot is built lazily so a probe for some other path does not ask
/// the core anything.
fn route(method: &str, path: &str, snapshot: impl FnOnce() -> Value) -> (u16, Value) {
    match (method, path) {
        ("GET", "/demo/status") => (200, snapshot()),
        ("GET", _) => (404, json!({ "error": "not found" })),
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
    )
}

/// The shape a floor monitor reads, from parts that are already decided.
fn payload(
    preferences: &showroom::KioskPreferencesV1,
    day: &showroom::DayTallyV1,
    printer: Value,
    cashgenic: Value,
    business_day: &str,
) -> Value {
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
    let health = crate::cashgenic::cached_health();
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
    }

    #[test]
    fn a_closed_kiosk_says_so_without_going_quiet() {
        let mut closed = kiosk();
        closed.kiosk_open = false;
        let body = payload(&closed, &day(), json!({}), json!({}), "OPEN");
        assert_eq!(body["kiosk_open"], false);
        // Still answering: a closed kiosk is not an offline one.
        assert_eq!(body["app_running"], true);
    }

    #[test]
    fn a_closed_day_reports_closed_and_keeps_its_figures() {
        let mut ended = day();
        ended.state = "CLOSED".to_owned();
        let body = payload(&kiosk(), &ended, json!({}), json!({}), "CLOSED");
        assert_eq!(body["business_day"], "CLOSED");
        assert_eq!(body["sales"]["transaction_count"], 42);
    }

    /// Only one route, and it only reads.
    #[test]
    fn the_endpoint_answers_one_path_and_refuses_the_rest() {
        let (code, body) = route("GET", "/demo/status", sample);
        assert_eq!(code, 200);
        assert_eq!(body["kiosk_id"], "K01");

        let (code, _) = route("GET", "/demo/other", || panic!("must not be built"));
        assert_eq!(code, 404);
        let (code, _) = route("POST", "/demo/status", || panic!("must not be built"));
        assert_eq!(
            code, 405,
            "the endpoint accepted a method that could change something"
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
        );
        println!("{}", serde_json::to_string_pretty(&body).unwrap());
    }
}
