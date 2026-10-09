//! The append-only movement ledger (ADR 026).
//!
//! A movement is a fact about this store: written once, never updated or
//! deleted. A correction is a new compensating movement, so the history always
//! reads as what happened rather than as what someone later wished had
//! happened.

use edge_domain::prelude::{
    BusinessDayId, ProductRef, Quantity, RefundId, SessionId, StoreId, Timestamp, TransactionId,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::InventoryError;

/// Which way stock moved.
///
/// Explicit rather than the sign of a `Quantity`: quantities are magnitudes
/// everywhere in PI.Edge, and reading a ledger must not depend on noticing a
/// minus sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::In => "IN",
            Direction::Out => "OUT",
        }
    }

    pub fn parse(value: &str) -> Result<Self, InventoryError> {
        match value {
            "IN" => Ok(Direction::In),
            "OUT" => Ok(Direction::Out),
            other => Err(InventoryError::UnknownEnumValue {
                kind: "Direction",
                value: other.to_owned(),
            }),
        }
    }
}

/// Why stock moved.
///
/// Four types, four operations. `ADJUSTMENT`, `REFUND`, `TRANSFER`, `RECEIPT`
/// and `COUNT` are absent because none of them has a command, an authorisation
/// policy or a workflow yet, and a type with none of those is a promise rather
/// than a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MovementType {
    /// The position the ledger starts from, written once by the bootstrap
    /// (ADR 028). Where it came from is `source_ref`, not a second type.
    OpeningBalance,
    /// Goods that left with a finalized sale (ADR 027).
    Sale,
    /// An operator putting stock in, with a stated reason.
    ManualIn,
    /// An operator taking stock out, with a stated reason.
    ManualOut,
    /// Goods that came back with an accepted commercial refund (ADR 033).
    /// Joined in milestone 9, when refunds gave it an operation; a refund is
    /// never recorded as a `MANUAL_IN`, because then nothing would say the
    /// stock came back from a customer.
    RefundReturn,
}

impl MovementType {
    pub const fn as_str(self) -> &'static str {
        match self {
            MovementType::OpeningBalance => "OPENING_BALANCE",
            MovementType::Sale => "SALE",
            MovementType::ManualIn => "MANUAL_IN",
            MovementType::ManualOut => "MANUAL_OUT",
            MovementType::RefundReturn => "REFUND_RETURN",
        }
    }

    pub fn parse(value: &str) -> Result<Self, InventoryError> {
        match value {
            "OPENING_BALANCE" => Ok(MovementType::OpeningBalance),
            "SALE" => Ok(MovementType::Sale),
            "MANUAL_IN" => Ok(MovementType::ManualIn),
            "MANUAL_OUT" => Ok(MovementType::ManualOut),
            "REFUND_RETURN" => Ok(MovementType::RefundReturn),
            other => Err(InventoryError::UnknownEnumValue {
                kind: "MovementType",
                value: other.to_owned(),
            }),
        }
    }

    /// The direction this type moves. Every type has exactly one, checked on
    /// construction and again by a database `CHECK`.
    pub const fn direction(self) -> Direction {
        match self {
            MovementType::OpeningBalance | MovementType::ManualIn | MovementType::RefundReturn => {
                Direction::In
            }
            MovementType::Sale | MovementType::ManualOut => Direction::Out,
        }
    }

    /// Whether an operator decided this, and therefore has to say why.
    pub const fn requires_reason(self) -> bool {
        matches!(self, MovementType::ManualIn | MovementType::ManualOut)
    }
}

/// What caused a movement, in a form that makes a duplicate impossible.
///
/// This is *source* identity, not command identity: a `request_id` makes a
/// retried call harmless, a `MovementSource` makes a duplicated fact
/// impossible. The two are kept apart on purpose (ADR 027).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MovementSource {
    /// The upstream snapshot that established the opening position.
    Bootstrap { source_snapshot_id: String },
    /// One line of one finalized sale.
    SaleLine {
        transaction_id: TransactionId,
        line_number: i64,
    },
    /// An operator action, identified by the command that requested it.
    Manual { request_ref: Uuid },
    /// One line of one accepted refund.
    RefundLine {
        refund_id: RefundId,
        line_number: i64,
    },
}

impl MovementSource {
    /// Stable string stored on the movement and made unique per
    /// `(store, movement_type, source_ref)`.
    pub fn as_ref_string(&self) -> String {
        match self {
            MovementSource::Bootstrap { source_snapshot_id } => source_snapshot_id.clone(),
            MovementSource::SaleLine {
                transaction_id,
                line_number,
            } => format!("{transaction_id}:{line_number}"),
            MovementSource::Manual { request_ref } => request_ref.to_string(),
            MovementSource::RefundLine {
                refund_id,
                line_number,
            } => format!("{refund_id}:{line_number}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryMovement {
    pub movement_id: Uuid,
    pub store_id: StoreId,
    pub product_ref: ProductRef,
    pub movement_type: MovementType,
    pub direction: Direction,
    /// A magnitude, never negative, in thousandths — the same `Quantity` the
    /// rest of the core uses, so `0.750 kg` means the same thing here.
    pub quantity: Quantity,
    /// What caused it. Also the idempotency key.
    pub source_ref: String,
    /// Set when the movement belongs to a sale.
    pub transaction_id: Option<TransactionId>,
    pub line_number: Option<i64>,
    /// Operational context, when the movement happened during trading.
    pub business_day_id: Option<BusinessDayId>,
    pub session_id: Option<SessionId>,
    pub reason_code: Option<String>,
    pub occurred_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct NewInventoryMovement {
    pub movement_id: Uuid,
    pub store_id: StoreId,
    pub product_ref: ProductRef,
    pub movement_type: MovementType,
    pub quantity: Quantity,
    pub source: MovementSource,
    pub business_day_id: Option<BusinessDayId>,
    pub session_id: Option<SessionId>,
    pub reason_code: Option<String>,
    pub occurred_at: Timestamp,
}

impl InventoryMovement {
    /// Record a movement. The direction comes from the type, so a caller
    /// cannot pair them wrongly.
    ///
    /// There is no quantity check here: `Quantity` refuses a non-positive
    /// value on construction, so a magnitude is positive by the time it
    /// arrives. Re-checking it would duplicate the type's own invariant.
    pub fn record(new: NewInventoryMovement) -> Result<Self, InventoryError> {
        if new.movement_type.requires_reason() {
            let blank = new
                .reason_code
                .as_deref()
                .map(|reason| reason.trim().is_empty())
                .unwrap_or(true);
            if blank {
                return Err(InventoryError::MissingReason {
                    field: "reason_code",
                });
            }
        }
        let (transaction_id, line_number) = match &new.source {
            MovementSource::SaleLine {
                transaction_id,
                line_number,
            } => (Some(*transaction_id), Some(*line_number)),
            _ => (None, None),
        };
        // A sale movement must come from a sale line, and only from one: the
        // pairing is what makes the ledger traceable back to the document.
        if (new.movement_type == MovementType::Sale) != transaction_id.is_some() {
            return Err(InventoryError::InvalidRecord {
                field: "source",
                reason: format!(
                    "{} must {}come from a sale line",
                    new.movement_type.as_str(),
                    if new.movement_type == MovementType::Sale {
                        ""
                    } else {
                        "not "
                    }
                ),
            });
        }
        Ok(Self {
            movement_id: new.movement_id,
            store_id: new.store_id,
            product_ref: new.product_ref,
            movement_type: new.movement_type,
            direction: new.movement_type.direction(),
            quantity: new.quantity,
            source_ref: new.source.as_ref_string(),
            transaction_id,
            line_number,
            business_day_id: new.business_day_id,
            session_id: new.session_id,
            reason_code: new.reason_code.map(|reason| reason.trim().to_owned()),
            occurred_at: new.occurred_at,
        })
    }

    /// Rebuild from storage without re-running the construction rules.
    pub fn rehydrate(movement: Self) -> Self {
        movement
    }

    /// Signed contribution to `on_hand`, for the fold in [`crate::balance`].
    /// Not exposed as the movement's quantity, which stays a magnitude.
    pub fn signed_thousandths(&self) -> i64 {
        match self.direction {
            Direction::In => self.quantity.thousandths(),
            Direction::Out => -self.quantity.thousandths(),
        }
    }
}

/// One line of an imported opening position (ADR 028).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapItem {
    pub product_ref: ProductRef,
    pub quantity: Quantity,
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreId {
        StoreId::new("STORE-DEMO").unwrap()
    }

    fn product(reference: &str) -> ProductRef {
        ProductRef::new(reference).unwrap()
    }

    fn ts() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-08T15:00:00Z").unwrap()
    }

    fn new(movement_type: MovementType, thousandths: i64) -> NewInventoryMovement {
        NewInventoryMovement {
            movement_id: Uuid::from_u128(1),
            store_id: store(),
            product_ref: product("P-1"),
            movement_type,
            quantity: Quantity::from_thousandths(thousandths).unwrap(),
            source: match movement_type {
                MovementType::Sale => MovementSource::SaleLine {
                    transaction_id: TransactionId::from_uuid(Uuid::from_u128(9)),
                    line_number: 1,
                },
                MovementType::OpeningBalance => MovementSource::Bootstrap {
                    source_snapshot_id: "SNAP-1".to_owned(),
                },
                _ => MovementSource::Manual {
                    request_ref: Uuid::from_u128(5),
                },
            },
            business_day_id: None,
            session_id: None,
            reason_code: movement_type
                .requires_reason()
                .then(|| "RECOUNT".to_owned()),
            occurred_at: ts(),
        }
    }

    #[test]
    fn a_type_decides_its_own_direction() {
        for (movement_type, expected) in [
            (MovementType::OpeningBalance, Direction::In),
            (MovementType::Sale, Direction::Out),
            (MovementType::ManualIn, Direction::In),
            (MovementType::ManualOut, Direction::Out),
        ] {
            let movement = InventoryMovement::record(new(movement_type, 1_000)).unwrap();
            assert_eq!(movement.direction, expected);
            // A caller cannot pair them wrongly: it never supplies a direction.
            assert_eq!(movement_type.direction(), expected);
        }
    }

    #[test]
    fn an_inbound_movement_adds_and_an_outbound_one_subtracts() {
        assert_eq!(
            InventoryMovement::record(new(MovementType::ManualIn, 2_500))
                .unwrap()
                .signed_thousandths(),
            2_500
        );
        assert_eq!(
            InventoryMovement::record(new(MovementType::Sale, 750))
                .unwrap()
                .signed_thousandths(),
            -750
        );
    }

    #[test]
    fn a_quantity_is_a_positive_magnitude_by_construction() {
        // The type carries the invariant, so the ledger does not re-check it.
        for thousandths in [0, -1, -1_000] {
            assert!(Quantity::from_thousandths(thousandths).is_err());
        }
    }

    #[test]
    fn an_operator_movement_must_say_why() {
        for movement_type in [MovementType::ManualIn, MovementType::ManualOut] {
            let mut blank = new(movement_type, 1_000);
            blank.reason_code = Some("   ".to_owned());
            assert_eq!(
                InventoryMovement::record(blank).unwrap_err().code(),
                "INVENTORY_REASON_REQUIRED"
            );
            let mut missing = new(movement_type, 1_000);
            missing.reason_code = None;
            assert!(InventoryMovement::record(missing).is_err());
        }
    }

    #[test]
    fn a_runtime_movement_needs_no_reason() {
        // A sale and an opening balance are caused by the runtime, not by an
        // operator deciding something, so there is nothing to explain.
        for movement_type in [MovementType::Sale, MovementType::OpeningBalance] {
            assert!(InventoryMovement::record(new(movement_type, 1_000)).is_ok());
        }
    }

    #[test]
    fn a_sale_movement_is_traceable_to_its_line_and_only_a_sale_is() {
        let sale = InventoryMovement::record(new(MovementType::Sale, 3_000)).unwrap();
        assert_eq!(sale.line_number, Some(1));
        assert!(sale.transaction_id.is_some());
        assert_eq!(
            sale.source_ref,
            format!("{}:1", TransactionId::from_uuid(Uuid::from_u128(9)))
        );

        // A sale that does not come from a line, and a manual movement that
        // does, are both confused records.
        let mut orphan_sale = new(MovementType::Sale, 1_000);
        orphan_sale.source = MovementSource::Manual {
            request_ref: Uuid::from_u128(5),
        };
        assert_eq!(
            InventoryMovement::record(orphan_sale).unwrap_err().code(),
            "INVENTORY_INVALID_RECORD"
        );
        let mut manual_from_line = new(MovementType::ManualIn, 1_000);
        manual_from_line.source = MovementSource::SaleLine {
            transaction_id: TransactionId::from_uuid(Uuid::from_u128(9)),
            line_number: 2,
        };
        assert!(InventoryMovement::record(manual_from_line).is_err());
    }

    #[test]
    fn a_source_reference_identifies_exactly_one_fact() {
        // Two lines of one sale are two different sources; the same line twice
        // is the same source, which is what makes a replay a no-op.
        let line_one = MovementSource::SaleLine {
            transaction_id: TransactionId::from_uuid(Uuid::from_u128(9)),
            line_number: 1,
        };
        let line_two = MovementSource::SaleLine {
            transaction_id: TransactionId::from_uuid(Uuid::from_u128(9)),
            line_number: 2,
        };
        assert_ne!(line_one.as_ref_string(), line_two.as_ref_string());
        assert_eq!(line_one.as_ref_string(), line_one.clone().as_ref_string());
    }

    #[test]
    fn fractional_quantities_keep_the_core_semantics() {
        // 0.750 kg is 750 thousandths here exactly as it is on a sale line.
        let movement = InventoryMovement::record(new(MovementType::Sale, 750)).unwrap();
        assert_eq!(movement.quantity.thousandths(), 750);
        assert_eq!(movement.quantity.to_string(), "0.750");
    }
}
