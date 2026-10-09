//! Who says an original sale may be refunded.
//!
//! The product rule is that a production refund needs **central authority**
//! over the original sale: PI.COMMERCE Server validates it and hands back the
//! decision (ADR 030). That server does not exist yet, and PI.Edge must not
//! pretend a locally finalized sale is the same thing.
//!
//! So this lives in the composition root, next to the payment simulator and
//! for the same reason: it is the development stand-in for something a server
//! will do, it is selected by an environment switch, and it is **off by
//! default**. With no switch, every refund is refused with
//! `REFUND_AUTHORITY_REQUIRED` — the production semantics, preserved by
//! default rather than weakened by one.
//!
//! The refund domain never sees this module. It receives an already-resolved
//! `RefundAuthorizationContext` and asks no questions about where it came
//! from, which is exactly what M11 will replace: the resolver becomes a call
//! to the server and nothing downstream changes.

use edge_domain::prelude::TransactionId;
use edge_refunds::{RefundAuthority, RefundAuthorizationContext, RefundError};

/// Name of the development switch that grants refund authority locally.
pub const REFUND_AUTHORITY_ENV: &str = "PI_EDGE_REFUND_AUTHORITY";

/// The value that turns the development stand-in on. Spelled so nobody can
/// mistake it for a production setting.
const DEV_ACCEPT_LOCAL: &str = "dev-accept-local";

/// How this runtime resolves authority over an original sale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefundAuthoritySource {
    /// No authority available. Every refund is refused.
    Unavailable,
    /// Development only: treat a local sale as centrally accepted.
    DevAcceptLocal,
}

impl RefundAuthoritySource {
    /// Read the switch. An unrecognised value is a configuration error, not a
    /// silent fallback: a till that grants authority nobody asked for is worse
    /// than one that refuses to start.
    pub fn from_env() -> Result<Self, String> {
        let Some(raw) = std::env::var_os(REFUND_AUTHORITY_ENV) else {
            return Ok(Self::Unavailable);
        };
        match raw.to_string_lossy().trim() {
            "" => Ok(Self::Unavailable),
            DEV_ACCEPT_LOCAL => {
                tracing::warn!(
                    "refund authority is the development stand-in; local sales are treated \
                     as centrally accepted. Not for production use"
                );
                Ok(Self::DevAcceptLocal)
            }
            other => Err(format!(
                "`{other}` is not a known {REFUND_AUTHORITY_ENV} value \
                 (expected `{DEV_ACCEPT_LOCAL}`)"
            )),
        }
    }

    /// Resolve authority over one sale.
    ///
    /// Returns the error the domain would raise, so a refused refund answers
    /// with `REFUND_AUTHORITY_REQUIRED` whether the refusal came from here or
    /// from the domain itself.
    pub fn authorize(
        self,
        transaction_id: TransactionId,
    ) -> Result<RefundAuthorizationContext, RefundError> {
        match self {
            Self::Unavailable => Err(RefundError::AuthorityRequired {
                transaction_id: transaction_id.to_string(),
            }),
            Self::DevAcceptLocal => Ok(RefundAuthorizationContext {
                original_transaction_id: transaction_id,
                authority: RefundAuthority::CentralAccepted,
                // Marked, so an audit of development data is never mistaken
                // for an audit of a real authorisation.
                authorization_ref: Some(format!("DEV-LOCAL:{transaction_id}")),
            }),
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_authority_refuses_every_refund() {
        let id = TransactionId::from_uuid(uuid::Uuid::from_u128(1));
        let error = RefundAuthoritySource::Unavailable
            .authorize(id)
            .unwrap_err();
        // The production rule, preserved by default.
        assert_eq!(error.code(), "REFUND_AUTHORITY_REQUIRED");
    }

    #[test]
    fn the_development_stand_in_marks_what_it_produced() {
        let id = TransactionId::from_uuid(uuid::Uuid::from_u128(2));
        let context = RefundAuthoritySource::DevAcceptLocal
            .authorize(id)
            .expect("the development source authorizes");
        assert_eq!(context.authority, RefundAuthority::CentralAccepted);
        assert_eq!(context.original_transaction_id, id);
        assert!(context
            .authorization_ref
            .as_deref()
            .is_some_and(|reference| reference.starts_with("DEV-LOCAL:")));
    }
}
