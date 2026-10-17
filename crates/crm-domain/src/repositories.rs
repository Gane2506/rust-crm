// SPDX-License-Identifier: GPL-3.0-only
//! Repository traits (persistence ports).
//!
//! These traits are the persistence boundary of the domain: the domain
//! services talk to these, and the SQLx/PostgreSQL implementations land
//! in task 02-DB. Methods are declared as `fn` returning
//! `impl Future<Output = …> + Send` (rather than native `async fn`):
//! the explicit `Send` bound documents that repository futures must be
//! `Send` so executors may move them across threads, and the style
//! satisfies the `async_fn_in_trait` lint without suppressions.
//!
//! Conventions applied consistently across all repositories:
//!
//! * `create` / `get` / `update` for single-entity operations.
//! * `list_for_client` for client-profile-scoped collection reads with an
//!   explicit `limit` (no unbounded reads).
//! * `&self` receivers; every method returns `Result<_, DomainError>`.
//! * Implementations must validate client-profile scope on every read and
//!   write and must never leak rows across client profiles.

use std::future::Future;

use crate::agent::AgentAction;
use crate::asset::Asset;
use crate::audit::AuditEvent;
use crate::campaign::{Campaign, OptOut, OptOutChannel, OwnerContext, ProspectPool};
use crate::contact::Contact;
use crate::contract::Contract;
use crate::email::{EmailAttachment, EmailMessage, EmailThread};
use crate::error::DomainError;
use crate::ids::{
    AgencyId, AgentActionId, AssetId, AuditEventId, CampaignId, ClientProfileId, ContactId,
    ContractId, EmailAttachmentId, EmailMessageId, EmailThreadId, GrantId, OptOutId,
    ProspectPoolId, StaffId,
};
use crate::profile::ClientProfile;
use crate::security::{Grant, Session, StaffUser};

/// Append-only store for [`AuditEvent`]s.
///
/// The audit log has no update or delete path. `append` is chain-aware:
/// the implementation must recompute the expected head with
/// [`crate::audit::compute_chain_hash`] and reject the event with
/// [`DomainError::ConcurrencyConflict`] when the supplied
/// `chain_hash` does not link to the current head (two writers raced).
pub trait AuditRepository {
    /// Appends one event after verifying its `chain_hash` links to the
    /// current head. Returns the stored event.
    fn append<'a>(
        &'a self,
        event: AuditEvent,
    ) -> impl Future<Output = Result<AuditEvent, DomainError>> + Send + 'a;

    /// Returns the chain hash of the most recently appended event, or
    /// `None` when the log is empty (the next event links from genesis).
    fn latest_chain_hash<'a>(
        &'a self,
    ) -> impl Future<Output = Result<Option<[u8; 32]>, DomainError>> + Send + 'a;

    /// Lists recent events for one client profile, newest first, capped
    /// at `limit`.
    fn list_for_client<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<AuditEvent>, DomainError>> + Send + 'a;

    /// Fetches one event by id.
    fn get<'a>(
        &'a self,
        event_id: AuditEventId,
    ) -> impl Future<Output = Result<AuditEvent, DomainError>> + Send + 'a;
}

/// CRUD plus duplicate-lookup for [`Contact`]s.
pub trait ContactRepository {
    /// Persists a new contact.
    fn create<'a>(
        &'a self,
        contact: Contact,
    ) -> impl Future<Output = Result<Contact, DomainError>> + Send + 'a;

    /// Fetches a contact by id.
    fn get<'a>(
        &'a self,
        contact_id: ContactId,
    ) -> impl Future<Output = Result<Contact, DomainError>> + Send + 'a;

    /// Persists changes to an existing contact.
    fn update<'a>(
        &'a self,
        contact: Contact,
    ) -> impl Future<Output = Result<Contact, DomainError>> + Send + 'a;

    /// Lists contacts for one client profile, capped at `limit`.
    fn list_for_client<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<Contact>, DomainError>> + Send + 'a;

    /// Finds contacts in one client profile by normalized email address.
    /// Used for duplicate detection on import and form capture.
    fn find_by_normalized_email<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        email: &'a str,
    ) -> impl Future<Output = Result<Vec<Contact>, DomainError>> + Send + 'a;
}

/// CRUD for [`Contract`]s.
pub trait ContractRepository {
    /// Persists a new contract.
    fn create<'a>(
        &'a self,
        contract: Contract,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a;

    /// Fetches a contract by id.
    fn get<'a>(
        &'a self,
        contract_id: ContractId,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a;

    /// Persists changes to an existing contract (state, signers,
    /// evidence references). State transitions themselves are validated
    /// by [`Contract::transition_to`](crate::contract::Contract::transition_to)
    /// before the update is stored.
    fn update<'a>(
        &'a self,
        contract: Contract,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a;

    /// Lists contracts for one client profile, capped at `limit`.
    fn list_for_client<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<Contract>, DomainError>> + Send + 'a;
}

/// CRUD for [`Campaign`]s.
pub trait CampaignRepository {
    /// Persists a new campaign.
    fn create<'a>(
        &'a self,
        campaign: Campaign,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Fetches a campaign by id.
    fn get<'a>(
        &'a self,
        campaign_id: CampaignId,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Persists changes to an existing campaign.
    fn update<'a>(
        &'a self,
        campaign: Campaign,
    ) -> impl Future<Output = Result<Campaign, DomainError>> + Send + 'a;

    /// Lists campaigns for one owner (agency or a client profile),
    /// capped at `limit`.
    fn list_for_owner<'a>(
        &'a self,
        owner: &'a OwnerContext,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<Campaign>, DomainError>> + Send + 'a;

    /// Lists campaigns for one client profile, capped at `limit`.
    fn list_by_profile<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<Campaign>, DomainError>> + Send + 'a;
}

/// CRUD for [`ProspectPool`]s.
pub trait ProspectPoolRepository {
    /// Persists a new prospect pool.
    fn create<'a>(
        &'a self,
        pool: ProspectPool,
    ) -> impl Future<Output = Result<ProspectPool, DomainError>> + Send + 'a;

    /// Fetches a pool by id.
    fn get<'a>(
        &'a self,
        pool_id: ProspectPoolId,
    ) -> impl Future<Output = Result<ProspectPool, DomainError>> + Send + 'a;

    /// Persists changes to an existing pool.
    fn update<'a>(
        &'a self,
        pool: ProspectPool,
    ) -> impl Future<Output = Result<ProspectPool, DomainError>> + Send + 'a;

    /// Lists pools for one client profile, capped at `limit`.
    fn list_by_profile<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<ProspectPool>, DomainError>> + Send + 'a;

    /// Adds a contact to the pool (deduplicated).
    fn add_member<'a>(
        &'a self,
        pool_id: ProspectPoolId,
        contact_id: ContactId,
    ) -> impl Future<Output = Result<ProspectPool, DomainError>> + Send + 'a;

    /// Removes a contact from the pool.
    fn remove_member<'a>(
        &'a self,
        pool_id: ProspectPoolId,
        contact_id: ContactId,
    ) -> impl Future<Output = Result<ProspectPool, DomainError>> + Send + 'a;
}

/// Append-only store for [`OptOut`] records.
///
/// Opt-outs are never updated or deleted (compliance); the repository
/// exposes no mutating path beyond `create`.
pub trait OptOutRepository {
    /// Records a new opt-out.
    fn create<'a>(
        &'a self,
        opt_out: OptOut,
    ) -> impl Future<Output = Result<OptOut, DomainError>> + Send + 'a;

    /// Fetches an opt-out by id.
    fn get<'a>(
        &'a self,
        opt_out_id: OptOutId,
    ) -> impl Future<Output = Result<Option<OptOut>, DomainError>> + Send + 'a;

    /// Lists a contact's opt-outs, newest first.
    fn list_by_contact<'a>(
        &'a self,
        contact_id: ContactId,
    ) -> impl Future<Output = Result<Vec<OptOut>, DomainError>> + Send + 'a;

    /// Whether the contact is opted out on `channel`. An
    /// [`OptOutChannel::All`] record suppresses every channel.
    fn is_opted_out<'a>(
        &'a self,
        contact_id: ContactId,
        channel: OptOutChannel,
    ) -> impl Future<Output = Result<bool, DomainError>> + Send + 'a;
}

/// Append-mostly store for [`AgentAction`] records.
pub trait AgentActionRepository {
    /// Persists a new agent action record. Records are never updated in
    /// place; outcome changes are new facts derived from the same
    /// `idempotency_key`.
    fn record<'a>(
        &'a self,
        action: AgentAction,
    ) -> impl Future<Output = Result<AgentAction, DomainError>> + Send + 'a;

    /// Fetches an action by id.
    fn get<'a>(
        &'a self,
        action_id: AgentActionId,
    ) -> impl Future<Output = Result<AgentAction, DomainError>> + Send + 'a;

    /// Finds an action by idempotency key for duplicate suppression.
    fn find_by_idempotency_key<'a>(
        &'a self,
        idempotency_key: &'a str,
    ) -> impl Future<Output = Result<Option<AgentAction>, DomainError>> + Send + 'a;
}

/// Persistence port for [`ClientProfile`] records (agency-managed
/// workspaces).
///
/// Intended storage shape (later task): `client_profiles` table keyed by
/// `profile_id` with the agency owner, name, status and timestamps.
/// Profiles are never deleted: archival is a status, because history
/// stays queryable for audit.
pub trait ClientProfileRepository {
    /// Persists a new client profile.
    fn create<'a>(
        &'a self,
        profile: ClientProfile,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Fetches a client profile by id.
    fn get<'a>(
        &'a self,
        profile_id: ClientProfileId,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Persists profile changes (status transitions, renames). Status
    /// transitions themselves are validated by
    /// [`ClientProfile::suspend`](crate::profile::ClientProfile::suspend)
    /// and friends before the update is stored.
    fn update<'a>(
        &'a self,
        profile: ClientProfile,
    ) -> impl Future<Output = Result<ClientProfile, DomainError>> + Send + 'a;

    /// Lists profiles for one agency, capped at `limit`.
    fn list_for_agency<'a>(
        &'a self,
        agency_id: AgencyId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<ClientProfile>, DomainError>> + Send + 'a;
}

/// Persistence port for [`Asset`] records (file/media attachments).
///
/// Intended storage shape (later task): `assets` table keyed by
/// `asset_id` with the owning profile, uploader, file metadata, the
/// SHA-256 content address and status. Assets are never deleted:
/// archival is a status.
pub trait AssetRepository {
    /// Persists a new asset record.
    fn create<'a>(
        &'a self,
        asset: Asset,
    ) -> impl Future<Output = Result<Asset, DomainError>> + Send + 'a;

    /// Fetches an asset by id.
    fn get<'a>(
        &'a self,
        asset_id: AssetId,
    ) -> impl Future<Output = Result<Asset, DomainError>> + Send + 'a;

    /// Persists asset changes (archival). Status transitions themselves
    /// are validated by [`Asset::archive`](crate::asset::Asset::archive)
    /// before the update is stored.
    fn update<'a>(
        &'a self,
        asset: Asset,
    ) -> impl Future<Output = Result<Asset, DomainError>> + Send + 'a;

    /// Lists assets for one client profile, capped at `limit`.
    fn list_for_client<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<Asset>, DomainError>> + Send + 'a;

    /// Finds an asset in one client profile by content hash, for upload
    /// dedupe. Returns `None` when these bytes were never uploaded under
    /// this profile.
    fn find_by_sha256<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        sha256: &'a [u8; 32],
    ) -> impl Future<Output = Result<Option<Asset>, DomainError>> + Send + 'a;
}

/// Persistence port for [`StaffUser`] records.
///
/// Intended storage shape (later task): `staff_users` table keyed by
/// `staff_id`, with the password credential in a `phc_hash TEXT` column —
/// never a plaintext column.
pub trait StaffRepository {
    /// Fetches a staff user by id.
    fn get<'a>(
        &'a self,
        staff_id: StaffId,
    ) -> impl Future<Output = Result<StaffUser, DomainError>> + Send + 'a;

    /// Finds a staff user by (normalized, lowercase) email for login.
    /// Returns `None` rather than an error for unknown emails; callers
    /// map that to [`DomainError::Unauthorized`] without revealing
    /// whether the account exists.
    fn find_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> impl Future<Output = Result<Option<StaffUser>, DomainError>> + Send + 'a;

    /// Persists a new staff user.
    fn create<'a>(
        &'a self,
        user: StaffUser,
    ) -> impl Future<Output = Result<StaffUser, DomainError>> + Send + 'a;

    /// Persists staff changes (status transitions, credential rotation).
    fn update<'a>(
        &'a self,
        user: StaffUser,
    ) -> impl Future<Output = Result<StaffUser, DomainError>> + Send + 'a;
}

/// Persistence port for login [`Session`] records.
///
/// Intended storage shape (later task): `sessions` table keyed by
/// `session_id` with a `token_hash BYTEA` column — the raw token is never
/// stored. Rows are never deleted: revocation is a timestamp, because
/// session history is security-relevant.
pub trait SessionRepository {
    /// Persists a new session.
    fn create<'a>(
        &'a self,
        session: Session,
    ) -> impl Future<Output = Result<Session, DomainError>> + Send + 'a;

    /// Finds a session by token hash. Returns `None` for unknown tokens;
    /// callers map that to [`DomainError::Unauthorized`].
    fn get_by_token_hash<'a>(
        &'a self,
        token_hash: &'a [u8; 32],
    ) -> impl Future<Output = Result<Option<Session>, DomainError>> + Send + 'a;

    /// Persists session changes (revocation, activity touch, refresh).
    fn update<'a>(
        &'a self,
        session: Session,
    ) -> impl Future<Output = Result<Session, DomainError>> + Send + 'a;

    /// Lists a staff user's sessions, newest first, capped at `limit`.
    fn list_for_staff<'a>(
        &'a self,
        staff_id: StaffId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<Session>, DomainError>> + Send + 'a;
}

/// Persistence port for [`Grant`] assignments.
///
/// Intended storage shape (later task): `grants` table (`grant_id` PK,
/// `staff_id`, resource, operation and scope columns). Grants are
/// additive; there are no negative grants — deny by default.
pub trait GrantRepository {
    /// Lists all grants assigned to a staff user.
    fn list_for_staff<'a>(
        &'a self,
        staff_id: StaffId,
    ) -> impl Future<Output = Result<Vec<Grant>, DomainError>> + Send + 'a;

    /// Assigns a grant to a staff user.
    fn assign<'a>(
        &'a self,
        staff_id: StaffId,
        grant: Grant,
    ) -> impl Future<Output = Result<Grant, DomainError>> + Send + 'a;

    /// Removes a grant assignment. Implementations should tombstone the
    /// assignment rather than delete the row: grant history is audit-
    /// relevant.
    fn revoke<'a>(
        &'a self,
        staff_id: StaffId,
        grant_id: GrantId,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a;
}

/// Persistence port for [`EmailMessage`] records.
///
/// Intended storage shape (later task): `email_messages` table keyed by
/// `message_id`, with `client_profile_id`, nullable `thread_id`,
/// normalized address columns, `direction`, `status`, nullable
/// `provider_message_id` (unique per profile, for idempotent retries
/// and reply correlation), and timestamps. Implementations must
/// validate client-profile scope on every read and write.
pub trait EmailMessageRepository {
    /// Persists a new message (outbound draft or ingested inbound).
    fn create<'a>(
        &'a self,
        message: EmailMessage,
    ) -> impl Future<Output = Result<EmailMessage, DomainError>> + Send + 'a;

    /// Fetches a message by id.
    fn get<'a>(
        &'a self,
        message_id: EmailMessageId,
    ) -> impl Future<Output = Result<EmailMessage, DomainError>> + Send + 'a;

    /// Persists message changes (status transitions, provider id).
    /// Status transitions themselves are validated by
    /// [`EmailMessage::advance_to`](crate::email::EmailMessage::advance_to)
    /// (and friends) before the update is stored.
    fn update<'a>(
        &'a self,
        message: EmailMessage,
    ) -> impl Future<Output = Result<EmailMessage, DomainError>> + Send + 'a;

    /// Lists messages in one thread, oldest first, capped at `limit`.
    fn list_for_thread<'a>(
        &'a self,
        thread_id: EmailThreadId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<EmailMessage>, DomainError>> + Send + 'a;

    /// Lists messages for one client profile, newest first, capped at
    /// `limit`.
    fn list_for_client<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<EmailMessage>, DomainError>> + Send + 'a;

    /// Finds a message in one client profile by its provider message id,
    /// for idempotent retries and reply correlation. Returns `None`
    /// when unknown.
    fn find_by_provider_message_id<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        provider_message_id: &'a str,
    ) -> impl Future<Output = Result<Option<EmailMessage>, DomainError>> + Send + 'a;
}

/// Persistence port for [`EmailThread`] records.
///
/// Intended storage shape (later task): `email_threads` table keyed by
/// `thread_id`, with `client_profile_id`, nullable `provider_thread_id`
/// (unique per profile, for exact-match reply correlation),
/// `message_count`, `last_message_at` and `status`.
pub trait EmailThreadRepository {
    /// Persists a new thread.
    fn create<'a>(
        &'a self,
        thread: EmailThread,
    ) -> impl Future<Output = Result<EmailThread, DomainError>> + Send + 'a;

    /// Fetches a thread by id.
    fn get<'a>(
        &'a self,
        thread_id: EmailThreadId,
    ) -> impl Future<Output = Result<EmailThread, DomainError>> + Send + 'a;

    /// Persists thread changes (message count, participants, status).
    fn update<'a>(
        &'a self,
        thread: EmailThread,
    ) -> impl Future<Output = Result<EmailThread, DomainError>> + Send + 'a;

    /// Lists threads for one client profile, most-recently-active
    /// first, capped at `limit`.
    fn list_for_client<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<EmailThread>, DomainError>> + Send + 'a;

    /// Finds a thread in one client profile by its provider thread id,
    /// for exact-match reply correlation. Returns `None` when unknown.
    fn find_by_provider_thread_id<'a>(
        &'a self,
        client_profile_id: ClientProfileId,
        provider_thread_id: &'a str,
    ) -> impl Future<Output = Result<Option<EmailThread>, DomainError>> + Send + 'a;
}

/// Persistence port for [`EmailAttachment`] links.
///
/// Attachments are links: the bytes live in the asset store (see
/// [`AssetRepository::find_by_sha256`]). Intended storage shape (later
/// task): `email_attachments` table keyed by `attachment_id`, with
/// `email_message_id` and `asset_id` foreign keys.
pub trait EmailAttachmentRepository {
    /// Persists a new attachment link.
    fn create<'a>(
        &'a self,
        attachment: EmailAttachment,
    ) -> impl Future<Output = Result<EmailAttachment, DomainError>> + Send + 'a;

    /// Fetches an attachment link by id.
    fn get<'a>(
        &'a self,
        attachment_id: EmailAttachmentId,
    ) -> impl Future<Output = Result<EmailAttachment, DomainError>> + Send + 'a;

    /// Lists attachment links for one message.
    fn list_for_message<'a>(
        &'a self,
        message_id: EmailMessageId,
    ) -> impl Future<Output = Result<Vec<EmailAttachment>, DomainError>> + Send + 'a;
}
