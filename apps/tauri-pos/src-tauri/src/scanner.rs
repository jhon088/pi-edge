//! Keyboard-wedge capture.
//!
//! A USB HID scanner in wedge mode types the barcode into whatever window has
//! focus, so the capture has to happen in the host that owns the window. The
//! webview forwards each keystroke here and **this side decides nothing**: the
//! deterministic parser lives in `edge-hardware`, so the rule for what counts
//! as a scan exists once, in Rust, and is tested without a device (ADR 036).
//!
//! What crosses into the core is a finished `ScanEvent` over the ordinary
//! protocol. React never resolves a product, never computes a quantity and
//! never holds commercial state.

use std::sync::Mutex;

use edge_hardware::{ScanBuffer, ScanPolicy};

/// Per-window scan state. One scanner per window in V1; the model carries an
/// opaque device id so a second one later is a value, not a redesign.
#[derive(Debug)]
pub struct ScannerCapture {
    buffer: Mutex<ScanBuffer>,
}

impl Default for ScannerCapture {
    fn default() -> Self {
        Self {
            buffer: Mutex::new(ScanBuffer::new(ScanPolicy::default())),
        }
    }
}

impl ScannerCapture {
    /// Feed one keystroke. Answers the barcode only when a scan completed.
    ///
    /// A plain `std::sync::Mutex`: this is microseconds of pure computation, so
    /// an async lock would add a runtime and a colour to every caller for
    /// nothing (CLAUDE.md).
    pub fn push(&self, key: &str, now: edge_domain::prelude::Timestamp) -> Option<String> {
        match self.buffer.lock() {
            Ok(mut buffer) => buffer.push_key(key, now),
            // A poisoned lock means a previous keystroke panicked. Dropping
            // the partial buffer is the safe answer: a half-read barcode must
            // never become a sale.
            Err(_) => None,
        }
    }
}
