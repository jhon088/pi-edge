//! A printer port whose adapter can be replaced while the process runs.
//!
//! Hardware is injected at the composition root and nothing downstream may
//! discover a device (ADR 039). That stays true: this *is* the port the root
//! injected, and the only thing holding the handle that replaces its adapter
//! is the root itself. The core keeps calling one `PrinterPort` and never
//! learns that what is behind it changed.
//!
//! It exists because an operator configuring a receipt printer on a kiosk
//! should not have to restart the runtime that is holding the day's sales
//! open.

use std::sync::{Arc, RwLock};

use crate::error::HardwareError;
use crate::printer::{PrintDocument, PrinterPort};

/// A `PrinterPort` that forwards to whichever adapter is currently installed.
pub struct SwappablePrinter {
    inner: RwLock<Arc<dyn PrinterPort>>,
}

impl std::fmt::Debug for SwappablePrinter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SwappablePrinter")
            .field("driver", &self.driver())
            .finish()
    }
}

impl SwappablePrinter {
    pub fn new(initial: Arc<dyn PrinterPort>) -> Self {
        Self {
            inner: RwLock::new(initial),
        }
    }

    /// Install a different adapter. In-flight prints finish against the one
    /// they started with.
    pub fn replace(&self, next: Arc<dyn PrinterPort>) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = next;
        }
    }

    /// The adapter in force, cloned so the lock is not held across a call that
    /// can block on a socket or a spooler.
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
        self.current()?.print(document)
    }

    fn probe(&self) -> Result<(), HardwareError> {
        self.current()?.probe()
    }
}
