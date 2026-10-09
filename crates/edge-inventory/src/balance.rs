//! `on_hand`, derived from the ledger (ADR 026).
//!
//! There is no stored balance to drift: the movements are the record and this
//! is the fold over them. No materialised table in V1 either — a local till
//! accumulates thousands of movements a day, and a stored balance would be a
//! second thing to keep true.
//!
//! The fold is over *this installation's* movements, so the result is what this
//! Edge knows, not what a store physically holds (ADR 026).

use edge_domain::prelude::{ProductRef, Quantity, StoreId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::movement::{Direction, InventoryMovement};

/// What this installation's ledger says of one product, and the facts behind
/// it. A projection, not a store-wide count (ADR 026).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryPosition {
    pub store_id: StoreId,
    pub product_ref: ProductRef,
    /// `Σ(IN) − Σ(OUT)` in thousandths.
    ///
    /// A signed `i64` rather than a `Quantity`, because `Quantity` is a
    /// positive magnitude and a position can legitimately be negative: selling
    /// something this till did not know it had is a real, reportable
    /// condition, and clamping it to zero would hide it (ADR 029).
    pub on_hand_thousandths: i64,
    pub movement_count: i64,
    pub last_movement_at: Option<Timestamp>,
}

impl InventoryPosition {
    /// A product with no movements at all.
    pub fn empty(store_id: StoreId, product_ref: ProductRef) -> Self {
        Self {
            store_id,
            product_ref,
            on_hand_thousandths: 0,
            movement_count: 0,
            last_movement_at: None,
        }
    }

    /// Rendered the way every other quantity in the core is: `9.250`.
    pub fn on_hand_display(&self) -> String {
        let units = self.on_hand_thousandths / Quantity::SCALE;
        let fraction = (self.on_hand_thousandths % Quantity::SCALE).unsigned_abs();
        let sign = if self.on_hand_thousandths < 0 && units == 0 {
            "-"
        } else {
            ""
        };
        format!("{sign}{units}.{fraction:03}")
    }

    pub fn is_negative(&self) -> bool {
        self.on_hand_thousandths < 0
    }

    /// Whether `requested` can be taken without this projection going below
    /// zero. A local answer, not a store-wide one (ADR 026).
    pub fn covers(&self, requested: Quantity) -> bool {
        self.on_hand_thousandths >= requested.thousandths()
    }
}

/// Fold one product's movements into its position.
///
/// Order does not matter: addition is commutative and no movement depends on
/// another, so a replayed ledger always yields the same figure.
pub fn on_hand(
    store_id: StoreId,
    product_ref: ProductRef,
    movements: &[InventoryMovement],
) -> InventoryPosition {
    let mut position = InventoryPosition::empty(store_id, product_ref);
    for movement in movements {
        position.on_hand_thousandths += movement.signed_thousandths();
        position.movement_count += 1;
        position.last_movement_at = match position.last_movement_at {
            Some(previous) if previous >= movement.occurred_at => Some(previous),
            _ => Some(movement.occurred_at),
        };
    }
    position
}

/// Whether a movement of this direction and type is inbound. Exposed so a
/// storage adapter can aggregate without reimplementing the sign rule.
pub const fn is_inbound(direction: Direction) -> bool {
    matches!(direction, Direction::In)
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::TransactionId;
    use uuid::Uuid;

    use crate::movement::{MovementSource, MovementType, NewInventoryMovement};

    fn store() -> StoreId {
        StoreId::new("STORE-DEMO").unwrap()
    }

    fn product(reference: &str) -> ProductRef {
        ProductRef::new(reference).unwrap()
    }

    fn ts(minute: u32) -> Timestamp {
        Timestamp::parse_rfc3339(&format!("2026-10-08T15:{minute:02}:00Z")).unwrap()
    }

    fn movement(
        seed: u128,
        movement_type: MovementType,
        thousandths: i64,
        minute: u32,
    ) -> InventoryMovement {
        InventoryMovement::record(NewInventoryMovement {
            movement_id: Uuid::from_u128(seed),
            store_id: store(),
            product_ref: product("P-1"),
            movement_type,
            quantity: Quantity::from_thousandths(thousandths).unwrap(),
            source: match movement_type {
                MovementType::Sale => MovementSource::SaleLine {
                    transaction_id: TransactionId::from_uuid(Uuid::from_u128(seed + 100)),
                    line_number: 1,
                },
                MovementType::OpeningBalance => MovementSource::Bootstrap {
                    source_snapshot_id: "SNAP-1".to_owned(),
                },
                _ => MovementSource::Manual {
                    request_ref: Uuid::from_u128(seed + 200),
                },
            },
            business_day_id: None,
            session_id: None,
            reason_code: movement_type
                .requires_reason()
                .then(|| "RECOUNT".to_owned()),
            occurred_at: ts(minute),
        })
        .unwrap()
    }

    #[test]
    fn a_product_with_no_movements_holds_nothing() {
        let position = on_hand(store(), product("P-1"), &[]);
        assert_eq!(position.on_hand_thousandths, 0);
        assert_eq!(position.movement_count, 0);
        assert!(position.last_movement_at.is_none());
        assert_eq!(position.on_hand_display(), "0.000");
    }

    #[test]
    fn an_opening_balance_is_the_whole_position_at_first() {
        let ledger = vec![movement(1, MovementType::OpeningBalance, 10_000, 0)];
        let position = on_hand(store(), product("P-1"), &ledger);
        assert_eq!(position.on_hand_thousandths, 10_000);
        assert_eq!(position.on_hand_display(), "10.000");
        assert_eq!(position.last_movement_at, Some(ts(0)));
    }

    #[test]
    fn inbound_adds_and_outbound_subtracts() {
        let ledger = vec![
            movement(1, MovementType::OpeningBalance, 10_000, 0),
            movement(2, MovementType::Sale, 2_000, 1),
            movement(3, MovementType::ManualIn, 5_000, 2),
            movement(4, MovementType::ManualOut, 2_000, 3),
        ];
        let position = on_hand(store(), product("P-1"), &ledger);
        // 10 − 2 + 5 − 2 = 11
        assert_eq!(position.on_hand_thousandths, 11_000);
        assert_eq!(position.on_hand_display(), "11.000");
        assert_eq!(position.movement_count, 4);
        assert_eq!(position.last_movement_at, Some(ts(3)));
    }

    #[test]
    fn the_order_of_movements_does_not_change_the_position() {
        let ledger = vec![
            movement(1, MovementType::OpeningBalance, 10_000, 0),
            movement(2, MovementType::Sale, 2_000, 1),
            movement(3, MovementType::ManualIn, 500, 2),
        ];
        let forward = on_hand(store(), product("P-1"), &ledger);
        let reversed: Vec<InventoryMovement> = ledger.iter().rev().cloned().collect();
        assert_eq!(on_hand(store(), product("P-1"), &reversed), forward);
    }

    #[test]
    fn fractional_quantities_fold_exactly() {
        // 10.000 − 0.750 = 9.250, with no rounding anywhere.
        let ledger = vec![
            movement(1, MovementType::OpeningBalance, 10_000, 0),
            movement(2, MovementType::Sale, 750, 1),
        ];
        let position = on_hand(store(), product("P-1"), &ledger);
        assert_eq!(position.on_hand_thousandths, 9_250);
        assert_eq!(position.on_hand_display(), "9.250");
    }

    #[test]
    fn a_position_can_be_negative_and_says_so() {
        // Clamping this to zero would hide having sold something the system
        // did not know about (ADR 029).
        let ledger = vec![
            movement(1, MovementType::OpeningBalance, 1_000, 0),
            movement(2, MovementType::Sale, 2_500, 1),
        ];
        let position = on_hand(store(), product("P-1"), &ledger);
        assert_eq!(position.on_hand_thousandths, -1_500);
        assert!(position.is_negative());
        assert_eq!(position.on_hand_display(), "-1.500");
    }

    #[test]
    fn a_fractional_negative_position_renders_its_sign() {
        let ledger = vec![movement(1, MovementType::Sale, 750, 0)];
        let position = on_hand(store(), product("P-1"), &ledger);
        assert_eq!(position.on_hand_thousandths, -750);
        assert_eq!(position.on_hand_display(), "-0.750");
    }

    #[test]
    fn coverage_is_what_an_availability_check_asks() {
        let ledger = vec![movement(1, MovementType::OpeningBalance, 2_500, 0)];
        let position = on_hand(store(), product("P-1"), &ledger);
        assert!(position.covers(Quantity::from_thousandths(2_500).unwrap()));
        assert!(position.covers(Quantity::from_thousandths(750).unwrap()));
        assert!(!position.covers(Quantity::from_thousandths(2_501).unwrap()));
        // An empty position covers nothing.
        assert!(!InventoryPosition::empty(store(), product("P-2"))
            .covers(Quantity::from_thousandths(1).unwrap()));
    }

    #[test]
    fn the_latest_movement_wins_regardless_of_insertion_order() {
        let ledger = vec![
            movement(1, MovementType::OpeningBalance, 1_000, 5),
            movement(2, MovementType::ManualIn, 1_000, 2),
        ];
        assert_eq!(
            on_hand(store(), product("P-1"), &ledger).last_movement_at,
            Some(ts(5))
        );
    }
}
