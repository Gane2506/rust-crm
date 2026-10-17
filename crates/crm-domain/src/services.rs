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

use uuid::Uuid;

use crate::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crate::contact::{Contact, Lifecycle};
use crate::contract::{Contract, ContractState};
use crate::error::DomainError;
use crate::ids::{ActorId, ClientProfileId, ContactId, ContractId};
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
