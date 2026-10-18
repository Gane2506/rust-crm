// SPDX-License-Identifier: GPL-3.0-only
//! Admin and operations service implementation (task 16-ADMIN-02).
//!
//! [`AdminServiceImpl`] implements
//! [`AdminService`](crm_domain::services::AdminService) over the
//! PostgreSQL repositories. It enforces the agency-scope rule of the
//! service contract (client-profile scopes never authorize admin
//! operations), drives the domain lifecycles, and records the pinned
//! `ADMIN_*` audit events.
//!
//! Audit failures are swallowed: audit is fail-loud at the HTTP layer
//! and never fails an admin operation (same convention as
//! [`BillingServiceImpl`](crate::billing::BillingServiceImpl)).
//!
//! [`AdminServiceImpl`] also carries four read helpers that are not
//! part of the [`AdminService`](crm_domain::services::AdminService)
//! trait (defined by 16-ADMIN-01): [`get_invitation`](AdminServiceImpl::get_invitation),
//! [`get_backup`](AdminServiceImpl::get_backup),
//! [`get_export`](AdminServiceImpl::get_export) and
//! [`get_upgrade`](AdminServiceImpl::get_upgrade). They exist for the
//! HTTP `GET` endpoints; the trait itself is left untouched.

use std::future::Future;

use chrono::Utc;

use crm_domain::admin::{
    summarize_health, AgencySettings, BackupRecord, DataExport, Installation, MaintenanceMode,
    RoleAssignment, StaffInvitation, SystemHealthSummary, SystemUpgrade, ADMIN_BACKUP_COMPLETED,
    ADMIN_BACKUP_FAILED, ADMIN_BACKUP_STARTED, ADMIN_EXPORT_FAILED, ADMIN_EXPORT_READY,
    ADMIN_EXPORT_REQUESTED, ADMIN_INSTALLATION_COMPLETED, ADMIN_INVITATION_ACCEPTED,
    ADMIN_INVITATION_REVOKED, ADMIN_MAINTENANCE_DISABLED, ADMIN_MAINTENANCE_ENABLED,
    ADMIN_ROLE_ASSIGNED, ADMIN_ROLE_REVOKED, ADMIN_SETTINGS_UPDATED, ADMIN_STAFF_DISABLED,
    ADMIN_STAFF_INVITED, ADMIN_STAFF_REACTIVATED, ADMIN_UPGRADE_COMPLETED, ADMIN_UPGRADE_FAILED,
    ADMIN_UPGRADE_ROLLED_BACK, ADMIN_UPGRADE_STARTED,
};
use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{BackupId, ExportId, InvitationId, RoleAssignmentId, StaffId, UpgradeId};
use crm_domain::observability::HealthReporter;
use crm_domain::repositories::{
    AgencySettingsRepository, BackupRepository, ExportRepository, InstallationRepository,
    MaintenanceRepository, RoleAssignmentRepository, StaffInvitationRepository, StaffRepository,
    UpgradeRepository,
};
use crm_domain::security::{StaffStatus, StaffUser};
use crm_domain::services::{
    ActionContext, AdminService, AuditService, NewExport, NewRoleAssignment, NewStaffInvitation,
    NewUpgrade, UpdateAgencySettings,
};

/// PostgreSQL-backed implementation of
/// [`AdminService`](crm_domain::services::AdminService).
///
/// The service owns the cross-entity admin rules:
/// * every method rejects client-profile-scoped contexts — admin is
///   agency-wide by construction (grant checks happen upstream, at
///   the HTTP layer);
/// * accepting an invitation creates the [`StaffUser`] account (in the
///   inviter's agency) and marks the invitation accepted;
/// * a staff member cannot disable their own account;
/// * the first settings update creates version 1 (the agency comes
///   from the acting staff member's record);
/// * a missing maintenance/installation singleton row reads as the
///   domain default instead of an error.
#[derive(Debug, Clone)]
pub struct AdminServiceImpl<I, RA, S, M, ST, B, E, U, N, A, H> {
    invitations: I,
    role_assignments: RA,
    settings: S,
    maintenance: M,
    staff: ST,
    backups: B,
    exports: E,
    upgrades: U,
    installation: N,
    audit: A,
    health_reporters: Vec<H>,
}

impl<I, RA, S, M, ST, B, E, U, N, A, H> AdminServiceImpl<I, RA, S, M, ST, B, E, U, N, A, H> {
    /// Creates the service over the admin repositories, the staff
    /// repository, the audit service and the health reporters folded
    /// into [`get_system_health`](AdminService::get_system_health).
    ///
    /// The argument count is the service's dependency set, not
    /// incidental complexity: every repository backs a distinct
    /// [`AdminService`] method group, so grouping them into fewer
    /// parameters would only hide the wiring.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        invitations: I,
        role_assignments: RA,
        settings: S,
        maintenance: M,
        staff: ST,
        backups: B,
        exports: E,
        upgrades: U,
        installation: N,
        audit: A,
        health_reporters: Vec<H>,
    ) -> Self {
        Self {
            invitations,
            role_assignments,
            settings,
            maintenance,
            staff,
            backups,
            exports,
            upgrades,
            installation,
            audit,
            health_reporters,
        }
    }

    /// Records an audit event for an admin operation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails
    /// the operation).
    async fn audit_admin(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_kind: &str,
        target_id: &str,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                target_kind,
                target_id,
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
    }

    /// Fetches an invitation for a lifecycle step.
    async fn scoped_invitation(
        &self,
        invitation_id: InvitationId,
    ) -> Result<StaffInvitation, DomainError>
    where
        I: StaffInvitationRepository,
    {
        self.invitations.get(invitation_id).await
    }

    /// Fetches a backup record for a lifecycle step.
    async fn scoped_backup(&self, backup_id: BackupId) -> Result<BackupRecord, DomainError>
    where
        B: BackupRepository,
    {
        self.backups.get(backup_id).await
    }

    /// Fetches an export for a lifecycle step.
    async fn scoped_export(&self, export_id: ExportId) -> Result<DataExport, DomainError>
    where
        E: ExportRepository,
    {
        self.exports.get(export_id).await
    }

    /// Fetches an upgrade for a lifecycle step.
    async fn scoped_upgrade(&self, upgrade_id: UpgradeId) -> Result<SystemUpgrade, DomainError>
    where
        U: UpgradeRepository,
    {
        self.upgrades.get(upgrade_id).await
    }

    /// Loads the maintenance mode, defaulting to disabled when the
    /// singleton row was never written.
    async fn load_maintenance(&self, ctx: &ActionContext) -> Result<MaintenanceMode, DomainError>
    where
        M: MaintenanceRepository,
    {
        match self.maintenance.get().await {
            Ok(Some(mode)) => Ok(mode),
            Ok(None) => Ok(MaintenanceMode::disabled(actor_staff_id(ctx), Utc::now())),
            Err(error) => Err(error),
        }
    }

    /// Fetches a single invitation. Read-only: no audit event.
    ///
    /// This is an [`AdminServiceImpl`] helper rather than an
    /// [`AdminService`](crm_domain::services::AdminService) trait
    /// method (the trait, defined by 16-ADMIN-01, has no single-get
    /// for invitations); it exists for the HTTP `GET` endpoint.
    pub async fn get_invitation(
        &self,
        invitation_id: InvitationId,
        ctx: &ActionContext,
    ) -> Result<StaffInvitation, DomainError>
    where
        I: StaffInvitationRepository,
    {
        require_agency_scope(ctx)?;
        self.invitations.get(invitation_id).await
    }

    /// Fetches a single backup record. Read-only: no audit event.
    ///
    /// See [`get_invitation`](AdminServiceImpl::get_invitation) for
    /// why this is an inherent method rather than a trait method.
    pub async fn get_backup(
        &self,
        backup_id: BackupId,
        ctx: &ActionContext,
    ) -> Result<BackupRecord, DomainError>
    where
        B: BackupRepository,
    {
        require_agency_scope(ctx)?;
        self.backups.get(backup_id).await
    }

    /// Fetches a single data export. Read-only: no audit event.
    ///
    /// See [`get_invitation`](AdminServiceImpl::get_invitation) for
    /// why this is an inherent method rather than a trait method.
    pub async fn get_export(
        &self,
        export_id: ExportId,
        ctx: &ActionContext,
    ) -> Result<DataExport, DomainError>
    where
        E: ExportRepository,
    {
        require_agency_scope(ctx)?;
        self.exports.get(export_id).await
    }

    /// Fetches a single system upgrade. Read-only: no audit event.
    ///
    /// See [`get_invitation`](AdminServiceImpl::get_invitation) for
    /// why this is an inherent method rather than a trait method.
    pub async fn get_upgrade(
        &self,
        upgrade_id: UpgradeId,
        ctx: &ActionContext,
    ) -> Result<SystemUpgrade, DomainError>
    where
        U: UpgradeRepository,
    {
        require_agency_scope(ctx)?;
        self.upgrades.get(upgrade_id).await
    }
}

/// Renders the acting staff id from the action context.
///
/// Admin operations are expected to come from staff sessions; the
/// actor's UUID is recorded as the staff actor (`invited_by`,
/// `assigned_by`, `set_by`, ...).
fn actor_staff_id(ctx: &ActionContext) -> StaffId {
    StaffId::from(ctx.actor_id.as_uuid())
}

/// Rejects client-profile-scoped contexts: admin is agency-wide by
/// construction, so a profile scope can never authorize an admin
/// operation. Agency-scoped contexts (`None`) pass — grant checks
/// happen upstream, at the HTTP layer.
fn require_agency_scope(ctx: &ActionContext) -> Result<(), DomainError> {
    if ctx.client_profile_id.is_some() {
        return Err(DomainError::Forbidden(
            "admin operations require an agency-scoped context".to_string(),
        ));
    }
    Ok(())
}

// The service trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl<I, RA, S, M, ST, B, E, U, N, A, H> AdminService
    for AdminServiceImpl<I, RA, S, M, ST, B, E, U, N, A, H>
where
    I: StaffInvitationRepository + Send + Sync,
    RA: RoleAssignmentRepository + Send + Sync,
    S: AgencySettingsRepository + Send + Sync,
    M: MaintenanceRepository + Send + Sync,
    ST: StaffRepository + Send + Sync,
    B: BackupRepository + Send + Sync,
    E: ExportRepository + Send + Sync,
    U: UpgradeRepository + Send + Sync,
    N: InstallationRepository + Send + Sync,
    A: AuditService + Send + Sync,
    H: HealthReporter + Send + Sync,
{
    fn invite_staff<'a>(
        &'a self,
        input: NewStaffInvitation,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<StaffInvitation, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let invitation = StaffInvitation::new(
                input.email,
                input.role_ids,
                actor_staff_id(ctx),
                input.expires_at,
                Utc::now(),
            )?;
            let invitation = self.invitations.save(invitation).await?;
            self.audit_admin(
                ctx,
                ADMIN_STAFF_INVITED,
                "staff_invitation",
                &invitation.invitation_id().to_string(),
            )
            .await;
            Ok(invitation)
        }
    }

    fn accept_invitation<'a>(
        &'a self,
        invitation_id: InvitationId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<StaffInvitation, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut invitation = self.scoped_invitation(invitation_id).await?;
            let now = Utc::now();
            invitation.accept(now)?;
            // The new account joins the inviter's agency. The trait
            // documents that accepting creates the staff user; the
            // signature returns the accepted invitation.
            let inviter = self.staff.get(invitation.invited_by()).await?;
            let email = invitation.email().to_owned();
            let user = StaffUser {
                staff_id: StaffId::new(),
                agency_id: inviter.agency_id,
                email: email.clone(),
                display_name: email,
                status: StaffStatus::Active,
                credential: None,
                created_at: now,
                updated_at: now,
            };
            self.staff.create(user).await?;
            let invitation = self.invitations.update(invitation).await?;
            self.audit_admin(
                ctx,
                ADMIN_INVITATION_ACCEPTED,
                "staff_invitation",
                &invitation.invitation_id().to_string(),
            )
            .await;
            Ok(invitation)
        }
    }

    fn revoke_invitation<'a>(
        &'a self,
        invitation_id: InvitationId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<StaffInvitation, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut invitation = self.scoped_invitation(invitation_id).await?;
            invitation.revoke()?;
            let invitation = self.invitations.update(invitation).await?;
            self.audit_admin(
                ctx,
                ADMIN_INVITATION_REVOKED,
                "staff_invitation",
                &invitation.invitation_id().to_string(),
            )
            .await;
            Ok(invitation)
        }
    }

    fn list_invitations<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<StaffInvitation>, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            self.invitations.list(limit).await
        }
    }

    fn disable_staff<'a>(
        &'a self,
        staff_id: StaffId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            if staff_id == actor_staff_id(ctx) {
                return Err(DomainError::Validation(
                    "a staff member cannot disable their own account".to_string(),
                ));
            }
            let mut user = self.staff.get(staff_id).await?;
            user.suspend();
            self.staff.update(user).await?;
            self.audit_admin(
                ctx,
                ADMIN_STAFF_DISABLED,
                "staff_user",
                &staff_id.to_string(),
            )
            .await;
            Ok(())
        }
    }

    fn reactivate_staff<'a>(
        &'a self,
        staff_id: StaffId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut user = self.staff.get(staff_id).await?;
            user.reactivate();
            self.staff.update(user).await?;
            self.audit_admin(
                ctx,
                ADMIN_STAFF_REACTIVATED,
                "staff_user",
                &staff_id.to_string(),
            )
            .await;
            Ok(())
        }
    }

    fn assign_role<'a>(
        &'a self,
        input: NewRoleAssignment,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<RoleAssignment, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            // `RoleAssignment::new` rejects self-assignment at the
            // domain level.
            let assignment = RoleAssignment::new(
                input.staff_id,
                input.role_id,
                input.scope,
                actor_staff_id(ctx),
                Utc::now(),
            )?;
            let assignment = self.role_assignments.save(assignment).await?;
            self.audit_admin(
                ctx,
                ADMIN_ROLE_ASSIGNED,
                "role_assignment",
                &assignment.assignment_id().to_string(),
            )
            .await;
            Ok(assignment)
        }
    }

    fn revoke_role_assignment<'a>(
        &'a self,
        assignment_id: RoleAssignmentId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<RoleAssignment, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut assignment = self.role_assignments.get(assignment_id).await?;
            assignment.revoke(Utc::now());
            let assignment = self.role_assignments.update(assignment).await?;
            self.audit_admin(
                ctx,
                ADMIN_ROLE_REVOKED,
                "role_assignment",
                &assignment.assignment_id().to_string(),
            )
            .await;
            Ok(assignment)
        }
    }

    fn list_role_assignments<'a>(
        &'a self,
        staff_id: StaffId,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<RoleAssignment>, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            self.role_assignments.list_for_staff(staff_id, limit).await
        }
    }

    fn get_settings<'a>(
        &'a self,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<AgencySettings, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let actor = self.staff.get(actor_staff_id(ctx)).await?;
            self.settings.get_current(actor.agency_id).await
        }
    }

    fn update_settings<'a>(
        &'a self,
        input: UpdateAgencySettings,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<AgencySettings, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let actor = actor_staff_id(ctx);
            let now = Utc::now();
            let staff_user = self.staff.get(actor).await?;
            let settings = match self.settings.get_current(staff_user.agency_id).await {
                Ok(current) => current.update(
                    input.display_name,
                    input.support_email,
                    input.session_ttl_secs,
                    actor,
                    now,
                )?,
                Err(DomainError::NotFound { .. }) => {
                    // First version: the agency comes from the acting
                    // staff member's record, and the display name is
                    // required because there is nothing to inherit.
                    let display_name = input.display_name.ok_or_else(|| {
                        DomainError::Validation(
                            "no agency settings exist yet; display_name is required \
                             to create the first version"
                                .to_string(),
                        )
                    })?;
                    let staff_user = self.staff.get(actor).await?;
                    AgencySettings::new(
                        staff_user.agency_id,
                        display_name,
                        input.support_email.flatten(),
                        input.session_ttl_secs.ok_or_else(|| {
                            DomainError::Validation(
                                "no agency settings exist yet; session_ttl_secs is required \
                                 to create the first version"
                                    .to_string(),
                            )
                        })?,
                        actor,
                        now,
                    )?
                }
                Err(error) => return Err(error),
            };
            let settings = self.settings.save(settings).await?;
            self.audit_admin(
                ctx,
                ADMIN_SETTINGS_UPDATED,
                "agency_settings",
                &settings.version().to_string(),
            )
            .await;
            Ok(settings)
        }
    }

    fn get_maintenance<'a>(
        &'a self,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<MaintenanceMode, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            self.load_maintenance(ctx).await
        }
    }

    fn enable_maintenance<'a>(
        &'a self,
        reason: String,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<MaintenanceMode, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut mode = self.load_maintenance(ctx).await?;
            mode.enable(reason, actor_staff_id(ctx), Utc::now())?;
            let mode = self.maintenance.save(mode).await?;
            self.audit_admin(
                ctx,
                ADMIN_MAINTENANCE_ENABLED,
                "maintenance_mode",
                "singleton",
            )
            .await;
            Ok(mode)
        }
    }

    fn disable_maintenance<'a>(
        &'a self,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<MaintenanceMode, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut mode = self.load_maintenance(ctx).await?;
            mode.disable(actor_staff_id(ctx), Utc::now());
            let mode = self.maintenance.save(mode).await?;
            self.audit_admin(
                ctx,
                ADMIN_MAINTENANCE_DISABLED,
                "maintenance_mode",
                "singleton",
            )
            .await;
            Ok(mode)
        }
    }

    fn get_system_health<'a>(
        &'a self,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<SystemHealthSummary, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut reports = Vec::with_capacity(self.health_reporters.len());
            for reporter in &self.health_reporters {
                reports.push(reporter.check_health().await);
            }
            Ok(summarize_health(reports, Utc::now()))
        }
    }

    fn schedule_backup<'a>(
        &'a self,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<BackupRecord, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let record = BackupRecord::schedule(actor_staff_id(ctx), Utc::now());
            self.backups.save(record).await
        }
    }

    fn start_backup<'a>(
        &'a self,
        backup_id: BackupId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<BackupRecord, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut record = self.scoped_backup(backup_id).await?;
            record.start(Utc::now())?;
            let record = self.backups.update(record).await?;
            self.audit_admin(
                ctx,
                ADMIN_BACKUP_STARTED,
                "backup",
                &record.backup_id().to_string(),
            )
            .await;
            Ok(record)
        }
    }

    fn complete_backup<'a>(
        &'a self,
        backup_id: BackupId,
        size_bytes: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<BackupRecord, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut record = self.scoped_backup(backup_id).await?;
            record.complete(size_bytes, Utc::now())?;
            let record = self.backups.update(record).await?;
            self.audit_admin(
                ctx,
                ADMIN_BACKUP_COMPLETED,
                "backup",
                &record.backup_id().to_string(),
            )
            .await;
            Ok(record)
        }
    }

    fn fail_backup<'a>(
        &'a self,
        backup_id: BackupId,
        reason: String,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<BackupRecord, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut record = self.scoped_backup(backup_id).await?;
            record.fail(reason, Utc::now())?;
            let record = self.backups.update(record).await?;
            self.audit_admin(
                ctx,
                ADMIN_BACKUP_FAILED,
                "backup",
                &record.backup_id().to_string(),
            )
            .await;
            Ok(record)
        }
    }

    fn list_backups<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<BackupRecord>, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            self.backups.list(limit).await
        }
    }

    fn request_export<'a>(
        &'a self,
        input: NewExport,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let export = DataExport::request(
                input.client_profile_id,
                input.format,
                actor_staff_id(ctx),
                Utc::now(),
            );
            let export = self.exports.save(export).await?;
            self.audit_admin(
                ctx,
                ADMIN_EXPORT_REQUESTED,
                "export",
                &export.export_id().to_string(),
            )
            .await;
            Ok(export)
        }
    }

    fn start_export<'a>(
        &'a self,
        export_id: ExportId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut export = self.scoped_export(export_id).await?;
            export.start()?;
            self.exports.update(export).await
        }
    }

    fn complete_export<'a>(
        &'a self,
        export_id: ExportId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut export = self.scoped_export(export_id).await?;
            export.complete(Utc::now())?;
            let export = self.exports.update(export).await?;
            self.audit_admin(
                ctx,
                ADMIN_EXPORT_READY,
                "export",
                &export.export_id().to_string(),
            )
            .await;
            Ok(export)
        }
    }

    fn fail_export<'a>(
        &'a self,
        export_id: ExportId,
        reason: String,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut export = self.scoped_export(export_id).await?;
            export.fail(reason)?;
            let export = self.exports.update(export).await?;
            self.audit_admin(
                ctx,
                ADMIN_EXPORT_FAILED,
                "export",
                &export.export_id().to_string(),
            )
            .await;
            Ok(export)
        }
    }

    fn expire_export<'a>(
        &'a self,
        export_id: ExportId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<DataExport, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut export = self.scoped_export(export_id).await?;
            export.expire()?;
            self.exports.update(export).await
        }
    }

    fn list_exports<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<DataExport>, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            self.exports.list(limit).await
        }
    }

    fn schedule_upgrade<'a>(
        &'a self,
        input: NewUpgrade,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let upgrade = SystemUpgrade::schedule(
                input.from_version,
                input.to_version,
                actor_staff_id(ctx),
                Utc::now(),
            )?;
            self.upgrades.save(upgrade).await
        }
    }

    fn start_upgrade<'a>(
        &'a self,
        upgrade_id: UpgradeId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut upgrade = self.scoped_upgrade(upgrade_id).await?;
            upgrade.start()?;
            let upgrade = self.upgrades.update(upgrade).await?;
            self.audit_admin(
                ctx,
                ADMIN_UPGRADE_STARTED,
                "upgrade",
                &upgrade.upgrade_id().to_string(),
            )
            .await;
            Ok(upgrade)
        }
    }

    fn complete_upgrade<'a>(
        &'a self,
        upgrade_id: UpgradeId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut upgrade = self.scoped_upgrade(upgrade_id).await?;
            upgrade.complete(Utc::now())?;
            let upgrade = self.upgrades.update(upgrade).await?;
            self.audit_admin(
                ctx,
                ADMIN_UPGRADE_COMPLETED,
                "upgrade",
                &upgrade.upgrade_id().to_string(),
            )
            .await;
            Ok(upgrade)
        }
    }

    fn fail_upgrade<'a>(
        &'a self,
        upgrade_id: UpgradeId,
        reason: String,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut upgrade = self.scoped_upgrade(upgrade_id).await?;
            upgrade.fail(reason, Utc::now())?;
            let upgrade = self.upgrades.update(upgrade).await?;
            self.audit_admin(
                ctx,
                ADMIN_UPGRADE_FAILED,
                "upgrade",
                &upgrade.upgrade_id().to_string(),
            )
            .await;
            Ok(upgrade)
        }
    }

    fn rollback_upgrade<'a>(
        &'a self,
        upgrade_id: UpgradeId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<SystemUpgrade, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut upgrade = self.scoped_upgrade(upgrade_id).await?;
            upgrade.rollback(Utc::now())?;
            let upgrade = self.upgrades.update(upgrade).await?;
            self.audit_admin(
                ctx,
                ADMIN_UPGRADE_ROLLED_BACK,
                "upgrade",
                &upgrade.upgrade_id().to_string(),
            )
            .await;
            Ok(upgrade)
        }
    }

    fn list_upgrades<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<SystemUpgrade>, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            self.upgrades.list(limit).await
        }
    }

    fn get_installation<'a>(
        &'a self,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Option<Installation>, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            match self.installation.get().await {
                Ok(installation) => Ok(installation),
                Err(error) => Err(error),
            }
        }
    }

    fn begin_installation<'a>(
        &'a self,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Installation, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut installation = match self.installation.get().await {
                Ok(Some(installation)) => installation,
                Ok(None) => Installation::not_started(),
                Err(error) => return Err(error),
            };
            installation.begin(Utc::now())?;
            self.installation.save(installation).await
        }
    }

    fn complete_installation<'a>(
        &'a self,
        version: String,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Installation, DomainError>> + Send + 'a {
        async move {
            require_agency_scope(ctx)?;
            let mut installation = match self.installation.get().await? {
                Some(installation) => installation,
                None => Installation::not_started(),
            };
            installation.complete(version, Utc::now())?;
            let installation = self.installation.save(installation).await?;
            self.audit_admin(
                ctx,
                ADMIN_INSTALLATION_COMPLETED,
                "installation",
                "singleton",
            )
            .await;
            Ok(installation)
        }
    }
}
