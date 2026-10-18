// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Calling vertical slice (task 14-VOICE-02).
//!
//! [`PgCallRepository`] implements
//! [`crm_domain::repositories::CallRepository`] over the `calls`
//! table (migration 0014). Recording rows live in
//! `call_recordings`; the latest row per call is hydrated into the
//! domain's [`CallRecording`](crm_domain::voice::CallRecording)
//! (consent is stored on `calls.recording_consent` — the legal
//! basis — while the recording lifecycle rows carry timing and the
//! audio asset id).
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::error::DomainError;
use crm_domain::ids::{AssetId, CallId, ClientProfileId, ContactId};
use crm_domain::repositories::CallRepository;
use crm_domain::voice::{
    Call, CallDirection, CallRecording, CallStatus, PhoneNumber, RecordingConsent,
};

use crate::postgres::{ensure_client_profile, map_storage_error};

/// Columns selected for a [`Call`] row.
/// A raw `call_recordings` row as returned by [`sqlx::query_as`].
type RecordingRow = (Option<Uuid>, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

const CALL_COLUMNS: &str = "call_id, client_profile_id, contact_id, direction, \
    from_number, to_number, status, provider_call_id, started_at, ended_at, \
    duration_secs, failure_reason, recording_consent";

/// Renders a [`CallDirection`] as its stable database string.
fn call_direction_as_str(direction: CallDirection) -> &'static str {
    direction.as_str()
}

/// Parses a [`CallDirection`] from its database string.
fn call_direction_from_str(raw: &str) -> Result<CallDirection, DomainError> {
    match raw {
        "outbound" => Ok(CallDirection::Outbound),
        "inbound" => Ok(CallDirection::Inbound),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown call direction '{other}'"
        ))),
    }
}

/// Renders a [`CallStatus`] as its stable database string.
fn call_status_as_str(status: CallStatus) -> &'static str {
    status.as_str()
}

/// Parses a [`CallStatus`] from its database string.
fn call_status_from_str(raw: &str) -> Result<CallStatus, DomainError> {
    match raw {
        "initiated" => Ok(CallStatus::Initiated),
        "ringing" => Ok(CallStatus::Ringing),
        "in_progress" => Ok(CallStatus::InProgress),
        "completed" => Ok(CallStatus::Completed),
        "failed" => Ok(CallStatus::Failed),
        "no_answer" => Ok(CallStatus::NoAnswer),
        "busy" => Ok(CallStatus::Busy),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown call status '{other}'"
        ))),
    }
}

/// Renders a [`RecordingConsent`] as its stable database string.
fn recording_consent_as_str(consent: RecordingConsent) -> &'static str {
    match consent {
        RecordingConsent::Granted => "granted",
        RecordingConsent::Denied => "denied",
        RecordingConsent::Unknown => "unknown",
    }
}

/// Parses a [`RecordingConsent`] from its database string.
fn recording_consent_from_str(raw: &str) -> Result<RecordingConsent, DomainError> {
    match raw {
        "granted" => Ok(RecordingConsent::Granted),
        "denied" => Ok(RecordingConsent::Denied),
        "unknown" => Ok(RecordingConsent::Unknown),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown recording consent '{other}'"
        ))),
    }
}

/// Loads the latest `call_recordings` row for `call_id`, mapped onto
/// the domain's [`CallRecording`]. Returns `None` when no recording
/// row exists yet — consent is always read from the `calls` row.
async fn load_recording(
    pool: &PgPool,
    call_id: CallId,
    consent: RecordingConsent,
) -> Result<Option<CallRecording>, DomainError> {
    let row: Option<RecordingRow> = sqlx::query_as(
        "SELECT asset_id, started_at, stopped_at FROM call_recordings \
             WHERE call_id = $1 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(call_id.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_storage_error)?;
    match row {
        None => Ok(None),
        Some((asset_id, started_at, stopped_at)) => Ok(Some(CallRecording {
            consent,
            started_at,
            active: started_at.is_some() && stopped_at.is_none(),
            asset_id: asset_id.map(AssetId::from),
        })),
    }
}

/// Hydrates a `calls` row into a [`Call`]. All aggregate fields are
/// `pub`, so hydration builds the struct directly (the status
/// machine stays behind the domain's transition methods).
async fn row_to_call(pool: &PgPool, row: &sqlx::postgres::PgRow) -> Result<Call, DomainError> {
    let call_id = CallId::from(
        row.try_get::<Uuid, _>("call_id")
            .map_err(map_storage_error)?,
    );
    let consent = recording_consent_from_str(
        &row.try_get::<String, _>("recording_consent")
            .map_err(map_storage_error)?,
    )?;
    let recording = load_recording(pool, call_id, consent).await?;
    Ok(Call {
        call_id,
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        contact_id: row
            .try_get::<Option<Uuid>, _>("contact_id")
            .map_err(map_storage_error)?
            .map(ContactId::from),
        direction: call_direction_from_str(
            &row.try_get::<String, _>("direction")
                .map_err(map_storage_error)?,
        )?,
        from: PhoneNumber::new(
            row.try_get::<String, _>("from_number")
                .map_err(map_storage_error)?,
        )?,
        to: PhoneNumber::new(
            row.try_get::<String, _>("to_number")
                .map_err(map_storage_error)?,
        )?,
        status: call_status_from_str(
            &row.try_get::<String, _>("status")
                .map_err(map_storage_error)?,
        )?,
        started_at: row
            .try_get::<DateTime<Utc>, _>("started_at")
            .map_err(map_storage_error)?,
        ended_at: row
            .try_get::<Option<DateTime<Utc>>, _>("ended_at")
            .map_err(map_storage_error)?,
        duration_secs: row
            .try_get::<Option<i64>, _>("duration_secs")
            .map_err(map_storage_error)?
            .map(|secs| secs.max(0) as u64),
        failure_reason: row
            .try_get::<Option<String>, _>("failure_reason")
            .map_err(map_storage_error)?,
        provider_call_id: row
            .try_get::<Option<String>, _>("provider_call_id")
            .map_err(map_storage_error)?,
        recording,
    })
}

/// PostgreSQL [`CallRepository`](crm_domain::repositories::CallRepository).
#[derive(Debug, Clone)]
pub struct PgCallRepository {
    pool: PgPool,
}

impl PgCallRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl CallRepository for PgCallRepository {
    async fn create(&self, call: Call) -> Result<Call, DomainError> {
        ensure_client_profile(&self.pool, call.client_profile_id).await?;
        let consent = call
            .recording
            .as_ref()
            .map(|recording| recording.consent)
            .unwrap_or(RecordingConsent::Unknown);
        let result = sqlx::query(
            "INSERT INTO calls (call_id, client_profile_id, contact_id, direction, \
             from_number, to_number, status, provider_call_id, started_at, ended_at, \
             duration_secs, failure_reason, recording_consent) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(call.call_id.as_uuid())
        .bind(call.client_profile_id.as_uuid())
        .bind(call.contact_id.map(|id| id.as_uuid()))
        .bind(call_direction_as_str(call.direction))
        .bind(call.from.as_str())
        .bind(call.to.as_str())
        .bind(call_status_as_str(call.status))
        .bind(call.provider_call_id.as_deref())
        .bind(call.started_at)
        .bind(call.ended_at)
        .bind(call.duration_secs.map(|secs| secs as i64))
        .bind(call.failure_reason.as_deref())
        .bind(recording_consent_as_str(consent))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() != 1 {
            return Err(DomainError::Validation(
                "storage error: call insert affected no rows".to_string(),
            ));
        }
        self.get(call.call_id).await
    }

    async fn get(&self, call_id: CallId) -> Result<Call, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {CALL_COLUMNS} FROM calls WHERE call_id = $1"
        ))
        .bind(call_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_call(&self.pool, &row).await,
            None => Err(DomainError::NotFound {
                kind: "call".to_string(),
                id: call_id.to_string(),
            }),
        }
    }

    async fn update(&self, call: Call) -> Result<Call, DomainError> {
        let consent = call
            .recording
            .as_ref()
            .map(|recording| recording.consent)
            .unwrap_or(RecordingConsent::Unknown);
        // Upsert the recording lifecycle row when a recording is
        // attached: the service keeps exactly one row per call.
        if let Some(recording) = call.recording.as_ref() {
            sqlx::query(
                "INSERT INTO call_recordings (recording_id, call_id, asset_id, started_at, stopped_at) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (call_id) DO UPDATE SET \
                 asset_id = EXCLUDED.asset_id, \
                 started_at = EXCLUDED.started_at, \
                 stopped_at = EXCLUDED.stopped_at",
            )
            .bind(Uuid::new_v4())
            .bind(call.call_id.as_uuid())
            .bind(recording.asset_id.map(|id| id.as_uuid()))
            .bind(recording.started_at.unwrap_or_else(Utc::now))
            .bind(if recording.active {
                None
            } else {
                Some(Utc::now())
            })
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        }
        let result = sqlx::query(
            "UPDATE calls SET contact_id = $2, status = $3, provider_call_id = $4, \
             ended_at = $5, duration_secs = $6, failure_reason = $7, \
             recording_consent = $8, updated_at = now() \
             WHERE call_id = $1",
        )
        .bind(call.call_id.as_uuid())
        .bind(call.contact_id.map(|id| id.as_uuid()))
        .bind(call_status_as_str(call.status))
        .bind(call.provider_call_id.as_deref())
        .bind(call.ended_at)
        .bind(call.duration_secs.map(|secs| secs as i64))
        .bind(call.failure_reason.as_deref())
        .bind(recording_consent_as_str(consent))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() != 1 {
            return Err(DomainError::NotFound {
                kind: "call".to_string(),
                id: call.call_id.to_string(),
            });
        }
        self.get(call.call_id).await
    }

    async fn list_by_profile(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Call>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {CALL_COLUMNS} FROM calls \
             WHERE client_profile_id = $1 \
             ORDER BY created_at DESC, call_id DESC LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        let mut calls = Vec::with_capacity(rows.len());
        for row in rows {
            calls.push(row_to_call(&self.pool, &row).await?);
        }
        Ok(calls)
    }

    async fn list_by_contact(
        &self,
        contact_id: ContactId,
        limit: u64,
    ) -> Result<Vec<Call>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {CALL_COLUMNS} FROM calls \
             WHERE contact_id = $1 \
             ORDER BY created_at DESC, call_id DESC LIMIT $2"
        ))
        .bind(contact_id.as_uuid())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        let mut calls = Vec::with_capacity(rows.len());
        for row in rows {
            calls.push(row_to_call(&self.pool, &row).await?);
        }
        Ok(calls)
    }
}
