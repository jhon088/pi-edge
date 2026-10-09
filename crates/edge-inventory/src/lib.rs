//! Local inventory.
//!
//! PI.Edge has durable authority over *the movements this installation
//! originated or observed* — a sale took two units out here, an operator put
//! five in here — whether or not anything upstream is reachable. It is not the
//! authority on global stock, and it keeps no product master of its own
//! (ADR 026).
//!
//! `on_hand` is derived from those movements, never stored: the ledger is the
//! record and the balance is a fold over it. It is therefore a **projection of
//! one installation**, not a census of a store — PI.Edge runs per POS, and a
//! store with several of them has several ledgers that cannot see each other
//! offline. Nothing here is mutable: a correction is a new compensating
//! movement.
//!
//! Pure: no database, no clock, no transport, no ERP. Time arrives as a
//! parameter.

pub mod balance;
pub mod error;
pub mod movement;

pub use balance::{on_hand, InventoryPosition};
pub use error::InventoryError;
pub use movement::{
    BootstrapItem, Direction, InventoryMovement, MovementSource, MovementType, NewInventoryMovement,
};
