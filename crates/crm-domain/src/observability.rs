// SPDX-License-Identifier: GPL-3.0-only
//! Domain-level observability hooks: tracing, health and anomaly signals.
//!
//! These are ports (traits) and value types only — no I/O, no HTTP, no
//! SQL. Implementations live outside `crm-domain` (04-AUD-02 and the
//! service layer); the domain defines the vocabulary so every layer
//! speaks about the same occurrences.
//!
//! ## Privacy rule
//!
//! Event and signal names are fixed `&'static str` so the vocabulary
//! stays enumerable and grep-able. Names must never contain PII;
//! identifiers travel as opaque references in typed fields, and secrets
//! (tokens, passwords, hashes) must never appear in any field.

use std::future::Future;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::ids::{ActorId, StaffId};

// ---------------------------------------------------------------------------
// Structured tracing
// ---------------------------------------------------------------------------

/// A structured tracing event: what happened, when, and in which
/// correlation context.
///
/// `name` is a fixed vocabulary word (e.g. `"auth.login_attempt"`); it
/// must never contain PII. `fields` carries opaque, non-secret detail as
/// key/value pairs — e.g. `("result", "deny")` — never tokens,
/// passwords or raw emails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEvent {
    /// Vocabulary word for the occurrence, e.g. `"auth.login_attempt"`.
    pub name: &'static str,
    /// When the occurrence happened.
    pub occurred_at: DateTime<Utc>,
    /// Correlates this event with the originating request/workflow run.
    pub correlation_id: Uuid,
    /// Who acted, when there is an actor.
    pub actor_id: Option<ActorId>,
    /// Opaque, non-secret detail as key/value pairs.
    pub fields: Vec<(String, String)>,
}

impl TraceEvent {
    /// Builds an event stamped with the current time.
    #[must_use]
    pub fn now(
        name: &'static str,
        correlation_id: Uuid,
        actor_id: Option<ActorId>,
        fields: Vec<(String, String)>,
    ) -> Self {
        Self {
            name,
            occurred_at: Utc::now(),
            correlation_id,
            actor_id,
            fields,
        }
    }
}

/// Emission port for [`TraceEvent`]s.
///
/// Fire-and-forget by contract: emission must never fail the business
/// operation it describes, so the output is `()`. Implementations batch,
/// sample or drop under load — but they must never block the caller
/// longer than a bounded flush.
pub trait Tracer {
    /// Emits one tracing event. Never fails the caller.
    fn emit<'a>(&'a self, event: TraceEvent) -> impl Future<Output = ()> + Send + 'a;
}

// ---------------------------------------------------------------------------
// Health signals
// ---------------------------------------------------------------------------

/// Health state of one component, from the domain's point of view.
///
/// This is the domain vocabulary behind the HTTP `/healthz` (liveness)
/// and `/readyz` (readiness) probes: reporters further down the stack
/// aggregate these into probe responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentHealth {
    /// The component is operating normally.
    Healthy,
    /// The component works but with reduced capacity; `reason` says how.
    Degraded {
        /// Human-readable reason, free of secrets.
        reason: String,
    },
    /// The component cannot serve; `reason` says why.
    Unhealthy {
        /// Human-readable reason, free of secrets.
        reason: String,
    },
}

/// A point-in-time health statement about one component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// Which component reported, e.g. `"database"` or `"auth"`.
    pub component: &'static str,
    /// Its health state.
    pub health: ComponentHealth,
    /// When the check ran.
    pub checked_at: DateTime<Utc>,
}

impl HealthReport {
    /// Whether the component reported [`ComponentHealth::Healthy`].
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self.health, ComponentHealth::Healthy)
    }

    /// Whether the component reported [`ComponentHealth::Unhealthy`].
    #[must_use]
    pub fn is_unhealthy(&self) -> bool {
        matches!(self.health, ComponentHealth::Unhealthy { .. })
    }
}

/// Port for components that can report their own health.
///
/// Implementations run one bounded check (a query, a handshake, a
/// budget comparison) and return the verdict; aggregation into overall
/// service health is the caller's job.
pub trait HealthReporter {
    /// Runs the component's health check.
    fn check_health(&self) -> impl Future<Output = HealthReport> + Send;
}

// ---------------------------------------------------------------------------
// Anomaly signals
// ---------------------------------------------------------------------------

/// A domain-level anomaly signal: something the service layer noticed
/// that a human should look at.
///
/// Detection is rule-based — counts, thresholds, state transitions —
/// never ML at this layer. Signals carry fingerprints and IDs, never
/// raw PII or secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnomalySignal {
    /// Many failed logins against one account in a short window.
    LoginVelocity {
        /// SHA-256 fingerprint of the normalized email (see
        /// [`crate::audit::fingerprint`]).
        email_fingerprint: String,
        /// Failed attempts observed in the window.
        attempts: u32,
        /// Window length in seconds.
        window_secs: u64,
    },
    /// A staff member's grants grew unusually fast.
    GrantEscalation {
        /// The staff account whose grants grew.
        staff_id: StaffId,
        /// Grants added in the window.
        grants_added: u32,
        /// Window length in seconds.
        window_secs: u64,
    },
    /// Many authorization denials for one actor in a short window —
    /// often probing.
    RepeatedAuthorizationDenials {
        /// The denied actor.
        actor_id: ActorId,
        /// Denials observed in the window.
        denials: u32,
        /// Window length in seconds.
        window_secs: u64,
    },
}

impl AnomalySignal {
    /// Stable signal name, e.g. `"anomaly.login_velocity"`.
    ///
    /// Part of the observability contract — never rename without a
    /// migration note.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            AnomalySignal::LoginVelocity { .. } => "anomaly.login_velocity",
            AnomalySignal::GrantEscalation { .. } => "anomaly.grant_escalation",
            AnomalySignal::RepeatedAuthorizationDenials { .. } => {
                "anomaly.repeated_authorization_denials"
            }
        }
    }
}

/// Sink port for [`AnomalySignal`]s.
///
/// Fire-and-forget by contract, like [`Tracer::emit`]: emitting a
/// signal must never fail the operation that triggered it.
pub trait AnomalySink {
    /// Emits one anomaly signal. Never fails the caller.
    fn emit<'a>(&'a self, signal: AnomalySignal) -> impl Future<Output = ()> + Send + 'a;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_event_carries_correlation_and_actor() {
        let correlation_id = Uuid::from_u128(7);
        let actor_id = ActorId::from(Uuid::from_u128(8));
        let event = TraceEvent::now(
            "auth.login_attempt",
            correlation_id,
            Some(actor_id),
            vec![("result".to_string(), "deny".to_string())],
        );
        assert_eq!(event.name, "auth.login_attempt");
        assert_eq!(event.correlation_id, correlation_id);
        assert_eq!(event.actor_id, Some(actor_id));
        assert_eq!(
            event.fields,
            vec![("result".to_string(), "deny".to_string())]
        );
    }

    #[test]
    fn health_report_predicates_match_state() {
        let at = Utc::now();
        let healthy = HealthReport {
            component: "database",
            health: ComponentHealth::Healthy,
            checked_at: at,
        };
        assert!(healthy.is_healthy());
        assert!(!healthy.is_unhealthy());

        let degraded = HealthReport {
            component: "queue",
            health: ComponentHealth::Degraded {
                reason: "depth 900/1000".to_string(),
            },
            checked_at: at,
        };
        assert!(!degraded.is_healthy());
        assert!(!degraded.is_unhealthy());

        let unhealthy = HealthReport {
            component: "database",
            health: ComponentHealth::Unhealthy {
                reason: "connection refused".to_string(),
            },
            checked_at: at,
        };
        assert!(!unhealthy.is_healthy());
        assert!(unhealthy.is_unhealthy());
    }

    #[test]
    fn anomaly_signal_names_are_stable() {
        // Pinned like the audit action names: renaming a signal breaks
        // downstream alerting, so the names are asserted here.
        let signals = [
            AnomalySignal::LoginVelocity {
                email_fingerprint: "fp".to_string(),
                attempts: 12,
                window_secs: 300,
            },
            AnomalySignal::GrantEscalation {
                staff_id: StaffId::new(),
                grants_added: 5,
                window_secs: 600,
            },
            AnomalySignal::RepeatedAuthorizationDenials {
                actor_id: ActorId::from(Uuid::from_u128(9)),
                denials: 20,
                window_secs: 300,
            },
        ];
        let names: Vec<&str> = signals.iter().map(AnomalySignal::name).collect();
        assert_eq!(
            names,
            vec![
                "anomaly.login_velocity",
                "anomaly.grant_escalation",
                "anomaly.repeated_authorization_denials",
            ]
        );
    }
}
