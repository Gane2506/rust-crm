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
use crate::audit::AuditEvent;
use crate::campaign::{Campaign, OwnerContext};
use crate::contact::Contact;
use crate::contract::Contract;
use crate::error::DomainError;
use crate::ids::{AgentActionId, AuditEventId, CampaignId, ClientProfileId, ContactId, ContractId};

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
