//! Stable error codes.
//!
//! Clients branch on these strings; they are part of the contract and must not
//! change meaning. Human-readable messages may change at any time, so no
//! client is allowed to parse them.
//!
//! Domain and application codes come from `AppError::kind()` in
//! `edge-application` and are listed here for reference:
//!
//! | Code | Meaning |
//! | --- | --- |
//! | `DEVICE_NOT_INITIALIZED` | the edge device has no tenant/store provisioned |
//! | `BUSINESS_DAY_ALREADY_OPEN` | a non-closed business day exists for the store |
//! | `NO_OPEN_BUSINESS_DAY` | the operation needs an open business day |
//! | `BUSINESS_DAY_NOT_OPEN` | the business day exists but is CLOSING/CLOSED |
//! | `BUSINESS_DAY_HAS_OPEN_SESSIONS` | cannot close the day with live sessions |
//! | `REGISTER_SESSION_ALREADY_OPEN` | the register already has a live session |
//! | `NO_OPEN_REGISTER_SESSION` | the operation needs an open session |
//! | `AMBIGUOUS_OPEN_SESSION` | more than one open session; the active one is ambiguous |
//! | `REGISTER_SESSION_NOT_OPEN` | the session is CLOSE_REQUESTED/CLOSED |
//! | `SESSION_HAS_ACTIVE_TRANSACTION` | cannot close the session mid-sale |
//! | `TRANSACTION_ALREADY_ACTIVE` | the session already has a live transaction |
//! | `NO_ACTIVE_TRANSACTION` | the operation needs an active transaction |
//! | `INVALID_TRANSACTION_STATE` | the aggregate refused the state transition |
//! | `DOMAIN_RULE_VIOLATION` | another domain invariant was violated |
//! | `CONCURRENT_MODIFICATION` | stale write detected |
//! | `STORAGE_FAILURE` | the local database failed |
//! | `SERIALIZATION_FAILURE` | a payload could not be (de)serialized |

// Codes produced by the core. They must stay identical to
// `AppError::kind()` in `edge-application`; the daemon has a test that pins
// the ones referenced here.

/// A state machine refused the transition (the most common business refusal).
pub const INVALID_TRANSACTION_STATE: &str = "INVALID_TRANSACTION_STATE";
/// Another domain invariant was violated.
pub const DOMAIN_RULE_VIOLATION: &str = "DOMAIN_RULE_VIOLATION";
/// The local database failed. The runtime reports itself as `DEGRADED`.
pub const STORAGE_FAILURE: &str = "STORAGE_FAILURE";

/// The `Hello` frame was rejected. Never discloses *why*.
pub const AUTHENTICATION_FAILED: &str = "AUTHENTICATION_FAILED";
/// A request arrived before a successful handshake.
pub const NOT_AUTHENTICATED: &str = "NOT_AUTHENTICATED";
/// The client speaks a protocol version this daemon does not support.
pub const PROTOCOL_VERSION_MISMATCH: &str = "PROTOCOL_VERSION_MISMATCH";
/// The frame was not valid JSON Lines, or not a known frame shape.
pub const MALFORMED_FRAME: &str = "MALFORMED_FRAME";
/// The operation payload failed validation at the boundary.
pub const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
/// The operation is not available on this connection (e.g. after SUBSCRIBE).
pub const UNSUPPORTED_OPERATION: &str = "UNSUPPORTED_OPERATION";
/// The daemon could not be reached, or the connection dropped. Produced by the
/// client, never received from the daemon.
pub const CORE_UNAVAILABLE: &str = "CORE_UNAVAILABLE";
/// The daemon answered with a result shape the client did not expect.
pub const UNEXPECTED_RESULT: &str = "UNEXPECTED_RESULT";
/// Another runtime already owns this installation's database.
pub const SINGLE_WRITER_VIOLATION: &str = "SINGLE_WRITER_VIOLATION";
