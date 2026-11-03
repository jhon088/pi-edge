//! The process log: one subscriber, two outputs.
//!
//! An installed kiosk is a Windows GUI process with no console, so stderr goes
//! nowhere and the file is the only evidence a startup leaves. The data
//! directory that holds the file is only known once Tauri is running, but the
//! subscriber has to exist before then — so it is installed once, at the top
//! of `run`, with a file output that holds what it is given until the file is
//! attached and then writes straight through.
//!
//! There was a second `try_init` here once, for the file, and it always lost:
//! a process has one global subscriber, the stderr one was already it, and the
//! refusal was discarded. Every installed kiosk wrote an empty log.
//!
//! Both outputs pass through [`redact`], so a CashGenic password or token that
//! finds its way into an error message is masked before it is written
//! anywhere.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// How much is held for the file before it is attached. Setup attaches it
/// within the first few lines; this only bounds a run that never does.
const PENDING_LIMIT: usize = 256 * 1024;

/// Keys whose value is a secret wherever it appears: a form body, a JSON
/// field, a query string.
const SECRET_KEYS: [&str; 5] = [
    "password",
    "access_token",
    "refresh_token",
    "client_secret",
    "token",
];

static SINK: OnceLock<LogFile> = OnceLock::new();

/// Install the process subscriber. Once: a second call returns the same file
/// output and installs nothing.
pub fn init() -> &'static LogFile {
    let mut first = false;
    let sink = SINK.get_or_init(|| {
        first = true;
        LogFile::default()
    });
    if first {
        if let Err(error) = subscriber(sink.clone()).try_init() {
            // Not discarded: this is the one failure that would leave the file
            // empty again, so it is said on the only channel left.
            eprintln!("PX_KIOSK_LOG_INIT_FAILED error={:?}", error.to_string());
        }
    }
    sink
}

fn subscriber(file: LogFile) -> impl tracing::Subscriber + Send + Sync {
    let filter = EnvFilter::try_from_env("PI_EDGE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(|| Redacted(io::stderr()))
                .with_target(false),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(move || Redacted(file.clone()))
                .with_ansi(false)
                .with_target(false),
        )
}

/// The file output: buffered until a path is attached, then unbuffered, so a
/// line written just before a panic is on disk when the process goes.
#[derive(Debug, Clone, Default)]
pub struct LogFile(Arc<Mutex<Target>>);

#[derive(Debug)]
enum Target {
    Pending(Vec<u8>),
    Open(File),
}

impl Default for Target {
    fn default() -> Self {
        Self::Pending(Vec::new())
    }
}

impl LogFile {
    /// Start writing to `path`, appending, with everything held so far first.
    pub fn attach(&self, path: &Path) -> io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut target = self
            .0
            .lock()
            .map_err(|_| io::Error::other("the log file lock is poisoned"))?;
        if let Target::Pending(held) = &*target {
            file.write_all(held)?;
        }
        *target = Target::Open(file);
        Ok(())
    }
}

impl Write for LogFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut target = self
            .0
            .lock()
            .map_err(|_| io::Error::other("the log file lock is poisoned"))?;
        match &mut *target {
            Target::Open(file) => file.write_all(bytes)?,
            Target::Pending(held) => {
                if held.len() + bytes.len() <= PENDING_LIMIT {
                    held.extend_from_slice(bytes);
                }
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut *self
            .0
            .lock()
            .map_err(|_| io::Error::other("the log file lock is poisoned"))?
        {
            Target::Open(file) => file.flush(),
            Target::Pending(_) => Ok(()),
        }
    }
}

/// A writer that masks secrets on the way through. The formatter hands over
/// one whole line per write, so a key and its value are never split.
struct Redacted<W>(W);

impl<W: Write> Write for Redacted<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let line = String::from_utf8_lossy(bytes);
        self.0.write_all(redact(&line).as_bytes())?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// `text` with the value of every secret key, and every bearer token, masked.
///
/// Matches `key=value`, `key: value`, `"key":"value"` and `Bearer value`,
/// case-insensitively. A key that is only a value — `grant_type=password` — is
/// left alone, because nothing follows it.
fn redact(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut masks: Vec<(usize, usize)> = Vec::new();

    for key in SECRET_KEYS {
        let mut from = 0;
        while let Some(found) = lower[from..].find(key) {
            let start = from + found;
            let after_key = start + key.len();
            from = after_key;
            // A whole key, not the tail of a longer word.
            let part_of_a_word = start
                .checked_sub(1)
                .and_then(|at| bytes.get(at))
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
            if part_of_a_word {
                continue;
            }
            let cursor = skip_spaces(bytes, skip_quote(bytes, after_key));
            if !matches!(bytes.get(cursor), Some(b'=' | b':')) {
                continue;
            }
            let value = skip_quote(bytes, skip_spaces(bytes, cursor + 1));
            masks.push((value, value_end(bytes, value)));
        }
    }

    let mut from = 0;
    while let Some(found) = lower[from..].find("bearer ") {
        let start = skip_spaces(bytes, from + found + "bearer ".len());
        let end = value_end(bytes, start);
        masks.push((start, end));
        from = end.max(start + 1).min(bytes.len());
    }

    masks.retain(|(start, end)| end > start);
    masks.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut written = 0;
    for (start, end) in masks {
        if start < written {
            continue;
        }
        out.push_str(&text[written..start]);
        out.push_str("***");
        written = end;
    }
    out.push_str(&text[written..]);
    out
}

/// Past one quote, escaped or not: a JSON body inside a `{:?}` error message
/// arrives as `\"password\":\"…\"`.
fn skip_quote(bytes: &[u8], at: usize) -> usize {
    match (bytes.get(at), bytes.get(at + 1)) {
        (Some(b'\\'), Some(b'"')) => at + 2,
        (Some(b'"'), _) => at + 1,
        _ => at,
    }
}

fn skip_spaces(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at) == Some(&b' ') {
        at += 1;
    }
    at
}

/// Where a secret value stops. Every delimiter is ASCII, so the cut is always
/// on a character boundary.
fn value_end(bytes: &[u8], mut at: usize) -> usize {
    while let Some(&b) = bytes.get(at) {
        if matches!(
            b,
            b'&' | b'"' | b'\'' | b',' | b';' | b'}' | b'\\' | b' ' | b'\r' | b'\n' | b'\t'
        ) {
            break;
        }
        at += 1;
    }
    at
}

/// Everything `work` logs on this thread, as the file would have it —
/// redacted, without colour.
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
pub(crate) fn captured(work: impl FnOnce()) -> String {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("px-kiosk.log");
    let file = LogFile::default();
    file.attach(&path).expect("a log file");
    // With a single dispatcher registered, tracing-core decides a callsite's
    // interest against whichever thread hits it first — and a parallel test
    // with no subscriber caches it as "never", which empties this capture. A
    // second dispatcher kept alive meanwhile makes every decision consult all
    // of them, this one included.
    let _second = tracing::Dispatch::new(tracing_subscriber::registry());
    tracing::subscriber::with_default(subscriber(file), work);
    std::fs::read_to_string(&path).unwrap()
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subscriber_is_installed_once() {
        let first = init();
        let second = init();
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert!(tracing::dispatcher::has_been_set());
    }

    #[test]
    fn lines_before_the_file_is_attached_reach_it_after() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("px-kiosk.log");
        let file = LogFile::default();
        tracing::subscriber::with_default(subscriber(file.clone()), || {
            tracing::info!("before the data directory is known");
            file.attach(&path).unwrap();
            tracing::error!("after it is");
        });
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("before the data directory is known"));
        assert!(written.contains("after it is"));
        assert!(!written.contains('\u{1b}'), "no colour codes in a file");
    }

    #[test]
    fn a_cashgenic_secret_never_reaches_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("px-kiosk.log");
        let file = LogFile::default();
        file.attach(&path).unwrap();
        tracing::subscriber::with_default(subscriber(file), || {
            tracing::error!(
                "SETUP_FAILED step=ensure_core error={:?}",
                "grant_type=password&username=ApiUserOne&password=1234 \
                 {\"access_token\":\"eyJhbGciOi\"} Authorization: Bearer s3cr3t"
            );
        });
        let written = std::fs::read_to_string(&path).unwrap();
        for secret in ["1234", "eyJhbGciOi", "s3cr3t"] {
            assert!(!written.contains(secret), "{secret} leaked: {written}");
        }
        assert!(written.contains("SETUP_FAILED step=ensure_core"));
        assert!(written.contains("username=ApiUserOne"));
    }

    #[test]
    fn redaction_masks_values_and_leaves_the_rest() {
        assert_eq!(
            redact("grant_type=password&username=u&password=1234"),
            "grant_type=password&username=u&password=***"
        );
        assert_eq!(
            redact(r#"{"password": "hunter2", "user":"x"}"#),
            r#"{"password": "***", "user":"x"}"#
        );
        assert_eq!(
            redact(r#"error="token: abc.def failed""#),
            r#"error="token: *** failed""#
        );
        assert_eq!(
            redact("Authorization: Bearer xyz"),
            "Authorization: Bearer ***"
        );
        assert_eq!(redact("POST /token HTTP 200"), "POST /token HTTP 200");
        assert_eq!(redact("crédito sin secretos"), "crédito sin secretos");
    }
}
