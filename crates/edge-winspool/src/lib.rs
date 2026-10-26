//! The Windows print spooler, behind a safe API.
//!
//! A receipt printer on Windows is a spooler queue, and ESC/POS reaches it by
//! opening that queue and writing bytes with the `RAW` datatype — anything
//! else and the spooler tries to typeset the escape codes. There is no
//! command-line route to that: `print` and `Out-Printer` both render, and
//! `copy /b` only reaches shared queues. The Win32 printing API is the way.
//!
//! So this crate is the FFI boundary, and it is the **only** crate in the
//! workspace that may write `unsafe`. Everything else keeps
//! `unsafe_code = "forbid"`; the unsafety is here, in one file, behind two
//! functions, where it can be read in a sitting.
//!
//! On any other platform the functions compile to honest "not this platform"
//! answers, so callers need no `cfg` of their own.

use std::fmt;

/// A queue the spooler knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queue {
    pub name: String,
    /// Whether the spooler says it is taking work. A paused or errored queue
    /// is reported, not hidden.
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpoolerError {
    /// This build does not drive the Windows spooler.
    Unsupported,
    /// The queue is not one this host has.
    NoSuchQueue(String),
    /// The spooler refused, with the Win32 error code it refused with.
    Failed { operation: &'static str, code: u32 },
}

impl fmt::Display for SpoolerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => {
                formatter.write_str("the Windows print spooler is not available on this platform")
            }
            Self::NoSuchQueue(name) => {
                write!(formatter, "this host has no printer called `{name}`")
            }
            Self::Failed { operation, code } => {
                write!(
                    formatter,
                    "the spooler failed during {operation} (error {code})"
                )
            }
        }
    }
}

impl std::error::Error for SpoolerError {}

#[cfg(windows)]
mod imp {
    #![allow(unsafe_code)]
    //! Four calls: enumerate, open, start a RAW document, write it.
    //!
    //! Every `unsafe` block below is a single FFI call whose preconditions are
    //! stated above it. Nothing returned by Windows is held past the call that
    //! produced it except the printer handle, which is closed on every path.

    use std::ffi::{OsStr, OsString};
    use std::mem::size_of;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::slice;

    use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Graphics::Printing::{
        ClosePrinter, EndDocPrinter, EndPagePrinter, EnumPrintersW, OpenPrinterW, StartDocPrinterW,
        StartPagePrinter, WritePrinter, DOC_INFO_1W, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL,
        PRINTER_INFO_2W, PRINTER_STATUS_ERROR, PRINTER_STATUS_NOT_AVAILABLE,
        PRINTER_STATUS_OFFLINE, PRINTER_STATUS_PAPER_OUT, PRINTER_STATUS_PAUSED,
    };

    use super::{Queue, SpoolerError};

    /// A NUL-terminated UTF-16 copy, which is what every `*W` entry point
    /// expects.
    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn from_wide(pointer: *const u16) -> String {
        if pointer.is_null() {
            return String::new();
        }
        // SAFETY: the spooler returns NUL-terminated strings inside the buffer
        // it just filled; we read only up to that NUL and copy out.
        let mut length = 0usize;
        while unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: `length` was just measured up to the terminating NUL.
        let slice = unsafe { slice::from_raw_parts(pointer, length) };
        OsString::from_wide(slice).to_string_lossy().into_owned()
    }

    fn last_error(operation: &'static str) -> SpoolerError {
        // SAFETY: no arguments, no state; reads this thread's last error.
        SpoolerError::Failed {
            operation,
            code: unsafe { GetLastError() },
        }
    }

    pub fn queues() -> Result<Vec<Queue>, SpoolerError> {
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let mut needed = 0u32;
        let mut returned = 0u32;

        // First call sizes the buffer. It is expected to fail; `needed` is the
        // answer we came for.
        // SAFETY: a null buffer with zero length is the documented way to ask
        // for the required size.
        unsafe {
            EnumPrintersW(
                flags,
                std::ptr::null(),
                2,
                std::ptr::null_mut(),
                0,
                &mut needed,
                &mut returned,
            )
        };
        if needed == 0 {
            // No printers at all. An answer, not a failure.
            return Ok(Vec::new());
        }

        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: `buffer` is `needed` bytes, which is exactly what the sizing
        // call asked for, and stays alive for the whole call.
        let ok = unsafe {
            EnumPrintersW(
                flags,
                std::ptr::null(),
                2,
                buffer.as_mut_ptr(),
                needed,
                &mut needed,
                &mut returned,
            )
        };
        if ok == 0 {
            return Err(last_error("enumerating printers"));
        }

        let mut found = Vec::with_capacity(returned as usize);
        for index in 0..returned as usize {
            // SAFETY: the spooler wrote `returned` consecutive PRINTER_INFO_2W
            // records at the start of the buffer we supplied.
            let info = unsafe {
                &*(buffer.as_ptr().add(index * size_of::<PRINTER_INFO_2W>())
                    as *const PRINTER_INFO_2W)
            };
            let name = from_wide(info.pPrinterName);
            if name.is_empty() {
                continue;
            }
            let unavailable = PRINTER_STATUS_ERROR
                | PRINTER_STATUS_NOT_AVAILABLE
                | PRINTER_STATUS_OFFLINE
                | PRINTER_STATUS_PAPER_OUT
                | PRINTER_STATUS_PAUSED;
            found.push(Queue {
                name,
                ready: info.Status & unavailable == 0,
            });
        }
        Ok(found)
    }

    /// Open a queue, confirming it exists, and close it again. Nothing is
    /// printed, so this is safe to call as a probe.
    pub fn probe(queue: &str) -> Result<(), SpoolerError> {
        let handle = open(queue)?;
        close(handle);
        Ok(())
    }

    fn open(queue: &str) -> Result<HANDLE, SpoolerError> {
        let name = wide(queue);
        let mut handle: HANDLE = INVALID_HANDLE_VALUE;
        // SAFETY: `name` is NUL-terminated and outlives the call; a null
        // defaults pointer requests the queue's own defaults.
        let ok = unsafe { OpenPrinterW(name.as_ptr(), &mut handle, std::ptr::null()) };
        if ok == 0 || handle == INVALID_HANDLE_VALUE {
            return Err(SpoolerError::NoSuchQueue(queue.to_owned()));
        }
        Ok(handle)
    }

    fn close(handle: HANDLE) {
        // SAFETY: `handle` came from OpenPrinterW and is closed exactly once.
        unsafe { ClosePrinter(handle) };
    }

    /// Send bytes to a queue untouched.
    ///
    /// `RAW` is the whole point: it tells the spooler to pass the bytes to the
    /// device as they are, instead of treating ESC/POS as text to lay out.
    pub fn print_raw(queue: &str, job: &str, bytes: &[u8]) -> Result<(), SpoolerError> {
        let handle = open(queue)?;

        let title = wide(job);
        let datatype = wide("RAW");
        let info = DOC_INFO_1W {
            pDocName: title.as_ptr().cast_mut(),
            pOutputFile: std::ptr::null_mut(),
            pDatatype: datatype.as_ptr().cast_mut(),
        };

        // SAFETY: `handle` is open, and `info` plus the buffers it points at
        // outlive the call.
        let job_id = unsafe { StartDocPrinterW(handle, 1, &info) };
        if job_id == 0 {
            let error = last_error("starting the print job");
            close(handle);
            return Err(error);
        }

        let result = (|| {
            // SAFETY: `handle` is open and inside a started document.
            if unsafe { StartPagePrinter(handle) } == 0 {
                return Err(last_error("starting the page"));
            }
            let mut written = 0u32;
            let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
            // SAFETY: `bytes` is valid for `length` and is only read.
            let ok = unsafe { WritePrinter(handle, bytes.as_ptr().cast(), length, &mut written) };
            if ok == 0 {
                return Err(last_error("writing to the printer"));
            }
            if written != length {
                return Err(SpoolerError::Failed {
                    operation: "writing to the printer",
                    code: 0,
                });
            }
            // SAFETY: matched with the StartPagePrinter above.
            unsafe { EndPagePrinter(handle) };
            Ok(())
        })();

        // SAFETY: matched with StartDocPrinterW; runs on success and failure
        // alike so the spooler never keeps a half-open job.
        unsafe { EndDocPrinter(handle) };
        close(handle);
        result
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Queue, SpoolerError};

    pub fn queues() -> Result<Vec<Queue>, SpoolerError> {
        Err(SpoolerError::Unsupported)
    }

    pub fn probe(_queue: &str) -> Result<(), SpoolerError> {
        Err(SpoolerError::Unsupported)
    }

    pub fn print_raw(_queue: &str, _job: &str, _bytes: &[u8]) -> Result<(), SpoolerError> {
        Err(SpoolerError::Unsupported)
    }
}

/// Every queue this host exposes, as the spooler reports them.
pub fn queues() -> Result<Vec<Queue>, SpoolerError> {
    imp::queues()
}

/// Confirm a queue exists and can be opened. Prints nothing.
pub fn probe(queue: &str) -> Result<(), SpoolerError> {
    imp::probe(queue)
}

/// Write bytes to a queue with the `RAW` datatype, untouched.
pub fn print_raw(queue: &str, job: &str, bytes: &[u8]) -> Result<(), SpoolerError> {
    imp::print_raw(queue, job, bytes)
}

/// Whether this build can drive the spooler at all.
pub const SUPPORTED: bool = cfg!(windows);
