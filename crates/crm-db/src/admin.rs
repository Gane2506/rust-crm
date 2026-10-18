// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Admin and operations vertical slice (task
//! 16-ADMIN-02).
//!
//! [`PgStaffInvitationRepository`], [`PgRoleAssignmentRepository`],
//! [`PgAgencySettingsRepository`], [`PgMaintenanceRepository`],
//! [`PgBackupRepository`], [`PgExportRepository`],
//! [`PgUpgradeRepository`] and [`PgInstallationRepository`] implement
//! the [`crm_domain::repositories`] admin traits over the `0016_admin`
//! tables.
//!
//! Every row stores its key columns (for indexing and lifecycle
//! lookups) plus a `payload` JSONB column carrying the whole entity.
//! Hydration always goes through the domain's manual `Deserialize`
//! impls (validating constructors), so a corrupt row surfaces as a
//! storage error — never as a bypassed domain invariant.
//!
//! [`DbHealthReporter`] implements
//! [`HealthReporter`](crm_domain::observability::HealthReporter) with a
//! bounded `SELECT 1` probe so the admin health endpoint can fold the
//! database into [`summarize_health`](crm_domain::admin::summarize_health).
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use std::future::Future;

use chrono::Utc;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::admin::{
    AgencySettings, AssignmentStatus, BackupRecord, BackupStatus, DataExport, ExportFormat,
    ExportStatus, Installation, InstallationState, InvitationStatus, MaintenanceMode,
    RoleAssignment, StaffInvitation, SystemUpgrade, UpgradeStatus,
};
use crm_domain::error::DomainError;
use crm_domain::ids::{
    AgencyId, BackupId, ClientProfileId, ExportId, InvitationId, RoleAssignmentId, StaffId,
    UpgradeId,
};
use crm_domain::observability::{ComponentHealth, HealthReport, HealthReporter};
use crm_domain::repositories::{
    AgencySettingsRepository, BackupRepository, ExportRepository, InstallationRepository,
    MaintenanceRepository, RoleAssignmentRepository, StaffInvitationRepository, UpgradeRepository,
};

use crate::postgres::{ensure_client_profile, map_storage_error};

/// Serializes a value to JSON, mapping failures to a storage error.
fn to_json<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(value).map_err(|error| {
        DomainError::Validation(format!(
            "storage error: cannot serialize admin payload: {error}"
        ))
    })
}

/// Hydrates an entity from its stored `payload`, running the domain's
/// validating `Deserialize` impl. A corrupt payload is a loud storage
/// error, never silently accepted invalid state.
fn from_payload<T>(payload: serde_json::Value, kind: &str) -> Result<T, DomainError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    serde_json::from_value(payload).map_err(|error| {
        DomainError::Validation(format!("storage error: invalid {kind} payload: {error}"))
    })
}

/// Renders an [`InvitationStatus`] as its stable database string.
fn invitation_status_as_str(status: InvitationStatus) -> &'static str {
    match status {
        InvitationStatus::Pending => "pending",
        InvitationStatus::Accepted => "accepted",
        InvitationStatus::Expired => "expired",
        InvitationStatus::Revoked => "revoked",
    }
}

/// Renders an [`AssignmentStatus`] as its stable database string.
fn assignment_status_as_str(status: AssignmentStatus) -> &'static str {
    match status {
        AssignmentStatus::Active => "active",
        AssignmentStatus::Revoked => "revoked",
    }
}

/// Renders a [`BackupStatus`] as its stable database string.
fn backup_status_as_str(status: BackupStatus) -> &'static str {
    match status {
        BackupStatus::Scheduled => "scheduled",
        BackupStatus::InProgress => "in_progress",
        BackupStatus::Completed => "completed",
        BackupStatus::Failed => "failed",
    }
}

/// Renders an [`ExportStatus`] as its stable database string.
fn export_status_as_str(status: ExportStatus) -> &'static str {
    match status {
        ExportStatus::Requested => "requested",
        ExportStatus::InProgress => "in_progress",
        ExportStatus::Ready => "ready",
        ExportStatus::Failed => "failed",
        ExportStatus::Expired => "expired",
    }
}

/// Renders an [`ExportFormat`] as its stable database string.
fn export_format_as_str(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Csv => "csv",
        ExportFormat::Json => "json",
    }
}

/// Renders an [`UpgradeStatus`] as its stable database string.
fn upgrade_status_as_str(status: UpgradeStatus) -> &'static str {
    match status {
        UpgradeStatus::Scheduled => "scheduled",
        UpgradeStatus::InProgress => "in_progress",
        UpgradeStatus::Completed => "completed",
        UpgradeStatus::Failed => "failed",
        UpgradeStatus::RolledBack => "rolled_back",
    }
}

/// Renders an [`InstallationState`] as its stable database string.
fn installation_state_as_str(state: InstallationState) -> &'static str {
    match state {
        InstallationState::NotStarted => "not_started",
        InstallationState::InProgress => "in_progress",
        InstallationState::Completed => "completed",
    }
}

/// PostgreSQL implementation of
/// [`StaffInvitationRepository`](crm_domain::repositories::StaffInvitationRepository).
#[derive(Debug, Clone)]
pub struct PgStaffInvitationRepository {
    pool: PgPool,
}

impl PgStaffInvitationRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl StaffInvitationRepository for PgStaffInvitationRepository {
    fn save<'a>(
        &'a self,
        invitation: StaffInvitation,
    ) -> impl Future<Output = Result<StaffInvitation, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&invitation)?;
            sqlx::query(
                "INSERT INTO staff_invitations \
                 (invitation_id, email, roles, invited_by, status, expires_at, created_at, payload) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(invitation.invitation_id().as_uuid())
            .bind(invitation.email())
            .bind(to_json(&invitation.role_ids())?)
            .bind(invitation.invited_by().as_uuid())
            .bind(invitation_status_as_str(invitation.status()))
            .bind(invitation.expires_at())
            .bind(invitation.created_at())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(invitation)
        }
    }

    fn get<'a>(
        &'a self,
        invitation_id: InvitationId,
    ) -> impl Future<Output = Result<StaffInvitation, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query("SELECT payload FROM staff_invitations WHERE invitation_id = $1")
                .bind(invitation_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            let row = row.ok_or_else(|| DomainError::NotFound {
                kind: "staff_invitation".to_string(),
                id: invitation_id.to_string(),
            })?;
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "staff invitation")
        }
    }

    fn update<'a>(
        &'a self,
        invitation: StaffInvitation,
    ) -> impl Future<Output = Result<StaffInvitation, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&invitation)?;
            let affected = sqlx::query(
                "UPDATE staff_invitations \
                 SET email = $2, roles = $3, invited_by = $4, status = $5, \
                     expires_at = $6, payload = $7 \
                 WHERE invitation_id = $1",
            )
            .bind(invitation.invitation_id().as_uuid())
            .bind(invitation.email())
            .bind(to_json(&invitation.role_ids())?)
            .bind(invitation.invited_by().as_uuid())
            .bind(invitation_status_as_str(invitation.status()))
            .bind(invitation.expires_at())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?
            .rows_affected();
            if affected == 0 {
                return Err(DomainError::NotFound {
                    kind: "staff_invitation".to_string(),
                    id: invitation.invitation_id().to_string(),
                });
            }
            Ok(invitation)
        }
    }

    fn list<'a>(
        &'a self,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<StaffInvitation>, DomainError>> + Send + 'a {
        async move {
            let rows = sqlx::query(
                "SELECT payload FROM staff_invitations \
                 ORDER BY created_at DESC, invitation_id DESC LIMIT $1",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(map_storage_error)?;
            let mut invitations = Vec::with_capacity(rows.len());
            for row in rows {
                let payload: serde_json::Value =
                    row.try_get("payload").map_err(map_storage_error)?;
                invitations.push(from_payload(payload, "staff invitation")?);
            }
            Ok(invitations)
        }
    }
}

/// PostgreSQL implementation of
/// [`RoleAssignmentRepository`](crm_domain::repositories::RoleAssignmentRepository).
#[derive(Debug, Clone)]
pub struct PgRoleAssignmentRepository {
    pool: PgPool,
}

impl PgRoleAssignmentRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl RoleAssignmentRepository for PgRoleAssignmentRepository {
    fn save<'a>(
        &'a self,
        assignment: RoleAssignment,
    ) -> impl Future<Output = Result<RoleAssignment, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&assignment)?;
            sqlx::query(
                "INSERT INTO role_assignments \
                 (assignment_id, staff_id, role_id, scope, assigned_by, status, \
                  assigned_at, revoked_at, payload) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(assignment.assignment_id().as_uuid())
            .bind(assignment.staff_id().as_uuid())
            .bind(assignment.role_id().as_uuid())
            .bind(to_json(&assignment.scope())?)
            .bind(assignment.assigned_by().as_uuid())
            .bind(assignment_status_as_str(assignment.status()))
            .bind(assignment.assigned_at())
            .bind(assignment.revoked_at())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(assignment)
        }
    }

    fn get<'a>(
        &'a self,
        assignment_id: RoleAssignmentId,
    ) -> impl Future<Output = Result<RoleAssignment, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query("SELECT payload FROM role_assignments WHERE assignment_id = $1")
                .bind(assignment_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            let row = row.ok_or_else(|| DomainError::NotFound {
                kind: "role_assignment".to_string(),
                id: assignment_id.to_string(),
            })?;
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "role assignment")
        }
    }

    fn update<'a>(
        &'a self,
        assignment: RoleAssignment,
    ) -> impl Future<Output = Result<RoleAssignment, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&assignment)?;
            let affected = sqlx::query(
                "UPDATE role_assignments \
                 SET staff_id = $2, role_id = $3, scope = $4, assigned_by = $5, \
                     status = $6, revoked_at = $7, payload = $8 \
                 WHERE assignment_id = $1",
            )
            .bind(assignment.assignment_id().as_uuid())
            .bind(assignment.staff_id().as_uuid())
            .bind(assignment.role_id().as_uuid())
            .bind(to_json(&assignment.scope())?)
            .bind(assignment.assigned_by().as_uuid())
            .bind(assignment_status_as_str(assignment.status()))
            .bind(assignment.revoked_at())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?
            .rows_affected();
            if affected == 0 {
                return Err(DomainError::NotFound {
                    kind: "role_assignment".to_string(),
                    id: assignment.assignment_id().to_string(),
                });
            }
            Ok(assignment)
        }
    }

    fn list_for_staff<'a>(
        &'a self,
        staff_id: StaffId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<RoleAssignment>, DomainError>> + Send + 'a {
        async move {
            let rows = sqlx::query(
                "SELECT payload FROM role_assignments WHERE staff_id = $1 \
                 ORDER BY assigned_at DESC, assignment_id DESC LIMIT $2",
            )
            .bind(staff_id.as_uuid())
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(map_storage_error)?;
            let mut assignments = Vec::with_capacity(rows.len());
            for row in rows {
                let payload: serde_json::Value =
                    row.try_get("payload").map_err(map_storage_error)?;
                assignments.push(from_payload(payload, "role assignment")?);
            }
            Ok(assignments)
        }
    }
}

/// PostgreSQL implementation of
/// [`AgencySettingsRepository`](crm_domain::repositories::AgencySettingsRepository).
#[derive(Debug, Clone)]
pub struct PgAgencySettingsRepository {
    pool: PgPool,
}

impl PgAgencySettingsRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl AgencySettingsRepository for PgAgencySettingsRepository {
    fn save<'a>(
        &'a self,
        settings: AgencySettings,
    ) -> impl Future<Output = Result<AgencySettings, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&settings)?;
            let version = i64::try_from(settings.version()).map_err(|_| {
                DomainError::Validation(
                    "storage error: settings version does not fit in BIGINT".to_string(),
                )
            })?;
            sqlx::query(
                "INSERT INTO agency_settings \
                 (settings_id, agency_id, version, payload) VALUES ($1, $2, $3, $4)",
            )
            .bind(Uuid::new_v4())
            .bind(settings.agency_id().as_uuid())
            .bind(version)
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(settings)
        }
    }

    fn get_current<'a>(
        &'a self,
        agency_id: AgencyId,
    ) -> impl Future<Output = Result<AgencySettings, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query(
                "SELECT payload FROM agency_settings \
                 WHERE agency_id = $1 ORDER BY version DESC LIMIT 1",
            )
            .bind(agency_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_storage_error)?;
            let row = row.ok_or_else(|| DomainError::NotFound {
                kind: "agency_settings".to_string(),
                id: "current".to_string(),
            })?;
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "agency settings")
        }
    }

    fn list_versions<'a>(
        &'a self,
        agency_id: AgencyId,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<AgencySettings>, DomainError>> + Send + 'a {
        async move {
            let rows = sqlx::query(
                "SELECT payload FROM agency_settings \
                 WHERE agency_id = $1 ORDER BY version DESC LIMIT $2",
            )
            .bind(agency_id.as_uuid())
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(map_storage_error)?;
            let mut versions = Vec::with_capacity(rows.len());
            for row in rows {
                let payload: serde_json::Value =
                    row.try_get("payload").map_err(map_storage_error)?;
                versions.push(from_payload(payload, "agency settings")?);
            }
            Ok(versions)
        }
    }
}

/// PostgreSQL implementation of
/// [`MaintenanceRepository`](crm_domain::repositories::MaintenanceRepository).
///
/// The singleton row (`id = 1`) is created lazily on the first
/// [`save`](MaintenanceRepository::save): a missing row reads as
/// [`DomainError::NotFound`], and the service layer maps that to the
/// domain default (disabled maintenance mode).
#[derive(Debug, Clone)]
pub struct PgMaintenanceRepository {
    pool: PgPool,
}

impl PgMaintenanceRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl MaintenanceRepository for PgMaintenanceRepository {
    fn save<'a>(
        &'a self,
        mode: MaintenanceMode,
    ) -> impl Future<Output = Result<MaintenanceMode, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&mode)?;
            sqlx::query(
                "INSERT INTO maintenance_state (id, enabled, reason, set_by, set_at, payload) \
                 VALUES (1, $1, $2, $3, $4, $5) \
                 ON CONFLICT (id) DO UPDATE SET \
                     enabled = EXCLUDED.enabled, reason = EXCLUDED.reason, \
                     set_by = EXCLUDED.set_by, set_at = EXCLUDED.set_at, \
                     payload = EXCLUDED.payload",
            )
            .bind(mode.is_enabled())
            .bind(mode.reason())
            .bind(mode.set_by().as_uuid())
            .bind(mode.set_at())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(mode)
        }
    }

    fn get<'a>(
        &'a self,
    ) -> impl Future<Output = Result<Option<MaintenanceMode>, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query("SELECT payload FROM maintenance_state WHERE id = 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            let Some(row) = row else { return Ok(None) };
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "maintenance mode").map(Some)
        }
    }
}

/// PostgreSQL implementation of
/// [`BackupRepository`](crm_domain::repositories::BackupRepository).
#[derive(Debug, Clone)]
pub struct PgBackupRepository {
    pool: PgPool,
}

impl PgBackupRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl BackupRepository for PgBackupRepository {
    fn save<'a>(
        &'a self,
        record: BackupRecord,
    ) -> impl Future<Output = Result<BackupRecord, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&record)?;
            sqlx::query(
                "INSERT INTO backups \
                 (backup_id, status, initiated_by, scheduled_at, payload) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(record.backup_id().as_uuid())
            .bind(backup_status_as_str(record.status()))
            .bind(record.initiated_by().as_uuid())
            .bind(record.scheduled_at())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(record)
        }
    }

    fn get<'a>(
        &'a self,
        backup_id: BackupId,
    ) -> impl Future<Output = Result<BackupRecord, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query("SELECT payload FROM backups WHERE backup_id = $1")
                .bind(backup_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            let row = row.ok_or_else(|| DomainError::NotFound {
                kind: "backup".to_string(),
                id: backup_id.to_string(),
            })?;
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "backup")
        }
    }

    fn update<'a>(
        &'a self,
        record: BackupRecord,
    ) -> impl Future<Output = Result<BackupRecord, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&record)?;
            let affected =
                sqlx::query("UPDATE backups SET status = $2, payload = $3 WHERE backup_id = $1")
                    .bind(record.backup_id().as_uuid())
                    .bind(backup_status_as_str(record.status()))
                    .bind(payload)
                    .execute(&self.pool)
                    .await
                    .map_err(map_storage_error)?
                    .rows_affected();
            if affected == 0 {
                return Err(DomainError::NotFound {
                    kind: "backup".to_string(),
                    id: record.backup_id().to_string(),
                });
            }
            Ok(record)
        }
    }

    fn list<'a>(
        &'a self,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<BackupRecord>, DomainError>> + Send + 'a {
        async move {
            let rows = sqlx::query(
                "SELECT payload FROM backups \
                 ORDER BY scheduled_at DESC, backup_id DESC LIMIT $1",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(map_storage_error)?;
            let mut records = Vec::with_capacity(rows.len());
            for row in rows {
                let payload: serde_json::Value =
                    row.try_get("payload").map_err(map_storage_error)?;
                records.push(from_payload(payload, "backup")?);
            }
            Ok(records)
        }
    }
}

/// PostgreSQL implementation of
/// [`ExportRepository`](crm_domain::repositories::ExportRepository).
#[derive(Debug, Clone)]
pub struct PgExportRepository {
    pool: PgPool,
}

impl PgExportRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl ExportRepository for PgExportRepository {
    fn save<'a>(
        &'a self,
        export: DataExport,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            if let Some(profile_id) = export.client_profile_id() {
                ensure_client_profile(&self.pool, profile_id).await?;
            }
            let payload = to_json(&export)?;
            sqlx::query(
                "INSERT INTO exports \
                 (export_id, client_profile_id, format, status, requested_by, payload) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(export.export_id().as_uuid())
            .bind(export.client_profile_id().map(ClientProfileId::as_uuid))
            .bind(export_format_as_str(export.format()))
            .bind(export_status_as_str(export.status()))
            .bind(export.requested_by().as_uuid())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(export)
        }
    }

    fn get<'a>(
        &'a self,
        export_id: ExportId,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query("SELECT payload FROM exports WHERE export_id = $1")
                .bind(export_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            let row = row.ok_or_else(|| DomainError::NotFound {
                kind: "export".to_string(),
                id: export_id.to_string(),
            })?;
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "export")
        }
    }

    fn update<'a>(
        &'a self,
        export: DataExport,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&export)?;
            let affected =
                sqlx::query("UPDATE exports SET status = $2, payload = $3 WHERE export_id = $1")
                    .bind(export.export_id().as_uuid())
                    .bind(export_status_as_str(export.status()))
                    .bind(payload)
                    .execute(&self.pool)
                    .await
                    .map_err(map_storage_error)?
                    .rows_affected();
            if affected == 0 {
                return Err(DomainError::NotFound {
                    kind: "export".to_string(),
                    id: export.export_id().to_string(),
                });
            }
            Ok(export)
        }
    }

    fn list<'a>(
        &'a self,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<DataExport>, DomainError>> + Send + 'a {
        async move {
            let rows = sqlx::query(
                "SELECT payload FROM exports \
                 ORDER BY requested_at DESC, export_id DESC LIMIT $1",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(map_storage_error)?;
            let mut exports = Vec::with_capacity(rows.len());
            for row in rows {
                let payload: serde_json::Value =
                    row.try_get("payload").map_err(map_storage_error)?;
                exports.push(from_payload(payload, "export")?);
            }
            Ok(exports)
        }
    }
}

/// PostgreSQL implementation of
/// [`UpgradeRepository`](crm_domain::repositories::UpgradeRepository).
#[derive(Debug, Clone)]
pub struct PgUpgradeRepository {
    pool: PgPool,
}

impl PgUpgradeRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl UpgradeRepository for PgUpgradeRepository {
    fn save<'a>(
        &'a self,
        upgrade: SystemUpgrade,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&upgrade)?;
            sqlx::query(
                "INSERT INTO upgrades \
                 (upgrade_id, from_version, to_version, status, scheduled_by, payload) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(upgrade.upgrade_id().as_uuid())
            .bind(upgrade.from_version())
            .bind(upgrade.to_version())
            .bind(upgrade_status_as_str(upgrade.status()))
            .bind(upgrade.scheduled_by().as_uuid())
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(upgrade)
        }
    }

    fn get<'a>(
        &'a self,
        upgrade_id: UpgradeId,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query("SELECT payload FROM upgrades WHERE upgrade_id = $1")
                .bind(upgrade_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            let row = row.ok_or_else(|| DomainError::NotFound {
                kind: "upgrade".to_string(),
                id: upgrade_id.to_string(),
            })?;
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "upgrade")
        }
    }

    fn update<'a>(
        &'a self,
        upgrade: SystemUpgrade,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&upgrade)?;
            let affected =
                sqlx::query("UPDATE upgrades SET status = $2, payload = $3 WHERE upgrade_id = $1")
                    .bind(upgrade.upgrade_id().as_uuid())
                    .bind(upgrade_status_as_str(upgrade.status()))
                    .bind(payload)
                    .execute(&self.pool)
                    .await
                    .map_err(map_storage_error)?
                    .rows_affected();
            if affected == 0 {
                return Err(DomainError::NotFound {
                    kind: "upgrade".to_string(),
                    id: upgrade.upgrade_id().to_string(),
                });
            }
            Ok(upgrade)
        }
    }

    fn list<'a>(
        &'a self,
        limit: u64,
    ) -> impl Future<Output = Result<Vec<SystemUpgrade>, DomainError>> + Send + 'a {
        async move {
            let rows = sqlx::query(
                "SELECT payload FROM upgrades \
                 ORDER BY scheduled_at DESC, upgrade_id DESC LIMIT $1",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(map_storage_error)?;
            let mut upgrades = Vec::with_capacity(rows.len());
            for row in rows {
                let payload: serde_json::Value =
                    row.try_get("payload").map_err(map_storage_error)?;
                upgrades.push(from_payload(payload, "upgrade")?);
            }
            Ok(upgrades)
        }
    }
}

/// PostgreSQL implementation of
/// [`InstallationRepository`](crm_domain::repositories::InstallationRepository).
///
/// The singleton row (`id = 1`) is created lazily on the first
/// [`save`](InstallationRepository::save): a missing row reads as
/// [`DomainError::NotFound`], and the service layer maps that to "the
/// installer never ran" (`None`).
#[derive(Debug, Clone)]
pub struct PgInstallationRepository {
    pool: PgPool,
}

impl PgInstallationRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// The repository trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl InstallationRepository for PgInstallationRepository {
    fn save<'a>(
        &'a self,
        installation: Installation,
    ) -> impl Future<Output = Result<Installation, DomainError>> + Send + 'a {
        async move {
            let payload = to_json(&installation)?;
            sqlx::query(
                "INSERT INTO installation_state (id, state, payload) VALUES (1, $1, $2) \
                 ON CONFLICT (id) DO UPDATE SET \
                     state = EXCLUDED.state, payload = EXCLUDED.payload",
            )
            .bind(installation_state_as_str(installation.state()))
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(installation)
        }
    }

    fn get<'a>(
        &'a self,
    ) -> impl Future<Output = Result<Option<Installation>, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query("SELECT payload FROM installation_state WHERE id = 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            let Some(row) = row else { return Ok(None) };
            let payload: serde_json::Value = row.try_get("payload").map_err(map_storage_error)?;
            from_payload(payload, "installation").map(Some)
        }
    }
}

/// A [`HealthReporter`](crm_domain::observability::HealthReporter)
/// that probes the database with a bounded `SELECT 1`.
///
/// The query carries no parameters and touches no tables, so the
/// check is a pure connectivity probe: it fails only when the pool
/// cannot reach PostgreSQL. The failure reason names the probe, never
/// connection details.
#[derive(Debug, Clone)]
pub struct DbHealthReporter {
    pool: PgPool,
}

impl DbHealthReporter {
    /// Creates the reporter over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl HealthReporter for DbHealthReporter {
    fn check_health(&self) -> impl Future<Output = HealthReport> + Send {
        let pool = self.pool.clone();
        async move {
            let checked_at = Utc::now();
            let health = match sqlx::query("SELECT 1").execute(&pool).await {
                Ok(_) => ComponentHealth::Healthy,
                Err(error) => ComponentHealth::Unhealthy {
                    reason: format!("database connectivity probe failed: {error}"),
                },
            };
            HealthReport {
                component: "database",
                health,
                checked_at,
            }
        }
    }
}
