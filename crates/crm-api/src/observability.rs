// SPDX-License-Identifier: GPL-3.0-only
//! Concrete observability implementations for `crm-api` (task 04-AUD-02).
//!
//! The domain defines the ports ([`Tracer`], [`AnomalySink`],
//! [`HealthReporter`]); this module provides the honest, minimal
//! implementations: structured `tracing` events for traces and anomaly
//! signals. These are hook points, not a monitoring system —
//! aggregation, alerting and dashboards live outside the process.
//!
//! Privacy: trace names are fixed vocabulary words and fields never
//! carry PII or secrets (see the domain privacy rule). Anomaly signals
//! carry fingerprints and IDs only.
//!
//! [`Tracer`]: crm_domain::observability::Tracer
//! [`AnomalySink`]: crm_domain::observability::AnomalySink
//! [`HealthReporter`]: crm_domain::observability::HealthReporter

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crm_domain::ids::ActorId;
use crm_domain::observability::{AnomalySignal, AnomalySink, TraceEvent, Tracer};

/// [`Tracer`] that emits structured `tracing` events.
///
/// Fire-and-forget: `tracing` never fails the caller, and the subscriber
/// is responsible for batching/dropping under load.
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingTracer;

impl Tracer for TracingTracer {
    async fn emit(&self, event: TraceEvent) {
        tracing::info!(
            trace.name = event.name,
            trace.correlation_id = %event.correlation_id,
            trace.actor_id = ?event.actor_id.map(|id| id.to_string()),
            trace.fields = ?event.fields,
            trace.occurred_at = %event.occurred_at,
            "trace event"
        );
    }
}

/// [`AnomalySink`] that logs signals as `tracing` warnings.
///
/// A human (or a log-based alerter) picks these up; the process itself
/// takes no automated action on signals.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoggingAnomalySink;

impl AnomalySink for LoggingAnomalySink {
    async fn emit(&self, signal: AnomalySignal) {
        tracing::warn!(
            anomaly.name = signal.name(),
            anomaly.signal = ?signal,
            "anomaly signal"
        );
    }
}

/// Denials observed before an [`AnomalySignal::RepeatedAuthorizationDenials`]
/// is emitted.
const DENIAL_THRESHOLD: u32 = 10;

/// Window in which denials count toward the threshold.
const DENIAL_WINDOW: Duration = Duration::from_secs(300);

/// Tracks per-actor authorization denials and reports when the pattern
/// looks like probing.
///
/// In-memory and per-process, like the login rate limiter: exact for the
/// single-instance deployment, approximate under multi-instance. When an
/// actor's denials reach [`DENIAL_THRESHOLD`] within [`DENIAL_WINDOW`],
/// [`DenialTracker::note_denial`] returns the signal to emit; the
/// counter keeps counting (no reset on emission) so a sustained probe
/// re-signals at most once per window — callers decide whether to
/// re-emit.
#[derive(Debug, Default)]
pub struct DenialTracker {
    state: Mutex<HashMap<ActorId, VecDeque<Instant>>>,
}

impl DenialTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one denial for `actor`, returning the anomaly signal when
    /// the threshold is reached.
    pub fn note_denial(&self, actor: ActorId) -> Option<AnomalySignal> {
        let now = Instant::now();
        let mut state = self.state.lock().ok()?;
        let denials = state.entry(actor).or_default();
        while denials
            .front()
            .is_some_and(|seen| now.duration_since(*seen) > DENIAL_WINDOW)
        {
            denials.pop_front();
        }
        denials.push_back(now);
        let count = denials.len() as u32;
        // Signal once per window: only the exact threshold crossing
        // emits, so a sustained probe does not spam the sink.
        if count == DENIAL_THRESHOLD {
            Some(AnomalySignal::RepeatedAuthorizationDenials {
                actor_id: actor,
                denials: count,
                window_secs: DENIAL_WINDOW.as_secs(),
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denial_tracker_signals_once_at_threshold() {
        let tracker = DenialTracker::new();
        let actor = ActorId::from(uuid::Uuid::from_u128(1));
        for _ in 0..DENIAL_THRESHOLD - 1 {
            assert!(tracker.note_denial(actor).is_none());
        }
        let signal = tracker
            .note_denial(actor)
            .expect("threshold crossing must signal");
        assert_eq!(signal.name(), "anomaly.repeated_authorization_denials");
        // Past the threshold: no re-signal within the same window.
        assert!(tracker.note_denial(actor).is_none());
    }

    #[test]
    fn denial_tracker_is_per_actor() {
        let tracker = DenialTracker::new();
        let first = ActorId::from(uuid::Uuid::from_u128(1));
        let second = ActorId::from(uuid::Uuid::from_u128(2));
        for _ in 0..DENIAL_THRESHOLD {
            tracker.note_denial(first);
        }
        // The second actor's denials are counted separately.
        assert!(tracker.note_denial(second).is_none());
    }
}
