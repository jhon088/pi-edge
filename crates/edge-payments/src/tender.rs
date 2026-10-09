//! Tenders and provider kinds.
//!
//! A tender is *commercial*: what the customer paid with, as a receipt and an
//! accountant see it. A provider is *technical*: who processed it. They are
//! separate so that routing a card payment through another processor is not a
//! change to the sale's commercial meaning.
//!
//! This is why there is no `MERCADO_PAGO` tender. Mercado Pago will be a
//! provider of `CARD`.

use serde::{Deserialize, Serialize};

use crate::error::PaymentError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TenderType {
    Cash,
    /// Generic card. The processor is a [`ProviderKind`], not a tender.
    Card,
}

impl TenderType {
    pub const fn as_str(self) -> &'static str {
        match self {
            TenderType::Cash => "CASH",
            TenderType::Card => "CARD",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PaymentError> {
        match value {
            "CASH" => Ok(TenderType::Cash),
            "CARD" => Ok(TenderType::Card),
            other => Err(PaymentError::UnknownEnumValue {
                kind: "TenderType",
                value: other.to_owned(),
            }),
        }
    }

    /// Whether this tender is settled by talking to a provider.
    ///
    /// Cash is not: the drawer movement is physical and immediate, so a cash
    /// payment has no attempts (ADR 016).
    pub const fn uses_provider(self) -> bool {
        match self {
            TenderType::Cash => false,
            TenderType::Card => true,
        }
    }

    /// Whether overpaying with this tender is allowed, producing change.
    ///
    /// Cash can: the customer hands over a note. A card cannot — the engine
    /// refuses a card request larger than the remaining balance, so a card
    /// never creates change.
    pub const fn may_overpay(self) -> bool {
        match self {
            TenderType::Cash => true,
            TenderType::Card => false,
        }
    }
}

/// Who processes a provider-backed tender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProviderKind {
    /// Deterministic in-process provider used by tests and development.
    Simulator,
}

impl ProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Simulator => "SIMULATOR",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PaymentError> {
        match value {
            "SIMULATOR" => Ok(ProviderKind::Simulator),
            other => Err(PaymentError::UnknownEnumValue {
                kind: "ProviderKind",
                value: other.to_owned(),
            }),
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenders_round_trip_through_their_wire_names() {
        for tender in [TenderType::Cash, TenderType::Card] {
            assert_eq!(TenderType::parse(tender.as_str()).unwrap(), tender);
        }
        assert!(TenderType::parse("MERCADO_PAGO").is_err());
    }

    #[test]
    fn cash_and_card_differ_in_exactly_two_ways() {
        assert!(!TenderType::Cash.uses_provider());
        assert!(TenderType::Cash.may_overpay());
        assert!(TenderType::Card.uses_provider());
        assert!(!TenderType::Card.may_overpay());
    }
}
