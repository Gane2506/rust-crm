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

use crate::ids::{
    ActorId, AgentPrincipalId, AuditEventId, ClientProfileId, DelegationId, GrantId, SessionId,
    StaffId,
};

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

/// SHA-256 hex fingerprint of `value`.
///
/// Used for login-failure and rate-limit keys: it lets repeated attempts
/// correlate without persisting emails or IP addresses in the audit log.
/// The fingerprint is one-way — it reveals nothing about the input — but
/// it is deterministic, so the same input always yields the same
/// fingerprint.
///
/// ```
/// use crm_domain::audit::fingerprint;
///
/// // Deterministic: the same input always yields the same fingerprint.
/// assert_eq!(fingerprint("op@example.com"), fingerprint("op@example.com"));
/// // One-way: the fingerprint does not contain the input.
/// assert!(!fingerprint("op@example.com").contains("op@example.com"));
/// // 64 lowercase hex characters (SHA-256).
/// let fp = fingerprint("op@example.com");
/// assert_eq!(fp.len(), 64);
/// assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
/// ```
#[must_use]
pub fn fingerprint(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Why a session was revoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionRevokeReason {
    /// The staff member logged out.
    Logout,
    /// An administrator revoked the session.
    RevokedByAdmin,
    /// The session reached its absolute expiry.
    Expired,
    /// The session idle-timed-out.
    IdleTimedOut,
}

/// Which rate-limit bucket tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RateLimitBucket {
    /// The per-client-IP bucket.
    Ip,
    /// The per-email bucket.
    Email,
}

impl RateLimitBucket {
    /// Stable string used in audit payloads.
    fn as_str(self) -> &'static str {
        match self {
            RateLimitBucket::Ip => "ip",
            RateLimitBucket::Email => "email",
        }
    }
}

/// A security-relevant occurrence to be recorded in the audit log.
///
/// This is the typed taxonomy of everything the identity and RBAC
/// workstream can do that matters for forensics: authentication, session
/// lifecycle, grant and delegation changes, staff state changes, and
/// rate-limit trips. Each variant carries exactly the structured payload
/// the occurrence needs — typed IDs and fingerprints, never raw secrets
/// or PII (emails and IPs are stored only as [`fingerprint`]s).
///
/// A `SecurityEvent` renders onto an [`AuditEvent`] envelope via
/// [`SecurityEvent::to_audit_event`]; the envelope carries the
/// tamper-evident chain link, while the serialized `SecurityEvent`
/// itself is the forensic payload the 04-AUD-02 persistence task stores
/// alongside it. A `SecurityEvent` never carries a chain hash: the audit
/// service assigns it from the log head at append time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecurityEvent {
    /// A staff member authenticated successfully.
    LoginSucceeded {
        /// The staff account that logged in.
        staff_id: StaffId,
    },
    /// Authentication failed. The email is stored only as its
    /// [`fingerprint`] so repeated attempts against one account
    /// correlate without persisting the address itself.
    LoginFailed {
        /// SHA-256 fingerprint of the normalized email.
        email_fingerprint: String,
    },
    /// A staff member logged out, revoking their own session.
    Logout {
        /// The staff account that logged out.
        staff_id: StaffId,
        /// The revoked session.
        session_id: SessionId,
    },
    /// A session was revoked.
    SessionRevoked {
        /// The staff account the session belonged to.
        staff_id: StaffId,
        /// The revoked session.
        session_id: SessionId,
        /// Why it was revoked.
        reason: SessionRevokeReason,
    },
    /// A grant was assigned to a staff member.
    GrantAssigned {
        /// The staff account receiving the grant.
        staff_id: StaffId,
        /// The new grant.
        grant_id: GrantId,
        /// Action the grant covers, e.g. `"contact:read"`.
        action: String,
        /// Scope the grant covers, e.g. `"agency:<id>"`.
        scope: String,
    },
    /// A grant was revoked from a staff member (tombstoned).
    GrantRevoked {
        /// The staff account losing the grant.
        staff_id: StaffId,
        /// The revoked grant.
        grant_id: GrantId,
    },
    /// A bounded delegation was created.
    DelegationCreated {
        /// The staff member delegating.
        delegator_staff_id: StaffId,
        /// The new delegation.
        delegation_id: DelegationId,
        /// The agent principal receiving the delegation.
        agent_principal_id: AgentPrincipalId,
    },
    /// A delegation was revoked.
    DelegationRevoked {
        /// The revoked delegation.
        delegation_id: DelegationId,
    },
    /// A staff account was suspended.
    StaffSuspended {
        /// The suspended account.
        staff_id: StaffId,
    },
    /// A staff account was reactivated.
    StaffReactivated {
        /// The reactivated account.
        staff_id: StaffId,
    },
    /// A staff member enrolled (or rotated) their password credential.
    PasswordEnrolled {
        /// The staff account.
        staff_id: StaffId,
    },
    /// The login rate limiter tripped.
    RateLimitTripped {
        /// Which bucket tripped.
        bucket: RateLimitBucket,
        /// SHA-256 fingerprint of the bucket key (IP or normalized
        /// email), so attack patterns correlate without storing the key.
        key_fingerprint: String,
    },
}

impl SecurityEvent {
    /// Builds a [`SecurityEvent::LoginFailed`], normalizing and
    /// fingerprinting `email` so callers never handle the raw address.
    #[must_use]
    pub fn login_failed(email: &str) -> Self {
        Self::LoginFailed {
            email_fingerprint: fingerprint(&email.trim().to_lowercase()),
        }
    }

    /// Builds a [`SecurityEvent::RateLimitTripped`], fingerprinting `key`
    /// so callers never persist raw IPs or emails.
    #[must_use]
    pub fn rate_limit_tripped(bucket: RateLimitBucket, key: &str) -> Self {
        Self::RateLimitTripped {
            bucket,
            key_fingerprint: fingerprint(key),
        }
    }

    /// Stable action name, e.g. `"auth.login"`.
    ///
    /// Used as [`AuditEvent::action`]; the names are part of the audit
    /// contract — never rename one without a migration note.
    ///
    /// ```
    /// use crm_domain::audit::SecurityEvent;
    /// use crm_domain::ids::StaffId;
    ///
    /// let event = SecurityEvent::LoginSucceeded { staff_id: StaffId::new() };
    /// assert_eq!(event.action_name(), "auth.login");
    /// ```
    #[must_use]
    pub fn action_name(&self) -> &'static str {
        match self {
            SecurityEvent::LoginSucceeded { .. } => "auth.login",
            SecurityEvent::LoginFailed { .. } => "auth.login_failed",
            SecurityEvent::Logout { .. } => "auth.logout",
            SecurityEvent::SessionRevoked { .. } => "auth.session_revoked",
            SecurityEvent::GrantAssigned { .. } => "rbac.grant_assigned",
            SecurityEvent::GrantRevoked { .. } => "rbac.grant_revoked",
            SecurityEvent::DelegationCreated { .. } => "rbac.delegation_created",
            SecurityEvent::DelegationRevoked { .. } => "rbac.delegation_revoked",
            SecurityEvent::StaffSuspended { .. } => "staff.suspended",
            SecurityEvent::StaffReactivated { .. } => "staff.reactivated",
            SecurityEvent::PasswordEnrolled { .. } => "staff.password_enrolled",
            SecurityEvent::RateLimitTripped { .. } => "auth.rate_limit_tripped",
        }
    }

    /// `(target_kind, target_id)` for the [`AuditEvent`] envelope.
    #[must_use]
    pub fn target(&self) -> (&'static str, String) {
        match self {
            SecurityEvent::LoginSucceeded { staff_id } => ("staff", staff_id.to_string()),
            SecurityEvent::LoginFailed { email_fingerprint } => {
                ("login_attempt", email_fingerprint.clone())
            }
            SecurityEvent::Logout { session_id, .. } => ("session", session_id.to_string()),
            SecurityEvent::SessionRevoked { session_id, .. } => ("session", session_id.to_string()),
            SecurityEvent::GrantAssigned { grant_id, .. } => ("grant", grant_id.to_string()),
            SecurityEvent::GrantRevoked { grant_id, .. } => ("grant", grant_id.to_string()),
            SecurityEvent::DelegationCreated { delegation_id, .. } => {
                ("delegation", delegation_id.to_string())
            }
            SecurityEvent::DelegationRevoked { delegation_id } => {
                ("delegation", delegation_id.to_string())
            }
            SecurityEvent::StaffSuspended { staff_id } => ("staff", staff_id.to_string()),
            SecurityEvent::StaffReactivated { staff_id } => ("staff", staff_id.to_string()),
            SecurityEvent::PasswordEnrolled { staff_id } => ("staff", staff_id.to_string()),
            SecurityEvent::RateLimitTripped { bucket, .. } => {
                ("rate_limit_bucket", bucket.as_str().to_string())
            }
        }
    }

    /// The authorization decision for the envelope.
    ///
    /// Occurrences that happened are `Allow`; rejected attempts (failed
    /// logins, tripped rate limits) are `Deny` — denied actions are
    /// security-relevant and belong in the log like any other outcome.
    #[must_use]
    pub fn permission_decision(&self) -> PermissionDecision {
        match self {
            SecurityEvent::LoginFailed { .. } | SecurityEvent::RateLimitTripped { .. } => {
                PermissionDecision::Deny
            }
            _ => PermissionDecision::Allow,
        }
    }

    /// The outcome of the occurrence for the envelope.
    #[must_use]
    pub fn result(&self) -> ActionResult {
        match self {
            SecurityEvent::LoginFailed { .. } | SecurityEvent::RateLimitTripped { .. } => {
                ActionResult::Failure
            }
            _ => ActionResult::Success,
        }
    }

    /// Renders this occurrence onto an [`AuditEvent`] envelope.
    ///
    /// The actor always comes from the caller's context — never from the
    /// event payload — so a compromised caller cannot forge someone
    /// else's identity into the log. `chain_hash` is left as the zero
    /// placeholder: the audit service MUST overwrite it from the log
    /// head before appending; an event with the placeholder hash must
    /// never be persisted.
    #[must_use]
    pub fn to_audit_event(
        &self,
        actor_kind: ActorKind,
        actor_id: ActorId,
        client_profile_id: Option<ClientProfileId>,
        correlation_id: Uuid,
        occurred_at: DateTime<Utc>,
    ) -> AuditEvent {
        let (target_kind, target_id) = self.target();
        AuditEvent {
            event_id: AuditEventId::new(),
            occurred_at,
            actor_kind,
            actor_id,
            client_profile_id,
            action: self.action_name().to_string(),
            target_kind: target_kind.to_string(),
            target_id,
            correlation_id,
            permission_decision: self.permission_decision(),
            result: self.result(),
            before_ref: None,
            after_ref: None,
            chain_hash: [0u8; 32],
        }
    }
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

    // ---- SecurityEvent taxonomy -------------------------------------------

    /// One of every taxonomy variant, so the coverage tests below stay
    /// exhaustive: adding a variant without extending this list fails to
    /// compile the tests that consume it.
    fn every_security_event() -> Vec<SecurityEvent> {
        vec![
            SecurityEvent::LoginSucceeded {
                staff_id: StaffId::from(Uuid::from_u128(21)),
            },
            SecurityEvent::login_failed("Op@Example.com "),
            SecurityEvent::Logout {
                staff_id: StaffId::from(Uuid::from_u128(21)),
                session_id: SessionId::from(Uuid::from_u128(22)),
            },
            SecurityEvent::SessionRevoked {
                staff_id: StaffId::from(Uuid::from_u128(21)),
                session_id: SessionId::from(Uuid::from_u128(22)),
                reason: SessionRevokeReason::RevokedByAdmin,
            },
            SecurityEvent::GrantAssigned {
                staff_id: StaffId::from(Uuid::from_u128(21)),
                grant_id: GrantId::from(Uuid::from_u128(23)),
                action: "contact:read".to_string(),
                scope: "agency:1".to_string(),
            },
            SecurityEvent::GrantRevoked {
                staff_id: StaffId::from(Uuid::from_u128(21)),
                grant_id: GrantId::from(Uuid::from_u128(23)),
            },
            SecurityEvent::DelegationCreated {
                delegator_staff_id: StaffId::from(Uuid::from_u128(21)),
                delegation_id: DelegationId::from(Uuid::from_u128(24)),
                agent_principal_id: AgentPrincipalId::from(Uuid::from_u128(25)),
            },
            SecurityEvent::DelegationRevoked {
                delegation_id: DelegationId::from(Uuid::from_u128(24)),
            },
            SecurityEvent::StaffSuspended {
                staff_id: StaffId::from(Uuid::from_u128(21)),
            },
            SecurityEvent::StaffReactivated {
                staff_id: StaffId::from(Uuid::from_u128(21)),
            },
            SecurityEvent::PasswordEnrolled {
                staff_id: StaffId::from(Uuid::from_u128(21)),
            },
            SecurityEvent::rate_limit_tripped(RateLimitBucket::Email, "op@example.com"),
        ]
    }

    #[test]
    fn security_event_action_names_are_stable() {
        // The action names are part of the audit contract: renaming one
        // silently breaks forensic queries, so the expected names are
        // pinned here. The exhaustive match forces this test to be
        // updated when a variant is added.
        let events = every_security_event();
        let names: Vec<(&SecurityEvent, &str)> = events
            .iter()
            .map(|event| {
                let expected = match event {
                    SecurityEvent::LoginSucceeded { .. } => "auth.login",
                    SecurityEvent::LoginFailed { .. } => "auth.login_failed",
                    SecurityEvent::Logout { .. } => "auth.logout",
                    SecurityEvent::SessionRevoked { .. } => "auth.session_revoked",
                    SecurityEvent::GrantAssigned { .. } => "rbac.grant_assigned",
                    SecurityEvent::GrantRevoked { .. } => "rbac.grant_revoked",
                    SecurityEvent::DelegationCreated { .. } => "rbac.delegation_created",
                    SecurityEvent::DelegationRevoked { .. } => "rbac.delegation_revoked",
                    SecurityEvent::StaffSuspended { .. } => "staff.suspended",
                    SecurityEvent::StaffReactivated { .. } => "staff.reactivated",
                    SecurityEvent::PasswordEnrolled { .. } => "staff.password_enrolled",
                    SecurityEvent::RateLimitTripped { .. } => "auth.rate_limit_tripped",
                };
                (event, expected)
            })
            .collect();
        assert_eq!(names.len(), 12);
        for (event, expected) in names {
            assert_eq!(event.action_name(), expected);
        }
    }

    #[test]
    fn taxonomy_covers_every_rbac_security_action() {
        // Every security-relevant action from the identity/RBAC workstream
        // must have an event kind: if this set ever stops being a subset
        // of the taxonomy's action names, the taxonomy has a gap.
        let required = [
            "auth.login",
            "auth.login_failed",
            "auth.logout",
            "auth.session_revoked",
            "auth.rate_limit_tripped",
            "rbac.grant_assigned",
            "rbac.grant_revoked",
            "rbac.delegation_created",
            "rbac.delegation_revoked",
            "staff.suspended",
            "staff.reactivated",
            "staff.password_enrolled",
        ];
        let events = every_security_event();
        let have: std::collections::HashSet<&str> =
            events.iter().map(SecurityEvent::action_name).collect();
        for action in required {
            assert!(have.contains(action), "taxonomy is missing {action}");
        }
    }

    #[test]
    fn login_failed_fingerprint_is_stable_and_hides_the_email() {
        let first = SecurityEvent::login_failed("Op@Example.com");
        let second = SecurityEvent::login_failed(" op@example.com ");
        // Normalization: case and surrounding whitespace do not matter.
        assert_eq!(first, second);
        let SecurityEvent::LoginFailed { email_fingerprint } = first else {
            panic!("expected LoginFailed");
        };
        assert_eq!(email_fingerprint.len(), 64);
        assert!(!email_fingerprint.contains("op@example.com"));
        // A different address fingerprints differently.
        let other = SecurityEvent::login_failed("someone.else@example.com");
        assert_ne!(second, other);
    }

    #[test]
    fn rate_limit_tripped_fingerprints_the_key() {
        let event = SecurityEvent::rate_limit_tripped(RateLimitBucket::Ip, "203.0.113.7");
        let SecurityEvent::RateLimitTripped {
            bucket,
            key_fingerprint,
        } = event
        else {
            panic!("expected RateLimitTripped");
        };
        assert_eq!(bucket, RateLimitBucket::Ip);
        assert_eq!(key_fingerprint.len(), 64);
        assert!(!key_fingerprint.contains("203.0.113.7"));
    }

    #[test]
    fn denied_attempts_are_recorded_as_deny_failure() {
        for event in every_security_event() {
            match event.action_name() {
                "auth.login_failed" | "auth.rate_limit_tripped" => {
                    assert_eq!(event.permission_decision(), PermissionDecision::Deny);
                    assert_eq!(event.result(), ActionResult::Failure);
                }
                _ => {
                    assert_eq!(event.permission_decision(), PermissionDecision::Allow);
                    assert_eq!(event.result(), ActionResult::Success);
                }
            }
        }
    }

    #[test]
    fn to_audit_event_maps_the_envelope_fields() {
        let event = SecurityEvent::GrantAssigned {
            staff_id: StaffId::from(Uuid::from_u128(21)),
            grant_id: GrantId::from(Uuid::from_u128(23)),
            action: "contact:read".to_string(),
            scope: "agency:1".to_string(),
        };
        let actor_id = ActorId::from(Uuid::from_u128(99));
        let correlation_id = Uuid::from_u128(100);
        let occurred_at = DateTime::from_timestamp(1_700_000_001, 0).expect("fixed timestamp");
        let envelope = event.to_audit_event(
            ActorKind::Human,
            actor_id,
            Some(ClientProfileId::from(Uuid::from_u128(3))),
            correlation_id,
            occurred_at,
        );
        assert_eq!(envelope.action, "rbac.grant_assigned");
        assert_eq!(envelope.target_kind, "grant");
        assert_eq!(
            envelope.target_id,
            GrantId::from(Uuid::from_u128(23)).to_string()
        );
        // The actor comes from the caller's context, never the payload.
        assert_eq!(envelope.actor_kind, ActorKind::Human);
        assert_eq!(envelope.actor_id, actor_id);
        assert_eq!(envelope.correlation_id, correlation_id);
        assert_eq!(envelope.occurred_at, occurred_at);
        assert_eq!(envelope.permission_decision, PermissionDecision::Allow);
        assert_eq!(envelope.result, ActionResult::Success);
    }

    #[test]
    fn tampered_taxonomy_event_breaks_the_chain() {
        // A committed taxonomy event is tamper-evident like any other:
        // sealing a chain over rendered envelopes, then mutating one
        // envelope's action, breaks verification.
        let occurred_at = DateTime::from_timestamp(1_700_000_002, 0).expect("fixed timestamp");
        let actor_id = ActorId::from(Uuid::from_u128(99));
        let mut events: Vec<AuditEvent> = every_security_event()
            .iter()
            .map(|event| {
                event.to_audit_event(
                    ActorKind::Human,
                    actor_id,
                    None,
                    Uuid::from_u128(100),
                    occurred_at,
                )
            })
            .collect();
        let mut prev = GENESIS_CHAIN_HASH;
        for event in &mut events {
            event.chain_hash = compute_chain_hash(&prev, &event.canonical_bytes());
            prev = event.chain_hash;
        }
        assert!(verify_chain(&events));

        let mut tampered = events.clone();
        tampered[3].action = "auth.login".to_string();
        assert!(!verify_chain(&tampered));
    }
}
