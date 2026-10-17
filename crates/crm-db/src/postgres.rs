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
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, ContactId};
use crm_domain::repositories::{AuditRepository, ContactRepository};

/// SQLSTATE for unique-violation (duplicate key) errors.
const UNIQUE_VIOLATION: &str = "23505";

/// Maps storage failures onto [`DomainError`].
///
/// See the module docs for the mapping contract.
fn map_storage_error(error: sqlx::Error) -> DomainError {
    if let sqlx::Error::Database(db_error) = &error {
        if db_error.code().as_deref() == Some(UNIQUE_VIOLATION) {
            return DomainError::Conflict(format!("storage conflict: {}", db_error.message()));
        }
    }
    DomainError::Validation(format!("storage error: {error}"))
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
fn actor_kind_as_str(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

/// Parses an [`ActorKind`] from its database string.
fn actor_kind_from_str(raw: &str) -> Result<ActorKind, DomainError> {
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
fn permission_decision_as_str(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Allow => "allow",
        PermissionDecision::Deny => "deny",
    }
}

/// Parses a [`PermissionDecision`] from its database string.
fn permission_decision_from_str(raw: &str) -> Result<PermissionDecision, DomainError> {
    match raw {
        "allow" => Ok(PermissionDecision::Allow),
        "deny" => Ok(PermissionDecision::Deny),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown permission decision '{other}'"
        ))),
    }
}

/// Renders an [`ActionResult`] as its stable database string.
fn action_result_as_str(result: ActionResult) -> &'static str {
    match result {
        ActionResult::Success => "success",
        ActionResult::Failure => "failure",
        ActionResult::Pending => "pending",
    }
}

/// Parses an [`ActionResult`] from its database string.
fn action_result_from_str(raw: &str) -> Result<ActionResult, DomainError> {
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
fn hash_from_bytes(raw: Vec<u8>) -> Result<[u8; 32], DomainError> {
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
struct AuditEventRow {
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
    fn into_event(self) -> Result<AuditEvent, DomainError> {
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
const AUDIT_EVENT_COLUMNS: &str = "event_id, occurred_at, actor_kind, actor_id, client_profile_id, action, target_kind, target_id, correlation_id, permission_decision, result, before_ref, after_ref, chain_hash";

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
}

impl ContactRepository for PgContactRepository {
    async fn create(&self, contact: Contact) -> Result<Contact, DomainError> {
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
