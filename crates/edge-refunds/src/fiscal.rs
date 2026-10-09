//! The fiscal consequence of a refund — the state, and nothing else.
//!
//! PI.Edge issues no CFDI, signs nothing and talks to no PAC or tax authority.
//! What it does is record, at the moment of the refund, which fiscal
//! consequence the operator chose, so the fiscal backend of PI.COMMERCE can
//! act on it later and so an auditor can see the decision (ADR 032).

use edge_domain::prelude::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::RefundError;

/// Whether the **original sale** was invoiced.
///
/// A projection of the sale's fiscal metadata, which has its own lifecycle: a
/// sale can be finalized today and invoiced tomorrow, so this never belongs to
/// the sale aggregate (ADR 032).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FiscalState {
    NotInvoiced,
    Invoiced,
}

impl FiscalState {
    pub const fn as_str(self) -> &'static str {
        match self {
            FiscalState::NotInvoiced => "NOT_INVOICED",
            FiscalState::Invoiced => "INVOICED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RefundError> {
        match value {
            "NOT_INVOICED" => Ok(FiscalState::NotInvoiced),
            "INVOICED" => Ok(FiscalState::Invoiced),
            other => Err(RefundError::UnknownEnumValue {
                kind: "FiscalState",
                value: other.to_owned(),
            }),
        }
    }
}

/// The fiscal metadata of one sale. Absent for a sale that never reached
/// `FINALIZED`; a finalized sale always has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionFiscalMetadata {
    pub fiscal_state: FiscalState,
    /// Canonical, opaque reference to the invoice, assigned upstream. Never a
    /// PAC payload, never an ERP document id.
    pub invoice_ref: Option<String>,
    pub updated_at: Timestamp,
}

/// What a refund asks to happen fiscally.
///
/// Three values because three are available in V1. `CREDIT_NOTE_COMPLETED` and
/// `CREDIT_NOTE_FAILED` are deliberately absent: nothing in PI.Edge can
/// produce them, and a state with no operation is a promise rather than a
/// model (frozen in milestone 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefundFiscalStatus {
    /// The original was never invoiced, so there is no credit note to speak of.
    None,
    /// The original was invoiced and the credit note is owed, later.
    CreditNotePending,
    /// The original was invoiced and the credit note was asked for now.
    CreditNoteRequested,
}

impl RefundFiscalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            RefundFiscalStatus::None => "NONE",
            RefundFiscalStatus::CreditNotePending => "CREDIT_NOTE_PENDING",
            RefundFiscalStatus::CreditNoteRequested => "CREDIT_NOTE_REQUESTED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RefundError> {
        match value {
            "NONE" => Ok(RefundFiscalStatus::None),
            "CREDIT_NOTE_PENDING" => Ok(RefundFiscalStatus::CreditNotePending),
            "CREDIT_NOTE_REQUESTED" => Ok(RefundFiscalStatus::CreditNoteRequested),
            other => Err(RefundError::UnknownEnumValue {
                kind: "RefundFiscalStatus",
                value: other.to_owned(),
            }),
        }
    }

    /// The only consequence available for a given original sale.
    ///
    /// Not invoiced leaves no choice at all: `NONE` is the only truthful
    /// answer, and offering a credit note for an invoice that does not exist
    /// would be inventing a fiscal document. Invoiced forces an explicit
    /// choice: the operator says whether the credit note is being asked for
    /// now or is owed later, and the refund records which (ADR 032).
    pub fn choose(
        original: FiscalState,
        requested: RefundFiscalStatus,
    ) -> Result<Self, RefundError> {
        match (original, requested) {
            (FiscalState::NotInvoiced, RefundFiscalStatus::None) => Ok(RefundFiscalStatus::None),
            (FiscalState::Invoiced, RefundFiscalStatus::CreditNotePending) => {
                Ok(RefundFiscalStatus::CreditNotePending)
            }
            (FiscalState::Invoiced, RefundFiscalStatus::CreditNoteRequested) => {
                Ok(RefundFiscalStatus::CreditNoteRequested)
            }
            (original, attempted) => Err(RefundError::FiscalActionInvalid {
                attempted: attempted.as_str(),
                original: original.as_str(),
            }),
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sale_that_was_never_invoiced_has_no_credit_note() {
        assert_eq!(
            RefundFiscalStatus::choose(FiscalState::NotInvoiced, RefundFiscalStatus::None).unwrap(),
            RefundFiscalStatus::None
        );
        for attempted in [
            RefundFiscalStatus::CreditNotePending,
            RefundFiscalStatus::CreditNoteRequested,
        ] {
            let error =
                RefundFiscalStatus::choose(FiscalState::NotInvoiced, attempted).unwrap_err();
            assert_eq!(error.code(), "REFUND_FISCAL_ACTION_INVALID");
        }
    }

    #[test]
    fn an_invoiced_sale_has_to_choose_and_cannot_choose_nothing() {
        for attempted in [
            RefundFiscalStatus::CreditNotePending,
            RefundFiscalStatus::CreditNoteRequested,
        ] {
            assert_eq!(
                RefundFiscalStatus::choose(FiscalState::Invoiced, attempted).unwrap(),
                attempted
            );
        }
        // Silently dropping the credit note of an invoiced sale is the one
        // outcome that leaves the books wrong.
        let error = RefundFiscalStatus::choose(FiscalState::Invoiced, RefundFiscalStatus::None)
            .unwrap_err();
        assert_eq!(error.code(), "REFUND_FISCAL_ACTION_INVALID");
    }

    #[test]
    fn fiscal_values_round_trip_through_their_wire_form() {
        for status in [
            RefundFiscalStatus::None,
            RefundFiscalStatus::CreditNotePending,
            RefundFiscalStatus::CreditNoteRequested,
        ] {
            assert_eq!(RefundFiscalStatus::parse(status.as_str()).unwrap(), status);
        }
        for state in [FiscalState::NotInvoiced, FiscalState::Invoiced] {
            assert_eq!(FiscalState::parse(state.as_str()).unwrap(), state);
        }
        assert!(RefundFiscalStatus::parse("CREDIT_NOTE_COMPLETED").is_err());
    }
}
