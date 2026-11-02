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
    /// What the spooler says is wrong with the device behind it.
    ///
    /// Carried separately from `ready` because the two answer different
    /// questions: `ready` decides whether to hand it a job, and these decide
    /// what to tell somebody standing in front of it. Every flag is the
    /// spooler's own — `PRINTER_INFO_2W.Status` — and not an inference from a
    /// job having been accepted.
    pub condition: QueueCondition,
}

/// What `PRINTER_INFO_2W.Status` says, reduced to what a receipt printer can
/// actually be.
///
/// This is the **fallback** status source: it is the only one available for a
/// USB receipt printer that the host exposes as a queue and nothing else, and
/// it is strictly less than the device's own firmware reports. It knows paper
/// *out* and not paper *near out*; it knows "door open" and not which door.
/// Said plainly here so nothing downstream reads its silence as health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueCondition {
    /// `PRINTER_STATUS_PAPER_OUT`.
    pub paper_out: bool,
    /// `PRINTER_STATUS_PAPER_PROBLEM` — paper, but not usable.
    pub paper_problem: bool,
    /// `PRINTER_STATUS_PAPER_JAM`.
    pub paper_jam: bool,
    /// `PRINTER_STATUS_DOOR_OPEN`.
    pub door_open: bool,
    /// `PRINTER_STATUS_OFFLINE` or `PRINTER_STATUS_NOT_AVAILABLE`.
    pub offline: bool,
    /// `PRINTER_STATUS_ERROR`.
    pub error: bool,
    /// `PRINTER_STATUS_PAUSED` — the operator stopped the queue, not the
    /// device.
    pub paused: bool,
    /// `PRINTER_STATUS_USER_INTERVENTION` — it wants somebody to look at it.
    pub needs_attention: bool,
    /// Whether the spooler reported anything at all for this queue.
    ///
    /// `false` means the status word was zero, which on Windows is the normal
    /// reading for a healthy idle printer — and is **not** the same as "this
    /// build could not ask".
    pub reported: bool,
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

    use windows_sys::core::GUID;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
        SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Graphics::Printing::{
        ClosePrinter, EndDocPrinter, EndPagePrinter, EnumPrintersW, OpenPrinterW, StartDocPrinterW,
        StartPagePrinter, WritePrinter, DOC_INFO_1W, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL,
        PRINTER_INFO_2W, PRINTER_STATUS_DOOR_OPEN, PRINTER_STATUS_ERROR,
        PRINTER_STATUS_NOT_AVAILABLE, PRINTER_STATUS_OFFLINE, PRINTER_STATUS_PAPER_JAM,
        PRINTER_STATUS_PAPER_OUT, PRINTER_STATUS_PAPER_PROBLEM, PRINTER_STATUS_PAUSED,
        PRINTER_STATUS_USER_INTERVENTION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, OPEN_EXISTING,
    };

    use super::{Queue, QueueCondition, SpoolerError, UsbDevice};

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
            let status = info.Status;
            let set = |flag: u32| status & flag != 0;
            found.push(Queue {
                name,
                ready: status & unavailable == 0,
                condition: QueueCondition {
                    paper_out: set(PRINTER_STATUS_PAPER_OUT),
                    paper_problem: set(PRINTER_STATUS_PAPER_PROBLEM),
                    paper_jam: set(PRINTER_STATUS_PAPER_JAM),
                    door_open: set(PRINTER_STATUS_DOOR_OPEN),
                    offline: set(PRINTER_STATUS_OFFLINE | PRINTER_STATUS_NOT_AVAILABLE),
                    error: set(PRINTER_STATUS_ERROR),
                    paused: set(PRINTER_STATUS_PAUSED),
                    needs_attention: set(PRINTER_STATUS_USER_INTERVENTION),
                    // Windows reports a healthy idle printer as zero, so
                    // "asked and told nothing is wrong" is a real answer.
                    reported: true,
                },
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

    // ------------------------------------------------------ USBPRINT, readable
    //
    // Four more calls, and the same rule as above: every `unsafe` block is a
    // single FFI call with its preconditions stated over it, and nothing
    // Windows hands back outlives the call that produced it except the device
    // handle, which is closed on every path.

    /// `GUID_DEVINTERFACE_USBPRINT` — the class every USB printer registers an
    /// interface under. From `usbprint.h`; it has never changed.
    const USBPRINT: GUID = GUID::from_u128(0x28d7_8fad_5a12_11d1_ae5b_0000_f803_a8c2);

    /// What `SetupDiGetClassDevsW` returns when it could not enumerate.
    /// `HDEVINFO` is an `isize` here, so `INVALID_HANDLE_VALUE` is not the
    /// right type to compare it against.
    const INVALID_DEVICE_SET: HDEVINFO = -1;

    pub fn usb_printers() -> Result<Vec<UsbDevice>, SpoolerError> {
        // SAFETY: a null enumerator and a null window with
        // `DIGCF_DEVICEINTERFACE` is the documented way to ask for every
        // present interface of one class.
        let set = unsafe {
            SetupDiGetClassDevsW(
                &USBPRINT,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if set == INVALID_DEVICE_SET || set == 0 {
            return Err(last_error("enumerating USB printers"));
        }

        let mut found = Vec::new();
        let mut index = 0u32;
        loop {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                InterfaceClassGuid: USBPRINT,
                Flags: 0,
                Reserved: 0,
            };
            // SAFETY: `set` is open, `interface` is sized as the API requires
            // and outlives the call.
            let more = unsafe {
                SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &USBPRINT, index, &mut interface)
            };
            if more == 0 {
                // Out of interfaces. The only expected way out of this loop.
                break;
            }
            index += 1;
            if let Some(path) = interface_path(set, &interface) {
                found.push(UsbDevice { path });
            }
        }

        // SAFETY: `set` came from SetupDiGetClassDevsW and is destroyed once.
        unsafe { SetupDiDestroyDeviceInfoList(set) };
        Ok(found)
    }

    /// One interface's device path, or `None` when Windows would not give it.
    ///
    /// `SP_DEVICE_INTERFACE_DETAIL_DATA_W` is the variable-length shape: a
    /// `cbSize` naming the *header*, then the path inline. The first call
    /// sizes it and is expected to fail.
    fn interface_path(set: HDEVINFO, interface: &SP_DEVICE_INTERFACE_DATA) -> Option<String> {
        let mut needed = 0u32;
        // SAFETY: a null buffer with zero length asks for the required size.
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                interface,
                std::ptr::null_mut(),
                0,
                &mut needed,
                std::ptr::null_mut(),
            )
        };
        if needed == 0 {
            return None;
        }

        // A `u32` buffer rather than bytes, so the allocation is aligned for
        // the `cbSize` field the API writes into it.
        let words = (needed as usize).div_ceil(size_of::<u32>()).max(2);
        let mut buffer = vec![0u32; words];
        let detail = buffer
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: `buffer` is at least `needed` bytes, aligned for the struct,
        // and `detail` points at its start.
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        // SAFETY: the buffer is `needed` bytes, which is what the sizing call
        // asked for, and stays alive for the whole call.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                interface,
                detail,
                needed,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: on success Windows wrote a NUL-terminated path into
        // `DevicePath`, inside the buffer we supplied.
        let path = unsafe { from_wide(std::ptr::addr_of!((*detail).DevicePath).cast::<u16>()) };
        (!path.is_empty()).then_some(path)
    }

    pub fn usb_exchange(path: &str, request: &[u8], want: usize) -> Result<Vec<u8>, SpoolerError> {
        let wide_path = wide(path);
        // Opened for read *and* write, which is the whole point, and with no
        // sharing: two readers of one printer would each take half the other's
        // status byte.
        // SAFETY: `wide_path` is NUL-terminated and outlives the call; null
        // security attributes and a null template are the documented defaults.
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(last_error("opening the USB printer"));
        }

        let exchange = (|| {
            let mut written = 0u32;
            let length = u32::try_from(request.len()).unwrap_or(u32::MAX);
            // SAFETY: `request` is valid for `length` and is only read.
            let ok = unsafe {
                WriteFile(
                    handle,
                    request.as_ptr(),
                    length,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(last_error("writing to the USB printer"));
            }

            let mut buffer = vec![0u8; want.max(1)];
            let mut read = 0u32;
            // SAFETY: `buffer` is `want` bytes and is written for at most
            // that many; `read` receives the count.
            let ok = unsafe {
                ReadFile(
                    handle,
                    buffer.as_mut_ptr(),
                    u32::try_from(buffer.len()).unwrap_or(u32::MAX),
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(last_error("reading from the USB printer"));
            }
            buffer.truncate(read as usize);
            Ok(buffer)
        })();

        // SAFETY: `handle` came from CreateFileW and is closed exactly once,
        // on success and failure alike.
        unsafe { CloseHandle(handle) };
        exchange
    }
}

#[cfg(not(windows))]
mod imp {
    #[allow(unused_imports)]
    use super::QueueCondition;
    use super::{Queue, SpoolerError, UsbDevice};

    pub fn queues() -> Result<Vec<Queue>, SpoolerError> {
        Err(SpoolerError::Unsupported)
    }

    pub fn probe(_queue: &str) -> Result<(), SpoolerError> {
        Err(SpoolerError::Unsupported)
    }

    pub fn print_raw(_queue: &str, _job: &str, _bytes: &[u8]) -> Result<(), SpoolerError> {
        Err(SpoolerError::Unsupported)
    }

    pub fn usb_printers() -> Result<Vec<UsbDevice>, SpoolerError> {
        Err(SpoolerError::Unsupported)
    }

    pub fn usb_exchange(
        _path: &str,
        _request: &[u8],
        _want: usize,
    ) -> Result<Vec<u8>, SpoolerError> {
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

// ============================================================ bidirectional
//
// A print queue is one-way. `WritePrinter` hands the spooler bytes and there
// is no call that hands anything back, so `DLE EOT` — which answers with a
// single byte, in real time — cannot be asked through it. The spooler's queue
// flags are a different thing and a poorer one: they are the driver's opinion
// about the queue, not the firmware's answer about itself.
//
// Windows does expose the printer itself. A USB receipt printer registers a
// **USBPRINT** device interface, which is a file: open it, write the three
// request bytes, read the reply. That is a real bidirectional channel to the
// device, and it is what the rest of this section is.

/// A USB printer this host exposes as a readable device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    /// The device interface path, which is what opens it. Carries the vendor
    /// and product ids, so it also identifies the unit to a person.
    pub path: String,
}

/// Every USB printer this host exposes a readable interface for.
///
/// Reports what Windows reports: present devices only, in enumeration order,
/// and an empty list when there are none. `Unsupported` on any other
/// platform — which is an answer, not a failure.
pub fn usb_printers() -> Result<Vec<UsbDevice>, SpoolerError> {
    imp::usb_printers()
}

/// Write bytes to a USB printer and read what it says back.
///
/// Blocking, and **not** bounded by this crate: `ReadFile` on a USBPRINT
/// handle returns when the driver has something or gives up, and there is no
/// timeout to set on it. The caller is the one that must not wait forever,
/// and `edge-hardware` is where that deadline lives — this is the FFI call
/// and nothing more.
///
/// `want` caps the read. A `DLE EOT` reply is one byte; anything beyond a
/// handful is not a reply this call has any business consuming.
pub fn usb_exchange(path: &str, request: &[u8], want: usize) -> Result<Vec<u8>, SpoolerError> {
    imp::usb_exchange(path, request, want)
}
