// SPDX-License-Identifier: GPL-3.0-only
//! Service traits (domain operations with authorization and audit).
//!
//! Services are the authorized entry points to the domain: every method
//! takes an [`ActionContext`] carrying the actor, the client-profile
//! scope and the correlation id. Services enforce the deny-by-default
//! rule (no grant ⇒ [`DomainError::Forbidden`]), validate client-profile
//! scope on all reads and writes, and record an [`AuditEvent`] for every
//! mutation — including denied ones.
//!
//! Methods are declared as `fn` returning `impl Future<Output = …> +
//! Send` (rather than native `async fn`): the explicit `Send` bound
//! documents that service futures must be `Send` so executors may move
//! them across threads, and the style satisfies the `async_fn_in_trait`
//! lint without suppressions.

use std::future::Future;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::asset::Asset;
use crate::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision, SecurityEvent};
use crate::contact::{Contact, Lifecycle};
use crate::contract::{Contract, ContractState};
use crate::error::DomainError;
use crate::ids::{
    ActorId, AgencyId, AssetId, AuditEventId, ClientProfileId, ContactId, ContractId,
};
use crate::profile::ClientProfile;
use crate::security::{Action, IssuedSession, Scope, Session, SessionToken};

/// Who is performing an operation and under what scope.
///
/// Carried by every service call so authorization, client-profile
/// isolation and audit correlation are never optional extras.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionContext {
    /// Whether a human, agent or the system is acting.
    pub actor_kind: ActorKind,
    /// Who is acting (staff, agent principal, or well-known system id).
    pub actor_id: ActorId,
    /// Client-profile scope of the operation, if any. Operations without
    /// a client scope (agency-level) use `None` and require an
    /// agency-scoped grant.
    pub client_profile_id: Option<ClientProfileId>,
    /// Correlates the operation with the originating request/run; also
    /// written onto the resulting audit event.
    pub correlation_id: Uuid,
}

/// Records audit events for domain mutations.
///
/// Implementations compute the event's `chain_hash` from the current log
/// head at append time and must surface
/// [`DomainError::ConcurrencyConflict`] when two writers race on the
/// head (see [`crate::repositories::AuditRepository::append`]).
pub trait AuditService {
    /// Records one action with full provenance. Denied actions
    /// (`permission_decision == Deny`) are recorded too — the log is
    /// append-only and denials are security-relevant.
    #[allow(clippy::too_many_arguments)]
    fn record_action<'a>(
        &'a self,
        ctx: &'a ActionContext,
        action: &'a str,
        target_kind: &'a str,
        target_id: &'a str,
        permission_decision: PermissionDecision,
        result: ActionResult,
        before_ref: Option<&'a str>,
        after_ref: Option<&'a str>,
    ) -> impl Future<Output = Result<AuditEvent, DomainError>> + Send + 'a;
}

/// Filter for querying the audit log.
///
/// Every field is optional; unset fields do not constrain the query.
/// Implementations must enforce the caller's client-profile isolation:
/// a query scoped to a client profile never returns another profile's
/// events, and an agency-wide query requires an agency-scoped context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery {
    /// Only events by this actor.
    pub actor_id: Option<ActorId>,
    /// Only events with this action name (see
    /// [`SecurityEvent::action_name`]).
    pub action: Option<String>,
    /// Only events attributed to this client profile.
    pub client_profile_id: Option<ClientProfileId>,
    /// Only events at or after this time.
    pub from: Option<DateTime<Utc>>,
    /// Only events before this time.
    pub to: Option<DateTime<Utc>>,
    /// Cursor: only events after this event id, in chain order.
    pub after: Option<AuditEventId>,
    /// Maximum events to return. Implementations cap this at a sane
    /// bound and document it.
    pub limit: u64,
}

/// One page of audit events in chain (ascending time) order.
///
/// Chain order — not newest-first — so a reader can stream the log and
/// verify each link as it goes. Display layers reverse for timelines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditPage {
    /// The events in this page, chain order.
    pub events: Vec<AuditEvent>,
    /// Whether more events match beyond this page.
    pub has_more: bool,
}

/// Outcome of verifying the audit chain from genesis.
///
/// The chain is global — one log, one chain — so verification always
/// covers the whole log; scope filtering is a query concern, not a
/// chain concern. (Per-scope sub-chains would need a different schema;
/// the existing `audit_events` table chains globally.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainVerification {
    /// Whether every checked event links correctly from
    /// [`crate::audit::GENESIS_CHAIN_HASH`].
    pub valid: bool,
    /// Number of events checked.
    pub events_checked: u64,
    /// Chain hash of the last checked event — the verified head.
    /// `None` when the log is empty (a vacuous `valid` chain).
    pub head: Option<[u8; 32]>,
    /// The first event that failed verification, if any.
    pub first_bad_event: Option<AuditEventId>,
}

/// Append-only security audit log with chain verification and query.
///
/// This is the domain port the 04-AUD-02 persistence task implements.
/// It extends the [`AuditService`] contract with the security-event
/// taxonomy ([`SecurityEvent`]), idempotent recording, chain
/// verification and filtered queries.
///
/// ## Storage shape (for the 04-AUD-02 implementer)
///
/// One row per event with a column for every [`AuditEvent`] field;
/// `chain_hash` (and the predecessor link) as `BYTEA`; the serialized
/// [`SecurityEvent`] payload alongside (e.g. a `JSONB` column) for
/// forensic detail; `idempotency_key UUID UNIQUE` so re-recording with
/// the same key returns the original event instead of appending a
/// duplicate. Suggested indexes to serve [`AuditQuery`]:
/// `(client_profile_id, occurred_at)`, `(actor_id, occurred_at)` and
/// `(action, occurred_at)`.
pub trait AuditLogService {
    /// Records one [`SecurityEvent`] as a chained [`AuditEvent`].
    ///
    /// The actor is stamped from `ctx` — never from the event payload —
    /// so a caller cannot forge another identity into the log.
    /// `idempotency_key` is a UUID minted per logical operation (render
    /// `crm_core::IdempotencyKey::as_uuid()` at the boundary);
    /// re-recording with the same key returns the original event.
    /// Implementations assign `chain_hash` from the log head at append
    /// time and surface [`DomainError::ConcurrencyConflict`] on head
    /// races, like [`crate::repositories::AuditRepository::append`].
    fn record<'a>(
        &'a self,
        ctx: &'a ActionContext,
        event: &'a SecurityEvent,
        idempotency_key: Uuid,
    ) -> impl Future<Output = Result<AuditEvent, DomainError>> + Send + 'a;

    /// Verifies the whole chain from genesis, returning a
    /// [`ChainVerification`]. Any tampering — rewritten, missing or
    /// reordered events — yields `valid: false` with `first_bad_event`
    /// set.
    fn verify_chain<'a>(
        &'a self,
    ) -> impl Future<Output = Result<ChainVerification, DomainError>> + Send + 'a;

    /// Queries the log with `filter`, enforcing `ctx`'s client-profile
    /// isolation. Events come back in chain (ascending time) order.
    fn query<'a>(
        &'a self,
        ctx: &'a ActionContext,
        filter: &'a AuditQuery,
    ) -> impl Future<Output = Result<AuditPage, DomainError>> + Send + 'a;
}

/// Input for creating a contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewContact {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Raw emails; the service normalizes and deduplicates them.
    pub emails: Vec<String>,
    /// Raw phone numbers; the service normalizes and deduplicates them.
    pub phones: Vec<String>,
    /// Acquisition source, e.g. `"csv-import"`.
    pub source: String,
}

/// Contact lifecycle operations with duplicate handling.
pub trait ContactService {
    /// Creates a contact after normalizing emails/phones and checking
    /// for duplicates via the repository's normalized-email lookup.
    fn create_contact<'a>(
        &'a self,
        input: NewContact,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contact, DomainError>> + Send + 'a;

    /// Moves a contact to the next lifecycle stage, enforcing
    /// [`Contact::transition_to`].
    fn transition_lifecycle<'a>(
        &'a self,
        contact_id: ContactId,
        next: Lifecycle,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contact, DomainError>> + Send + 'a;

    /// Merges `duplicate_id` into `survivor_id`: the duplicate is
    /// archived with `merged_into` set to the survivor. Only for
    /// unambiguous duplicates; ambiguous pairs go through
    /// [`ContactService::request_merge_review`].
    fn merge_contacts<'a>(
        &'a self,
        survivor_id: ContactId,
        duplicate_id: ContactId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contact, DomainError>> + Send + 'a;

    /// Opens a human review case for an ambiguous duplicate pair.
    /// Returns the review case id; the actual review workflow and its
    /// approval state live in the workflow layer.
    fn request_merge_review<'a>(
        &'a self,
        first_id: ContactId,
        second_id: ContactId,
        reason: String,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Uuid, DomainError>> + Send + 'a;
}

/// Input for creating a client profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewClientProfile {
    /// Display name of the client's business.
    pub name: String,
    /// The agency operating the profile.
    pub agency_id: AgencyId,
}

/// Agency-managed workspace lifecycle.
///
/// Profiles are the tenant boundary: every method takes an
/// [`ActionContext`] and implementations must enforce that the caller
/// holds the grant for the profile's agency. Clients never act on
/// profiles directly — there is no client login.
pub trait ClientProfileService {
    /// Creates a new active profile for the agency.
    fn create_profile<'a>(
        &'a self,
        input: NewClientProfile,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Fetches a profile by id, enforcing agency scope.
    fn get_profile<'a>(
        &'a self,
        profile_id: ClientProfileId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Suspends an active profile, pausing work under it.
    fn suspend_profile<'a>(
        &'a self,
        profile_id: ClientProfileId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Reactivates a suspended profile.
    fn reactivate_profile<'a>(
        &'a self,
        profile_id: ClientProfileId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Archives a profile, retiring it permanently.
    fn archive_profile<'a>(
        &'a self,
        profile_id: ClientProfileId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Lists profiles for one agency, capped at `limit`.
    fn list_profiles<'a>(
        &'a self,
        agency_id: AgencyId,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<ClientProfile>, DomainError>> + Send + 'a;
}

/// Input for registering an uploaded asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAsset {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Original file name.
    pub file_name: String,
    /// MIME type supplied at upload.
    pub mime_type: String,
    /// Size in bytes; must be greater than zero.
    pub size_bytes: u64,
    /// SHA-256 of the file bytes, for upload dedupe and integrity.
    pub sha256: [u8; 32],
}

/// File/media assets attached to a client profile.
///
/// The uploader is taken from the [`ActionContext`]'s actor — callers
/// cannot forge another actor's attribution. Implementations should
/// dedupe uploads by content hash within the profile
/// ([`crate::repositories::AssetRepository::find_by_sha256`]).
pub trait AssetService {
    /// Registers an uploaded asset.
    fn register_asset<'a>(
        &'a self,
        input: NewAsset,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Asset, DomainError>> + Send + 'a;

    /// Fetches an asset by id, enforcing client-profile scope.
    fn get_asset<'a>(
        &'a self,
        asset_id: AssetId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Asset, DomainError>> + Send + 'a;

    /// Archives an asset, retiring it from use.
    fn archive_asset<'a>(
        &'a self,
        asset_id: AssetId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Asset, DomainError>> + Send + 'a;

    /// Lists assets for one client profile, capped at `limit`.
    fn list_assets<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<Asset>, DomainError>> + Send + 'a;
}

/// Contract state transitions honoring the payment gate.
pub trait ContractService {
    /// Moves a contract to the next state via
    /// [`Contract::transition_to`]. `payment_verified` comes from payment
    /// reconciliation and gates `SignedAwaitingPayment -> Effective`;
    /// the service must not invent it.
    fn transition_state<'a>(
        &'a self,
        contract_id: ContractId,
        next: ContractState,
        payment_verified: bool,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a;
}

/// Authentication: verifies credentials and manages the session lifecycle.
///
/// Credential verification itself (constant-time hash comparison) is an
/// implementation concern of task 03-SEC-02; the domain defines the
/// contract: successful authentication yields an [`IssuedSession`] whose
/// raw token is shown to the client exactly once, while only the token
/// hash is persisted.
pub trait AuthenticationService {
    /// Verifies `username`/`password` and issues a session.
    ///
    /// Returns [`DomainError::Unauthorized`] when the credentials do not
    /// match — without revealing whether the username exists. Suspended
    /// accounts fail closed with [`DomainError::Forbidden`].
    fn authenticate<'a>(
        &'a self,
        username: &'a str,
        password: &'a [u8],
    ) -> impl Future<Output = Result<IssuedSession, DomainError>> + Send + 'a;

    /// Extends a live session's expiry (sliding expiration). The token
    /// itself is not rotated. Returns [`DomainError::Unauthorized`] when
    /// the token is unknown, expired, revoked or idle-timed-out.
    fn refresh<'a>(
        &'a self,
        token: &'a SessionToken,
    ) -> impl Future<Output = Result<Session, DomainError>> + Send + 'a;

    /// Revokes the session for `token`. Idempotent: revoking an already
    /// revoked or unknown token succeeds — logout must never leak whether
    /// a token existed.
    fn revoke<'a>(
        &'a self,
        token: &'a SessionToken,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a;
}

/// Authorization: resolves a session token to an [`ActionContext`] or denies.
///
/// This is the server-side enforcement point: every request, event
/// handler, workflow node and agent tool call must pass through it.
/// It replaces ad-hoc actor attribution — the returned
/// [`ActionContext`] carries the real actor (staff or agent principal),
/// the scope and the correlation id that the domain services require.
pub trait AuthorizationService {
    /// Authorizes `action` within `scope` for the holder of `token`.
    ///
    /// Returns [`DomainError::Unauthorized`] when the token is unknown,
    /// expired, revoked or idle-timed-out, or its staff account is not
    /// active; [`DomainError::Forbidden`] when no grant covers the
    /// action.
    ///
    /// For agent sessions the implementation MUST, in order:
    /// 1. load the delegation and require
    ///    [`Delegation::is_effective`](crate::security::Delegation::is_effective)
    ///    with the delegator's current status — expired or revoked
    ///    delegations, and delegations whose delegator is no longer
    ///    active, yield `Unauthorized`;
    /// 2. check the action against
    ///    [`Delegation::effective_grants`](crate::security::Delegation::effective_grants)
    ///    computed from the delegator's *current* grants — never the
    ///    delegation's stored grants alone, and never the delegator's
    ///    full grant set.
    fn authorize<'a>(
        &'a self,
        token: &'a SessionToken,
        action: &'a Action,
        scope: &'a Scope,
        correlation_id: Uuid,
    ) -> impl Future<Output = Result<ActionContext, DomainError>> + Send + 'a;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{
        compute_chain_hash, verify_chain as verify_event_chain, GENESIS_CHAIN_HASH,
    };
    use crate::ids::StaffId;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory reference implementation of [`AuditLogService`].
    ///
    /// This is a test double, but it is written as a faithful
    /// implementation of the trait contract — the tests below prove the
    /// contract is implementable and precise, which is what the 04-AUD-02
    /// persistence task will build against.
    struct MemoryAuditLog {
        events: Mutex<Vec<AuditEvent>>,
        by_idempotency_key: Mutex<HashMap<Uuid, AuditEventId>>,
    }

    impl MemoryAuditLog {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                by_idempotency_key: Mutex::new(HashMap::new()),
            }
        }

        /// Scope gate shared by `record` and `query`: a context scoped to
        /// a client profile only ever sees that profile's events.
        fn event_visible(ctx: &ActionContext, event: &AuditEvent) -> bool {
            match (ctx.client_profile_id, event.client_profile_id) {
                (Some(ctx_profile), Some(event_profile)) => ctx_profile == event_profile,
                // Agency-scoped contexts see everything; a
                // profile-scoped context never sees agency-wide events.
                (None, _) => true,
                (Some(_), None) => false,
            }
        }
    }

    impl AuditLogService for MemoryAuditLog {
        async fn record<'a>(
            &'a self,
            ctx: &'a ActionContext,
            event: &'a SecurityEvent,
            idempotency_key: Uuid,
        ) -> Result<AuditEvent, DomainError> {
            // Idempotent replay: the same key returns the original event.
            if let Some(event_id) = self
                .by_idempotency_key
                .lock()
                .expect("test lock")
                .get(&idempotency_key)
            {
                let events = self.events.lock().expect("test lock");
                return events
                    .iter()
                    .find(|e| e.event_id == *event_id)
                    .cloned()
                    .ok_or_else(|| {
                        DomainError::Validation("idempotency index corrupt".to_string())
                    });
            }
            // The actor is stamped from the context — never the payload.
            let mut envelope = event.to_audit_event(
                ctx.actor_kind,
                ctx.actor_id,
                ctx.client_profile_id,
                ctx.correlation_id,
                Utc::now(),
            );
            let mut events = self.events.lock().expect("test lock");
            let prev = events
                .last()
                .map(|e| e.chain_hash)
                .unwrap_or(GENESIS_CHAIN_HASH);
            envelope.chain_hash = compute_chain_hash(&prev, &envelope.canonical_bytes());
            self.by_idempotency_key
                .lock()
                .expect("test lock")
                .insert(idempotency_key, envelope.event_id);
            events.push(envelope.clone());
            Ok(envelope)
        }

        async fn verify_chain(&self) -> Result<ChainVerification, DomainError> {
            let events = self.events.lock().expect("test lock");
            let mut prev = GENESIS_CHAIN_HASH;
            for event in events.iter() {
                if event.chain_hash != compute_chain_hash(&prev, &event.canonical_bytes()) {
                    return Ok(ChainVerification {
                        valid: false,
                        events_checked: events.len() as u64,
                        head: events.last().map(|e| e.chain_hash),
                        first_bad_event: Some(event.event_id),
                    });
                }
                prev = event.chain_hash;
            }
            Ok(ChainVerification {
                valid: true,
                events_checked: events.len() as u64,
                head: events.last().map(|e| e.chain_hash),
                first_bad_event: None,
            })
        }

        async fn query<'a>(
            &'a self,
            ctx: &'a ActionContext,
            filter: &'a AuditQuery,
        ) -> Result<AuditPage, DomainError> {
            let events = self.events.lock().expect("test lock");
            let mut matched: Vec<AuditEvent> = events
                .iter()
                .filter(|e| Self::event_visible(ctx, e))
                .filter(|e| filter.actor_id.map(|a| e.actor_id == a).unwrap_or(true))
                .filter(|e| {
                    filter
                        .action
                        .as_deref()
                        .map(|a| e.action == a)
                        .unwrap_or(true)
                })
                .filter(|e| {
                    filter
                        .client_profile_id
                        .map(|p| e.client_profile_id == Some(p))
                        .unwrap_or(true)
                })
                .filter(|e| {
                    filter
                        .from
                        .map(|from| e.occurred_at >= from)
                        .unwrap_or(true)
                })
                .filter(|e| filter.to.map(|to| e.occurred_at < to).unwrap_or(true))
                .cloned()
                .collect();
            // Cursor: exclusive, in chain order.
            if let Some(after) = filter.after {
                if let Some(pos) = matched.iter().position(|e| e.event_id == after) {
                    matched = matched.split_off(pos + 1);
                }
            }
            let limit = filter.limit.max(1) as usize;
            let has_more = matched.len() > limit;
            matched.truncate(limit);
            Ok(AuditPage {
                events: matched,
                has_more,
            })
        }
    }

    fn test_ctx(actor: ActorId, profile: Option<ClientProfileId>) -> ActionContext {
        ActionContext {
            actor_kind: ActorKind::Human,
            actor_id: actor,
            client_profile_id: profile,
            correlation_id: Uuid::from_u128(500),
        }
    }

    #[tokio::test]
    async fn record_assigns_chain_link_from_head() {
        let log = MemoryAuditLog::new();
        let actor = ActorId::from(Uuid::from_u128(1));
        let ctx = test_ctx(actor, None);
        let first = log
            .record(
                &ctx,
                &SecurityEvent::LoginSucceeded {
                    staff_id: StaffId::from(Uuid::from_u128(2)),
                },
                Uuid::from_u128(11),
            )
            .await
            .expect("record");
        let second = log
            .record(
                &ctx,
                &SecurityEvent::Logout {
                    staff_id: StaffId::from(Uuid::from_u128(2)),
                    session_id: crate::ids::SessionId::from(Uuid::from_u128(3)),
                },
                Uuid::from_u128(12),
            )
            .await
            .expect("record");
        // The second event links from the first.
        assert_eq!(
            second.chain_hash,
            compute_chain_hash(&first.chain_hash, &second.canonical_bytes())
        );
        // ...and the whole log verifies from genesis.
        let verification = log.verify_chain().await.expect("verify");
        assert!(verification.valid);
        assert_eq!(verification.events_checked, 2);
        assert_eq!(verification.head, Some(second.chain_hash));
        assert_eq!(verification.first_bad_event, None);
    }

    #[tokio::test]
    async fn record_is_idempotent_on_key_reuse() {
        let log = MemoryAuditLog::new();
        let ctx = test_ctx(ActorId::from(Uuid::from_u128(1)), None);
        let event = SecurityEvent::StaffSuspended {
            staff_id: StaffId::from(Uuid::from_u128(2)),
        };
        let key = Uuid::from_u128(21);
        let first = log.record(&ctx, &event, key).await.expect("record");
        // Same key, even with a different event: the original comes back,
        // and nothing is appended twice.
        let replay = log
            .record(
                &ctx,
                &SecurityEvent::StaffReactivated {
                    staff_id: StaffId::from(Uuid::from_u128(2)),
                },
                key,
            )
            .await
            .expect("replay");
        assert_eq!(replay.event_id, first.event_id);
        assert_eq!(replay.action, "staff.suspended");
        let page = log
            .query(
                &ctx,
                &AuditQuery {
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(page.events.len(), 1);
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn record_stamps_actor_from_context_not_payload() {
        // The event payload names a *subject* (the staff account being
        // suspended); the audit actor must be the caller from the
        // context, so a compromised caller cannot forge another
        // identity into the log.
        let log = MemoryAuditLog::new();
        let caller = ActorId::from(Uuid::from_u128(7));
        let ctx = test_ctx(caller, None);
        let recorded = log
            .record(
                &ctx,
                &SecurityEvent::StaffSuspended {
                    staff_id: StaffId::from(Uuid::from_u128(2)),
                },
                Uuid::from_u128(31),
            )
            .await
            .expect("record");
        assert_eq!(recorded.actor_id, caller);
        assert_eq!(recorded.actor_kind, ActorKind::Human);
        // ...while the subject stays in the payload target.
        assert_eq!(recorded.target_kind, "staff");
    }

    #[tokio::test]
    async fn query_filters_by_actor_action_and_time() {
        let log = MemoryAuditLog::new();
        let alice = ActorId::from(Uuid::from_u128(1));
        let bob = ActorId::from(Uuid::from_u128(2));
        let before = Utc::now();
        log.record(
            &test_ctx(alice, None),
            &SecurityEvent::LoginSucceeded {
                staff_id: StaffId::from(Uuid::from_u128(10)),
            },
            Uuid::from_u128(41),
        )
        .await
        .expect("record");
        let cutoff = Utc::now();
        log.record(
            &test_ctx(bob, None),
            &SecurityEvent::login_failed("bob@example.com"),
            Uuid::from_u128(42),
        )
        .await
        .expect("record");

        let ctx = test_ctx(alice, None);
        let by_actor = log
            .query(
                &ctx,
                &AuditQuery {
                    actor_id: Some(bob),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(by_actor.events.len(), 1);
        assert_eq!(by_actor.events[0].action, "auth.login_failed");

        let by_action = log
            .query(
                &ctx,
                &AuditQuery {
                    action: Some("auth.login".to_string()),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(by_action.events.len(), 1);
        assert_eq!(by_action.events[0].actor_id, alice);

        let by_time = log
            .query(
                &ctx,
                &AuditQuery {
                    from: Some(cutoff),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(by_time.events.len(), 1);
        assert_eq!(by_time.events[0].action, "auth.login_failed");

        // `to` is exclusive: everything before the cutoff is just Alice.
        let before_cutoff = log
            .query(
                &ctx,
                &AuditQuery {
                    from: Some(before),
                    to: Some(cutoff),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(before_cutoff.events.len(), 1);
        assert_eq!(before_cutoff.events[0].action, "auth.login");
    }

    #[tokio::test]
    async fn query_enforces_client_profile_isolation() {
        let log = MemoryAuditLog::new();
        let profile_a = ClientProfileId::from(Uuid::from_u128(61));
        let profile_b = ClientProfileId::from(Uuid::from_u128(62));
        let actor = ActorId::from(Uuid::from_u128(1));
        log.record(
            &test_ctx(actor, Some(profile_a)),
            &SecurityEvent::GrantAssigned {
                staff_id: StaffId::from(Uuid::from_u128(2)),
                grant_id: crate::ids::GrantId::from(Uuid::from_u128(3)),
                action: "contact:read".to_string(),
                scope: "profile".to_string(),
            },
            Uuid::from_u128(51),
        )
        .await
        .expect("record");
        log.record(
            &test_ctx(actor, Some(profile_b)),
            &SecurityEvent::GrantRevoked {
                staff_id: StaffId::from(Uuid::from_u128(2)),
                grant_id: crate::ids::GrantId::from(Uuid::from_u128(3)),
            },
            Uuid::from_u128(52),
        )
        .await
        .expect("record");

        // A profile-A context sees only profile A's events.
        let page = log
            .query(
                &test_ctx(actor, Some(profile_a)),
                &AuditQuery {
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].action, "rbac.grant_assigned");

        // An agency-wide context sees both.
        let page = log
            .query(
                &test_ctx(actor, None),
                &AuditQuery {
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(page.events.len(), 2);
    }

    #[tokio::test]
    async fn query_paginates_in_chain_order() {
        let log = MemoryAuditLog::new();
        let ctx = test_ctx(ActorId::from(Uuid::from_u128(1)), None);
        for i in 0..5u128 {
            log.record(
                &ctx,
                &SecurityEvent::PasswordEnrolled {
                    staff_id: StaffId::from(Uuid::from_u128(100 + i)),
                },
                Uuid::from_u128(70 + i),
            )
            .await
            .expect("record");
        }
        let first_page = log
            .query(
                &ctx,
                &AuditQuery {
                    limit: 2,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(first_page.events.len(), 2);
        assert!(first_page.has_more);
        // Chain order: oldest first.
        assert!(verify_event_chain(&first_page.events));

        let cursor = first_page.events[1].event_id;
        let second_page = log
            .query(
                &ctx,
                &AuditQuery {
                    after: Some(cursor),
                    limit: 2,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(second_page.events.len(), 2);
        assert!(second_page.has_more);
        assert_ne!(
            second_page.events[0].event_id,
            first_page.events[0].event_id
        );

        let last_page = log
            .query(
                &ctx,
                &AuditQuery {
                    after: Some(second_page.events[1].event_id),
                    limit: 2,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        assert_eq!(last_page.events.len(), 1);
        assert!(!last_page.has_more);
    }

    #[tokio::test]
    async fn verify_chain_reports_the_first_bad_event() {
        // Tampering is detected through the service port, not just the
        // free function: the double's own store is mutated behind its
        // back to simulate a compromised row.
        let log = MemoryAuditLog::new();
        let ctx = test_ctx(ActorId::from(Uuid::from_u128(1)), None);
        for i in 0..3u128 {
            log.record(
                &ctx,
                &SecurityEvent::LoginSucceeded {
                    staff_id: StaffId::from(Uuid::from_u128(200 + i)),
                },
                Uuid::from_u128(80 + i),
            )
            .await
            .expect("record");
        }
        let bad_id = {
            let mut events = log.events.lock().expect("test lock");
            events[1].action = "auth.login_failed".to_string();
            events[1].event_id
        };
        let verification = log.verify_chain().await.expect("verify");
        assert!(!verification.valid);
        assert_eq!(verification.first_bad_event, Some(bad_id));
        assert_eq!(verification.events_checked, 3);
    }
}
