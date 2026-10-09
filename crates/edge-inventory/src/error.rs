//! Inventory errors, with stable machine-readable codes.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InventoryError {
    #[error("{movement_type} must move stock {expected}, not {actual}")]
    WrongDirection {
        movement_type: &'static str,
        expected: &'static str,
        actual: &'static str,
    },

    #[error("a {field} is required and must not be blank")]
    MissingReason { field: &'static str },

    /// Inventory keeps no product master of its own, so a movement is only
    /// accepted for a product the active catalog snapshot knows (ADR 026).
    #[error("product `{product_ref}` is not in the active catalog snapshot")]
    UnknownProduct { product_ref: String },

    /// The catalog has to be in place before inventory is.
    #[error("inventory needs an active catalog snapshot before it can be bootstrapped")]
    CatalogRequired,

    /// One bootstrap per installation, ever. A later baseline is a
    /// reconciliation, and half a reconciliation is worse than none (ADR 028).
    #[error(
        "this installation was already bootstrapped from snapshot \
         `{source_snapshot_id}`; a different baseline must be reconciled, not \
         re-imported"
    )]
    AlreadyBootstrapped { source_snapshot_id: String },

    #[error("an inventory bootstrap must contain at least one item")]
    EmptyBootstrap,

    #[error("product `{product_ref}` appears more than once in the bootstrap")]
    DuplicateBootstrapItem { product_ref: String },

    /// `allow_negative_inventory` is off and the sale would overdraw. Raised
    /// before any money moves, never after (ADR 029).
    #[error("product `{product_ref}` has {on_hand} on hand and {requested} was requested")]
    InsufficientStock {
        product_ref: String,
        on_hand: String,
        requested: String,
    },

    #[error("inventory record is invalid: {field} {reason}")]
    InvalidRecord { field: &'static str, reason: String },

    #[error("unknown {kind} value `{value}`")]
    UnknownEnumValue { kind: &'static str, value: String },

    #[error("{0}")]
    Quantity(#[from] edge_domain::prelude::MoneyError),
}

impl InventoryError {
    /// Stable code. Part of the protocol contract.
    pub const fn code(&self) -> &'static str {
        match self {
            InventoryError::WrongDirection { .. } => "INVENTORY_INVALID_MOVEMENT",
            InventoryError::MissingReason { .. } => "INVENTORY_REASON_REQUIRED",
            InventoryError::UnknownProduct { .. } => "INVENTORY_UNKNOWN_PRODUCT",
            InventoryError::CatalogRequired => "INVENTORY_CATALOG_REQUIRED",
            InventoryError::AlreadyBootstrapped { .. } => "INVENTORY_ALREADY_BOOTSTRAPPED",
            InventoryError::EmptyBootstrap | InventoryError::DuplicateBootstrapItem { .. } => {
                "INVENTORY_INVALID_BOOTSTRAP"
            }
            InventoryError::InsufficientStock { .. } => "INVENTORY_INSUFFICIENT_STOCK",
            InventoryError::InvalidRecord { .. } | InventoryError::UnknownEnumValue { .. } => {
                "INVENTORY_INVALID_RECORD"
            }
            InventoryError::Quantity(_) => "DOMAIN_RULE_VIOLATION",
        }
    }
}
