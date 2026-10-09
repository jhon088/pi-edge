//! Cash management.
//!
//! What the payment engine answers is *how a sale was covered*. What this
//! crate answers is *how much physical money should be in a drawer, and why*
//! (ADR 025). The two are different numbers whenever a customer overpays with
//! cash, so they are different contexts.
//!
//! Everything here is pure: no database, no clock, no transport, no hardware.
//! Time arrives as a parameter and the ledger is append-only — a correction is
//! a new movement, never an edit (ADR 021).

pub mod approval;
pub mod close;
pub mod error;
pub mod movement;
pub mod position;
pub mod safe_drop;

pub use approval::{Approval, ApprovalAction, ApprovalStatus, NewApproval};
pub use close::{CloseDeclaration, NewCloseDeclaration, SessionCloseRequirements};
pub use error::CashError;
pub use movement::{CashMovement, Direction, MovementType, NewCashMovement};
pub use position::{position, CashPosition};
pub use safe_drop::{NewSafeDrop, SafeDrop, SafeDropStatus};
