//! An ESC/POS printer reached through the host's print spooler.
//!
//! The 3nStar receipt printers on a desk are USB, and every desktop OS
//! exposes a USB receipt printer as a spooler queue. So the transport is the
//! spooler: hand it the same ESC/POS bytes the TCP adapter puts on a socket,
//! marked raw so it passes them through instead of trying to typeset them.
//!
//! Two backends, one port. On Unix that is `lp`/`lpstat` over CUPS; on
//! Windows it is the spooler API, behind the safe wrapper in `edge-winspool`.
//! **Neither is a second print engine** — the bytes come from
//! [`crate::escpos::render`], the one encoder, so a receipt is the same
//! receipt whichever queue it went out of (ADR 038).

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::process::{Command, Stdio};

use crate::error::HardwareError;
use crate::escpos::render;
use crate::printer::{PrintDocument, PrinterPort};

#[cfg(unix)]
/// Where the spooler lives. Absolute so the adapter does not depend on the
/// daemon's `PATH`, and checked in order because distributions differ.
#[cfg(unix)]
const LP: [&str; 2] = ["/usr/bin/lp", "/usr/local/bin/lp"];
#[cfg(unix)]
const LPSTAT: [&str; 2] = ["/usr/bin/lpstat", "/usr/local/bin/lpstat"];

#[cfg(unix)]
fn tool(candidates: &[&'static str], device: &'static str) -> Result<&'static str, HardwareError> {
    for candidate in candidates {
        if std::path::Path::new(candidate).is_file() {
            return Ok(candidate);
        }
    }
    Err(HardwareError::NotConfigured {
        device,
        detail: "this host has no print spooler (`lp` was not found)".to_owned(),
    })
}

/// A print queue the host knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolerQueue {
    /// The queue name, which is what `lp -d` takes.
    pub name: String,
    /// Whether the spooler says it is accepting work right now.
    pub accepting: bool,
}

/// Every queue the host exposes, as the spooler reports them.
///
/// Reports what the host reports and nothing else: no guessing, no invented
/// devices, and an empty list when there are none.
pub fn list_queues() -> Result<Vec<SpoolerQueue>, HardwareError> {
    #[cfg(windows)]
    {
        list_queues_windows()
    }
    #[cfg(unix)]
    {
        list_queues_unix()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(Vec::new())
    }
}

/// The Windows spooler, through the safe wrapper over its API.
#[cfg(windows)]
fn list_queues_windows() -> Result<Vec<SpoolerQueue>, HardwareError> {
    match edge_winspool::queues() {
        Ok(queues) => Ok(queues
            .into_iter()
            .map(|queue| SpoolerQueue {
                name: queue.name,
                accepting: queue.ready,
            })
            .collect()),
        Err(error) => Err(HardwareError::Io {
            device: "printer",
            detail: error.to_string(),
        }),
    }
}

#[cfg(unix)]
/// Every queue the host exposes.
///
/// Uses `lpstat -e`, which prints one bare queue name per line. The obvious
/// `lpstat -p` is prose, and the spooler translates it — a Spanish host says
/// "la impresora X está inactiva", so parsing it means parsing the operator's
/// language. Names are names in every locale.
///
/// Reports what the spooler reports and nothing else: no guessing, no
/// invented devices, and an empty list when there are none.
fn list_queues_unix() -> Result<Vec<SpoolerQueue>, HardwareError> {
    let lpstat = tool(&LPSTAT, "printer")?;
    let output = Command::new(lpstat)
        .arg("-e")
        .output()
        .map_err(|error| HardwareError::Io {
            device: "printer",
            detail: format!("cannot ask the spooler for its queues ({})", error.kind()),
        })?;
    // A host with no printers exits non-zero on some builds. That is an
    // answer, not a failure.
    let names = String::from_utf8_lossy(&output.stdout);

    // Which of them are taking work. `lpstat -a` is prose too, but the queue
    // name is still the first token, so a name appearing at all means the
    // spooler listed it as accepting.
    let accepting_raw = Command::new(lpstat)
        .arg("-a")
        .output()
        .map(|done| String::from_utf8_lossy(&done.stdout).into_owned())
        .unwrap_or_default();
    let accepting: Vec<&str> = accepting_raw
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();

    Ok(names
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| SpoolerQueue {
            name: name.to_owned(),
            accepting: accepting.contains(&name),
        })
        .collect())
}

/// An ESC/POS printer driven through a named spooler queue.
#[derive(Debug, Clone)]
pub struct EscPosSpoolerPrinter {
    queue: String,
}

impl EscPosSpoolerPrinter {
    pub fn new(queue: impl Into<String>) -> Self {
        Self {
            queue: queue.into(),
        }
    }

    /// Hand the bytes to the host spooler, marked raw.
    #[cfg(windows)]
    fn spool(&self, device: &'static str, bytes: &[u8]) -> Result<(), HardwareError> {
        edge_winspool::print_raw(&self.queue, "PX Kiosk receipt", bytes).map_err(
            |error| match error {
                edge_winspool::SpoolerError::NoSuchQueue(name) => HardwareError::NotConfigured {
                    device,
                    detail: format!("this host has no printer called `{name}`"),
                },
                other => HardwareError::Unavailable {
                    device,
                    detail: other.to_string(),
                },
            },
        )
    }

    #[cfg(unix)]
    fn spool(&self, device: &'static str, bytes: &[u8]) -> Result<(), HardwareError> {
        let lp = tool(&LP, device)?;
        let mut child = Command::new(lp)
            .arg("-d")
            .arg(&self.queue)
            // Pass the bytes through untouched. Without this the spooler
            // treats ESC/POS as text and prints the escape codes.
            .arg("-o")
            .arg("raw")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| HardwareError::Unavailable {
                device,
                detail: format!("cannot reach the print spooler ({})", error.kind()),
            })?;

        let written = child
            .stdin
            .take()
            .ok_or(HardwareError::Io {
                device,
                detail: "the spooler accepted no input".to_owned(),
            })
            .and_then(|mut stdin| {
                stdin.write_all(bytes).map_err(|error| HardwareError::Io {
                    device,
                    detail: format!("write to the spooler failed ({})", error.kind()),
                })
            });

        let finished = child
            .wait_with_output()
            .map_err(|error| HardwareError::Io {
                device,
                detail: format!("the spooler did not finish ({})", error.kind()),
            })?;
        written?;

        if finished.status.success() {
            return Ok(());
        }
        // The queue name is the operator's own configuration, so it is safe to
        // echo; it is not a network address.
        Err(HardwareError::Unavailable {
            device,
            detail: format!(
                "the spooler rejected the job for `{}`: {}",
                self.queue,
                String::from_utf8_lossy(&finished.stderr).trim()
            ),
        })
    }
}

impl PrinterPort for EscPosSpoolerPrinter {
    fn driver(&self) -> &'static str {
        "escpos_spooler"
    }

    fn print(&self, document: &PrintDocument) -> Result<(), HardwareError> {
        let bytes = render(document);
        for _ in 0..document.copies.max(1) {
            self.spool("printer", &bytes)?;
        }
        Ok(())
    }

    fn probe(&self) -> Result<(), HardwareError> {
        // As far as a probe may go: ask the spooler whether the queue exists
        // and is taking work. Nothing is sent, so no paper moves.
        let queues = list_queues()?;
        match queues.iter().find(|queue| queue.name == self.queue) {
            Some(queue) if queue.accepting => Ok(()),
            Some(_) => Err(HardwareError::Unavailable {
                device: "printer",
                detail: format!("the queue `{}` is disabled", self.queue),
            }),
            None => Err(HardwareError::NotConfigured {
                device: "printer",
                detail: format!("this host has no queue called `{}`", self.queue),
            }),
        }
    }
}
