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
//! from.
//!
//! Milestone 11 added the third source: asking through the configured sync
//! transport. What it does **not** add is a durable grant. The answer
//! authorises *this* operation — it is materialised on the refund as
//! evidence and nowhere else. There is no `sale_refund_authorized` flag, no
//! "centrally accepted" projection, nothing that would let a later refund
//! skip the question. **Production refunds are online**, and that is a
//! product rule, not a limitation of this file (ADR 042).

use std::sync::Arc;

use edge_domain::prelude::{StoreId, TenantId, TransactionId};
use edge_refunds::{RefundAuthority, RefundAuthorizationContext, RefundError};
use edge_sync::{RefundAuthorizationDecision, RefundAuthorizationRequest, SyncTransport};
use uuid::Uuid;

/// Name of the development switch that grants refund authority locally.
pub const REFUND_AUTHORITY_ENV: &str = "PI_EDGE_REFUND_AUTHORITY";

/// The value that turns the development stand-in on. Spelled so nobody can
/// mistake it for a production setting.
const DEV_ACCEPT_LOCAL: &str = "dev-accept-local";

/// How this runtime resolves authority over an original sale.
#[derive(Debug, Clone)]
pub enum RefundAuthoritySource {
    /// No authority available. Every refund is refused.
    Unavailable,
    /// Development only: treat a local sale as centrally accepted.
    DevAcceptLocal,
    /// Ask the authority through the configured sync transport, now, for this
    /// one refund.
    ///
    /// Chosen automatically when a transport is configured, because a till
    /// that can reach the authority has no business guessing. With no
    /// transport this variant is never built and the default refusal stands.
    Transport(Arc<dyn SyncTransport>),
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

    /// Resolve authority over one sale, for one refund, right now.
    ///
    /// Three refusals, three codes, because an operator does something
    /// different about each (ADR 042):
    ///
    /// | Situation | Code |
    /// | --- | --- |
    /// | no authority configured | `REFUND_AUTHORITY_REQUIRED` |
    /// | configured, unreachable or timed out | `REFUND_AUTHORITY_UNAVAILABLE` |
    /// | reached, and it said no | `REFUND_NOT_AUTHORIZED` |
    ///
    /// All three are refusals. **No authority, no refund**: PI.Edge sells
    /// offline; it does not refund offline.
    pub fn authorize(
        &self,
        transaction_id: TransactionId,
        tenant_id: &TenantId,
        store_id: &StoreId,
        request_id: Uuid,
    ) -> Result<RefundAuthorizationContext, RefundError> {
        match self {
            // Nothing to ask.
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
            Self::Transport(transport) => {
                let request = RefundAuthorizationRequest {
                    transaction_id: transaction_id.to_string(),
                    tenant_id: tenant_id.as_str().to_owned(),
                    store_id: store_id.as_str().to_owned(),
                    request_id,
                };
                match transport.authorize_refund(&request) {
                    Ok(RefundAuthorizationDecision::Accepted { authorization_ref }) => {
                        Ok(RefundAuthorizationContext {
                            original_transaction_id: transaction_id,
                            authority: RefundAuthority::CentralAccepted,
                            authorization_ref: Some(authorization_ref),
                        })
                    }
                    // Reached, and it said no. Retrying changes nothing, so
                    // the code says so.
                    Ok(RefundAuthorizationDecision::Refused { reason }) => {
                        tracing::info!(%transaction_id, reason, "the authority refused the refund");
                        Err(RefundError::NotAuthorized {
                            transaction_id: transaction_id.to_string(),
                            reason,
                        })
                    }
                    // Configured and unreachable. A different answer to an
                    // operator than "no authority is configured", and a
                    // different one again from "the authority said no"
                    // (ADR 042).
                    Err(error) => {
                        tracing::warn!(
                            %transaction_id,
                            code = error.code(),
                            "the refund authority could not be reached"
                        );
                        Err(RefundError::AuthorityUnavailable {
                            transaction_id: transaction_id.to_string(),
                            detail: error.code().to_owned(),
                        })
                    }
                }
            }
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    use edge_sync::simulator::{SimulatorSyncTransport, SIMULATOR};
    use edge_sync::SyncError;

    fn tenant() -> TenantId {
        TenantId::new("TENANT-TEST").expect("a valid opaque id")
    }

    fn store() -> StoreId {
        StoreId::new("STORE-TEST").expect("a valid opaque id")
    }

    fn ask(
        source: &RefundAuthoritySource,
        id: TransactionId,
    ) -> Result<RefundAuthorizationContext, RefundError> {
        source.authorize(id, &tenant(), &store(), Uuid::from_u128(99))
    }

    #[test]
    fn no_authority_refuses_every_refund() {
        let id = TransactionId::from_uuid(Uuid::from_u128(1));
        let error = ask(&RefundAuthoritySource::Unavailable, id).unwrap_err();
        // The production rule, preserved by default.
        assert_eq!(error.code(), "REFUND_AUTHORITY_REQUIRED");
    }

    #[test]
    fn an_authority_that_cannot_be_reached_refuses_the_refund() {
        // The point of the online rule: an unreachable authority is not a
        // reason to refund anyway, and nothing durable remembers that we
        // asked.
        let transport = Arc::new(SimulatorSyncTransport::new());
        transport.script_refund_decision(Err(SyncError::Unavailable {
            detail: "no route".to_owned(),
        }));
        let source = RefundAuthoritySource::Transport(transport.clone());
        let id = TransactionId::from_uuid(Uuid::from_u128(3));
        let error = ask(&source, id).unwrap_err();
        // Not `REFUND_AUTHORITY_REQUIRED`: this installation *is* configured,
        // the server is simply not there right now.
        assert_eq!(error.code(), "REFUND_AUTHORITY_UNAVAILABLE");
        assert_eq!(transport.refund_requests().len(), 1);
        assert_eq!(transport.driver(), SIMULATOR);
    }

    #[test]
    fn an_authority_that_says_no_refuses_the_refund() {
        let transport = Arc::new(SimulatorSyncTransport::new());
        transport.script_refund_decision(Ok(RefundAuthorizationDecision::Refused {
            reason: "the sale is older than the refund window".to_owned(),
        }));
        let source = RefundAuthoritySource::Transport(transport);
        let id = TransactionId::from_uuid(Uuid::from_u128(4));
        let error = ask(&source, id).unwrap_err();
        // A decision, not a connectivity problem. Retrying changes nothing.
        assert_eq!(error.code(), "REFUND_NOT_AUTHORIZED");
        assert!(
            error.to_string().contains("refund window"),
            "the authority's reason reaches the operator: {error}"
        );
    }

    #[test]
    fn an_authorized_refund_carries_the_authoritys_reference_as_evidence() {
        let transport = Arc::new(SimulatorSyncTransport::new());
        transport.script_refund_decision(Ok(RefundAuthorizationDecision::Accepted {
            authorization_ref: "AUTH-4711".to_owned(),
        }));
        let source = RefundAuthoritySource::Transport(transport);
        let id = TransactionId::from_uuid(Uuid::from_u128(5));
        let context = ask(&source, id).expect("the authority accepted");
        assert_eq!(context.authority, RefundAuthority::CentralAccepted);
        assert_eq!(context.authorization_ref.as_deref(), Some("AUTH-4711"));
        // Asking again is a fresh question: nothing was stored that would let
        // the second refund skip it.
        let again = ask(&source, id);
        assert!(again.is_ok(), "the simulator answers every question");
    }

    #[test]
    fn the_development_stand_in_marks_what_it_produced() {
        let id = TransactionId::from_uuid(Uuid::from_u128(2));
        let context = ask(&RefundAuthoritySource::DevAcceptLocal, id)
            .expect("the development source authorizes");
        assert_eq!(context.authority, RefundAuthority::CentralAccepted);
        assert_eq!(context.original_transaction_id, id);
        assert!(context
            .authorization_ref
            .as_deref()
            .is_some_and(|reference| reference.starts_with("DEV-LOCAL:")));
    }
}
