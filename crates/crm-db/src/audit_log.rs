// SPDX-License-Identifier: GPL-3.0-only
//! SQLx implementation of the [`AuditLogService`] security audit log
//! (task 04-AUD-02).
//!
//! [`PgAuditLogService`] persists [`SecurityEvent`]s onto the shared
//! `audit_events` table — one log, one chain, shared with the
//! contact-lifecycle events appended by [`PgAuditRepository`]. The
//! serialized `SecurityEvent` travels in the `payload` JSONB column
//! (migration 0004); the [`AuditEvent`] envelope columns carry the
//! tamper-evident chain link.
//!
//! ## Concurrency
//!
//! Appends serialize on the same transaction-scoped advisory lock as
//! [`PgAuditRepository`] (`'crm_audit_head'`), so a security event and a
//! contact event racing each other still link onto one head and the log
//! never forks. Because the chain hash is computed from the head *inside*
//! the lock, a head race is impossible here — unlike
//! [`crate::repositories::AuditRepository::append`], which verifies a
//! caller-supplied hash.
//!
//! ## Idempotency
//!
//! `record` checks `idempotency_key` before appending: a retried
//! recording returns the original event instead of duplicating the row.
//! A lost race on the unique index falls back to reading the winner's
//! row, so concurrent retries converge on one event.
//!
//! [`AuditLogService`]: crm_domain::services::AuditLogService
//! [`PgAuditRepository`]: crate::postgres::PgAuditRepository
//! [`SecurityEvent`]: crm_domain::audit::SecurityEvent
//! [`AuditEvent`]: crm_domain::audit::AuditEvent

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::audit::{compute_chain_hash, AuditEvent, SecurityEvent, GENESIS_CHAIN_HASH};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId};
use crm_domain::services::{
    ActionContext, AuditLogService, AuditPage, AuditQuery, ChainVerification,
};

use crate::postgres::{
    action_result_as_str, action_result_from_str, actor_kind_as_str, actor_kind_from_str,
    ensure_client_profile, hash_from_bytes, map_storage_error, permission_decision_as_str,
    permission_decision_from_str,
};

/// Maximum events returned by one [`AuditLogService::query`] call.
///
/// The HTTP layer defaults to a smaller page; this is the hard ceiling
/// so a single query cannot dump the whole log.
const MAX_QUERY_LIMIT: u64 = 100;

/// Default page size when the caller passes `limit: 0`.
const DEFAULT_QUERY_LIMIT: u64 = 50;

/// SQLx implementation of [`AuditLogService`].
#[derive(Debug, Clone)]
pub struct PgAuditLogService {
    pool: PgPool,
}

impl PgAuditLogService {
    /// Creates the service over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reads the current log head inside an advisory-locked transaction.
    async fn locked_head(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<[u8; 32], DomainError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('crm_audit_head')::bigint)")
            .execute(&mut **tx)
            .await
            .map_err(map_storage_error)?;
        let head: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT chain_hash FROM audit_events ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_storage_error)?;
        match head {
            Some(bytes) => hash_from_bytes(bytes),
            None => Ok(GENESIS_CHAIN_HASH),
        }
    }

    /// Hydrates one full audit row (envelope + payload) into its event.
    fn row_to_event(
        row: &sqlx::postgres::PgRow,
    ) -> Result<(AuditEvent, Option<serde_json::Value>), DomainError> {
        let event = AuditEvent {
            event_id: AuditEventId::from(
                row.try_get::<Uuid, _>("event_id")
                    .map_err(map_storage_error)?,
            ),
            occurred_at: row
                .try_get::<DateTime<Utc>, _>("occurred_at")
                .map_err(map_storage_error)?,
            actor_kind: actor_kind_from_str(
                &row.try_get::<String, _>("actor_kind")
                    .map_err(map_storage_error)?,
            )?,
            actor_id: ActorId::from(
                row.try_get::<Uuid, _>("actor_id")
                    .map_err(map_storage_error)?,
            ),
            client_profile_id: row
                .try_get::<Option<Uuid>, _>("client_profile_id")
                .map_err(map_storage_error)?
                .map(ClientProfileId::from),
            action: row
                .try_get::<String, _>("action")
                .map_err(map_storage_error)?,
            target_kind: row
                .try_get::<String, _>("target_kind")
                .map_err(map_storage_error)?,
            target_id: row
                .try_get::<String, _>("target_id")
                .map_err(map_storage_error)?,
            correlation_id: row
                .try_get::<Uuid, _>("correlation_id")
                .map_err(map_storage_error)?,
            permission_decision: permission_decision_from_str(
                &row.try_get::<String, _>("permission_decision")
                    .map_err(map_storage_error)?,
            )?,
            result: action_result_from_str(
                &row.try_get::<String, _>("result")
                    .map_err(map_storage_error)?,
            )?,
            before_ref: row
                .try_get::<Option<String>, _>("before_ref")
                .map_err(map_storage_error)?,
            after_ref: row
                .try_get::<Option<String>, _>("after_ref")
                .map_err(map_storage_error)?,
            chain_hash: hash_from_bytes(
                row.try_get::<Vec<u8>, _>("chain_hash")
                    .map_err(map_storage_error)?,
            )?,
        };
        let payload: Option<serde_json::Value> = row
            .try_get::<Option<serde_json::Value>, _>("payload")
            .map_err(map_storage_error)?;
        Ok((event, payload))
    }

    /// Fetches one event (envelope) by idempotency key.
    async fn by_idempotency_key(&self, key: Uuid) -> Result<Option<AuditEvent>, DomainError> {
        let row: Option<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT event_id, occurred_at, actor_kind, actor_id, client_profile_id, action, target_kind, target_id, correlation_id, permission_decision, result, before_ref, after_ref, chain_hash, payload
             FROM audit_events WHERE idempotency_key = $1",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(|row| Self::row_to_event(&row).map(|(event, _)| event))
            .transpose()
    }

    /// Queries the log like [`AuditLogService::query`], but returns each
    /// event with its forensic payload for HTTP responses.
    ///
    /// The domain port returns envelopes only; this is the
    /// persistence-layer escape hatch the API uses to render full
    /// forensic pages without a second round trip per event.
    pub async fn query_detailed(
        &self,
        ctx: &ActionContext,
        filter: &AuditQuery,
    ) -> Result<(Vec<(AuditEvent, Option<serde_json::Value>)>, bool), DomainError> {
        Self::query_rows(&self.pool, ctx, filter).await
    }

    /// Shared row-fetching for [`AuditLogService::query`] and
    /// [`PgAuditLogService::query_detailed`].
    async fn query_rows(
        pool: &PgPool,
        ctx: &ActionContext,
        filter: &AuditQuery,
    ) -> Result<(Vec<(AuditEvent, Option<serde_json::Value>)>, bool), DomainError> {
        // Scope isolation: a profile-scoped context never sees another
        // profile's events. An agency-scoped context (client_profile_id
        // None) may query across profiles, optionally narrowed by the
        // filter.
        let effective_profile: Option<ClientProfileId> = match ctx.client_profile_id {
            Some(profile) => {
                if let Some(requested) = filter.client_profile_id {
                    if requested != profile {
                        // Asking for another profile's log from a
                        // profile-scoped context: fail closed with an
                        // empty page rather than leaking existence.
                        return Ok((vec![], false));
                    }
                }
                Some(profile)
            }
            None => filter.client_profile_id,
        };

        let take = if filter.limit == 0 {
            DEFAULT_QUERY_LIMIT
        } else {
            filter.limit.min(MAX_QUERY_LIMIT)
        };

        // Cursor: chain order is seq order; `after` names the last event
        // of the previous page. An unknown cursor yields an empty page
        // (fail closed) rather than guessing a position.
        let after_seq: Option<i64> = match filter.after {
            Some(event_id) => {
                let seq: Option<i64> =
                    sqlx::query_scalar("SELECT seq FROM audit_events WHERE event_id = $1")
                        .bind(event_id.as_uuid())
                        .fetch_optional(pool)
                        .await
                        .map_err(map_storage_error)?;
                match seq {
                    Some(seq) => Some(seq),
                    None => return Ok((vec![], false)),
                }
            }
            None => None,
        };

        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT event_id, occurred_at, actor_kind, actor_id, client_profile_id, action, target_kind, target_id, correlation_id, permission_decision, result, before_ref, after_ref, chain_hash, payload
             FROM audit_events
             WHERE ($1::uuid IS NULL OR actor_id = $1)
               AND ($2::text IS NULL OR action = $2)
               AND ($3::uuid IS NULL OR client_profile_id = $3)
               AND ($4::timestamptz IS NULL OR occurred_at >= $4)
               AND ($5::timestamptz IS NULL OR occurred_at < $5)
               AND ($6::bigint IS NULL OR seq > $6)
             ORDER BY seq ASC
             LIMIT $7",
        )
        .bind(filter.actor_id.map(ActorId::as_uuid))
        .bind(filter.action.clone())
        .bind(effective_profile.map(ClientProfileId::as_uuid))
        .bind(filter.from)
        .bind(filter.to)
        .bind(after_seq)
        // Fetch one extra to know whether another page exists.
        .bind((take + 1) as i64)
        .fetch_all(pool)
        .await
        .map_err(map_storage_error)?;

        let has_more = rows.len() as u64 > take;
        let mut events = Vec::with_capacity(rows.len().min(take as usize));
        for row in rows.into_iter().take(take as usize) {
            events.push(Self::row_to_event(&row)?);
        }
        Ok((events, has_more))
    }
}

impl AuditLogService for PgAuditLogService {
    async fn record(
        &self,
        ctx: &ActionContext,
        event: &SecurityEvent,
        idempotency_key: Uuid,
    ) -> Result<AuditEvent, DomainError> {
        // Idempotent fast path: a retried recording returns the original
        // event instead of appending a duplicate.
        if let Some(existing) = self.by_idempotency_key(idempotency_key).await? {
            return Ok(existing);
        }
        let mut envelope = event.to_audit_event(
            ctx.actor_kind,
            ctx.actor_id,
            ctx.client_profile_id,
            ctx.correlation_id,
            Utc::now(),
        );
        let payload = event.payload();

        let mut tx = self.pool.begin().await.map_err(map_storage_error)?;
        let prev = Self::locked_head(&mut tx).await?;
        envelope.chain_hash = compute_chain_hash(&prev, &envelope.canonical_bytes());
        // Security events are agency-level; a profile-scoped event still
        // needs its profile row to exist for the foreign key.
        if let Some(profile_id) = envelope.client_profile_id {
            ensure_client_profile(&mut *tx, profile_id).await?;
        }
        let insert = sqlx::query(
            "INSERT INTO audit_events
             (event_id, occurred_at, actor_kind, actor_id, client_profile_id, action, target_kind, target_id, correlation_id, permission_decision, result, before_ref, after_ref, prev_hash, chain_hash, payload, idempotency_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
        )
        .bind(envelope.event_id.as_uuid())
        .bind(envelope.occurred_at)
        .bind(actor_kind_as_str(envelope.actor_kind))
        .bind(envelope.actor_id.as_uuid())
        .bind(envelope.client_profile_id.map(ClientProfileId::as_uuid))
        .bind(&envelope.action)
        .bind(&envelope.target_kind)
        .bind(&envelope.target_id)
        .bind(envelope.correlation_id)
        .bind(permission_decision_as_str(envelope.permission_decision))
        .bind(action_result_as_str(envelope.result))
        .bind(&envelope.before_ref)
        .bind(&envelope.after_ref)
        .bind(prev.to_vec())
        .bind(envelope.chain_hash.to_vec())
        .bind(&payload)
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await;
        match insert {
            Ok(_) => {}
            Err(sqlx::Error::Database(db_error))
                if db_error.kind() == sqlx::error::ErrorKind::UniqueViolation =>
            {
                // Lost a race with a concurrent retry holding the same
                // key: roll back and return the winner's event.
                tx.rollback().await.map_err(map_storage_error)?;
                if let Some(existing) = self.by_idempotency_key(idempotency_key).await? {
                    return Ok(existing);
                }
                return Err(DomainError::Validation(
                    "idempotency key conflicted but no event was found".to_owned(),
                ));
            }
            Err(error) => return Err(map_storage_error(error)),
        }
        tx.commit().await.map_err(map_storage_error)?;
        Ok(envelope)
    }

    async fn verify_chain(&self) -> Result<ChainVerification, DomainError> {
        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT event_id, occurred_at, actor_kind, actor_id, client_profile_id, action, target_kind, target_id, correlation_id, permission_decision, result, before_ref, after_ref, chain_hash, payload
             FROM audit_events ORDER BY seq ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        let mut prev = GENESIS_CHAIN_HASH;
        let mut checked: u64 = 0;
        let mut head: Option<[u8; 32]> = None;
        for row in &rows {
            let (event, _) = Self::row_to_event(row)?;
            if event.chain_hash != compute_chain_hash(&prev, &event.canonical_bytes()) {
                return Ok(ChainVerification {
                    valid: false,
                    events_checked: checked,
                    head,
                    first_bad_event: Some(event.event_id),
                });
            }
            prev = event.chain_hash;
            head = Some(prev);
            checked += 1;
        }
        Ok(ChainVerification {
            valid: true,
            events_checked: checked,
            head,
            first_bad_event: None,
        })
    }

    async fn query(
        &self,
        ctx: &ActionContext,
        filter: &AuditQuery,
    ) -> Result<AuditPage, DomainError> {
        let (rows, has_more) = Self::query_rows(&self.pool, ctx, filter).await?;
        Ok(AuditPage {
            events: rows.into_iter().map(|(event, _)| event).collect(),
            has_more,
        })
    }
}
