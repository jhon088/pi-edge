//! A deterministic payment provider for tests, development and the demo.
//!
//! No sleeps, no randomness, no wall clock: the outcome of a call is a pure
//! function of the request amount and a script decided up front. That is what
//! makes a crash-recovery test reproducible.
//!
//! The script is keyed by the requested minor amount rather than by a counter
//! so that the same behaviour can be driven from outside the process — the CLI
//! demo configures the daemon's simulator and then simply tenders the amount
//! that triggers the scenario it wants to show.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use edge_domain::prelude::Money;
use uuid::Uuid;

use crate::error::PaymentError;
use crate::provider::{
    PaymentProviderPort, ProviderOutcome, ProviderQuery, ProviderRequest, ProviderReversal,
    ReversalOutcome,
};
use crate::tender::ProviderKind;

/// What the simulator should answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulatedOutcome {
    Approve,
    Decline,
    /// The call is lost: the charge may or may not exist.
    Unknown,
}

impl SimulatedOutcome {
    pub fn parse(value: &str) -> Result<Self, PaymentError> {
        match value {
            "approve" | "APPROVE" | "approved" | "AUTHORIZED" => Ok(SimulatedOutcome::Approve),
            "decline" | "DECLINE" | "declined" | "DECLINED" => Ok(SimulatedOutcome::Decline),
            "unknown" | "UNKNOWN" | "timeout" | "TIMEOUT" => Ok(SimulatedOutcome::Unknown),
            other => Err(PaymentError::UnknownEnumValue {
                kind: "SimulatedOutcome",
                value: other.to_owned(),
            }),
        }
    }
}

/// The simulator's behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatorScript {
    /// What `start_payment` answers, by requested minor amount.
    by_amount: BTreeMap<i64, SimulatedOutcome>,
    /// What `start_payment` answers for any other amount.
    default_outcome: SimulatedOutcome,
    /// What `query_payment` answers — how an `UNKNOWN` resolves.
    query_outcome: SimulatedOutcome,
    /// Whether `reverse_payment` confirms.
    reversal_confirms: bool,
    /// Fault injection: die after answering, before anything is persisted.
    #[cfg(feature = "fault-injection")]
    crash_after_answer: bool,
}

impl Default for SimulatorScript {
    fn default() -> Self {
        Self {
            by_amount: BTreeMap::new(),
            default_outcome: SimulatedOutcome::Approve,
            query_outcome: SimulatedOutcome::Approve,
            reversal_confirms: true,
            #[cfg(feature = "fault-injection")]
            crash_after_answer: false,
        }
    }
}

impl SimulatorScript {
    pub fn approving() -> Self {
        Self::default()
    }

    /// Answer `outcome` when exactly `minor` is requested.
    pub fn on_amount(mut self, minor: i64, outcome: SimulatedOutcome) -> Self {
        self.by_amount.insert(minor, outcome);
        self
    }

    pub fn defaulting_to(mut self, outcome: SimulatedOutcome) -> Self {
        self.default_outcome = outcome;
        self
    }

    /// How a reconciliation query resolves an unknown attempt.
    pub fn resolving_queries_to(mut self, outcome: SimulatedOutcome) -> Self {
        self.query_outcome = outcome;
        self
    }

    pub fn with_reversal_confirmed(mut self, confirms: bool) -> Self {
        self.reversal_confirms = confirms;
        self
    }

    /// Parse a development configuration string such as
    /// `"default:approve,amount:70000=unknown,query:approve,reversal:confirm"`.
    ///
    /// Only the composition root ever calls this; the domain never reads an
    /// environment variable.
    pub fn parse(spec: &str) -> Result<Self, PaymentError> {
        let mut script = Self::default();
        for clause in spec.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            let (key, value) = clause.split_once(':').ok_or(PaymentError::InvalidRecord {
                field: "simulator_script",
                reason: format!("`{clause}` is not `key:value`"),
            })?;
            match key.trim() {
                "default" => script.default_outcome = SimulatedOutcome::parse(value.trim())?,
                "query" => script.query_outcome = SimulatedOutcome::parse(value.trim())?,
                "reversal" => script.reversal_confirms = value.trim() != "unknown",
                // Fault injection, test builds only. A production binary does
                // not know this key and refuses to start with it.
                #[cfg(feature = "fault-injection")]
                "crash" => script.crash_after_answer = value.trim() == "after-answer",
                "amount" => {
                    let (minor, outcome) =
                        value.split_once('=').ok_or(PaymentError::InvalidRecord {
                            field: "simulator_script",
                            reason: format!("`{value}` is not `<minor>=<outcome>`"),
                        })?;
                    let minor: i64 =
                        minor
                            .trim()
                            .parse()
                            .map_err(|_| PaymentError::InvalidRecord {
                                field: "simulator_script",
                                reason: format!("`{minor}` is not an amount in minor units"),
                            })?;
                    script
                        .by_amount
                        .insert(minor, SimulatedOutcome::parse(outcome.trim())?);
                }
                other => {
                    return Err(PaymentError::InvalidRecord {
                        field: "simulator_script",
                        reason: format!("unknown key `{other}`"),
                    })
                }
            }
        }
        Ok(script)
    }

    fn outcome_for(&self, amount: Money) -> SimulatedOutcome {
        self.by_amount
            .get(&amount.minor())
            .copied()
            .unwrap_or(self.default_outcome)
    }
}

/// Deterministic in-process provider.
#[derive(Debug, Default)]
pub struct SimulatorPaymentProvider {
    script: SimulatorScript,
    /// Only used to mint distinct provider references. Not a clock.
    sequence: AtomicU64,
}

impl SimulatorPaymentProvider {
    pub fn new(script: SimulatorScript) -> Self {
        Self {
            script,
            sequence: AtomicU64::new(0),
        }
    }

    pub fn approving() -> Self {
        Self::new(SimulatorScript::approving())
    }

    fn reference(&self, correlation_id: Uuid) -> String {
        // Derived from the correlation id, so the same interaction always
        // yields the same reference even across a restart.
        format!("SIM-{}", &correlation_id.simple().to_string()[..12])
    }

    fn render(
        &self,
        outcome: SimulatedOutcome,
        correlation_id: Uuid,
        amount: Money,
    ) -> ProviderOutcome {
        match outcome {
            SimulatedOutcome::Approve => {
                self.sequence.fetch_add(1, Ordering::Relaxed);
                ProviderOutcome::Authorized {
                    authorized: amount,
                    provider_reference: self.reference(correlation_id),
                }
            }
            SimulatedOutcome::Decline => ProviderOutcome::Declined {
                failure_code: "SIMULATED_DECLINED".to_owned(),
            },
            SimulatedOutcome::Unknown => ProviderOutcome::Unknown,
        }
    }
}

impl PaymentProviderPort for SimulatorPaymentProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Simulator
    }

    fn start_payment(&self, request: &ProviderRequest) -> Result<ProviderOutcome, PaymentError> {
        let outcome = self.render(
            self.script.outcome_for(request.amount),
            request.correlation_id,
            request.amount,
        );
        // The provider has decided — in the real world, the customer's card is
        // charged by now. Dying here is the window ADR 020 is designed around:
        // phase 1 is already durable, phase 2 never runs, and the stored
        // attempt must come back as unresolved rather than failed.
        #[cfg(feature = "fault-injection")]
        if self.script.crash_after_answer {
            std::process::abort();
        }
        Ok(outcome)
    }

    fn query_payment(&self, query: &ProviderQuery) -> Result<ProviderOutcome, PaymentError> {
        // A query reports; it never charges. It echoes the amount of the
        // interaction being asked about.
        Ok(match self.script.query_outcome {
            SimulatedOutcome::Approve => ProviderOutcome::Authorized {
                authorized: query.amount,
                provider_reference: query
                    .provider_reference
                    .clone()
                    .unwrap_or_else(|| self.reference(query.correlation_id)),
            },
            SimulatedOutcome::Decline => ProviderOutcome::Declined {
                failure_code: "SIMULATED_NOT_CHARGED".to_owned(),
            },
            SimulatedOutcome::Unknown => ProviderOutcome::Unknown,
        })
    }

    fn reverse_payment(
        &self,
        _reversal: &ProviderReversal,
    ) -> Result<ReversalOutcome, PaymentError> {
        Ok(if self.script.reversal_confirms {
            ReversalOutcome::Reversed
        } else {
            ReversalOutcome::Unknown
        })
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use edge_domain::prelude::{Currency, Timestamp};

    use crate::tender::TenderType;

    fn request(minor: i64) -> ProviderRequest {
        ProviderRequest {
            correlation_id: Uuid::from_u128(7),
            tender_type: TenderType::Card,
            amount: Money::from_minor(minor, Currency::Mxn),
            requested_at: Timestamp::parse_rfc3339("2026-10-08T15:00:00Z").unwrap(),
        }
    }

    #[test]
    fn the_same_request_always_produces_the_same_outcome() {
        let provider = SimulatorPaymentProvider::approving();
        let first = provider.start_payment(&request(70_000)).unwrap();
        let second = provider.start_payment(&request(70_000)).unwrap();
        assert_eq!(first, second, "no counters, no clock, no randomness");
        assert!(matches!(first, ProviderOutcome::Authorized { .. }));
    }

    #[test]
    fn a_scripted_amount_declines_or_vanishes() {
        let provider = SimulatorPaymentProvider::new(
            SimulatorScript::approving()
                .on_amount(11_300, SimulatedOutcome::Decline)
                .on_amount(70_000, SimulatedOutcome::Unknown),
        );
        assert!(matches!(
            provider.start_payment(&request(11_300)).unwrap(),
            ProviderOutcome::Declined { .. }
        ));
        assert_eq!(
            provider.start_payment(&request(70_000)).unwrap(),
            ProviderOutcome::Unknown
        );
        assert!(matches!(
            provider.start_payment(&request(50_000)).unwrap(),
            ProviderOutcome::Authorized { .. }
        ));
    }

    #[test]
    fn the_provider_reference_is_derived_from_the_correlation_id() {
        let provider = SimulatorPaymentProvider::approving();
        let ProviderOutcome::Authorized {
            provider_reference, ..
        } = provider.start_payment(&request(1_000)).unwrap()
        else {
            panic!("expected an authorization")
        };
        // Stable across calls and across restarts, so a reconciliation finds
        // the same interaction.
        assert_eq!(provider_reference, provider.reference(Uuid::from_u128(7)));
    }

    #[test]
    fn a_script_can_be_parsed_from_a_development_string() {
        let script =
            SimulatorScript::parse("default:approve, amount:70000=unknown, query:decline").unwrap();
        let provider = SimulatorPaymentProvider::new(script);
        assert_eq!(
            provider.start_payment(&request(70_000)).unwrap(),
            ProviderOutcome::Unknown
        );
        assert!(matches!(
            provider.start_payment(&request(1_000)).unwrap(),
            ProviderOutcome::Authorized { .. }
        ));
        assert!(matches!(
            provider
                .query_payment(&ProviderQuery {
                    correlation_id: Uuid::from_u128(7),
                    provider_reference: None,
                    amount: Money::from_minor(70_000, Currency::Mxn),
                })
                .unwrap(),
            ProviderOutcome::Declined { .. }
        ));
        assert!(SimulatorScript::parse("nonsense").is_err());
        assert!(SimulatorScript::parse("default:maybe").is_err());
    }

    #[test]
    fn a_reversal_can_be_scripted_to_stay_unconfirmed() {
        let provider = SimulatorPaymentProvider::new(
            SimulatorScript::approving().with_reversal_confirmed(false),
        );
        let reversal = ProviderReversal {
            correlation_id: Uuid::from_u128(7),
            provider_reference: None,
            amount: Money::from_minor(1_000, Currency::Mxn),
        };
        assert_eq!(
            provider.reverse_payment(&reversal).unwrap(),
            ReversalOutcome::Unknown
        );
    }
}
