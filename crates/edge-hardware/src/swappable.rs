//! The one printer this installation drives: its adapter, its status channel,
//! and the lock that stops the two talking at once.
//!
//! Hardware is injected at the composition root and nothing downstream may
//! discover a device (ADR 039). That stays true: this *is* the port the root
//! injected, and the only thing holding the handle that replaces its adapter
//! is the root itself. The core keeps calling one `PrinterPort` and never
//! learns that what is behind it changed.
//!
//! Two reasons it exists rather than the adapter being handed over directly:
//!
//! * an operator configuring a receipt printer on a kiosk should not have to
//!   restart the runtime that is holding the day's sales open; and
//! * a `DLE EOT` status query and a receipt travel down the **same wire**,
//!   and a status byte asked for halfway through a ticket is a status byte
//!   mixed into a ticket. One mutex, held for a whole document and for a
//!   whole read, is what makes that impossible — and it has to live here,
//!   because this is the one place both paths pass through.

use std::sync::{Arc, Mutex, RwLock};

use crate::error::HardwareError;
use crate::printer::{PrintDocument, PrinterPort, PrinterStatus, PrinterStatusPort, StatusSource};

/// A `PrinterPort` that forwards to whichever adapter is currently installed,
/// and the status channel beside it.
pub struct SwappablePrinter {
    inner: RwLock<Arc<dyn PrinterPort>>,
    /// How to ask this printer how it is. `None` when the installation has no
    /// channel that can carry the question back.
    status: RwLock<Option<Arc<dyn PrinterStatusPort>>>,
    /// The wire. Held across a whole print and across a whole status read.
    wire: Mutex<()>,
}

impl std::fmt::Debug for SwappablePrinter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SwappablePrinter")
            .field("driver", &self.driver())
            .field("status_source", &self.status_source())
            .finish()
    }
}

impl SwappablePrinter {
    pub fn new(initial: Arc<dyn PrinterPort>) -> Self {
        Self {
            inner: RwLock::new(initial),
            status: RwLock::new(None),
            wire: Mutex::new(()),
        }
    }

    /// Install a different adapter. In-flight prints finish against the one
    /// they started with.
    pub fn replace(&self, next: Arc<dyn PrinterPort>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = next;
        }
    }

    /// Install, or remove, the channel that answers "how is the printer?".
    ///
    /// Removing it is as meaningful as installing one: a kiosk reconfigured
    /// onto a printer that cannot answer must go back to saying `UNKNOWN`
    /// rather than keep reporting the last device's paper.
    pub fn replace_status(&self, next: Option<Arc<dyn PrinterStatusPort>>) {
        if let Ok(mut guard) = self.status.write() {
            *guard = next;
        }
    }

    /// Who would answer a status read. `NONE` when nobody would.
    pub fn status_source(&self) -> StatusSource {
        self.status
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|port| port.source()))
            .unwrap_or(StatusSource::None)
    }

    /// Ask the printer how it is, without interrupting a document.
    ///
    /// Blocks until whatever is printing has finished, which is the point: on
    /// a shared wire there is no such thing as a read that does not interrupt.
    /// An installation with no status channel answers `UNKNOWN` rather than
    /// guessing, and `UNKNOWN` does not stop a sale (see
    /// [`PrinterStatus::blocks_payment`]).
    pub fn read_status(&self) -> PrinterStatus {
        let port = match self.status.read() {
            Ok(guard) => guard.as_ref().map(Arc::clone),
            Err(_) => None,
        };
        let Some(port) = port else {
            return PrinterStatus::unknown();
        };
        let Ok(_held) = self.wire.lock() else {
            return PrinterStatus::no_response(
                port.source(),
                "the printer lock is poisoned; nothing was asked",
            );
        };
        match port.read() {
            Ok(status) => status,
            // A channel that could not be used at all is a printer that did
            // not answer, which is exactly what it means to the rule.
            Err(error) => PrinterStatus::no_response(port.source(), error.to_string()),
        }
    }

    /// The adapter in force, cloned so the configuration lock is not held
    /// across a call that can block on a socket or a spooler.
    fn current(&self) -> Result<Arc<dyn PrinterPort>, HardwareError> {
        self.inner
            .read()
            .map(|guard| Arc::clone(&guard))
            .map_err(|_| HardwareError::Io {
                device: "printer",
                detail: "the printer configuration lock is poisoned".to_owned(),
            })
    }
}

impl PrinterPort for SwappablePrinter {
    fn driver(&self) -> &'static str {
        // A `&'static str` cannot be borrowed from behind the lock, so report
        // the indirection. The configured driver reaches the UI through the
        // settings, which is where an operator looks for it anyway.
        self.current().map(|inner| inner.driver()).unwrap_or("none")
    }

    fn print(&self, document: &PrintDocument) -> Result<(), HardwareError> {
        let inner = self.current()?;
        // Every copy under one hold: a ticket is not a place to answer a
        // status query, and a second copy is still the same ticket.
        let _held = self.wire.lock().map_err(|_| HardwareError::Io {
            device: "printer",
            detail: "the printer lock is poisoned; nothing was printed".to_owned(),
        })?;
        inner.print(document)
    }

    fn probe(&self) -> Result<(), HardwareError> {
        self.current()?.probe()
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::{PaperLevel, PrinterState};
    use crate::simulator::SimulatorPrinter;
    use crate::unconfigured::UnconfiguredPrinter;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A status channel that records when it was asked, and how many prints
    /// were in flight at that moment.
    #[derive(Debug, Default)]
    struct Nosy {
        printing: Arc<AtomicUsize>,
        caught: AtomicUsize,
        reads: AtomicUsize,
    }

    impl PrinterStatusPort for Nosy {
        fn source(&self) -> StatusSource {
            StatusSource::DleEot
        }

        fn read(&self) -> Result<PrinterStatus, HardwareError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.printing.load(Ordering::SeqCst) > 0 {
                self.caught.fetch_add(1, Ordering::SeqCst);
            }
            Ok(PrinterStatus {
                state: PrinterState::Online,
                online: true,
                paper: PaperLevel::Ok,
                source: StatusSource::DleEot,
                ..PrinterStatus::unknown()
            })
        }
    }

    /// A printer that takes long enough for a racing read to land inside it.
    #[derive(Debug)]
    struct Slow(Arc<AtomicUsize>);

    impl PrinterPort for Slow {
        fn driver(&self) -> &'static str {
            "slow"
        }

        fn print(&self, _document: &PrintDocument) -> Result<(), HardwareError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(40));
            self.0.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }

        fn probe(&self) -> Result<(), HardwareError> {
            Ok(())
        }
    }

    fn document() -> PrintDocument {
        PrintDocument::new(
            uuid::Uuid::new_v4(),
            crate::printer::PrintKind::TestPage,
            48,
        )
    }

    /// Nothing configured answers `UNKNOWN`, and `UNKNOWN` does not stop a
    /// sale: an installation that cannot ask knows nothing.
    #[test]
    fn a_printer_with_no_status_channel_answers_unknown_and_blocks_nothing() {
        let printer = SwappablePrinter::new(Arc::new(UnconfiguredPrinter));
        assert_eq!(printer.status_source(), StatusSource::None);
        let status = printer.read_status();
        assert_eq!(status.state, PrinterState::Unknown);
        assert_eq!(status.source, StatusSource::None);
        assert!(!status.blocks_payment());
    }

    /// The whole reason the mutex is here: a status query must never land in
    /// the middle of a ticket.
    #[test]
    fn a_status_read_never_lands_in_the_middle_of_a_document() {
        let printing = Arc::new(AtomicUsize::new(0));
        let nosy = Arc::new(Nosy {
            printing: Arc::clone(&printing),
            ..Nosy::default()
        });
        let printer = Arc::new(SwappablePrinter::new(Arc::new(Slow(Arc::clone(&printing)))));
        printer.replace_status(Some(Arc::clone(&nosy) as Arc<dyn PrinterStatusPort>));

        // Four printers and four pollers, going at it at once.
        let mut threads = Vec::new();
        for _ in 0..4 {
            let paper = Arc::clone(&printer);
            threads.push(std::thread::spawn(move || {
                paper.print(&document()).unwrap();
            }));
            let poller = Arc::clone(&printer);
            threads.push(std::thread::spawn(move || {
                for _ in 0..6 {
                    let _ = poller.read_status();
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }

        assert!(nosy.reads.load(Ordering::SeqCst) >= 24, "nothing polled");
        assert_eq!(
            nosy.caught.load(Ordering::SeqCst),
            0,
            "a status query was answered while a document was printing"
        );
    }

    /// And the channel can be taken away again, which has to put the answer
    /// back to `UNKNOWN` rather than leave the last device's reading behind.
    #[test]
    fn removing_the_channel_stops_it_reporting_the_old_printer() {
        let printer = SwappablePrinter::new(Arc::new(SimulatorPrinter::default()));
        printer.replace_status(Some(Arc::new(Nosy::default())));
        assert_eq!(printer.status_source(), StatusSource::DleEot);
        assert!(printer.read_status().online);

        printer.replace_status(None);
        assert_eq!(printer.status_source(), StatusSource::None);
        assert_eq!(printer.read_status().state, PrinterState::Unknown);
    }

    /// A channel that fails to read at all is a printer that did not answer.
    #[test]
    fn a_channel_that_cannot_be_read_is_no_response() {
        #[derive(Debug)]
        struct Broken;
        impl PrinterStatusPort for Broken {
            fn source(&self) -> StatusSource {
                StatusSource::DleEot
            }
            fn read(&self) -> Result<PrinterStatus, HardwareError> {
                Err(HardwareError::Unavailable {
                    device: "printer",
                    detail: "the status channel is not there".to_owned(),
                })
            }
        }
        let printer = SwappablePrinter::new(Arc::new(UnconfiguredPrinter));
        printer.replace_status(Some(Arc::new(Broken)));
        let status = printer.read_status();
        assert_eq!(status.state, PrinterState::NoResponse);
        assert!(status.blocks_payment());
        assert!(status.error.is_some());
    }
}
