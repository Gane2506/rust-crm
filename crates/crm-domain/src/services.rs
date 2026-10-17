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
use crate::campaign::{
    AttributionModel, Campaign, CampaignStep, Channel, Conversion, OptOut, OptOutChannel,
    OwnerContext, ProspectPool, StepKind, Touchpoint,
};
use crate::contact::{Contact, Lifecycle};
use crate::contract::{Contract, ContractState};
use crate::error::DomainError;
use crate::ids::{
    ActorId, AgencyId, AssetId, AuditEventId, CampaignId, ClientProfileId, ContactId, ContractId,
    EmailThreadId,
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

/// Input for [`EmailService::send`]: an outbound message to compose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEmailMessage {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Thread to append to, if replying in-thread.
    pub thread_id: Option<EmailThreadId>,
    /// Sender address; blank means the agency's configured from-address.
    pub from: String,
    /// Recipient addresses.
    pub to: Vec<String>,
    /// CC addresses.
    pub cc: Vec<String>,
    /// BCC addresses.
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body_text: String,
    /// HTML body, if supplied.
    pub body_html: Option<String>,
    /// Idempotency key for the send: retries with the same key return
    /// the original message instead of sending twice.
    pub idempotency_key: String,
}

/// An inbound message handed to [`EmailService::ingest_reply`] by the
/// provider adapter after fetching it from Gmail/Zoho.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEmail {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Thread hints for reply correlation (see
    /// [`crate::email::InboundThreadHints`]).
    pub hints: crate::email::InboundThreadHints,
    /// Sender address.
    pub from: String,
    /// Recipient addresses.
    pub to: Vec<String>,
    /// Plain-text body.
    pub body_text: String,
    /// HTML body, if supplied.
    pub body_html: Option<String>,
    /// Provider's message id.
    pub provider_message_id: String,
    /// When the provider received it.
    pub received_at: DateTime<Utc>,
}

/// Outcome of a provider send, returned by
/// [`EmailProvider::send_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSendOutcome {
    /// Provider's message id for correlation and idempotent retries.
    pub provider_message_id: String,
    /// Provider's thread id, when the provider threads messages.
    pub provider_thread_id: Option<String>,
}

/// Domain operations for email: authorized entry points.
///
/// Every method takes an [`ActionContext`] (actor, scope, correlation
/// id), enforces deny-by-default authorization, validates
/// client-profile scope, and records an [`AuditEvent`] for every
/// mutation — including denied ones. Email mutations will authorize
/// against `Resource::Email` / `Resource::EmailThread` once those
/// variants land with 07-MAIL-02 (see [`crate::email`]).
pub trait EmailService {
    /// Composes and queues an outbound message.
    ///
    /// Validates via [`EmailMessage::new_outbound`](crate::email::EmailMessage::new_outbound),
    /// creating the message in `Draft` and immediately advancing it to
    /// `Queued`. The idempotency key makes retries safe: a second
    /// `send` with the same key returns the original message.
    /// Records an `"email.sent"` audit event on success.
    fn send<'a>(
        &'a self,
        input: NewEmailMessage,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::email::EmailMessage, DomainError>> + Send + 'a;

    /// Ingests one inbound reply fetched by the provider adapter.
    ///
    /// Correlates the reply with a thread via the
    /// [`match_thread_by_subject`](crate::email::match_thread_by_subject)
    /// cascade (provider thread id → parent message → subject
    /// heuristic), creating a new thread when nothing matches. Creates
    /// the message via
    /// [`EmailMessage::new_inbound`](crate::email::EmailMessage::new_inbound)
    /// and records an `"email.received"` audit event. Duplicate
    /// `provider_message_id`s are ignored (at-most-once ingestion).
    fn ingest_reply<'a>(
        &'a self,
        inbound: InboundEmail,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::email::EmailMessage, DomainError>> + Send + 'a;

    /// Fetches a thread by id, enforcing client-profile scope.
    fn get_thread<'a>(
        &'a self,
        thread_id: EmailThreadId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::email::EmailThread, DomainError>> + Send + 'a;

    /// Lists threads for one client profile, most-recently-active
    /// first, capped at `limit`.
    fn list_threads<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<crate::email::EmailThread>, DomainError>> + Send + 'a;
}

/// Provider adapter port: Gmail and Zoho implementations land in
/// 07-MAIL-02. This trait is the seam between the domain and the
/// provider APIs.
///
/// # Contract for implementors
///
/// * **Idempotency.** `send_message` must be safe to retry: the domain
///   passes the provider message id back via
///   [`EmailMessage::set_provider_message_id`](crate::email::EmailMessage::set_provider_message_id),
///   and adapters must not send a duplicate when the same logical
///   message is submitted twice (key retries on the idempotency key
///   the service supplies out-of-band).
/// * **Error mapping.** Provider failures (auth, rate limit, rejected
///   recipient, network) map to [`DomainError`] without leaking
///   provider credentials or raw API bodies. Permanent rejections
///   (bad address, policy refusal) become
///   [`DomainError::Validation`]; transient failures should be
///   retried by the caller and surfaced distinctly from validation
///   failures.
/// * **No scope escape.** Adapters operate on one agency's credentials
///   and must never mix messages across client profiles; the
///   `client_profile_id` on every message is the tenant boundary.
/// * **`fetch_message`** returns the full inbound payload the service
///   needs for [`EmailService::ingest_reply`], including thread hints.
pub trait EmailProvider {
    /// Sends one queued message via the provider. Returns the
    /// provider's ids for correlation.
    fn send_message<'a>(
        &'a self,
        message: &'a crate::email::EmailMessage,
    ) -> impl Future<Output = Result<ProviderSendOutcome, DomainError>> + Send + 'a;

    /// Fetches one inbound message by provider message id.
    fn fetch_message<'a>(
        &'a self,
        provider_message_id: &'a str,
    ) -> impl Future<Output = Result<InboundEmail, DomainError>> + Send + 'a;
}

/// Input for [`CampaignService::create_campaign`].
#[derive(Debug, Clone)]
pub struct NewCampaign {
    /// Human-readable campaign name (non-blank).
    pub name: String,
    /// Who owns the campaign (agency or one client profile).
    pub owner_context: OwnerContext,
    /// Outreach channel.
    pub channel: Channel,
    /// Identifier of the prospect pool the campaign draws from.
    pub prospect_pool_id: String,
    /// Cron expression for the delivery schedule.
    pub schedule: String,
    /// Suppression rules reference (opt-outs, do-not-contact lists).
    pub suppression_policy: String,
    /// How conversions are attributed to this campaign.
    pub conversion_rules: String,
    /// Days after a touch during which a conversion still attributes.
    pub attribution_window_days: u32,
    /// Idempotency key: retries with the same key return the original
    /// campaign instead of creating a duplicate.
    pub idempotency_key: String,
}

/// Input for [`CampaignService::add_step`].
#[derive(Debug, Clone)]
pub struct NewCampaignStep {
    /// The campaign the step belongs to.
    pub campaign_id: CampaignId,
    /// What the step does.
    pub kind: StepKind,
    /// Position in the sequence (0-based; unique per campaign).
    pub position: u32,
    /// Template (email) or script (call) reference.
    pub template_id: Option<String>,
    /// Wait duration in seconds (wait steps only).
    pub wait_duration_secs: Option<u64>,
}

/// Input for [`CampaignService::record_conversion`].
#[derive(Debug, Clone)]
pub struct NewConversion {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// The contact who converted.
    pub contact_id: ContactId,
    /// Candidate touchpoints for attribution; the service applies the
    /// campaign's attribution window and
    /// [`AttributionModel`](crate::campaign::AttributionModel) before
    /// crediting.
    pub touchpoints: Vec<Touchpoint>,
    /// Which attribution model to apply.
    pub attribution_model: AttributionModel,
    /// Monetary value in cents, if known.
    pub value_cents: Option<u64>,
    /// When the conversion happened.
    pub occurred_at: DateTime<Utc>,
}

/// Authorized entry point for campaign operations.
///
/// Every method takes an [`ActionContext`]: the service enforces
/// deny-by-default authorization against
/// [`Resource::Campaign`](crate::security::Resource::Campaign),
/// validates client-profile scope, and records an [`AuditEvent`] for
/// every mutation.
pub trait CampaignService {
    /// Creates a campaign in `Draft` via
    /// [`Campaign::new`](crate::campaign::Campaign::new). Records a
    /// `"campaign.created"` audit event.
    fn create_campaign<'a>(
        &'a self,
        input: NewCampaign,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Appends a step to a draft campaign via
    /// [`CampaignStep::new`](crate::campaign::CampaignStep::new).
    /// Rejected once the campaign has left `Draft` — the sequence is
    /// frozen at schedule time. Records a `"campaign.step_added"` audit
    /// event.
    fn add_step<'a>(
        &'a self,
        input: NewCampaignStep,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<CampaignStep, DomainError>> + Send + 'a;

    /// Schedules a draft campaign: `Draft → Scheduled`. The launch time
    /// must be in the future. Records a `"campaign.scheduled"` audit
    /// event.
    fn schedule<'a>(
        &'a self,
        campaign_id: CampaignId,
        schedule_for: DateTime<Utc>,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Launches a draft or scheduled campaign: `Draft|Scheduled →
    /// Running`. Requires at least one step. Records a
    /// `"campaign.launched"` audit event.
    fn launch<'a>(
        &'a self,
        campaign_id: CampaignId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Pauses a running campaign. Records a `"campaign.paused"` audit
    /// event.
    fn pause<'a>(
        &'a self,
        campaign_id: CampaignId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Resumes a paused campaign. Records a `"campaign.resumed"` audit
    /// event.
    fn resume<'a>(
        &'a self,
        campaign_id: CampaignId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Completes a running or paused campaign. Records a
    /// `"campaign.completed"` audit event.
    fn complete<'a>(
        &'a self,
        campaign_id: CampaignId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Cancels a campaign from any non-terminal state. Records a
    /// `"campaign.cancelled"` audit event.
    fn cancel<'a>(
        &'a self,
        campaign_id: CampaignId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Records a conversion, attributing it to one of the candidate
    /// touchpoints per the campaign's attribution model and window.
    /// The credited campaign id (if any) is stored on the
    /// [`Conversion`](crate::campaign::Conversion). Records a
    /// `"campaign.conversion_recorded"` audit event.
    fn record_conversion<'a>(
        &'a self,
        input: NewConversion,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Conversion, DomainError>> + Send + 'a;

    /// Creates a prospect pool via
    /// [`ProspectPool::new`](crate::campaign::ProspectPool::new).
    /// Records a `"campaign.pool_created"` audit event.
    fn create_pool<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        name: String,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<ProspectPool, DomainError>> + Send + 'a;

    /// Records a contact's opt-out via
    /// [`OptOut::new`](crate::campaign::OptOut::new). Opt-outs are
    /// immutable and append-only. Records a `"campaign.opted_out"`
    /// audit event.
    fn record_opt_out<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        contact_id: ContactId,
        channel: OptOutChannel,
        reason: Option<String>,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<OptOut, DomainError>> + Send + 'a;
}

/// Input for [`AgentService::start_run`]: a new delegated agent run.
#[derive(Debug, Clone)]
pub struct NewAgentRun {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Which kind of agent executes the run.
    pub agent_kind: String,
    /// Fallback chain of model-adapter names; the initial adapter is the
    /// chain's primary (task 09-JEV-02: the chain is stored on the run).
    pub fallback_chain: crate::jev::FallbackChain,
    /// Input payload for the run.
    pub input: serde_json::Value,
}

/// Input for [`AgentService::record_tool_call`]: a model-requested tool call.
#[derive(Debug, Clone)]
pub struct NewToolCall {
    /// The run this call belongs to.
    pub run_id: crate::ids::AgentRunId,
    /// Tool name (matches [`AgentTool::name`](crate::jev::AgentTool::name)).
    pub tool_name: String,
    /// Arguments supplied by the model.
    pub input: serde_json::Value,
    /// Whether the tool requires human approval. Declared by the caller
    /// from the tool definition (a tool registry is a later task); the
    /// service enforces the approval flow from this flag.
    pub requires_approval: bool,
}

/// Input for [`AgentService::handoff`]: an agent-to-agent context transfer.
#[derive(Debug, Clone)]
pub struct NewHandoff {
    /// The run handing work off.
    pub from_run_id: crate::ids::AgentRunId,
    /// Which agent kind receives the work.
    pub to_agent_kind: String,
    /// Context for the receiving agent (opaque JSON, never null).
    pub context: serde_json::Value,
}

/// Checks whether an actor holds a named approval role (RPITIT).
///
/// Roles are not persisted as assignments in the identity schema —
/// grants are the stored unit — so this port lets the deployment define
/// the role source. The reference implementation resolves a role to the
/// grant that materializes it (e.g. `"agent_approver"` ⇔
/// `AgentRun`/`Approve` in the run's scope).
///
/// Fail closed: an unresolvable check is an error, never an implicit
/// grant. The agent service refuses the approval when the check errors
/// or reports no membership.
pub trait RoleMembership {
    /// Whether `actor` holds `role` for operations inside
    /// `client_profile_id`.
    fn has_role<'a>(
        &'a self,
        actor: ActorId,
        role: &'a str,
        client_profile_id: ClientProfileId,
    ) -> impl Future<Output = Result<bool, DomainError>> + Send + 'a;
}

/// Agent operations: run lifecycle, tool-call approval and handoffs.
///
/// All methods take an [`ActionContext`] and enforce scoped
/// authorization against `Resource::AgentRun` (task 09-JEV-02 adds the
/// variant): profile-scoped contexts may only touch their own profile's
/// runs; agency-scoped contexts require an agency grant. Every mutation
/// records an audit event (`"agent.run_started"`, `"agent.approved"`,
/// `"agent.tool_executed"`, `"agent.handed_off"`, …).
pub trait AgentService {
    /// Starts a run via [`AgentRun::new`](crate::jev::AgentRun::new),
    /// immediately transitioning it to `Running`. Records an
    /// `"agent.run_started"` audit event.
    fn start_run<'a>(
        &'a self,
        input: NewAgentRun,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::AgentRun, DomainError>> + Send + 'a;

    /// Pauses a running run to await human approval via
    /// [`AgentRun::request_approval`](crate::jev::AgentRun::request_approval).
    /// Records an `"agent.approval_requested"` audit event.
    fn request_approval<'a>(
        &'a self,
        run_id: crate::ids::AgentRunId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::AgentRun, DomainError>> + Send + 'a;

    /// Approves an approval-awaiting run (human decision), resuming it
    /// via [`AgentRun::approve`](crate::jev::AgentRun::approve).
    /// Records an `"agent.approved"` audit event.
    fn approve_step<'a>(
        &'a self,
        run_id: crate::ids::AgentRunId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::AgentRun, DomainError>> + Send + 'a;

    /// Denies an approval-awaiting run (human decision), failing it via
    /// [`AgentRun::reject`](crate::jev::AgentRun::reject). Records an
    /// `"agent.denied"` audit event.
    fn deny_step<'a>(
        &'a self,
        run_id: crate::ids::AgentRunId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::AgentRun, DomainError>> + Send + 'a;

    /// Completes a running run with its output. Records an
    /// `"agent.run_completed"` audit event.
    fn complete_run<'a>(
        &'a self,
        run_id: crate::ids::AgentRunId,
        output: serde_json::Value,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::AgentRun, DomainError>> + Send + 'a;

    /// Fails a run (error path). Records an `"agent.run_failed"` audit
    /// event.
    fn fail_run<'a>(
        &'a self,
        run_id: crate::ids::AgentRunId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::AgentRun, DomainError>> + Send + 'a;

    /// Cancels a run from any non-terminal status. Idempotent.
    /// Records an `"agent.run_cancelled"` audit event.
    fn cancel_run<'a>(
        &'a self,
        run_id: crate::ids::AgentRunId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::AgentRun, DomainError>> + Send + 'a;

    /// Records a model-requested tool call via
    /// [`ToolCall::new`](crate::jev::ToolCall::new). When the tool
    /// requires approval, the call stays `Pending` until a human
    /// approves; otherwise the service may execute it immediately.
    /// Records an `"agent.tool_recorded"` audit event.
    fn record_tool_call<'a>(
        &'a self,
        input: NewToolCall,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::ToolCall, DomainError>> + Send + 'a;

    /// Hands work from one run to another agent kind via
    /// [`Handoff::new`](crate::jev::Handoff::new). Records an
    /// `"agent.handed_off"` audit event.
    fn handoff<'a>(
        &'a self,
        input: NewHandoff,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::jev::Handoff, DomainError>> + Send + 'a;
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

/// Input for [`WorkflowService::create_workflow`]: a new automation workflow.
#[derive(Debug, Clone)]
pub struct NewWorkflow {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Human-readable workflow name (validated non-blank).
    pub name: String,
}

/// Input for [`WorkflowService::add_node`]: a new typed canvas node.
#[derive(Debug, Clone)]
pub struct NewWorkflowNode {
    /// The workflow the node belongs to.
    pub workflow_id: crate::ids::WorkflowId,
    /// What kind of step this is.
    pub node_type: crate::workflow::WorkflowNodeType,
    /// Node configuration (opaque JSON, never null).
    pub config: serde_json::Value,
    /// Canvas position in pixels: `(x, y)`.
    pub position: (i32, i32),
}

/// Input for [`WorkflowService::add_edge`]: a new typed connection.
#[derive(Debug, Clone)]
pub struct NewWorkflowEdge {
    /// The workflow the edge belongs to.
    pub workflow_id: crate::ids::WorkflowId,
    /// The node the edge leaves.
    pub from_node: crate::ids::WorkflowNodeId,
    /// The node the edge enters.
    pub to_node: crate::ids::WorkflowNodeId,
    /// Optional label (e.g. the condition branch).
    pub label: Option<String>,
}

/// Input for [`WorkflowService::set_trigger`]: the workflow's trigger.
///
/// Replaces any existing trigger: exactly one trigger per workflow.
#[derive(Debug, Clone)]
pub struct NewTrigger {
    /// The workflow the trigger starts.
    pub workflow_id: crate::ids::WorkflowId,
    /// How runs are started.
    pub trigger_type: crate::workflow::TriggerType,
    /// Trigger configuration (opaque JSON, never null).
    pub config: serde_json::Value,
}

/// Authorized operations on automation workflows.
///
/// All mutations authorize against `Resource::Workflow` (profile-scoped;
/// the variant lands in task 10-WORK-02 — see the
/// [`crate::workflow`] module docs) and record an [`AuditEvent`] with
/// the pinned action name documented on each method.
pub trait WorkflowService {
    /// Creates a workflow via [`Workflow::new`](crate::workflow::Workflow::new).
    /// Records a `"workflow.created"` audit event.
    fn create_workflow<'a>(
        &'a self,
        input: NewWorkflow,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::workflow::Workflow, DomainError>> + Send + 'a;

    /// Publishes a draft workflow via
    /// [`Workflow::publish`](crate::workflow::Workflow::publish) and
    /// freezes a [`WorkflowVersion`](crate::workflow::WorkflowVersion)
    /// snapshot. Records a `"workflow.published"` audit event.
    fn publish_workflow<'a>(
        &'a self,
        workflow_id: crate::ids::WorkflowId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::workflow::Workflow, DomainError>> + Send + 'a;

    /// Archives a workflow via
    /// [`Workflow::archive`](crate::workflow::Workflow::archive).
    /// Idempotent. Records a `"workflow.archived"` audit event.
    fn archive_workflow<'a>(
        &'a self,
        workflow_id: crate::ids::WorkflowId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::workflow::Workflow, DomainError>> + Send + 'a;

    /// Adds a typed node via
    /// [`WorkflowNode::new`](crate::workflow::WorkflowNode::new).
    /// Records a `"workflow.node_added"` audit event.
    fn add_node<'a>(
        &'a self,
        input: NewWorkflowNode,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::workflow::WorkflowNode, DomainError>> + Send + 'a;

    /// Removes a node and its dangling edges. Records a
    /// `"workflow.node_removed"` audit event.
    fn remove_node<'a>(
        &'a self,
        node_id: crate::ids::WorkflowNodeId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a;

    /// Adds a typed edge via
    /// [`WorkflowEdge::new`](crate::workflow::WorkflowEdge::new);
    /// both endpoints must exist in the workflow. Records a
    /// `"workflow.edge_added"` audit event.
    fn add_edge<'a>(
        &'a self,
        input: NewWorkflowEdge,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::workflow::WorkflowEdge, DomainError>> + Send + 'a;

    /// Removes an edge. Records a `"workflow.edge_removed"` audit event.
    fn remove_edge<'a>(
        &'a self,
        edge_id: crate::ids::WorkflowEdgeId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a;

    /// Sets (or replaces) the workflow's trigger via
    /// [`Trigger::new`](crate::workflow::Trigger::new). Records a
    /// `"workflow.trigger_set"` audit event.
    fn set_trigger<'a>(
        &'a self,
        input: NewTrigger,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::workflow::Trigger, DomainError>> + Send + 'a;

    /// Dry-runs the workflow with
    /// [`Simulator::simulate`](crate::workflow::Simulator::simulate).
    /// Read-only: never executes actions, never mutates state. Records
    /// a `"workflow.simulated"` audit event.
    fn simulate<'a>(
        &'a self,
        workflow_id: crate::ids::WorkflowId,
        input: serde_json::Value,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crate::workflow::SimulationTrace, DomainError>> + Send + 'a;
}
