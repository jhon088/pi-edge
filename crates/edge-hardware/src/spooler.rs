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
use crate::printer::{
    PaperLevel, PrintDocument, PrinterPort, PrinterState, PrinterStatus, PrinterStatusPort,
    StatusSource,
};

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
    /// What the spooler says is wrong with the device behind it.
    pub fault: SpoolerFault,
}

/// What a host spooler can say about the printer behind a queue.
///
/// The **fallback** status source, and deliberately poorer than the device's
/// own firmware: it is what is available for a USB receipt printer the host
/// exposes as a queue and nothing else, which is every 3nStar on the stand.
/// Windows fills it from `PRINTER_INFO_2W.Status`; CUPS has no equivalent and
/// leaves it empty, which is why `reported` exists — "nothing is wrong" and
/// "nobody asked" are different answers and a kiosk must not confuse them
/// (ADR 035).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpoolerFault {
    pub paper_out: bool,
    pub paper_problem: bool,
    pub paper_jam: bool,
    pub door_open: bool,
    pub offline: bool,
    pub error: bool,
    pub paused: bool,
    pub needs_attention: bool,
    /// Whether this host's spooler reports device conditions at all.
    pub reported: bool,
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
                fault: SpoolerFault {
                    paper_out: queue.condition.paper_out,
                    paper_problem: queue.condition.paper_problem,
                    paper_jam: queue.condition.paper_jam,
                    door_open: queue.condition.door_open,
                    offline: queue.condition.offline,
                    error: queue.condition.error,
                    paused: queue.condition.paused,
                    needs_attention: queue.condition.needs_attention,
                    reported: queue.condition.reported,
                },
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
            // CUPS reports queue state in prose and in the operator's own
            // language, and nothing in it is a device sensor. Left empty
            // rather than parsed into a claim about paper.
            fault: SpoolerFault::default(),
        })
        .collect())
}

/// A printer's status as the **host spooler** reports it.
///
/// The fallback source, used when no `DLE EOT` channel is configured — which
/// is the normal case for a USB receipt printer, because a spooler queue is
/// write-only and cannot carry a status query back. It reports less than the
/// firmware would and says so: `source` is `SPOOLER`, paper is only ever
/// `END` or `UNKNOWN`, and `NEAR_END` is not a state it can produce.
///
/// What it does report is exactly what the brief refused to accept alone and
/// what the rule needs: a queue that has told the spooler the paper is out,
/// the door is open or the device is offline. Those three stop a payment here
/// as surely as they do over `DLE EOT`.
#[derive(Debug, Clone)]
pub struct SpoolerStatus {
    queue: String,
}

impl SpoolerStatus {
    pub fn new(queue: impl Into<String>) -> Self {
        Self {
            queue: queue.into(),
        }
    }
}

impl PrinterStatusPort for SpoolerStatus {
    fn source(&self) -> StatusSource {
        StatusSource::Spooler
    }

    fn read(&self) -> Result<PrinterStatus, HardwareError> {
        let queues = list_queues()?;
        let Some(queue) = queues.iter().find(|entry| entry.name == self.queue) else {
            // The configured printer is not on this host any more. Not an
            // error to raise — a state to report, and one that stops a sale.
            return Ok(PrinterStatus::no_response(
                StatusSource::Spooler,
                format!("this host has no printer called `{}`", self.queue),
            ));
        };
        Ok(from_fault(queue))
    }
}

/// One queue's flags, as a printer status.
fn from_fault(queue: &SpoolerQueue) -> PrinterStatus {
    let fault = queue.fault;
    // A spooler that reports no conditions at all has told us nothing about
    // the device, only that the queue exists and takes work. Claiming the
    // paper is `OK` from that would be inventing a sensor (ADR 035).
    if !fault.reported {
        return PrinterStatus {
            state: if queue.accepting {
                PrinterState::Online
            } else {
                PrinterState::Offline
            },
            online: queue.accepting,
            source: StatusSource::Spooler,
            error: (!queue.accepting)
                .then(|| format!("the queue `{}` is not taking work", queue.name)),
            ..PrinterStatus::unknown()
        };
    }

    let paper = if fault.paper_out || fault.paper_problem {
        PaperLevel::End
    } else {
        // The spooler has no near-end sensor and no "adequate" reading; the
        // absence of a complaint is not a measurement.
        PaperLevel::Unknown
    };
    let state = if fault.offline || fault.paused || paper == PaperLevel::End || fault.door_open {
        PrinterState::Offline
    } else if fault.error || fault.paper_jam || fault.needs_attention {
        PrinterState::Error
    } else {
        PrinterState::Online
    };

    PrinterStatus {
        state,
        online: state == PrinterState::Online,
        cover_open: fault.door_open,
        paper,
        // The spooler has no cutter line at all.
        cutter_error: false,
        jammed: fault.paper_jam,
        unrecoverable_error: false,
        source: StatusSource::Spooler,
        error: if fault.door_open {
            Some("the spooler reports the cover open".to_owned())
        } else if paper == PaperLevel::End {
            Some("the spooler reports no usable paper".to_owned())
        } else if fault.paper_jam {
            Some("the spooler reports a paper jam".to_owned())
        } else if fault.offline {
            Some("the spooler reports the printer offline".to_owned())
        } else if fault.paused {
            Some(format!("the queue `{}` is paused", queue.name))
        } else if fault.needs_attention || fault.error {
            Some("the spooler reports an error on the printer".to_owned())
        } else {
            None
        },
    }
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

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn queue(accepting: bool, fault: SpoolerFault) -> SpoolerQueue {
        SpoolerQueue {
            name: "3nStar RPT008".to_owned(),
            accepting,
            fault,
        }
    }

    fn reporting() -> SpoolerFault {
        SpoolerFault {
            reported: true,
            ..SpoolerFault::default()
        }
    }

    /// A spooler that reports no device conditions has told us nothing about
    /// the paper, and must not be read as a healthy roll.
    #[test]
    fn a_spooler_with_no_device_conditions_claims_nothing_about_paper() {
        let status = from_fault(&queue(true, SpoolerFault::default()));
        assert_eq!(status.state, PrinterState::Online);
        assert_eq!(
            status.paper,
            PaperLevel::Unknown,
            "a queue taking work was read as a measured roll"
        );
        assert_eq!(status.source, StatusSource::Spooler);
        assert!(!status.blocks_payment());
    }

    /// Nor when it does report, and reports nothing wrong: the absence of a
    /// complaint is not a sensor reading (ADR 035).
    #[test]
    fn a_healthy_queue_still_does_not_claim_the_paper_is_fine() {
        let status = from_fault(&queue(true, reporting()));
        assert_eq!(status.state, PrinterState::Online);
        assert!(status.online);
        assert_eq!(status.paper, PaperLevel::Unknown);
        assert_eq!(status.error, None);
        assert!(!status.blocks_payment());
    }

    /// And `NEAR_END` is a state this source cannot produce at all.
    #[test]
    fn the_spooler_can_never_report_paper_near_its_end() {
        for fault in [
            reporting(),
            SpoolerFault {
                paper_out: true,
                ..reporting()
            },
            SpoolerFault {
                paper_problem: true,
                ..reporting()
            },
        ] {
            assert_ne!(from_fault(&queue(true, fault)).paper, PaperLevel::NearEnd);
        }
    }

    /// The three conditions the rule names, through the fallback source.
    #[test]
    fn the_spooler_reports_the_three_things_that_stop_a_payment() {
        let out = from_fault(&queue(
            true,
            SpoolerFault {
                paper_out: true,
                ..reporting()
            },
        ));
        assert_eq!(out.paper, PaperLevel::End);
        assert_eq!(out.state, PrinterState::Offline);
        assert!(out.blocks_payment());

        let open = from_fault(&queue(
            true,
            SpoolerFault {
                door_open: true,
                ..reporting()
            },
        ));
        assert!(open.cover_open);
        assert_eq!(open.state, PrinterState::Offline);
        assert!(open.blocks_payment());

        let gone = from_fault(&queue(
            false,
            SpoolerFault {
                offline: true,
                ..reporting()
            },
        ));
        assert_eq!(gone.state, PrinterState::Offline);
        assert!(gone.blocks_payment());
    }

    /// A paused queue is the operator's doing and still stops a sale: nothing
    /// will come out of it.
    #[test]
    fn a_paused_queue_stops_a_payment_and_says_which_queue() {
        let status = from_fault(&queue(
            false,
            SpoolerFault {
                paused: true,
                ..reporting()
            },
        ));
        assert!(status.blocks_payment());
        assert!(
            status
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("3nStar"),
            "{:?}",
            status.error
        );
    }

    /// A jam is reported and does not stop a payment — the same rule the
    /// firmware path follows, so the two sources cannot disagree about it.
    #[test]
    fn a_jam_is_reported_without_closing_the_lane() {
        let status = from_fault(&queue(
            true,
            SpoolerFault {
                paper_jam: true,
                ..reporting()
            },
        ));
        assert!(status.jammed);
        assert_eq!(status.state, PrinterState::Error);
        assert!(!status.blocks_payment());
        // The spooler has no cutter line, and does not pretend to.
        assert!(!status.cutter_error);
    }

    /// A configured printer that is no longer on this host is not an error to
    /// raise at an operator; it is a state that stops a sale.
    #[test]
    fn a_printer_that_left_the_host_reads_as_no_response() {
        let status = SpoolerStatus::new("a printer nothing has ever installed")
            .read()
            .expect("reading a missing queue is an answer, not a failure");
        assert_eq!(status.state, PrinterState::NoResponse);
        assert_eq!(status.source, StatusSource::Spooler);
        assert!(status.blocks_payment());
        assert!(status.error.is_some());
    }
}
