//! DIAGNOSTIC, temporary — not part of the product.
//!
//! `cashgenic-diagnostic HOST PORT [--rounds N] [--interval-ms MS]
//! [--preferences PATH] [--no-curl]`
//!
//! Runs the kiosk's own CashGenic health round — the same `CashGenic`
//! client, the same rustls verifier, the same request writer and parser, the
//! same 4 s budget, the same token → `/system` → `/status` — N times in a
//! row, outside the watcher and the window. Every stage of every call is
//! timed. When a round fails, curl is asked the same three questions at once
//! from the same machine, so "the device is not answering" and "our client is
//! not getting through" can be told apart. The sockets to the device's port
//! are counted after every round.
//!
//! The credentials come from the kiosk's own `preferences.json`, read through
//! the same function the kiosk uses, and are never printed: curl receives the
//! grant and the bearer on stdin, not on its command line.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cashgenic::{self, CashGenic, TraceStep};
use crate::showroom::{self, CashGenicSettingsV1};

/// Run it and return the process exit code: 0 when every round passed.
pub fn run(args: &[String]) -> i32 {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(usage) => {
            eprintln!("{usage}");
            return 2;
        }
    };
    let Some(dir) = options.preferences.parent() else {
        eprintln!("no directory around {}", options.preferences.display());
        return 2;
    };
    let stored = showroom::stored(dir).cashgenic;
    let settings = CashGenicSettingsV1 {
        // The diagnostic runs whatever Admin says: an acceptor switched off
        // in the file is still the one being tested.
        enabled: true,
        host: options.host.clone(),
        port: options.port,
        ..stored
    };

    println!(
        "CASHGENIC DIAGNOSTIC {}:{} rounds={} interval_ms={} timeout_ms={}",
        settings.host,
        settings.port,
        options.rounds,
        options.interval.as_millis(),
        cashgenic::PROBE_TIMEOUT.as_millis()
    );
    println!(
        "credentials: preferences={} username={} password={}",
        options.preferences.display(),
        if settings.username.is_empty() {
            "EMPTY"
        } else {
            "set"
        },
        if settings.password.is_empty() {
            "EMPTY"
        } else {
            "set"
        },
    );
    println!("sockets before: {}", sockets(settings.port));

    let mut rounds = Vec::new();
    for number in 1..=options.rounds {
        let round = Round::run(&settings, number);
        let verdict = match round.failed() {
            None => "PASS".to_owned(),
            Some(step) => format!(
                "FAIL step={}.{} detail={:?}",
                step.call, step.step, step.detail
            ),
        };
        println!(
            "ROUND {number}: {verdict} total_ms={} sockets: {}",
            round.total.as_millis(),
            sockets(settings.port)
        );
        if round.failed().is_some() {
            for step in &round.steps {
                println!("    {}", line(step));
            }
            if options.curl {
                println!("    CURL NOW: {}", curl_round(&settings));
            }
        }
        rounds.push(round);
        if number < options.rounds {
            std::thread::sleep(options.interval);
        }
    }

    print!("{}", summary(&rounds));
    println!("sockets after: {}", sockets(settings.port));
    if rounds.iter().all(|round| round.failed().is_none()) {
        0
    } else {
        1
    }
}

#[derive(Debug)]
struct Options {
    host: String,
    port: u16,
    rounds: u32,
    interval: Duration,
    preferences: PathBuf,
    curl: bool,
}

const USAGE: &str = "usage: cashgenic-diagnostic HOST PORT [--rounds N] [--interval-ms MS] \
                     [--preferences PATH] [--no-curl]";

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut rounds = 20;
        let mut interval = Duration::ZERO;
        let mut preferences = None;
        let mut curl = true;
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--rounds" => rounds = number(rest.next())?,
                "--interval-ms" => interval = Duration::from_millis(number(rest.next())?),
                "--preferences" => {
                    preferences = Some(PathBuf::from(rest.next().ok_or(USAGE)?));
                }
                "--no-curl" => curl = false,
                _ => positional.push(arg.clone()),
            }
        }
        let [host, port] = positional.as_slice() else {
            return Err(USAGE.to_owned());
        };
        Ok(Self {
            host: host.clone(),
            port: port.parse().map_err(|_| USAGE.to_owned())?,
            rounds,
            interval,
            preferences: preferences
                .or_else(default_preferences)
                .ok_or("no --preferences given and no default location")?,
            curl,
        })
    }
}

fn number<T: std::str::FromStr>(value: Option<&String>) -> Result<T, String> {
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| USAGE.to_owned())
}

/// Where the installed kiosk keeps its preferences.
fn default_preferences() -> Option<PathBuf> {
    const IDENTIFIER: &str = "com.pxcommerce.pxkiosk";
    let base = if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".local/share")
    };
    Some(base.join(IDENTIFIER).join("preferences.json"))
}

/// One health round and every stage it went through.
#[derive(Debug)]
struct Round {
    steps: Vec<TraceStep>,
    online: bool,
    detail: Option<String>,
    total: Duration,
}

impl Round {
    fn run(settings: &CashGenicSettingsV1, number: u32) -> Self {
        let started = Instant::now();
        let device = CashGenic::traced(settings, cashgenic::PROBE_TIMEOUT, u64::from(number));
        let status = cashgenic::probe_round(&device);
        Self {
            steps: device.steps(),
            online: status.online,
            detail: status.detail,
            total: started.elapsed(),
        }
    }

    /// The stage that failed, if any. A round that went offline with every
    /// stage passing is reported as such rather than called a pass.
    fn failed(&self) -> Option<TraceStep> {
        if let Some(step) = self.steps.iter().find(|step| !step.ok) {
            return Some(step.clone());
        }
        (!self.online).then(|| TraceStep {
            call: "round",
            step: "offline",
            elapsed: Duration::ZERO,
            ok: false,
            detail: self.detail.clone().unwrap_or_default(),
        })
    }
}

fn line(step: &TraceStep) -> String {
    format!(
        "{}.{} {} ms={} {}",
        step.call,
        step.step,
        if step.ok { "OK" } else { "ERROR" },
        millis(step.elapsed),
        step.detail
    )
}

/// Milliseconds with one decimal, without a float.
fn millis(elapsed: Duration) -> String {
    let micros = elapsed.as_micros();
    format!("{}.{}", micros / 1_000, (micros % 1_000) / 100)
}

/// The totals: passes, failures by stage, and min/avg/max per stage.
fn summary(rounds: &[Round]) -> String {
    let failed: Vec<TraceStep> = rounds.iter().filter_map(Round::failed).collect();
    let mut out = String::new();
    out.push_str("SUMMARY\n");
    out.push_str(&format!(
        "rounds: {}  PASS: {}  FAIL: {}\n",
        rounds.len(),
        rounds.len() - failed.len(),
        failed.len()
    ));

    let mut stages: BTreeMap<String, usize> = BTreeMap::new();
    for step in &failed {
        *stages
            .entry(format!("{}.{}", step.call, step.step))
            .or_default() += 1;
    }
    out.push_str("failure stages:");
    if stages.is_empty() {
        out.push_str(" none");
    }
    for (stage, count) in &stages {
        out.push_str(&format!(" {stage}={count}"));
    }
    out.push('\n');

    // In the order a round goes through them, not alphabetically.
    let mut order: Vec<String> = Vec::new();
    let mut timings: BTreeMap<String, Vec<Duration>> = BTreeMap::new();
    for step in rounds.iter().flat_map(|round| &round.steps) {
        let key = format!("{}.{}", step.call, step.step);
        if !order.contains(&key) {
            order.push(key.clone());
        }
        timings.entry(key).or_default().push(step.elapsed);
    }
    out.push_str("latency per stage (ms min/avg/max, n):\n");
    for key in &order {
        let Some(all) = timings.get(key) else {
            continue;
        };
        let min = all.iter().min().copied().unwrap_or_default();
        let max = all.iter().max().copied().unwrap_or_default();
        let count = u32::try_from(all.len()).unwrap_or(u32::MAX).max(1);
        let average = all.iter().sum::<Duration>() / count;
        out.push_str(&format!(
            "  {key:<22} {} / {} / {}  n={}\n",
            millis(min),
            millis(average),
            millis(max),
            all.len()
        ));
    }

    out.push_str("response headers (first seen):\n");
    for call in ["token", "system", "status"] {
        let seen = rounds
            .iter()
            .flat_map(|round| &round.steps)
            .find(|step| step.call == call && step.step == "http_headers" && step.ok);
        out.push_str(&format!(
            "  {call}: {}\n",
            seen.map_or("never received", |step| step.detail.as_str())
        ));
    }
    let tls = rounds
        .iter()
        .flat_map(|round| &round.steps)
        .find(|step| step.step == "tls_handshake" && step.ok);
    out.push_str(&format!(
        "tls: {}\n",
        tls.map_or("no handshake completed", |step| step.detail.as_str())
    ));
    out
}

// ------------------------------------------------------------- sockets

/// TCP connections to the device's port on this machine, by state.
fn sockets(port: u16) -> String {
    let output = if cfg!(windows) {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Get-NetTCPConnection -RemotePort {port} -ErrorAction SilentlyContinue \
                     | Group-Object State | ForEach-Object {{ $_.Name + '=' + $_.Count }}"
                ),
            ])
            .output()
    } else {
        Command::new("netstat").args(["-an", "-p", "tcp"]).output()
    };
    let Ok(output) = output else {
        return "unavailable".to_owned();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let counted = if cfg!(windows) {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        count_netstat(&text, port)
    };
    if counted.is_empty() {
        "none".to_owned()
    } else {
        counted
    }
}

/// `netstat -an` lines whose remote end is this port, counted by state.
fn count_netstat(text: &str, port: u16) -> String {
    let mut states: BTreeMap<&str, usize> = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // proto recv send local remote state
        let (Some(remote), Some(state)) = (fields.get(4), fields.get(5)) else {
            continue;
        };
        if remote.ends_with(&format!(".{port}")) || remote.ends_with(&format!(":{port}")) {
            *states.entry(state).or_default() += 1;
        }
    }
    states
        .iter()
        .map(|(state, count)| format!("{state}={count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- curl

/// The same three questions, asked by curl from this machine, now.
fn curl_round(settings: &CashGenicSettingsV1) -> String {
    let base = format!("https://{}:{}", settings.host, settings.port);
    let token = curl(&[
        format!("url = {}", quoted(&format!("{base}/token"))),
        "request = \"POST\"".to_owned(),
        "header = \"Content-Type: application/x-www-form-urlencoded\"".to_owned(),
        format!("data = {}", quoted(&cashgenic::grant_form(settings))),
    ]);
    let bearer = token
        .body
        .as_deref()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .and_then(|json| {
            json.get("token")
                .or_else(|| json.get("access_token"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    let Some(bearer) = bearer else {
        return format!(
            "token {} | system skipped | status skipped (no token)",
            token.timing
        );
    };
    let authorized = |path: &str| {
        curl(&[
            format!("url = {}", quoted(&format!("{base}{path}"))),
            format!(
                "header = {}",
                quoted(&format!("Authorization: Bearer {bearer}"))
            ),
            "header = \"Accept: application/json\"".to_owned(),
        ])
    };
    format!(
        "token {} | system {} | status {}",
        token.timing,
        authorized("/system").timing,
        authorized("/status?eventVersion=2").timing
    )
}

#[derive(Debug)]
struct Curl {
    /// `http=200 connect_ms=… tls_ms=… total_ms=…`, or why there is none.
    timing: String,
    body: Option<String>,
}

const MARK: &str = "__CASHGENIC_DIAGNOSTIC__";

/// One curl call configured on stdin, so nothing secret is on a command line.
fn curl(config: &[String]) -> Curl {
    let program = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut lines = vec![
        "insecure".to_owned(),
        "silent".to_owned(),
        "show-error".to_owned(),
        format!("max-time = {}", cashgenic::PROBE_TIMEOUT.as_secs()),
        format!(
            "write-out = \"\\n{MARK} %{{http_code}} %{{time_connect}} %{{time_appconnect}} %{{time_total}}\""
        ),
    ];
    lines.extend_from_slice(config);
    let child = Command::new(program)
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return Curl {
            timing: format!("unavailable ({program} not found)"),
            body: None,
        };
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(lines.join("\n").as_bytes());
    }
    let Ok(output) = child.wait_with_output() else {
        return Curl {
            timing: "did not finish".to_owned(),
            body: None,
        };
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let Some((body, written)) = stdout.rsplit_once(MARK) else {
        return Curl {
            timing: format!(
                "FAIL {:?}",
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            ),
            body: None,
        };
    };
    let fields: Vec<&str> = written.split_whitespace().collect();
    let (code, connect, tls, total) = match fields.as_slice() {
        [code, connect, tls, total] => (*code, ms(connect), ms(tls), ms(total)),
        _ => {
            return Curl {
                timing: format!("unreadable write-out {written:?}"),
                body: None,
            }
        }
    };
    let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let handshake = tls.saturating_sub(connect);
    Curl {
        timing: format!(
            "{} http={code} connect_ms={connect} tls_ms={handshake} total_ms={total}{}",
            if code.starts_with('2') {
                "PASS"
            } else {
                "FAIL"
            },
            if error.is_empty() {
                String::new()
            } else {
                format!(" error={error:?}")
            }
        ),
        body: Some(body.to_owned()),
    }
}

/// curl's seconds (`0.012345`) as whole milliseconds, without a float.
fn ms(seconds: &str) -> u64 {
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let fraction: String = fraction.chars().chain("000".chars()).take(3).collect();
    whole.parse::<u64>().unwrap_or(0) * 1_000 + fraction.parse::<u64>().unwrap_or(0)
}

/// A curl config value: double-quoted, with `\` and `"` escaped.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn step(call: &'static str, step: &'static str, ms: u64, ok: bool) -> TraceStep {
        TraceStep {
            call,
            step,
            elapsed: Duration::from_millis(ms),
            ok,
            detail: format!("{call}.{step}"),
        }
    }

    #[test]
    fn the_summary_counts_failures_by_stage_and_times_every_stage() {
        let pass = Round {
            steps: vec![
                step("token", "tcp_connect", 10, true),
                step("token", "tls_handshake", 40, true),
            ],
            online: true,
            detail: None,
            total: Duration::from_millis(50),
        };
        let fail = Round {
            steps: vec![step("token", "tcp_connect", 4_000, false)],
            online: false,
            detail: Some("timed out".to_owned()),
            total: Duration::from_millis(4_000),
        };
        let text = summary(&[pass, fail]);
        assert!(text.contains("rounds: 2  PASS: 1  FAIL: 1"), "{text}");
        assert!(
            text.contains("failure stages: token.tcp_connect=1"),
            "{text}"
        );
        assert!(
            text.contains("token.tcp_connect      10.0 / 2005.0 / 4000.0  n=2"),
            "{text}"
        );
        assert!(
            text.contains("token.tls_handshake    40.0 / 40.0 / 40.0  n=1"),
            "{text}"
        );
    }

    #[test]
    fn a_round_offline_with_every_stage_passing_is_still_a_failure() {
        let round = Round {
            steps: vec![step("token", "json_parse", 1, true)],
            online: false,
            detail: Some("a module is disconnected".to_owned()),
            total: Duration::ZERO,
        };
        let failed = round.failed().unwrap();
        assert_eq!((failed.call, failed.step), ("round", "offline"));
    }

    #[test]
    fn curls_seconds_become_milliseconds_without_a_float() {
        assert_eq!(ms("0.012345"), 12);
        assert_eq!(ms("1.5"), 1_500);
        assert_eq!(ms("4"), 4_000);
    }

    #[test]
    fn netstat_lines_are_counted_by_state_for_the_port_only() {
        let text = "tcp4 0 0 192.168.88.10.51000 192.168.88.240.44333 TIME_WAIT\n\
                    tcp4 0 0 192.168.88.10.51001 192.168.88.240.44333 TIME_WAIT\n\
                    tcp4 0 0 192.168.88.10.51002 192.168.88.240.44333 ESTABLISHED\n\
                    tcp4 0 0 192.168.88.10.51003 10.0.0.1.443 ESTABLISHED\n";
        assert_eq!(count_netstat(text, 44_333), "ESTABLISHED=1 TIME_WAIT=2");
    }

    #[test]
    fn a_curl_config_value_cannot_break_out_of_its_quotes() {
        assert_eq!(quoted(r#"pa"ss\word"#), r#""pa\"ss\\word""#);
    }

    #[test]
    fn options_need_a_host_and_a_port_and_take_the_rest_as_flags() {
        let args: Vec<String> = [
            "192.168.88.240",
            "44333",
            "--rounds",
            "5",
            "--preferences",
            "/x/preferences.json",
            "--no-curl",
        ]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
        let options = Options::parse(&args).unwrap();
        assert_eq!(options.host, "192.168.88.240");
        assert_eq!(options.port, 44_333);
        assert_eq!(options.rounds, 5);
        assert!(!options.curl);
        assert_eq!(options.preferences, PathBuf::from("/x/preferences.json"));
        assert!(Options::parse(&["only-host".to_owned()]).is_err());
    }
}
