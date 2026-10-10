//! Timestamps.
//!
//! The domain never reads the clock; callers pass a [`Timestamp`] obtained from
//! the `Clock` port. Persistence uses RFC-3339 UTC with microsecond precision,
//! which sorts lexicographically.

use std::fmt;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("`{0}` is not a valid RFC-3339 UTC timestamp")]
    InvalidTimestamp(String),
}

/// An instant in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    pub const fn from_datetime(value: DateTime<Utc>) -> Self {
        Self(value)
    }

    pub const fn as_datetime(&self) -> &DateTime<Utc> {
        &self.0
    }

    /// Canonical persisted representation.
    pub fn to_rfc3339(&self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Micros, true)
    }

    /// This instant, `seconds` later.
    ///
    /// Used to schedule a retry. Saturates instead of wrapping: a clock near
    /// the end of representable time is not a reason to schedule an attempt in
    /// the past.
    pub fn plus_seconds(self, seconds: i64) -> Self {
        match chrono::TimeDelta::try_seconds(seconds) {
            Some(delta) => Self(self.0.checked_add_signed(delta).unwrap_or(self.0)),
            None => self,
        }
    }

    pub fn parse_rfc3339(value: &str) -> Result<Self, TimeError> {
        DateTime::parse_from_rfc3339(value)
            .map(|dt| Self(dt.with_timezone(&Utc)))
            .map_err(|_| TimeError::InvalidTimestamp(value.to_owned()))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_rfc3339() {
        let raw = "2026-10-08T15:04:05.123456Z";
        let ts = Timestamp::parse_rfc3339(raw).unwrap();
        assert_eq!(ts.to_rfc3339(), raw);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Timestamp::parse_rfc3339("yesterday").is_err());
    }

    #[test]
    fn persisted_form_sorts_chronologically() {
        let a = Timestamp::parse_rfc3339("2026-10-08T09:00:00Z").unwrap();
        let b = Timestamp::parse_rfc3339("2026-10-08T10:00:00Z").unwrap();
        assert!(a < b);
        assert!(a.to_rfc3339() < b.to_rfc3339());
    }
}
