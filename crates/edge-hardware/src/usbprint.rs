//! A USB printer's own device interface — the channel a print queue cannot be.
//!
//! The 3nStar and Epson receipt printers on a showroom stand are USB, and
//! every desktop OS exposes a USB receipt printer as a **print queue**. A
//! queue is one-way: `WritePrinter` hands the spooler bytes and there is no
//! call that hands anything back, so `DLE EOT` — which answers with a single
//! byte, in real time — cannot be asked through it. The spooler's own queue
//! flags are a different and poorer source, and conflating the two is how a
//! kiosk comes to claim paper it never measured (ADR 035, ADR 048).
//!
//! Windows does expose the printer itself. A USB printer registers a
//! **USBPRINT** device interface, which is a file: open it, write the three
//! request bytes, read the reply. That is a real bidirectional channel to the
//! firmware, and this module is the safe end of it — the FFI lives in
//! `edge-winspool`, the only crate permitted `unsafe`.
//!
//! Printing is untouched and stays on the spooler. This only asks.
//!
//! No escape sequence appears here: the request bytes come from
//! [`crate::escpos::StatusQuery`] and the reply is folded by
//! [`crate::escpos::read_status`], which remain the one place that knows the
//! protocol (CLAUDE.md).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::HardwareError;
use crate::escpos::{is_status_byte, read_status, StatusQuery};
use crate::printer::{PrinterStatus, PrinterStatusPort, StatusSource};

/// The host's USBPRINT devices, or nothing on a platform that has none.
///
/// One place that knows `edge-winspool` is Windows-only, so neither the
/// adapter below nor its caller carries a `cfg`.
mod host {
    #[cfg(windows)]
    pub fn devices() -> Result<Vec<String>, String> {
        edge_winspool::usb_printers()
            .map(|found| found.into_iter().map(|device| device.path).collect())
            .map_err(|error| error.to_string())
    }

    #[cfg(windows)]
    pub fn exchange(path: &str, request: &[u8], want: usize) -> Result<Vec<u8>, String> {
        edge_winspool::usb_exchange(path, request, want).map_err(|error| error.to_string())
    }

    #[cfg(not(windows))]
    pub fn devices() -> Result<Vec<String>, String> {
        Err(UNSUPPORTED.to_owned())
    }

    #[cfg(not(windows))]
    pub fn exchange(_path: &str, _request: &[u8], _want: usize) -> Result<Vec<u8>, String> {
        Err(UNSUPPORTED.to_owned())
    }

    #[cfg(not(windows))]
    pub const UNSUPPORTED: &str =
        "reading a USB printer directly is a Windows path; this build is not on Windows";
}

/// A USB printer's status, read over the device interface it registers.
///
/// The channel that makes `DLE EOT` possible for the hardware the showroom
/// actually has. The 3nStar and Epson units on a stand are USB, the host
/// exposes them as print queues, and a print queue is one-way — so the bytes
/// go to the **device**, through its USBPRINT interface, which is readable.
/// The spooler keeps printing; this only asks.
///
/// Windows only. On any other platform it reports `NO_RESPONSE` with the
/// reason, which is the honest answer and not a silent fall back to the
/// spooler's queue flags — those are a different and poorer source, and
/// conflating the two is how a kiosk comes to claim paper it never measured.
#[derive(Debug, Clone)]
pub struct UsbDleEotStatus {
    /// Which device to open, as a fragment of its interface path. Empty means
    /// "the only one", which is what a kiosk with one receipt printer has.
    hint: String,
    /// Set while an exchange is in flight.
    ///
    /// `ReadFile` on a USBPRINT handle has no timeout to set: it returns when
    /// the driver has something or when it gives up, and a firmware that
    /// answers nothing can leave it there. So a read that passed its deadline
    /// is *abandoned*, and this flag stops another being started on top of
    /// it — the handle is still open, a second `CreateFile` would be refused
    /// anyway, and threads would otherwise pile up one per poll.
    in_flight: Arc<AtomicBool>,
}

/// How long a whole four-byte read may take before it is abandoned.
///
/// `DLE EOT` is a real-time command: the printer answers ahead of its own
/// buffer, in microseconds. A second is already far longer than any working
/// device needs, and the deadline exists for the one that is not working.
const USB_DEADLINE: Duration = Duration::from_millis(1_200);

impl UsbDleEotStatus {
    pub fn new(hint: impl Into<String>) -> Self {
        Self {
            hint: hint.into(),
            in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Which USB printer this is configured for.
    ///
    /// With a hint, the first device whose path contains it. Without one, the
    /// only device there is — and **not** the first of several: a kiosk with
    /// two USB printers and no hint is a kiosk nobody has told which one to
    /// ask, and guessing would report the wrong printer's paper.
    fn device(&self) -> Result<String, String> {
        let devices = host::devices()?;
        let hint = self.hint.trim();
        if hint.is_empty() {
            return match devices.as_slice() {
                [only] => Ok(only.clone()),
                [] => Err("this host exposes no readable USB printer".to_owned()),
                many => Err(format!(
                    "this host has {} readable USB printers; name one in the \
                     status channel as `usb:<fragment>`",
                    many.len()
                )),
            };
        }
        let folded = hint.to_lowercase();
        devices
            .iter()
            .find(|path| path.to_lowercase().contains(&folded))
            .cloned()
            .ok_or_else(|| format!("no readable USB printer matches `{hint}`"))
    }

    /// Ask all four questions down one device, on a thread with a deadline.
    fn ask_all(&self) -> Result<[Option<u8>; 4], String> {
        if self.in_flight.swap(true, Ordering::SeqCst) {
            return Err(
                "a previous status read has not come back; the printer is not answering".to_owned(),
            );
        }
        let device = match self.device() {
            Ok(device) => device,
            Err(reason) => {
                self.in_flight.store(false, Ordering::SeqCst);
                return Err(reason);
            }
        };

        let (sender, receiver) = std::sync::mpsc::channel();
        let flag = Arc::clone(&self.in_flight);
        std::thread::spawn(move || {
            let mut answers = [None; 4];
            for (slot, query) in answers.iter_mut().zip(StatusQuery::ALL) {
                *slot = host::exchange(&device, &query.request(), 8)
                    .ok()
                    // The first byte that can be a reply. Firmware
                    // occasionally prefixes a byte or leaves a stale one, and
                    // a reply is recognisable by its fixed bits.
                    .and_then(|read| read.into_iter().find(|byte| is_status_byte(*byte)));
            }
            // Sent before the flag is cleared, so a receiver that is still
            // waiting gets the answer rather than racing the next poll.
            let _ = sender.send(answers);
            flag.store(false, Ordering::SeqCst);
        });

        receiver.recv_timeout(USB_DEADLINE).map_err(|_| {
            // The thread is left to finish. It holds the only handle, so the
            // next read is refused by `in_flight` until it does.
            "the printer did not answer DLE EOT within its deadline".to_owned()
        })
    }
}

impl PrinterStatusPort for UsbDleEotStatus {
    fn source(&self) -> StatusSource {
        StatusSource::DleEot
    }

    fn read(&self) -> Result<PrinterStatus, HardwareError> {
        match self.ask_all() {
            Ok([printer, offline_cause, error, paper]) => {
                Ok(read_status(printer, offline_cause, error, paper))
            }
            Err(reason) => Ok(PrinterStatus::no_response(StatusSource::DleEot, reason)),
        }
    }
}

/// Every USB printer this host can be asked about, for the admin screen.
///
/// Reports what the host reports and nothing else: an empty list where there
/// are none, and an empty list on a platform with no USBPRINT at all rather
/// than an error an operator cannot act on.
pub fn readable_usb_printers() -> Vec<String> {
    host::devices().unwrap_or_default()
}
