// SPDX-License-Identifier: GPL-3.0-only
//! In-memory implementations of the `crm-domain` repository and service traits.
//!
//! # Vertical-slice stand-ins
//!
//! Everything in this module exists to back the 01-FND-02 vertical slice
//! (HTTP API over the domain) **without a database**. The stores are
//! `tokio::sync::RwLock`-guarded `HashMap`s:
//!
//! * data does not survive restarts;
//! * there is no cross-process consistency;
//! * authorization is limited to client-profile scope checks — grant-based
//!   RBAC lands in task 03-SEC;
//! * phone normalization is trim + dedupe only (full E.164 handling is
//!   deferred to the telephony workstream).
//!
//! **Task 02-DB replaces these with SQLx/PostgreSQL implementations.**
//! Do not build production behavior on these types.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crm_domain::audit::{
    compute_chain_hash, ActionResult, ActorKind, AuditEvent, PermissionDecision, GENESIS_CHAIN_HASH,
};
use crm_domain::contact::{Contact, Lifecycle};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, ContactId};
use crm_domain::repositories::{AuditRepository, ContactRepository};
use crm_domain::services::{ActionContext, AuditService, ContactService, NewContact};

/// Builds the [`ActionContext`] used by the unauthenticated vertical-slice
/// HTTP layer.
///
/// **Stand-in:** every request is attributed to the well-known system actor
/// ([`Uuid::nil`]). Task 03-SEC replaces this with authenticated staff
/// sessions and grant checks.
#[must_use]
pub fn api_action_context(
    client_profile_id: Option<ClientProfileId>,
    correlation_id: Uuid,
) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id,
        correlation_id,
    }
}

/// Normalizes one email address: trims surrounding whitespace and
/// lowercases it so duplicate detection is a plain equality check.
fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Normalizes one phone number: trims surrounding whitespace.
///
/// Full E.164 normalization is deferred to the telephony workstream; this
/// stand-in only guarantees trim + dedupe.
fn normalize_phone(raw: &str) -> String {
    raw.trim().to_owned()
}

/// Normalizes a list: applies `normalize` to each item, drops empties and
/// dedupes, preserving first-seen order.
fn normalize_list(raw: Vec<String>, normalize: fn(&str) -> String) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in raw {
        let normalized = normalize(&item);
        if !normalized.is_empty() && seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }
    out
}

/// In-memory [`ContactRepository`].
///
/// **Vertical-slice stand-in** — replaced by SQLx in task 02-DB.
#[derive(Debug, Clone, Default)]
pub struct InMemoryContactRepository {
    contacts: Arc<RwLock<HashMap<ContactId, Contact>>>,
}

impl InMemoryContactRepository {
    /// Creates an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts contacts owned by one client profile.
    ///
    /// Non-trait helper for the vertical-slice list endpoint's `total`.
    pub async fn count_for_client(&self, client_profile_id: ClientProfileId) -> usize {
        self.contacts
            .read()
            .await
            .values()
            .filter(|contact| contact.client_profile_id == client_profile_id)
            .count()
    }
}

impl ContactRepository for InMemoryContactRepository {
    async fn create(&self, contact: Contact) -> Result<Contact, DomainError> {
        let mut contacts = self.contacts.write().await;
        if contacts.contains_key(&contact.contact_id) {
            return Err(DomainError::Conflict(format!(
                "contact '{}' already exists",
                contact.contact_id
            )));
        }
        contacts.insert(contact.contact_id, contact.clone());
        Ok(contact)
    }

    async fn get(&self, contact_id: ContactId) -> Result<Contact, DomainError> {
        self.contacts
            .read()
            .await
            .get(&contact_id)
            .cloned()
            .ok_or(DomainError::NotFound {
                kind: "contact".to_owned(),
                id: contact_id.to_string(),
            })
    }

    async fn update(&self, contact: Contact) -> Result<Contact, DomainError> {
        let mut contacts = self.contacts.write().await;
        if !contacts.contains_key(&contact.contact_id) {
            return Err(DomainError::NotFound {
                kind: "contact".to_owned(),
                id: contact.contact_id.to_string(),
            });
        }
        contacts.insert(contact.contact_id, contact.clone());
        Ok(contact)
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Contact>, DomainError> {
        let contacts = self.contacts.read().await;
        let mut result: Vec<Contact> = contacts
            .values()
            .filter(|contact| contact.client_profile_id == client_profile_id)
            .cloned()
            .collect();
        // Deterministic order for the stand-in; the SQL implementation
        // in 02-DB orders by (created_at, id).
        result.sort_by_key(|contact| contact.contact_id.to_string());
        let cap = usize::try_from(limit).unwrap_or(usize::MAX);
        result.truncate(cap);
        Ok(result)
    }

    async fn find_by_normalized_email(
        &self,
        client_profile_id: ClientProfileId,
        email: &str,
    ) -> Result<Vec<Contact>, DomainError> {
        let contacts = self.contacts.read().await;
        let result = contacts
            .values()
            .filter(|contact| {
                contact.client_profile_id == client_profile_id
                    && contact.normalized_emails.iter().any(|e| e == email)
            })
            .cloned()
            .collect();
        Ok(result)
    }
}

/// Ordered audit-log state. A single lock guards both the map and the
/// order so "read head + append" is atomic.
#[derive(Debug, Default)]
struct AuditLog {
    events: HashMap<AuditEventId, AuditEvent>,
    /// Event ids in append order (oldest first).
    order: Vec<AuditEventId>,
}

/// In-memory [`AuditRepository`] with atomic chain-linked appends.
///
/// **Vertical-slice stand-in** — replaced by SQLx in task 02-DB.
#[derive(Debug, Clone, Default)]
pub struct InMemoryAuditRepository {
    log: Arc<RwLock<AuditLog>>,
}

impl InMemoryAuditRepository {
    /// Creates an empty audit log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Assigns `chain_hash` from the current head and appends, atomically.
    ///
    /// Non-trait helper used by [`MemoryAuditService`]: reading the head
    /// and appending under one write lock removes the race between
    /// [`AuditRepository::latest_chain_hash`] and
    /// [`AuditRepository::append`].
    pub async fn append_chained(&self, event: &mut AuditEvent) -> Result<AuditEvent, DomainError> {
        let mut log = self.log.write().await;
        let prev = log
            .order
            .last()
            .and_then(|id| log.events.get(id))
            .map_or(GENESIS_CHAIN_HASH, |head| head.chain_hash);
        event.chain_hash = compute_chain_hash(&prev, &event.canonical_bytes());
        if log.events.contains_key(&event.event_id) {
            return Err(DomainError::Conflict(format!(
                "audit event '{}' already recorded",
                event.event_id
            )));
        }
        log.order.push(event.event_id);
        log.events.insert(event.event_id, event.clone());
        Ok(event.clone())
    }
}

impl AuditRepository for InMemoryAuditRepository {
    async fn append(&self, event: AuditEvent) -> Result<AuditEvent, DomainError> {
        let mut log = self.log.write().await;
        let prev = log
            .order
            .last()
            .and_then(|id| log.events.get(id))
            .map_or(GENESIS_CHAIN_HASH, |head| head.chain_hash);
        let expected = compute_chain_hash(&prev, &event.canonical_bytes());
        if event.chain_hash != expected {
            return Err(DomainError::ConcurrencyConflict);
        }
        if log.events.contains_key(&event.event_id) {
            return Err(DomainError::Conflict(format!(
                "audit event '{}' already recorded",
                event.event_id
            )));
        }
        log.order.push(event.event_id);
        log.events.insert(event.event_id, event.clone());
        Ok(event)
    }

    async fn latest_chain_hash(&self) -> Result<Option<[u8; 32]>, DomainError> {
        let log = self.log.read().await;
        Ok(log
            .order
            .last()
            .and_then(|id| log.events.get(id))
            .map(|head| head.chain_hash))
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<AuditEvent>, DomainError> {
        let log = self.log.read().await;
        let result = log
            .order
            .iter()
            .rev()
            .filter_map(|id| log.events.get(id))
            .filter(|event| event.client_profile_id == Some(client_profile_id))
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .cloned()
            .collect();
        Ok(result)
    }

    async fn get(&self, event_id: AuditEventId) -> Result<AuditEvent, DomainError> {
        self.log
            .read()
            .await
            .events
            .get(&event_id)
            .cloned()
            .ok_or(DomainError::NotFound {
                kind: "audit_event".to_owned(),
                id: event_id.to_string(),
            })
    }
}

/// In-memory [`AuditService`].
///
/// **Vertical-slice stand-in** — replaced by the SQLx-backed service in
/// task 02-DB.
#[derive(Debug, Clone, Default)]
pub struct MemoryAuditService {
    log: InMemoryAuditRepository,
}

impl MemoryAuditService {
    /// Creates the service over the given repository.
    #[must_use]
    pub fn new(log: InMemoryAuditRepository) -> Self {
        Self { log }
    }
}

impl AuditService for MemoryAuditService {
    #[allow(clippy::too_many_arguments)]
    async fn record_action(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_kind: &str,
        target_id: &str,
        permission_decision: PermissionDecision,
        result: ActionResult,
        before_ref: Option<&str>,
        after_ref: Option<&str>,
    ) -> Result<AuditEvent, DomainError> {
        let mut event = AuditEvent {
            event_id: AuditEventId::new(),
            occurred_at: Utc::now(),
            actor_kind: ctx.actor_kind,
            actor_id: ctx.actor_id,
            client_profile_id: ctx.client_profile_id,
            action: action.to_owned(),
            target_kind: target_kind.to_owned(),
            target_id: target_id.to_owned(),
            correlation_id: ctx.correlation_id,
            permission_decision,
            result,
            before_ref: before_ref.map(str::to_owned),
            after_ref: after_ref.map(str::to_owned),
            // Assigned atomically from the current head by
            // `append_chained`.
            chain_hash: [0u8; 32],
        };
        self.log.append_chained(&mut event).await
    }
}

/// A stubbed merge-review case.
///
/// The review workflow and its approval state live in the workflow layer;
/// this store only remembers that a review was requested.
#[allow(dead_code)] // Stub store: fields are read by the future review workflow.
#[derive(Debug, Clone)]
struct MergeReview {
    first_id: ContactId,
    second_id: ContactId,
    reason: String,
}

/// In-memory [`ContactService`].
///
/// **Vertical-slice stand-in** — replaced by the SQLx-backed service in
/// task 02-DB.
#[derive(Debug, Clone)]
pub struct MemoryContactService {
    contacts: InMemoryContactRepository,
    audit: MemoryAuditService,
    merge_reviews: Arc<RwLock<HashMap<Uuid, MergeReview>>>,
}

impl MemoryContactService {
    /// Creates the service over the given repository and audit service.
    #[must_use]
    pub fn new(contacts: InMemoryContactRepository, audit: MemoryAuditService) -> Self {
        Self {
            contacts,
            audit,
            merge_reviews: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Borrows the underlying contact repository (read path for the
    /// vertical-slice HTTP layer; later tasks add a dedicated query
    /// service).
    #[must_use]
    pub fn repository(&self) -> &InMemoryContactRepository {
        &self.contacts
    }

    /// Enforces client-profile scope: a scoped context may only touch its
    /// own profile. Grant-based authorization lands in task 03-SEC.
    fn check_scope(ctx: &ActionContext, profile: ClientProfileId) -> Result<(), DomainError> {
        if let Some(scope) = ctx.client_profile_id {
            if scope != profile {
                return Err(DomainError::Forbidden(
                    "operation is outside the request's client-profile scope".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

impl ContactService for MemoryContactService {
    async fn create_contact(
        &self,
        input: NewContact,
        ctx: &ActionContext,
    ) -> Result<Contact, DomainError> {
        Self::check_scope(ctx, input.client_profile_id)?;

        let emails = normalize_list(input.emails, normalize_email);
        let phones = normalize_list(input.phones, normalize_phone);
        if emails.is_empty() && phones.is_empty() {
            return Err(DomainError::Validation(
                "at least one email or phone number is required".to_owned(),
            ));
        }
        for email in &emails {
            let duplicates = self
                .contacts
                .find_by_normalized_email(input.client_profile_id, email)
                .await?;
            if !duplicates.is_empty() {
                // The address itself is not echoed: no PII in errors.
                return Err(DomainError::Conflict(
                    "a contact with this email already exists in the client profile".to_owned(),
                ));
            }
        }

        let contact = Contact {
            contact_id: ContactId::new(),
            client_profile_id: input.client_profile_id,
            normalized_emails: emails,
            normalized_phones: phones,
            lifecycle: Lifecycle::Prospect,
            first_source: input.source.clone(),
            latest_source: input.source.clone(),
            source_touches: vec![input.source],
            consent_records: vec![],
            merged_into: None,
        };
        let contact = self.contacts.create(contact).await?;
        // Best effort ordering for the stand-in: the transactional
        // outbox in 02-DB makes create + audit atomic.
        self.audit
            .record_action(
                ctx,
                "contact.create",
                "contact",
                &contact.contact_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                Some(&contact.contact_id.to_string()),
            )
            .await?;
        Ok(contact)
    }

    async fn transition_lifecycle(
        &self,
        contact_id: ContactId,
        next: Lifecycle,
        ctx: &ActionContext,
    ) -> Result<Contact, DomainError> {
        let mut contact = self.contacts.get(contact_id).await?;
        Self::check_scope(ctx, contact.client_profile_id)?;
        let before = format!("{:?}", contact.lifecycle);
        contact.transition_to(next)?;
        let contact = self.contacts.update(contact).await?;
        self.audit
            .record_action(
                ctx,
                "contact.transition",
                "contact",
                &contact.contact_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                Some(&before),
                Some(&format!("{:?}", contact.lifecycle)),
            )
            .await?;
        Ok(contact)
    }

    async fn merge_contacts(
        &self,
        survivor_id: ContactId,
        duplicate_id: ContactId,
        ctx: &ActionContext,
    ) -> Result<Contact, DomainError> {
        if survivor_id == duplicate_id {
            return Err(DomainError::Validation(
                "cannot merge a contact into itself".to_owned(),
            ));
        }
        let survivor = self.contacts.get(survivor_id).await?;
        let mut duplicate = self.contacts.get(duplicate_id).await?;
        Self::check_scope(ctx, survivor.client_profile_id)?;
        Self::check_scope(ctx, duplicate.client_profile_id)?;
        if survivor.client_profile_id != duplicate.client_profile_id {
            return Err(DomainError::Forbidden(
                "cannot merge contacts across client profiles".to_owned(),
            ));
        }
        // Archiving is idempotent: re-merging an already-archived
        // duplicate keeps it archived and re-points the merge target.
        duplicate.transition_to(Lifecycle::Archived)?;
        duplicate.merged_into = Some(survivor_id);
        let duplicate = self.contacts.update(duplicate).await?;
        self.audit
            .record_action(
                ctx,
                "contact.merge",
                "contact",
                &duplicate.contact_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                Some(&survivor_id.to_string()),
            )
            .await?;
        Ok(duplicate)
    }

    async fn request_merge_review(
        &self,
        first_id: ContactId,
        second_id: ContactId,
        reason: String,
        ctx: &ActionContext,
    ) -> Result<Uuid, DomainError> {
        let first = self.contacts.get(first_id).await?;
        let second = self.contacts.get(second_id).await?;
        Self::check_scope(ctx, first.client_profile_id)?;
        Self::check_scope(ctx, second.client_profile_id)?;
        if reason.trim().is_empty() {
            return Err(DomainError::Validation(
                "a merge-review reason is required".to_owned(),
            ));
        }
        let review_id = Uuid::new_v4();
        self.merge_reviews.write().await.insert(
            review_id,
            MergeReview {
                first_id,
                second_id,
                reason,
            },
        );
        self.audit
            .record_action(
                ctx,
                "contact.merge_review_requested",
                "contact",
                &first_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Pending,
                None,
                Some(&review_id.to_string()),
            )
            .await?;
        Ok(review_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crm_domain::audit::verify_chain;

    fn ctx_for(profile: ClientProfileId) -> ActionContext {
        api_action_context(Some(profile), Uuid::new_v4())
    }

    fn input_for(profile: ClientProfileId) -> NewContact {
        NewContact {
            client_profile_id: profile,
            emails: vec![" A@Example.com ".to_owned(), "a@example.com".to_owned()],
            phones: vec![" +1-555-0100 ".to_owned(), "+1-555-0100".to_owned()],
            source: "web-form".to_owned(),
        }
    }

    fn service() -> MemoryContactService {
        MemoryContactService::new(
            InMemoryContactRepository::new(),
            MemoryAuditService::new(InMemoryAuditRepository::new()),
        )
    }

    #[tokio::test]
    async fn create_normalizes_dedupes_and_starts_as_prospect() {
        let service = service();
        let profile = ClientProfileId::new();
        let contact = service
            .create_contact(input_for(profile), &ctx_for(profile))
            .await
            .expect("create");

        assert_eq!(contact.normalized_emails, vec!["a@example.com"]);
        assert_eq!(contact.normalized_phones, vec!["+1-555-0100"]);
        assert_eq!(contact.lifecycle, Lifecycle::Prospect);
        assert_eq!(contact.first_source, "web-form");
        assert_eq!(contact.source_touches, vec!["web-form"]);
        assert_eq!(contact.merged_into, None);
    }

    #[tokio::test]
    async fn create_rejects_duplicate_email_with_conflict() {
        let service = service();
        let profile = ClientProfileId::new();
        service
            .create_contact(input_for(profile), &ctx_for(profile))
            .await
            .expect("first create");

        let mut second = input_for(profile);
        second.phones = vec![];
        let err = service
            .create_contact(second, &ctx_for(profile))
            .await
            .expect_err("duplicate email must conflict");
        assert!(matches!(err, DomainError::Conflict(_)));
        // No PII in the message.
        assert!(!err.to_string().contains("a@example.com"));
    }

    #[tokio::test]
    async fn create_rejects_empty_identity() {
        let service = service();
        let profile = ClientProfileId::new();
        let mut input = input_for(profile);
        input.emails = vec!["   ".to_owned()];
        input.phones = vec![];
        let err = service
            .create_contact(input, &ctx_for(profile))
            .await
            .expect_err("empty identity must fail validation");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[tokio::test]
    async fn create_rejects_out_of_scope_context() {
        let service = service();
        let err = service
            .create_contact(
                input_for(ClientProfileId::new()),
                &ctx_for(ClientProfileId::new()),
            )
            .await
            .expect_err("cross-scope create must be forbidden");
        assert!(matches!(err, DomainError::Forbidden(_)));
    }

    #[tokio::test]
    async fn transition_delegates_to_contact_state_machine() {
        let service = service();
        let profile = ClientProfileId::new();
        let contact = service
            .create_contact(input_for(profile), &ctx_for(profile))
            .await
            .expect("create");

        let advanced = service
            .transition_lifecycle(contact.contact_id, Lifecycle::Qualified, &ctx_for(profile))
            .await
            .expect("prospect -> qualified");
        assert_eq!(advanced.lifecycle, Lifecycle::Qualified);

        // Skipping a stage is rejected: a *fresh* prospect cannot jump
        // straight to Customer.
        let mut fresh_input = input_for(profile);
        fresh_input.emails = vec!["fresh@example.com".to_owned()];
        let fresh = service
            .create_contact(fresh_input, &ctx_for(profile))
            .await
            .expect("create");
        let err = service
            .transition_lifecycle(fresh.contact_id, Lifecycle::Customer, &ctx_for(profile))
            .await
            .expect_err("skipping a stage must fail");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[tokio::test]
    async fn merge_archives_duplicate_and_points_at_survivor() {
        let service = service();
        let profile = ClientProfileId::new();
        let ctx = ctx_for(profile);
        let survivor = service
            .create_contact(input_for(profile), &ctx)
            .await
            .expect("survivor");
        let mut dup_input = input_for(profile);
        dup_input.emails = vec!["other@example.com".to_owned()];
        let duplicate = service
            .create_contact(dup_input, &ctx)
            .await
            .expect("duplicate");

        let merged = service
            .merge_contacts(survivor.contact_id, duplicate.contact_id, &ctx)
            .await
            .expect("merge");
        assert_eq!(merged.lifecycle, Lifecycle::Archived);
        assert_eq!(merged.merged_into, Some(survivor.contact_id));
    }

    #[tokio::test]
    async fn merge_rejects_cross_profile_pair() {
        let service = service();
        let profile_a = ClientProfileId::new();
        let profile_b = ClientProfileId::new();
        // Unscoped context so the per-call scope check passes and the
        // cross-profile rule is what fires.
        let ctx = api_action_context(None, Uuid::new_v4());
        let a = service
            .create_contact(input_for(profile_a), &ctx)
            .await
            .expect("a");
        let b = service
            .create_contact(input_for(profile_b), &ctx)
            .await
            .expect("b");

        let err = service
            .merge_contacts(a.contact_id, b.contact_id, &ctx)
            .await
            .expect_err("cross-profile merge must be forbidden");
        assert!(matches!(err, DomainError::Forbidden(_)));
    }

    #[tokio::test]
    async fn request_merge_review_returns_fresh_ids() {
        let service = service();
        let profile = ClientProfileId::new();
        let ctx = ctx_for(profile);
        let a = service
            .create_contact(input_for(profile), &ctx)
            .await
            .expect("a");
        let mut b_input = input_for(profile);
        b_input.emails = vec!["other@example.com".to_owned()];
        let b = service.create_contact(b_input, &ctx).await.expect("b");

        let first = service
            .request_merge_review(a.contact_id, b.contact_id, "similar names".to_owned(), &ctx)
            .await
            .expect("review");
        let second = service
            .request_merge_review(a.contact_id, b.contact_id, "similar names".to_owned(), &ctx)
            .await
            .expect("review");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn recorded_actions_form_a_verifiable_chain() {
        let audit_repo = InMemoryAuditRepository::new();
        let audit = MemoryAuditService::new(audit_repo.clone());
        let profile = ClientProfileId::new();
        let ctx = ctx_for(profile);

        for action in ["contact.create", "contact.transition"] {
            audit
                .record_action(
                    &ctx,
                    action,
                    "contact",
                    "contact-1",
                    PermissionDecision::Allow,
                    ActionResult::Success,
                    None,
                    None,
                )
                .await
                .expect("record");
        }

        let events = audit_repo.list_for_client(profile, 10).await.expect("list");
        assert_eq!(events.len(), 2);
        // Newest first; reverse for chain verification.
        let mut chronological = events;
        chronological.reverse();
        assert!(verify_chain(&chronological));

        let head = audit_repo
            .latest_chain_hash()
            .await
            .expect("head")
            .expect("non-empty log");
        assert_eq!(head, chronological.last().expect("last").chain_hash);
    }

    #[tokio::test]
    async fn append_rejects_a_broken_chain_link() {
        let repo = InMemoryAuditRepository::new();
        let profile = ClientProfileId::new();
        let mut event = AuditEvent {
            event_id: AuditEventId::new(),
            occurred_at: Utc::now(),
            actor_kind: ActorKind::System,
            actor_id: ActorId::from(Uuid::nil()),
            client_profile_id: Some(profile),
            action: "contact.create".to_owned(),
            target_kind: "contact".to_owned(),
            target_id: "contact-1".to_owned(),
            correlation_id: Uuid::new_v4(),
            permission_decision: PermissionDecision::Allow,
            result: ActionResult::Success,
            before_ref: None,
            after_ref: None,
            // Deliberately wrong: not linked from genesis.
            chain_hash: [9u8; 32],
        };
        let err = repo
            .append(event.clone())
            .await
            .expect_err("broken link must be rejected");
        assert!(matches!(err, DomainError::ConcurrencyConflict));

        // The correctly linked event appends fine.
        repo.append_chained(&mut event).await.expect("append");
        assert_eq!(
            repo.latest_chain_hash()
                .await
                .expect("head")
                .expect("non-empty"),
            event.chain_hash
        );
    }

    #[tokio::test]
    async fn audit_list_is_scoped_to_client_profile() {
        let repo = InMemoryAuditRepository::new();
        let audit = MemoryAuditService::new(repo.clone());
        let profile_a = ClientProfileId::new();
        let profile_b = ClientProfileId::new();

        audit
            .record_action(
                &ctx_for(profile_a),
                "contact.create",
                "contact",
                "c-a",
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await
            .expect("record a");
        audit
            .record_action(
                &ctx_for(profile_b),
                "contact.create",
                "contact",
                "c-b",
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await
            .expect("record b");

        let events = repo.list_for_client(profile_a, 10).await.expect("list");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target_id, "c-a");
    }
}
