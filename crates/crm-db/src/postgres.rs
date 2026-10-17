// SPDX-License-Identifier: GPL-3.0-only
//! SQLx/PostgreSQL implementations of the `crm-domain` repository traits.
//!
//! [`PgContactRepository`] and [`PgAuditRepository`] persist contacts and
//! audit events against the schema in `migrations/0001_foundation.sql`.
//! All queries use the dynamic `sqlx::query` form (no `query!` macros), so
//! this crate compiles with no database available; column types are
//! checked at runtime.
//!
//! ## Audit chain writes
//!
//! [`AuditRepository::append`](crm_domain::repositories::AuditRepository::append)
//! serializes writers with a transaction-scoped advisory lock, reads the
//! current chain head (ordered by the `seq` column, which gives a total
//! append order), and verifies the supplied event's `chain_hash` with
//! [`crm_domain::audit::compute_chain_hash`], rejecting mismatches with
//! [`DomainError::ConcurrencyConflict`] — the same contract as the
//! in-memory implementation this replaces.
//!
//! ## Error mapping
//!
//! `crm-domain`'s [`DomainError`] has no dedicated infrastructure
//! variant, so storage failures are mapped: unique violations (SQLSTATE
//! 23505) become [`DomainError::Conflict`], missing rows become
//! [`DomainError::NotFound`], and every other storage failure becomes
//! [`DomainError::Validation`] with a `storage error:` prefix so it is
//! never silently swallowed. If `crm-domain` ever gains an
//! infrastructure variant, this mapping should move to it.

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use crm_domain::audit::{
    compute_chain_hash, ActionResult, ActorKind, AuditEvent, PermissionDecision, GENESIS_CHAIN_HASH,
};
use crm_domain::contact::{ConsentRecord, Contact, Lifecycle};
use crm_domain::error::DomainError;
use crm_domain::ids::{
    ActorId, AgencyId, AuditEventId, ClientProfileId, ContactId, DelegationId, GrantId, SessionId,
    StaffId,
};
use crm_domain::repositories::{
    AuditRepository, ContactRepository, GrantRepository, SessionRepository, StaffRepository,
};
use crm_domain::security::{
    Action, Grant, Operation, PasswordCredential, Resource, Scope, Session, StaffStatus, StaffUser,
};
use crm_domain::services::{ActionContext, AuditService};

use crm_core::memory::{BackendHealth, ContactReadModel};
use crm_core::IdempotencyKey;

/// SQLSTATE for unique-violation (duplicate key) errors.
const UNIQUE_VIOLATION: &str = "23505";

/// Maps storage failures onto [`DomainError`].
///
/// See the module docs for the mapping contract.
pub(crate) fn map_storage_error(error: sqlx::Error) -> DomainError {
    if let sqlx::Error::Database(db_error) = &error {
        if db_error.code().as_deref() == Some(UNIQUE_VIOLATION) {
            return DomainError::Conflict(format!("storage conflict: {}", db_error.message()));
        }
    }
    DomainError::Validation(format!("storage error: {error}"))
}

/// Ensures the tenant row exists for `client_profile_id`.
///
/// The vertical slice creates contacts under caller-supplied profile ids
/// (the agency-managed profile lifecycle lands in a later task), so the
/// repositories satisfy the `contacts`/`audit_events` foreign keys with an
/// idempotent upsert instead of failing on unknown profiles.
pub(crate) async fn ensure_client_profile<'e, E>(
    executor: E,
    client_profile_id: ClientProfileId,
) -> Result<(), DomainError>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        "INSERT INTO client_profiles (client_profile_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(client_profile_id.as_uuid())
    .execute(executor)
    .await
    .map_err(map_storage_error)?;
    Ok(())
}

/// Renders a [`Lifecycle`] as its stable database string.
fn lifecycle_as_str(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Prospect => "Prospect",
        Lifecycle::Qualified => "Qualified",
        Lifecycle::Customer => "Customer",
        Lifecycle::Archived => "Archived",
    }
}

/// Parses a [`Lifecycle`] from its database string.
fn lifecycle_from_str(raw: &str) -> Result<Lifecycle, DomainError> {
    match raw {
        "Prospect" => Ok(Lifecycle::Prospect),
        "Qualified" => Ok(Lifecycle::Qualified),
        "Customer" => Ok(Lifecycle::Customer),
        "Archived" => Ok(Lifecycle::Archived),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown lifecycle '{other}'"
        ))),
    }
}

/// Renders an [`ActorKind`] as its stable database string.
///
/// Mirrors the strings used in
/// [`AuditEvent::canonical_bytes`](crm_domain::audit::AuditEvent::canonical_bytes).
pub(crate) fn actor_kind_as_str(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

/// Parses an [`ActorKind`] from its database string.
pub(crate) fn actor_kind_from_str(raw: &str) -> Result<ActorKind, DomainError> {
    match raw {
        "human" => Ok(ActorKind::Human),
        "agent" => Ok(ActorKind::Agent),
        "system" => Ok(ActorKind::System),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown actor kind '{other}'"
        ))),
    }
}

/// Renders a [`PermissionDecision`] as its stable database string.
pub(crate) fn permission_decision_as_str(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Allow => "allow",
        PermissionDecision::Deny => "deny",
    }
}

/// Parses a [`PermissionDecision`] from its database string.
pub(crate) fn permission_decision_from_str(raw: &str) -> Result<PermissionDecision, DomainError> {
    match raw {
        "allow" => Ok(PermissionDecision::Allow),
        "deny" => Ok(PermissionDecision::Deny),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown permission decision '{other}'"
        ))),
    }
}

/// Renders an [`ActionResult`] as its stable database string.
pub(crate) fn action_result_as_str(result: ActionResult) -> &'static str {
    match result {
        ActionResult::Success => "success",
        ActionResult::Failure => "failure",
        ActionResult::Pending => "pending",
    }
}

/// Parses an [`ActionResult`] from its database string.
pub(crate) fn action_result_from_str(raw: &str) -> Result<ActionResult, DomainError> {
    match raw {
        "success" => Ok(ActionResult::Success),
        "failure" => Ok(ActionResult::Failure),
        "pending" => Ok(ActionResult::Pending),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown action result '{other}'"
        ))),
    }
}

/// Converts a stored 32-byte hash into a fixed array.
pub(crate) fn hash_from_bytes(raw: Vec<u8>) -> Result<[u8; 32], DomainError> {
    raw.try_into()
        .map_err(|_| DomainError::Validation("storage error: corrupt chain hash length".to_owned()))
}

/// Row shape of the `contacts` table.
#[derive(Debug, sqlx::FromRow)]
struct ContactRow {
    contact_id: Uuid,
    client_profile_id: Uuid,
    normalized_emails: Vec<String>,
    normalized_phones: Vec<String>,
    lifecycle: String,
    first_source: String,
    latest_source: String,
    source_touches: Vec<String>,
    consent_records: JsonValue,
    merged_into: Option<Uuid>,
}

impl ContactRow {
    /// Converts the row into a domain [`Contact`].
    fn into_contact(self) -> Result<Contact, DomainError> {
        let consent_records: Vec<ConsentRecord> = serde_json::from_value(self.consent_records)
            .map_err(|error| {
                DomainError::Validation(format!("storage error: corrupt consent_records: {error}"))
            })?;
        Ok(Contact {
            contact_id: ContactId::from(self.contact_id),
            client_profile_id: ClientProfileId::from(self.client_profile_id),
            normalized_emails: self.normalized_emails,
            normalized_phones: self.normalized_phones,
            lifecycle: lifecycle_from_str(&self.lifecycle)?,
            first_source: self.first_source,
            latest_source: self.latest_source,
            source_touches: self.source_touches,
            consent_records,
            merged_into: self.merged_into.map(ContactId::from),
        })
    }
}

/// Columns read for every contact query; kept in one place so `SELECT`
/// lists cannot drift between methods.
const CONTACT_COLUMNS: &str = "contact_id, client_profile_id, normalized_emails, normalized_phones, lifecycle, first_source, latest_source, source_touches, consent_records, merged_into";

/// Row shape of the `audit_events` table.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct AuditEventRow {
    event_id: Uuid,
    occurred_at: DateTime<Utc>,
    actor_kind: String,
    actor_id: Uuid,
    client_profile_id: Option<Uuid>,
    action: String,
    target_kind: String,
    target_id: String,
    correlation_id: Uuid,
    permission_decision: String,
    result: String,
    before_ref: Option<String>,
    after_ref: Option<String>,
    chain_hash: Vec<u8>,
}

impl AuditEventRow {
    /// Converts the row into a domain [`AuditEvent`].
    pub(crate) fn into_event(self) -> Result<AuditEvent, DomainError> {
        Ok(AuditEvent {
            event_id: AuditEventId::from(self.event_id),
            occurred_at: self.occurred_at,
            actor_kind: actor_kind_from_str(&self.actor_kind)?,
            actor_id: ActorId::from(self.actor_id),
            client_profile_id: self.client_profile_id.map(ClientProfileId::from),
            action: self.action,
            target_kind: self.target_kind,
            target_id: self.target_id,
            correlation_id: self.correlation_id,
            permission_decision: permission_decision_from_str(&self.permission_decision)?,
            result: action_result_from_str(&self.result)?,
            before_ref: self.before_ref,
            after_ref: self.after_ref,
            chain_hash: hash_from_bytes(self.chain_hash)?,
        })
    }
}

/// Columns read for every audit-event query.
pub(crate) const AUDIT_EVENT_COLUMNS: &str = "event_id, occurred_at, actor_kind, actor_id, client_profile_id, action, target_kind, target_id, correlation_id, permission_decision, result, before_ref, after_ref, chain_hash";

/// SQLx implementation of [`ContactRepository`].
#[derive(Debug, Clone)]
pub struct PgContactRepository {
    pool: PgPool,
}

impl PgContactRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Counts contacts owned by one client profile.
    ///
    /// Read-model helper for the vertical-slice list endpoint's `total`;
    /// see [`ContactReadModel`](crm_core::memory::ContactReadModel).
    pub async fn count_for_client(
        &self,
        client_profile_id: ClientProfileId,
    ) -> Result<usize, DomainError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE client_profile_id = $1")
                .bind(client_profile_id.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(map_storage_error)?;
        usize::try_from(count).map_err(|_| {
            DomainError::Validation(format!("storage error: absurd contact count {count}"))
        })
    }
}

impl ContactReadModel for PgContactRepository {
    async fn count_for_client(
        &self,
        client_profile_id: ClientProfileId,
    ) -> Result<usize, DomainError> {
        PgContactRepository::count_for_client(self, client_profile_id).await
    }
}

impl BackendHealth for PgContactRepository {
    async fn is_healthy(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }
}

impl ContactRepository for PgContactRepository {
    async fn create(&self, contact: Contact) -> Result<Contact, DomainError> {
        // The vertical slice addresses profiles by caller-supplied id;
        // satisfy the foreign key before inserting the contact.
        ensure_client_profile(&self.pool, contact.client_profile_id).await?;
        let consent_records = serde_json::to_value(&contact.consent_records).map_err(|error| {
            DomainError::Validation(format!(
                "storage error: cannot encode consent_records: {error}"
            ))
        })?;
        sqlx::query(
            "INSERT INTO contacts (contact_id, client_profile_id, normalized_emails, normalized_phones, lifecycle, first_source, latest_source, source_touches, consent_records, merged_into)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(contact.contact_id.as_uuid())
        .bind(contact.client_profile_id.as_uuid())
        .bind(&contact.normalized_emails)
        .bind(&contact.normalized_phones)
        .bind(lifecycle_as_str(contact.lifecycle))
        .bind(&contact.first_source)
        .bind(&contact.latest_source)
        .bind(&contact.source_touches)
        .bind(&consent_records)
        .bind(contact.merged_into.map(ContactId::as_uuid))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(contact)
    }

    async fn get(&self, contact_id: ContactId) -> Result<Contact, DomainError> {
        let row: Option<ContactRow> = sqlx::query_as(&format!(
            "SELECT {CONTACT_COLUMNS} FROM contacts WHERE contact_id = $1"
        ))
        .bind(contact_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row.into_contact(),
            None => Err(DomainError::NotFound {
                kind: "contact".to_owned(),
                id: contact_id.to_string(),
            }),
        }
    }

    async fn update(&self, contact: Contact) -> Result<Contact, DomainError> {
        let consent_records = serde_json::to_value(&contact.consent_records).map_err(|error| {
            DomainError::Validation(format!(
                "storage error: cannot encode consent_records: {error}"
            ))
        })?;
        let result = sqlx::query(
            "UPDATE contacts
             SET client_profile_id = $2, normalized_emails = $3, normalized_phones = $4, lifecycle = $5,
                 first_source = $6, latest_source = $7, source_touches = $8, consent_records = $9,
                 merged_into = $10, updated_at = now()
             WHERE contact_id = $1",
        )
        .bind(contact.contact_id.as_uuid())
        .bind(contact.client_profile_id.as_uuid())
        .bind(&contact.normalized_emails)
        .bind(&contact.normalized_phones)
        .bind(lifecycle_as_str(contact.lifecycle))
        .bind(&contact.first_source)
        .bind(&contact.latest_source)
        .bind(&contact.source_touches)
        .bind(&consent_records)
        .bind(contact.merged_into.map(ContactId::as_uuid))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "contact".to_owned(),
                id: contact.contact_id.to_string(),
            });
        }
        Ok(contact)
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Contact>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows: Vec<ContactRow> = sqlx::query_as(&format!(
            "SELECT {CONTACT_COLUMNS} FROM contacts
             WHERE client_profile_id = $1
             ORDER BY created_at ASC, contact_id ASC
             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.into_iter().map(ContactRow::into_contact).collect()
    }

    async fn find_by_normalized_email(
        &self,
        client_profile_id: ClientProfileId,
        email: &str,
    ) -> Result<Vec<Contact>, DomainError> {
        let rows: Vec<ContactRow> = sqlx::query_as(&format!(
            "SELECT {CONTACT_COLUMNS} FROM contacts
             WHERE client_profile_id = $1 AND $2 = ANY (normalized_emails)
             ORDER BY created_at ASC, contact_id ASC"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(email)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.into_iter().map(ContactRow::into_contact).collect()
    }
}

/// SQLx implementation of [`AuditRepository`].
#[derive(Debug, Clone)]
pub struct PgAuditRepository {
    pool: PgPool,
}

impl PgAuditRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl AuditRepository for PgAuditRepository {
    async fn append(&self, event: AuditEvent) -> Result<AuditEvent, DomainError> {
        let mut tx = self.pool.begin().await.map_err(map_storage_error)?;
        // Serialize audit writers: the second of two racing appends blocks
        // here, then reads the first's committed head and fails the
        // chain verification below with ConcurrencyConflict instead of
        // forking the log.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('crm_audit_head')::bigint)")
            .execute(&mut *tx)
            .await
            .map_err(map_storage_error)?;
        let head: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT chain_hash FROM audit_events ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_storage_error)?;
        let prev: [u8; 32] = match head {
            Some(bytes) => hash_from_bytes(bytes)?,
            None => GENESIS_CHAIN_HASH,
        };
        let expected = compute_chain_hash(&prev, &event.canonical_bytes());
        if event.chain_hash != expected {
            return Err(DomainError::ConcurrencyConflict);
        }
        // The vertical slice addresses profiles by caller-supplied id;
        // satisfy the foreign key before inserting the event.
        if let Some(profile_id) = event.client_profile_id {
            ensure_client_profile(&mut *tx, profile_id).await?;
        }
        sqlx::query(
            "INSERT INTO audit_events
             (event_id, occurred_at, actor_kind, actor_id, client_profile_id, action, target_kind, target_id, correlation_id, permission_decision, result, before_ref, after_ref, prev_hash, chain_hash)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(event.event_id.as_uuid())
        .bind(event.occurred_at)
        .bind(actor_kind_as_str(event.actor_kind))
        .bind(event.actor_id.as_uuid())
        .bind(event.client_profile_id.map(ClientProfileId::as_uuid))
        .bind(&event.action)
        .bind(&event.target_kind)
        .bind(&event.target_id)
        .bind(event.correlation_id)
        .bind(permission_decision_as_str(event.permission_decision))
        .bind(action_result_as_str(event.result))
        .bind(&event.before_ref)
        .bind(&event.after_ref)
        .bind(prev.to_vec())
        .bind(event.chain_hash.to_vec())
        .execute(&mut *tx)
        .await
        .map_err(map_storage_error)?;
        tx.commit().await.map_err(map_storage_error)?;
        Ok(event)
    }

    async fn latest_chain_hash(&self) -> Result<Option<[u8; 32]>, DomainError> {
        let head: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT chain_hash FROM audit_events ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
        head.map(hash_from_bytes).transpose()
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<AuditEvent>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows: Vec<AuditEventRow> = sqlx::query_as(&format!(
            "SELECT {AUDIT_EVENT_COLUMNS} FROM audit_events
             WHERE client_profile_id = $1
             ORDER BY occurred_at DESC, seq DESC
             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.into_iter().map(AuditEventRow::into_event).collect()
    }

    async fn get(&self, event_id: AuditEventId) -> Result<AuditEvent, DomainError> {
        let row: Option<AuditEventRow> = sqlx::query_as(&format!(
            "SELECT {AUDIT_EVENT_COLUMNS} FROM audit_events WHERE event_id = $1"
        ))
        .bind(event_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row.into_event(),
            None => Err(DomainError::NotFound {
                kind: "audit_event".to_owned(),
                id: event_id.to_string(),
            }),
        }
    }
}

/// SQLx implementation of [`AuditService`].
///
/// Builds the [`AuditEvent`] from its parts, links it to the current log
/// head, and appends. Two writers racing on the head serialize inside
/// [`AuditRepository::append`](PgAuditRepository); the loser gets
/// [`DomainError::ConcurrencyConflict`] and retries with the new head, so
/// the log never forks. Retries are bounded — a persistently contended log
/// surfaces the last conflict instead of spinning forever.
#[derive(Debug, Clone)]
pub struct PgAuditService {
    log: PgAuditRepository,
}

/// Maximum head-link retries for [`PgAuditService::record_action`] before
/// the last [`DomainError::ConcurrencyConflict`] is returned.
const AUDIT_APPEND_RETRIES: usize = 8;

impl PgAuditService {
    /// Creates the service over the given audit repository.
    pub fn new(log: PgAuditRepository) -> Self {
        Self { log }
    }
}

impl AuditService for PgAuditService {
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
            // Linked to the current head on every attempt below.
            chain_hash: [0u8; 32],
        };
        let mut attempts = 0;
        loop {
            let prev = self
                .log
                .latest_chain_hash()
                .await?
                .unwrap_or(GENESIS_CHAIN_HASH);
            event.chain_hash = compute_chain_hash(&prev, &event.canonical_bytes());
            match self.log.append(event.clone()).await {
                Ok(stored) => return Ok(stored),
                Err(DomainError::ConcurrencyConflict) if attempts < AUDIT_APPEND_RETRIES => {
                    attempts += 1;
                }
                Err(other) => return Err(other),
            }
        }
    }
}

/// Persistent store for idempotent HTTP responses.
///
/// Maps an [`IdempotencyKey`] to the request fingerprint and response body
/// recorded for it, so `Idempotency-Key` replays survive restarts (the
/// 01-FND-02 vertical slice kept these in process memory). Backed by the
/// `idempotency_keys` table from migration `0002`.
#[derive(Debug, Clone)]
pub struct PgIdempotencyStore {
    pool: PgPool,
}

impl PgIdempotencyStore {
    /// Creates the store backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns the stored `(fingerprint, response body)` for `key`, or
    /// `None` for a fresh key.
    pub async fn get(&self, key: &IdempotencyKey) -> Result<Option<(u64, JsonValue)>, DomainError> {
        let row: Option<(i64, JsonValue)> = sqlx::query_as(
            "SELECT fingerprint, response FROM idempotency_keys WHERE idempotency_key = $1",
        )
        .bind(key.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        // `as` casts between u64/i64 are bit-preserving round-trips.
        Ok(row.map(|(fingerprint, body)| (fingerprint as u64, body)))
    }

    /// Stores `(fingerprint, response body)` for `key`, replacing any
    /// previous entry for the key.
    pub async fn put(
        &self,
        key: &IdempotencyKey,
        fingerprint: u64,
        body: &JsonValue,
    ) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO idempotency_keys (idempotency_key, fingerprint, response)
             VALUES ($1, $2, $3)
             ON CONFLICT (idempotency_key) DO UPDATE
             SET fingerprint = EXCLUDED.fingerprint, response = EXCLUDED.response",
        )
        .bind(key.as_uuid())
        .bind(fingerprint as i64)
        .bind(body)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(())
    }
}

/// A domain event staged in the transactional outbox.
///
/// The outbox relay (a later task) publishes rows with
/// `published_at IS NULL` — see the `outbox_unpublished_idx` partial
/// index — and stamps `published_at` once the event is delivered.
#[derive(Debug, Clone)]
pub struct OutboxMessage {
    /// Stable id of the staged message.
    pub id: Uuid,
    /// Aggregate kind, e.g. `"contact"`.
    pub aggregate_type: String,
    /// Id of the aggregate the event belongs to.
    pub aggregate_id: Uuid,
    /// Event name, e.g. `"contact.created"`.
    pub event_type: String,
    /// Event payload as JSON.
    pub payload: JsonValue,
}

/// Stages `message` in the `outbox` table using `executor`.
///
/// Pass the caller's transaction (`&mut *tx`) so the insert commits
/// atomically with the domain writes it describes, or `&pool` for a
/// standalone stage. Duplicate ids surface as [`DomainError::Conflict`].
pub async fn publish_outbox<'e, E>(executor: E, message: &OutboxMessage) -> Result<(), DomainError>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        "INSERT INTO outbox (id, aggregate_type, aggregate_id, event_type, payload)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(message.id)
    .bind(&message.aggregate_type)
    .bind(message.aggregate_id)
    .bind(&message.event_type)
    .bind(&message.payload)
    .execute(executor)
    .await
    .map_err(map_storage_error)?;
    Ok(())
}
// ---------------------------------------------------------------------------
// Identity and RBAC repositories (task 03-SEC-02)
// ---------------------------------------------------------------------------

/// Renders a [`Resource`] as its stable database string.
fn resource_as_str(resource: Resource) -> &'static str {
    match resource {
        Resource::Contact => "contact",
        Resource::Contract => "contract",
        Resource::Campaign => "campaign",
        Resource::AgentAction => "agent_action",
        Resource::AuditLog => "audit_log",
        Resource::ClientProfile => "client_profile",
        Resource::StaffUser => "staff_user",
        Resource::Session => "session",
        Resource::Asset => "asset",
        Resource::Email => "email",
        Resource::EmailThread => "email_thread",
        Resource::AgentRun => "agent_run",
    }
}

/// Parses a [`Resource`] from its database string.
fn resource_from_str(raw: &str) -> Result<Resource, DomainError> {
    match raw {
        "contact" => Ok(Resource::Contact),
        "contract" => Ok(Resource::Contract),
        "campaign" => Ok(Resource::Campaign),
        "agent_action" => Ok(Resource::AgentAction),
        "audit_log" => Ok(Resource::AuditLog),
        "client_profile" => Ok(Resource::ClientProfile),
        "staff_user" => Ok(Resource::StaffUser),
        "session" => Ok(Resource::Session),
        "asset" => Ok(Resource::Asset),
        "email" => Ok(Resource::Email),
        "email_thread" => Ok(Resource::EmailThread),
        "agent_run" => Ok(Resource::AgentRun),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown resource '{other}'"
        ))),
    }
}

/// Renders an [`Operation`] as its stable database string.
fn operation_as_str(operation: Operation) -> &'static str {
    match operation {
        Operation::Read => "read",
        Operation::Create => "create",
        Operation::Update => "update",
        Operation::Delete => "delete",
        Operation::Approve => "approve",
        Operation::Manage => "manage",
    }
}

/// Parses an [`Operation`] from its database string.
fn operation_from_str(raw: &str) -> Result<Operation, DomainError> {
    match raw {
        "read" => Ok(Operation::Read),
        "create" => Ok(Operation::Create),
        "update" => Ok(Operation::Update),
        "delete" => Ok(Operation::Delete),
        "approve" => Ok(Operation::Approve),
        "manage" => Ok(Operation::Manage),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown operation '{other}'"
        ))),
    }
}

/// Renders a [`StaffStatus`] as its stable database string.
fn staff_status_as_str(status: StaffStatus) -> &'static str {
    match status {
        StaffStatus::Active => "active",
        StaffStatus::Suspended => "suspended",
    }
}

/// Parses a [`StaffStatus`] from its database string.
fn staff_status_from_str(raw: &str) -> Result<StaffStatus, DomainError> {
    match raw {
        "active" => Ok(StaffStatus::Active),
        "suspended" => Ok(StaffStatus::Suspended),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown staff status '{other}'"
        ))),
    }
}

/// Row shape of the `staff_users` table.
#[derive(Debug, sqlx::FromRow)]
struct StaffRow {
    staff_id: Uuid,
    agency_id: Uuid,
    email: String,
    display_name: String,
    status: String,
    phc_hash: Option<String>,
    credential_updated_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl StaffRow {
    /// Converts the row into a domain [`StaffUser`].
    fn into_staff_user(self) -> Result<StaffUser, DomainError> {
        let credential = match (self.phc_hash, self.credential_updated_at) {
            (Some(phc_hash), Some(updated_at)) => {
                Some(PasswordCredential::new(phc_hash, updated_at)?)
            }
            // No credential enrolled, or a partially-written row: treat as
            // unenrolled rather than failing the read. Writes always set
            // both columns together.
            _ => None,
        };
        Ok(StaffUser {
            staff_id: StaffId::from(self.staff_id),
            agency_id: AgencyId::from(self.agency_id),
            email: self.email,
            display_name: self.display_name,
            status: staff_status_from_str(&self.status)?,
            credential,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// Columns read for every staff query; kept in one place so `SELECT`
/// lists cannot drift between methods.
const STAFF_COLUMNS: &str = "staff_id, agency_id, email, display_name, status, phc_hash, credential_updated_at, created_at, updated_at";

/// SQLx implementation of [`StaffRepository`].
///
/// Emails are stored exactly as passed; the service layer normalizes to
/// lowercase before write and lookup, and the `UNIQUE` constraint on
/// `email` enforces one account per address.
#[derive(Debug, Clone)]
pub struct PgStaffRepository {
    pool: PgPool,
}

impl PgStaffRepository {
    /// Creates the repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl StaffRepository for PgStaffRepository {
    async fn get(&self, staff_id: StaffId) -> Result<StaffUser, DomainError> {
        let row: Option<StaffRow> = sqlx::query_as(&format!(
            "SELECT {STAFF_COLUMNS} FROM staff_users WHERE staff_id = $1"
        ))
        .bind(staff_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row.into_staff_user(),
            None => Err(DomainError::NotFound {
                kind: "staff_user".to_owned(),
                id: staff_id.as_uuid().to_string(),
            }),
        }
    }

    async fn find_by_email(&self, email: &str) -> Result<Option<StaffUser>, DomainError> {
        let row: Option<StaffRow> = sqlx::query_as(&format!(
            "SELECT {STAFF_COLUMNS} FROM staff_users WHERE email = $1"
        ))
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(StaffRow::into_staff_user).transpose()
    }

    async fn create(&self, user: StaffUser) -> Result<StaffUser, DomainError> {
        let (phc_hash, credential_updated_at) = match &user.credential {
            Some(credential) => (
                Some(credential.phc_hash().to_owned()),
                Some(credential.updated_at()),
            ),
            None => (None, None),
        };
        sqlx::query(
            "INSERT INTO staff_users
             (staff_id, agency_id, email, display_name, status, phc_hash,
              credential_updated_at, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(user.staff_id.as_uuid())
        .bind(user.agency_id.as_uuid())
        .bind(&user.email)
        .bind(&user.display_name)
        .bind(staff_status_as_str(user.status))
        .bind(phc_hash)
        .bind(credential_updated_at)
        .bind(user.created_at)
        .bind(user.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(user)
    }

    async fn update(&self, user: StaffUser) -> Result<StaffUser, DomainError> {
        let (phc_hash, credential_updated_at) = match &user.credential {
            Some(credential) => (
                Some(credential.phc_hash().to_owned()),
                Some(credential.updated_at()),
            ),
            None => (None, None),
        };
        let updated = sqlx::query(
            "UPDATE staff_users
             SET agency_id = $2, email = $3, display_name = $4, status = $5,
                 phc_hash = $6, credential_updated_at = $7, updated_at = $8
             WHERE staff_id = $1",
        )
        .bind(user.staff_id.as_uuid())
        .bind(user.agency_id.as_uuid())
        .bind(&user.email)
        .bind(&user.display_name)
        .bind(staff_status_as_str(user.status))
        .bind(phc_hash)
        .bind(credential_updated_at)
        .bind(user.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if updated.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "staff_user".to_owned(),
                id: user.staff_id.as_uuid().to_string(),
            });
        }
        Ok(user)
    }
}

/// Row shape of the `sessions` table.
#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    session_id: Uuid,
    staff_id: Uuid,
    token_hash: Vec<u8>,
    delegation_id: Option<Uuid>,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    last_activity_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl SessionRow {
    /// Converts the row into a domain [`Session`].
    fn into_session(self) -> Result<Session, DomainError> {
        let token_hash: [u8; 32] = self.token_hash.try_into().map_err(|_| {
            DomainError::Validation("storage error: corrupt session token_hash".to_owned())
        })?;
        Ok(Session {
            session_id: SessionId::from(self.session_id),
            staff_id: StaffId::from(self.staff_id),
            token_hash,
            delegation_id: self.delegation_id.map(DelegationId::from),
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            last_activity_at: self.last_activity_at,
            revoked_at: self.revoked_at,
        })
    }
}

/// Columns read for every session query; kept in one place so `SELECT`
/// lists cannot drift between methods.
const SESSION_COLUMNS: &str = "session_id, staff_id, token_hash, delegation_id, issued_at, expires_at, last_activity_at, revoked_at";

/// SQLx implementation of [`SessionRepository`].
///
/// Session rows are never deleted: revocation is a `revoked_at` timestamp,
/// because session history is security-relevant.
#[derive(Debug, Clone)]
pub struct PgSessionRepository {
    pool: PgPool,
}

impl PgSessionRepository {
    /// Creates the repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl SessionRepository for PgSessionRepository {
    async fn create(&self, session: Session) -> Result<Session, DomainError> {
        sqlx::query(
            "INSERT INTO sessions
             (session_id, staff_id, token_hash, delegation_id, issued_at,
              expires_at, last_activity_at, revoked_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(session.session_id.as_uuid())
        .bind(session.staff_id.as_uuid())
        .bind(session.token_hash.to_vec())
        .bind(session.delegation_id.map(DelegationId::as_uuid))
        .bind(session.issued_at)
        .bind(session.expires_at)
        .bind(session.last_activity_at)
        .bind(session.revoked_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(session)
    }

    async fn get_by_token_hash(
        &self,
        token_hash: &[u8; 32],
    ) -> Result<Option<Session>, DomainError> {
        let row: Option<SessionRow> = sqlx::query_as(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE token_hash = $1"
        ))
        .bind(token_hash.to_vec())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(SessionRow::into_session).transpose()
    }

    async fn update(&self, session: Session) -> Result<Session, DomainError> {
        let updated = sqlx::query(
            "UPDATE sessions
             SET staff_id = $2, token_hash = $3, delegation_id = $4,
                 issued_at = $5, expires_at = $6, last_activity_at = $7,
                 revoked_at = $8
             WHERE session_id = $1",
        )
        .bind(session.session_id.as_uuid())
        .bind(session.staff_id.as_uuid())
        .bind(session.token_hash.to_vec())
        .bind(session.delegation_id.map(DelegationId::as_uuid))
        .bind(session.issued_at)
        .bind(session.expires_at)
        .bind(session.last_activity_at)
        .bind(session.revoked_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if updated.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "session".to_owned(),
                id: session.session_id.as_uuid().to_string(),
            });
        }
        Ok(session)
    }

    async fn list_for_staff(
        &self,
        staff_id: StaffId,
        limit: u64,
    ) -> Result<Vec<Session>, DomainError> {
        let rows: Vec<SessionRow> = sqlx::query_as(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions
             WHERE staff_id = $1
             ORDER BY issued_at DESC
             LIMIT $2"
        ))
        .bind(staff_id.as_uuid())
        // `limit` is caller-bounded; the cast is safe for realistic values.
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.into_iter().map(SessionRow::into_session).collect()
    }
}

/// Row shape of the `grants` table.
#[derive(Debug, sqlx::FromRow)]
struct GrantRow {
    grant_id: Uuid,
    resource: String,
    operation: String,
    scope_kind: String,
    agency_id: Uuid,
    client_profile_id: Option<Uuid>,
}

impl GrantRow {
    /// Converts the row into a domain [`Grant`].
    fn into_grant(self) -> Result<Grant, DomainError> {
        let scope = match self.scope_kind.as_str() {
            "agency" => Scope::Agency {
                agency_id: AgencyId::from(self.agency_id),
            },
            "client_profile" => {
                let client_profile_id = self.client_profile_id.ok_or_else(|| {
                    DomainError::Validation(
                        "storage error: client_profile scope without client_profile_id".to_owned(),
                    )
                })?;
                Scope::ClientProfile {
                    agency_id: AgencyId::from(self.agency_id),
                    client_profile_id: ClientProfileId::from(client_profile_id),
                }
            }
            other => {
                return Err(DomainError::Validation(format!(
                    "storage error: unknown scope kind '{other}'"
                )))
            }
        };
        Ok(Grant {
            grant_id: GrantId::from(self.grant_id),
            action: Action {
                resource: resource_from_str(&self.resource)?,
                operation: operation_from_str(&self.operation)?,
            },
            scope,
        })
    }
}

/// SQLx implementation of [`GrantRepository`].
///
/// Grants are additive and revocation is a `revoked_at` tombstone:
/// [`GrantRepository::list_for_staff`] returns only live (non-revoked)
/// rows, so revoking a grant immediately stops authorizing it — including
/// for delegated agents via
/// [`Delegation::effective_grants`](crm_domain::security::Delegation::effective_grants).
#[derive(Debug, Clone)]
pub struct PgGrantRepository {
    pool: PgPool,
}

impl PgGrantRepository {
    /// Creates the repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl GrantRepository for PgGrantRepository {
    async fn list_for_staff(&self, staff_id: StaffId) -> Result<Vec<Grant>, DomainError> {
        let rows: Vec<GrantRow> = sqlx::query_as(
            "SELECT grant_id, resource, operation, scope_kind, agency_id, client_profile_id
             FROM grants
             WHERE staff_id = $1 AND revoked_at IS NULL",
        )
        .bind(staff_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.into_iter().map(GrantRow::into_grant).collect()
    }

    async fn assign(&self, staff_id: StaffId, grant: Grant) -> Result<Grant, DomainError> {
        let (scope_kind, client_profile_id) = match grant.scope {
            Scope::Agency { .. } => ("agency", None),
            Scope::ClientProfile {
                client_profile_id, ..
            } => ("client_profile", Some(client_profile_id.as_uuid())),
        };
        sqlx::query(
            "INSERT INTO grants
             (grant_id, staff_id, resource, operation, scope_kind, agency_id, client_profile_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(grant.grant_id.as_uuid())
        .bind(staff_id.as_uuid())
        .bind(resource_as_str(grant.action.resource))
        .bind(operation_as_str(grant.action.operation))
        .bind(scope_kind)
        .bind(grant.scope.agency_id().as_uuid())
        .bind(client_profile_id)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(grant)
    }

    async fn revoke(&self, staff_id: StaffId, grant_id: GrantId) -> Result<(), DomainError> {
        // Tombstone, not delete: grant history is audit-relevant. Idempotent:
        // revoking an already-revoked or unknown grant is a no-op so retried
        // revocations stay safe.
        sqlx::query(
            "UPDATE grants SET revoked_at = now()
             WHERE grant_id = $1 AND staff_id = $2 AND revoked_at IS NULL",
        )
        .bind(grant_id.as_uuid())
        .bind(staff_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(())
    }
}
