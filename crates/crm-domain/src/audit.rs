// SPDX-License-Identifier: GPL-3.0-only
//! Tamper-evident audit events and the hash-chain helpers.
//!
//! ## Tamper-evidence invariant
//!
//! Every event carries a `chain_hash` computed as
//!
//! ```text
//! chain_hash = SHA-256(prev_chain_hash || canonical_bytes(event))
//! ```
//!
//! where `prev_chain_hash` is the chain hash of the previously appended
//! event ([`GENESIS_CHAIN_HASH`] for the first event) and
//! `canonical_bytes` is the event's canonical encoding **excluding**
//! `chain_hash` itself. Rewriting any historical event changes its
//! canonical bytes, which changes its chain hash and breaks every
//! subsequent link — detectable by [`verify_chain`], which recomputes the
//! chain from genesis. The log is append-only: there is no update or
//! delete path, and denied actions are recorded like any other outcome.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ids::{ActorId, AuditEventId, ClientProfileId};

/// Chain hash used as the predecessor of the very first audit event.
pub const GENESIS_CHAIN_HASH: [u8; 32] = [0u8; 32];

/// Whether the actor was a human operator, an AI agent, or the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActorKind {
    /// An agency staff member operating the CRM.
    Human,
    /// An AI agent acting under a delegated grant.
    Agent,
    /// The system itself (schedulers, reconcilers, migrations).
    System,
}

impl ActorKind {
    /// Stable string used in the canonical byte encoding.
    fn as_str(self) -> &'static str {
        match self {
            ActorKind::Human => "human",
            ActorKind::Agent => "agent",
            ActorKind::System => "system",
        }
    }
}

/// The authorization decision recorded for the action.
///
/// Deny by default: an action is allowed only when an explicit grant
/// covered it. Denied actions are still appended to the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// An explicit grant authorized the action.
    Allow,
    /// No grant authorized the action; it was not performed.
    Deny,
}

impl PermissionDecision {
    /// Stable string used in the canonical byte encoding.
    fn as_str(self) -> &'static str {
        match self {
            PermissionDecision::Allow => "allow",
            PermissionDecision::Deny => "deny",
        }
    }
}

/// The outcome of the recorded action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActionResult {
    /// The action completed successfully.
    Success,
    /// The action was attempted and failed.
    Failure,
    /// The action is durably paused (e.g. awaiting human approval).
    Pending,
}

impl ActionResult {
    /// Stable string used in the canonical byte encoding.
    fn as_str(self) -> &'static str {
        match self {
            ActionResult::Success => "success",
            ActionResult::Failure => "failure",
            ActionResult::Pending => "pending",
        }
    }
}

/// A single event in the tamper-evident audit log.
///
/// Every human and agent mutation produces one of these, carrying actor,
/// client scope, correlation, the authorization decision, and
/// before/after references. `before_ref`/`after_ref` are opaque
/// references (e.g. content hashes or version ids), never raw payloads,
/// so the log stays free of secrets and bulk PII.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Stable identifier of this event.
    pub event_id: AuditEventId,
    /// When the event occurred.
    pub occurred_at: DateTime<Utc>,
    /// Whether a human, agent or the system acted.
    pub actor_kind: ActorKind,
    /// Who acted (staff, agent principal, or well-known system id).
    pub actor_id: ActorId,
    /// Client-profile scope of the action, if any.
    pub client_profile_id: Option<ClientProfileId>,
    /// What was done, e.g. `"contact.transition"`.
    pub action: String,
    /// Kind of the target entity, e.g. `"contact"`.
    pub target_kind: String,
    /// Opaque identifier of the target entity.
    pub target_id: String,
    /// Correlates this event with the originating request/workflow run.
    pub correlation_id: Uuid,
    /// Whether the action was authorized. Denied actions are logged too.
    pub permission_decision: PermissionDecision,
    /// The outcome of the action.
    pub result: ActionResult,
    /// Opaque reference to the state before the action, if any.
    pub before_ref: Option<String>,
    /// Opaque reference to the state after the action, if any.
    pub after_ref: Option<String>,
    /// `SHA-256(prev_chain_hash || canonical_bytes)`; see module docs.
    pub chain_hash: [u8; 32],
}

impl AuditEvent {
    /// Canonical byte encoding of this event, **excluding** `chain_hash`.
    ///
    /// This is the sketch that [`compute_chain_hash`] signs: fields are
    /// concatenated in a fixed order with a one-byte tag and an explicit
    /// 8-byte big-endian length prefix per field, so the encoding is
    /// deterministic regardless of struct layout or serializer choices.
    /// The production canonicalization keeps this exact byte layout and
    /// bumps a format version if it ever changes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut push = |tag: u8, bytes: &[u8]| {
            out.push(tag);
            out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            out.extend_from_slice(bytes);
        };
        push(0x01, self.event_id.as_uuid().as_bytes());
        push(
            0x02,
            self.occurred_at
                .to_rfc3339_opts(SecondsFormat::Secs, true)
                .as_bytes(),
        );
        push(0x03, self.actor_kind.as_str().as_bytes());
        push(0x04, self.actor_id.as_uuid().as_bytes());
        match self.client_profile_id {
            Some(id) => push(0x05, id.as_uuid().as_bytes()),
            None => push(0x05, &[]),
        }
        push(0x06, self.action.as_bytes());
        push(0x07, self.target_kind.as_bytes());
        push(0x08, self.target_id.as_bytes());
        push(0x09, self.correlation_id.as_bytes());
        push(0x0a, self.permission_decision.as_str().as_bytes());
        push(0x0b, self.result.as_str().as_bytes());
        match &self.before_ref {
            Some(reference) => push(0x0c, reference.as_bytes()),
            None => push(0x0c, &[]),
        }
        match &self.after_ref {
            Some(reference) => push(0x0d, reference.as_bytes()),
            None => push(0x0d, &[]),
        }
        out
    }
}

/// Computes the chain hash linking an event to its predecessor.
///
/// `SHA-256(prev || canonical_bytes)`, where `canonical_bytes` is the
/// output of [`AuditEvent::canonical_bytes`].
///
/// ```
/// use crm_domain::audit::compute_chain_hash;
///
/// let prev = [0u8; 32];
/// let bytes = b"canonical event bytes";
/// let first = compute_chain_hash(&prev, bytes);
/// // Deterministic: the same inputs always yield the same hash.
/// assert_eq!(first, compute_chain_hash(&prev, bytes));
/// // Avalanche: a one-byte change in the payload changes the hash.
/// let mut tampered = bytes.to_vec();
/// tampered[0] ^= 0x01;
/// assert_ne!(first, compute_chain_hash(&prev, &tampered));
/// // Linkage: a different predecessor changes the hash.
/// assert_ne!(first, compute_chain_hash(&[1u8; 32], bytes));
/// ```
#[must_use]
pub fn compute_chain_hash(prev: &[u8; 32], canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(prev);
    hasher.update(canonical_bytes);
    hasher.finalize().into()
}

/// Verifies a sequence of events forms an unbroken chain from genesis.
///
/// Returns `true` when every event's `chain_hash` equals
/// `SHA-256(prev || canonical_bytes)` starting from
/// [`GENESIS_CHAIN_HASH`]. Any tampering with a historical event (or a
/// missing/reordered event) makes verification fail.
#[must_use]
pub fn verify_chain(events: &[AuditEvent]) -> bool {
    let mut prev = GENESIS_CHAIN_HASH;
    for event in events {
        if event.chain_hash != compute_chain_hash(&prev, &event.canonical_bytes()) {
            return false;
        }
        prev = event.chain_hash;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_event(action: &str) -> AuditEvent {
        AuditEvent {
            event_id: AuditEventId::from(Uuid::from_u128(1)),
            occurred_at: DateTime::from_timestamp(1_700_000_000, 0).expect("fixed timestamp"),
            actor_kind: ActorKind::Human,
            actor_id: ActorId::from(Uuid::from_u128(2)),
            client_profile_id: Some(ClientProfileId::from(Uuid::from_u128(3))),
            action: action.to_string(),
            target_kind: "contact".to_string(),
            target_id: "contact-1".to_string(),
            correlation_id: Uuid::from_u128(4),
            permission_decision: PermissionDecision::Allow,
            result: ActionResult::Success,
            before_ref: None,
            after_ref: Some("v2".to_string()),
            chain_hash: [0u8; 32],
        }
    }

    fn chained(mut event: AuditEvent, prev: &[u8; 32]) -> AuditEvent {
        event.chain_hash = compute_chain_hash(prev, &event.canonical_bytes());
        event
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        let event = test_event("contact.transition");
        assert_eq!(event.canonical_bytes(), event.canonical_bytes());
        assert!(!event.canonical_bytes().is_empty());
    }

    #[test]
    fn canonical_bytes_change_when_any_field_changes() {
        let base = test_event("contact.transition").canonical_bytes();
        let mut tampered = test_event("contact.transition");
        tampered.action = "contact.delete".to_string();
        assert_ne!(base, tampered.canonical_bytes());

        let mut tampered = test_event("contact.transition");
        tampered.permission_decision = PermissionDecision::Deny;
        assert_ne!(base, tampered.canonical_bytes());

        let mut tampered = test_event("contact.transition");
        tampered.result = ActionResult::Failure;
        assert_ne!(base, tampered.canonical_bytes());
    }

    #[test]
    fn chain_hash_avalanche_on_single_bit_flip() {
        let prev = [7u8; 32];
        let base = compute_chain_hash(&prev, b"payload");
        let mut tampered = b"payload".to_vec();
        tampered[0] ^= 0x01;
        let flipped = compute_chain_hash(&prev, &tampered);
        assert_ne!(base, flipped);
        // Different predecessor => different hash even with identical bytes.
        assert_ne!(base, compute_chain_hash(&[8u8; 32], b"payload"));
    }

    #[test]
    fn verify_chain_accepts_a_well_formed_chain() {
        let first = chained(test_event("contact.create"), &GENESIS_CHAIN_HASH);
        let mut second = test_event("contact.transition");
        second.event_id = AuditEventId::from(Uuid::from_u128(10));
        let second = chained(second, &first.chain_hash);
        assert!(verify_chain(&[first, second]));
    }

    #[test]
    fn verify_chain_rejects_tampered_history() {
        let first = chained(test_event("contact.create"), &GENESIS_CHAIN_HASH);
        let mut second = test_event("contact.transition");
        second.event_id = AuditEventId::from(Uuid::from_u128(10));
        let second = chained(second, &first.chain_hash);

        // Tamper with the first event after the chain was sealed.
        let mut tampered_first = first.clone();
        tampered_first.action = "contact.delete".to_string();
        assert!(!verify_chain(&[tampered_first, second.clone()]));

        // Tamper with a link without recomputing the hash.
        let mut broken_link = second;
        broken_link.chain_hash = [9u8; 32];
        assert!(!verify_chain(&[first, broken_link]));
    }

    #[test]
    fn verify_chain_rejects_reordered_events() {
        let first = chained(test_event("contact.create"), &GENESIS_CHAIN_HASH);
        let mut second = test_event("contact.transition");
        second.event_id = AuditEventId::from(Uuid::from_u128(10));
        let second = chained(second, &first.chain_hash);
        assert!(!verify_chain(&[second, first]));
    }
}
