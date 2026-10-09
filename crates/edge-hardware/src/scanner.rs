//! Turning keystrokes into exactly one barcode.
//!
//! A USB HID scanner in keyboard-wedge mode is a very fast typist: it sends the
//! barcode's characters and a terminator to whatever has focus. So the physical
//! capture happens in the host that owns the window, and **this** is the only
//! place that decides what a scan is (ADR 036).
//!
//! Pure and clock-free: time arrives as a parameter, so every rule below is
//! testable without a device.

use edge_domain::prelude::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One physical scan, ready to be acted on.
///
/// `event_id` identifies *this keypress sequence*, not the barcode. Two scans
/// of the same product are two events and two units: deduplicating by content
/// would silently refuse to sell the second one (ADR 036).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanEvent {
    pub event_id: Uuid,
    pub barcode: String,
    pub occurred_at: Timestamp,
    /// Which scanner, opaque. One in V1, but the model does not say "scanner1".
    pub device_id: String,
}

/// What counts as a scan rather than typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanPolicy {
    /// Shorter than this and it is not a barcode.
    pub min_length: usize,
    /// Longer than this and something is wrong; the buffer resets.
    pub max_length: usize,
    /// A gap longer than this means a human, not a scanner. The buffer resets
    /// rather than joining two unrelated inputs.
    pub inter_key_timeout_ms: i64,
}

impl Default for ScanPolicy {
    fn default() -> Self {
        Self {
            // EAN-8 is the shortest barcode worth accepting.
            min_length: 6,
            max_length: 64,
            // A scanner emits a full code in a few milliseconds; 50ms between
            // characters is already far slower than any device and far faster
            // than any person.
            inter_key_timeout_ms: 50,
        }
    }
}

/// What the host saw. Classified here, not by the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    /// A printable character.
    Char(char),
    /// `Enter`, `Tab`, `\r` or `\n`: the scanner saying "that was all".
    Terminator,
    /// A modifier or a navigation key. Ignored, and it does not reset the gap.
    Ignored,
}

impl KeyInput {
    /// Classify a key the way a browser's `KeyboardEvent.key` names it.
    ///
    /// The host passes the name through; it does not decide what the name
    /// means. That keeps the rule in one place for every host.
    pub fn from_key_name(key: &str) -> Self {
        match key {
            "Enter" | "NumpadEnter" | "Tab" | "\r" | "\n" | "\r\n" => KeyInput::Terminator,
            other => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    // Exactly one character, and not a control one.
                    (Some(c), None) if !c.is_control() => KeyInput::Char(c),
                    _ => KeyInput::Ignored,
                }
            }
        }
    }
}

/// Accumulates keystrokes and emits a barcode when one is complete.
///
/// One per scanner. Not `Sync` on purpose: it is per-input-source state, and
/// sharing it between sources is how two scanners would corrupt each other.
#[derive(Debug, Clone)]
pub struct ScanBuffer {
    policy: ScanPolicy,
    buffer: String,
    last_key_at: Option<Timestamp>,
}

impl Default for ScanBuffer {
    fn default() -> Self {
        Self::new(ScanPolicy::default())
    }
}

impl ScanBuffer {
    pub fn new(policy: ScanPolicy) -> Self {
        Self {
            policy,
            buffer: String::new(),
            last_key_at: None,
        }
    }

    /// Feed one key. Answers `Some(barcode)` exactly when a scan completed.
    pub fn push(&mut self, key: KeyInput, now: Timestamp) -> Option<String> {
        match key {
            // A modifier is not input: it neither extends nor interrupts a
            // scan, so the gap is measured between real characters.
            KeyInput::Ignored => None,
            KeyInput::Char(c) => {
                if self.gap_exceeded(now) {
                    // Too slow to be one scan. Whatever was buffered was
                    // typing, or a scan that never finished.
                    self.buffer.clear();
                }
                self.last_key_at = Some(now);
                self.buffer.push(c);
                if self.buffer.len() > self.policy.max_length {
                    self.buffer.clear();
                }
                None
            }
            KeyInput::Terminator => {
                // A terminator on an empty buffer is the second half of a
                // CRLF, or a stray Enter. Either way there is nothing to emit,
                // so CRLF yields one scan rather than two.
                if self.buffer.is_empty() {
                    return None;
                }
                let complete = self.gap_exceeded(now) || self.buffer.len() < self.policy.min_length;
                let barcode = std::mem::take(&mut self.buffer);
                self.last_key_at = None;
                if complete {
                    None
                } else {
                    Some(barcode)
                }
            }
        }
    }

    /// Feed a key by its host-level name.
    pub fn push_key(&mut self, key: &str, now: Timestamp) -> Option<String> {
        self.push(KeyInput::from_key_name(key), now)
    }

    /// Whatever is half-typed right now. Diagnostic only.
    pub fn pending_len(&self) -> usize {
        self.buffer.len()
    }

    fn gap_exceeded(&self, now: Timestamp) -> bool {
        let Some(last) = self.last_key_at else {
            return false;
        };
        let elapsed = now
            .as_datetime()
            .signed_duration_since(*last.as_datetime())
            .num_milliseconds();
        elapsed > self.policy.inter_key_timeout_ms
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> Timestamp {
        let base = chrono::DateTime::parse_from_rfc3339("2026-10-09T15:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        Timestamp::from_datetime(base + chrono::Duration::milliseconds(ms))
    }

    /// Type a barcode at scanner speed: one character per millisecond.
    fn scan(buffer: &mut ScanBuffer, barcode: &str, start: i64) -> Option<String> {
        for (index, c) in barcode.chars().enumerate() {
            let emitted = buffer.push(KeyInput::Char(c), at(start + index as i64));
            assert!(emitted.is_none(), "a character is never a complete scan");
        }
        buffer.push(KeyInput::Terminator, at(start + barcode.len() as i64))
    }

    #[test]
    fn a_barcode_and_enter_is_one_scan() {
        let mut buffer = ScanBuffer::default();
        assert_eq!(
            scan(&mut buffer, "7501234567890", 0).as_deref(),
            Some("7501234567890")
        );
        assert_eq!(buffer.pending_len(), 0);
    }

    #[test]
    fn cr_lf_and_crlf_each_terminate_one_scan() {
        for terminator in ["\r", "\n", "Enter", "Tab", "NumpadEnter"] {
            let mut buffer = ScanBuffer::default();
            for (index, c) in "7501234567890".chars().enumerate() {
                buffer.push_key(&c.to_string(), at(index as i64));
            }
            assert_eq!(
                buffer.push_key(terminator, at(20)).as_deref(),
                Some("7501234567890"),
                "{terminator}"
            );
        }
    }

    #[test]
    fn crlf_is_one_scan_not_two() {
        let mut buffer = ScanBuffer::default();
        for (index, c) in "7501234567890".chars().enumerate() {
            buffer.push_key(&c.to_string(), at(index as i64));
        }
        // The device sends both halves of the line ending.
        assert_eq!(
            buffer.push_key("\r", at(20)).as_deref(),
            Some("7501234567890")
        );
        // The second half finds an empty buffer and emits nothing.
        assert_eq!(buffer.push_key("\n", at(21)), None);
    }

    #[test]
    fn a_partial_buffer_is_discarded_after_the_timeout() {
        let mut buffer = ScanBuffer::default();
        for (index, c) in "750123".chars().enumerate() {
            buffer.push(KeyInput::Char(c), at(index as i64));
        }
        assert_eq!(buffer.pending_len(), 6);

        // The rest of the barcode arrives far too late to belong to it.
        let emitted = scan(&mut buffer, "4567890", 5_000);
        assert_eq!(
            emitted.as_deref(),
            Some("4567890"),
            "the late input is its own scan, not glued to the abandoned one"
        );
    }

    #[test]
    fn human_typing_never_becomes_a_scan() {
        let mut buffer = ScanBuffer::default();
        // A person typing digits, 200ms apart.
        for (index, c) in "7501234567890".chars().enumerate() {
            let emitted = buffer.push(KeyInput::Char(c), at(index as i64 * 200));
            assert!(emitted.is_none());
        }
        // Every keystroke reset the buffer, so Enter has one character to work
        // with and that is not a barcode.
        assert_eq!(buffer.push(KeyInput::Terminator, at(3_000)), None);
    }

    #[test]
    fn something_too_short_is_not_a_barcode() {
        let mut buffer = ScanBuffer::default();
        assert_eq!(scan(&mut buffer, "123", 0), None);
        assert_eq!(buffer.pending_len(), 0, "and the buffer is cleared");
    }

    #[test]
    fn control_and_modifier_keys_are_ignored_without_breaking_a_scan() {
        let mut buffer = ScanBuffer::default();
        for (index, key) in [
            "7",
            "5",
            "0",
            "Shift",
            "1",
            "Control",
            "2",
            "3",
            "ArrowLeft",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            "0",
        ]
        .iter()
        .enumerate()
        {
            buffer.push_key(key, at(index as i64));
        }
        assert_eq!(
            buffer.push_key("Enter", at(30)).as_deref(),
            Some("7501234567890")
        );
    }

    #[test]
    fn the_same_barcode_scanned_twice_is_two_scans() {
        let mut buffer = ScanBuffer::default();
        let first = scan(&mut buffer, "7501234567890", 0);
        let second = scan(&mut buffer, "7501234567890", 1_000);
        // Two physical scans of one product mean two units. Nothing here
        // deduplicates by content (ADR 036).
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn a_stray_terminator_emits_nothing() {
        let mut buffer = ScanBuffer::default();
        assert_eq!(buffer.push(KeyInput::Terminator, at(0)), None);
        assert_eq!(buffer.push_key("Enter", at(1)), None);
    }

    #[test]
    fn an_absurdly_long_run_resets_rather_than_growing() {
        let mut buffer = ScanBuffer::new(ScanPolicy {
            min_length: 4,
            max_length: 8,
            inter_key_timeout_ms: 50,
        });
        for (index, c) in "123456789".chars().enumerate() {
            buffer.push(KeyInput::Char(c), at(index as i64));
        }
        assert_eq!(buffer.pending_len(), 0);
    }

    #[test]
    fn key_names_are_classified_in_one_place() {
        assert_eq!(KeyInput::from_key_name("7"), KeyInput::Char('7'));
        assert_eq!(KeyInput::from_key_name("A"), KeyInput::Char('A'));
        assert_eq!(KeyInput::from_key_name("Enter"), KeyInput::Terminator);
        assert_eq!(KeyInput::from_key_name("Shift"), KeyInput::Ignored);
        assert_eq!(KeyInput::from_key_name("F5"), KeyInput::Ignored);
        assert_eq!(KeyInput::from_key_name(""), KeyInput::Ignored);
    }
}
