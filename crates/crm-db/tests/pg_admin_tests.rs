// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Admin and operations vertical slice (task
//! 16-ADMIN-02).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.
//! `postgres://postgres:postgres@localhost:5432/crm_test`
//!
//! ```sh
//! DATABASE_URL=<redacted>
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Each test creates its own database, migrates a fresh schema, and
//! drops it afterwards. The service tests wire the real SQLx
//! repositories with the database pool and a no-op audit service.

mod common;

use std::future::Future;

use chrono::{Duration, SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::admin::AdminServiceImpl;
use crm_db::admin::{
    DbHealthReporter, PgAgencySettingsRepository, PgBackupRepository, PgExportRepository,
    PgInstallationRepository, PgMaintenanceRepository, PgRoleAssignmentRepository,
    PgStaffInvitationRepository, PgUpgradeRepository,
};
use crm_db::postgres::PgStaffRepository;
use crm_db::run_migrations;
use crm_domain::admin::{
    AssignmentStatus, BackupStatus, ExportFormat, ExportStatus, InstallationState,
    InvitationStatus, UpgradeStatus,
};
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AgencyId, AuditEventId, ClientProfileId, RoleId, StaffId};
use crm_domain::repositories::{
    ExportRepository, InstallationRepository, MaintenanceRepository, StaffRepository,
    UpgradeRepository,
};
use crm_domain::security::{Scope, StaffStatus, StaffUser};
use crm_domain::services::{
    ActionContext, AdminService, AuditService, NewExport, NewRoleAssignment, NewStaffInvitation,
    NewUpgrade, UpdateAgencySettings,
};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// No-op [`AuditService`] for service tests: audit must never fail the
/// operation under test.
#[derive(Debug, Clone, Default)]
struct NoopAudit;

impl AuditService for NoopAudit {
    // The trait declares `fn -> impl Future` (not `async fn`), so the
    // impl must keep that exact form.
    #[allow(clippy::manual_async_fn)]
    fn record_action<'a>(
        &'a self,
        ctx: &'a ActionContext,
        action: &'a str,
        target_kind: &'a str,
        target_id: &'a str,
        permission_decision: PermissionDecision,
        result: ActionResult,
        _before_ref: Option<&'a str>,
        _after_ref: Option<&'a str>,
    ) -> impl Future<Output = Result<AuditEvent, DomainError>> + Send + 'a {
        async move {
            Ok(AuditEvent {
                event_id: AuditEventId::new(),
                occurred_at: Utc::now().round_subsecs(6),
                actor_kind: ActorKind::System,
                actor_id: ctx.actor_id,
                client_profile_id: ctx.client_profile_id,
                action: action.to_owned(),
                target_kind: target_kind.to_owned(),
                target_id: target_id.to_owned(),
                correlation_id: ctx.correlation_id,
                permission_decision,
                result,
                before_ref: None,
                after_ref: None,
                chain_hash: [0u8; 32],
            })
        }
    }
}

type TestService = AdminServiceImpl<
    PgStaffInvitationRepository,
    PgRoleAssignmentRepository,
    PgAgencySettingsRepository,
    PgMaintenanceRepository,
    PgStaffRepository,
    PgBackupRepository,
    PgExportRepository,
    PgUpgradeRepository,
    PgInstallationRepository,
    NoopAudit,
    DbHealthReporter,
>;

/// Builds the real service over `pool` with a no-op audit service and
/// a database health reporter.
fn service(pool: &PgPool) -> TestService {
    AdminServiceImpl::new(
        PgStaffInvitationRepository::new(pool.clone()),
        PgRoleAssignmentRepository::new(pool.clone()),
        PgAgencySettingsRepository::new(pool.clone()),
        PgMaintenanceRepository::new(pool.clone()),
        PgStaffRepository::new(pool.clone()),
        PgBackupRepository::new(pool.clone()),
        PgExportRepository::new(pool.clone()),
        PgUpgradeRepository::new(pool.clone()),
        PgInstallationRepository::new(pool.clone()),
        NoopAudit,
        vec![DbHealthReporter::new(pool.clone())],
    )
}

/// Test action context for `actor`, agency-scoped unless `profile` is
/// given.
fn ctx_for(actor: StaffId, profile: Option<ClientProfileId>) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::from(actor),
        client_profile_id: profile,
        correlation_id: Uuid::new_v4(),
    }
}

/// Creates an active staff user and returns it.
async fn make_staff(pool: &PgPool, email: &str) -> StaffUser {
    let repo = PgStaffRepository::new(pool.clone());
    let now = Utc::now().round_subsecs(6);
    repo.create(StaffUser {
        staff_id: StaffId::new(),
        agency_id: AgencyId::new(),
        email: email.to_owned(),
        display_name: "Test Admin".to_owned(),
        status: StaffStatus::Active,
        credential: None,
        created_at: now,
        updated_at: now,
    })
    .await
    .expect("create staff")
}

#[tokio::test]
async fn invitation_lifecycle() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let inviter = make_staff(&pool, "inviter@example.com").await;
    let ctx = ctx_for(inviter.staff_id, None);

    // Invite.
    let invitation = service
        .invite_staff(
            NewStaffInvitation {
                email: "newbie@example.com".to_string(),
                role_ids: vec![RoleId::new()],
                expires_at: Utc::now() + Duration::days(7),
            },
            &ctx,
        )
        .await
        .expect("invite staff");
    assert_eq!(invitation.status(), InvitationStatus::Pending);
    assert_eq!(invitation.email(), "newbie@example.com");

    // Listed, newest first.
    let invitations = service
        .list_invitations(10, &ctx)
        .await
        .expect("list invitations");
    assert!(invitations
        .iter()
        .any(|i| i.invitation_id() == invitation.invitation_id()));

    // Accepting creates the staff account in the inviter's agency and
    // marks the invitation accepted.
    let accepted = service
        .accept_invitation(invitation.invitation_id(), &ctx)
        .await
        .expect("accept invitation");
    assert_eq!(accepted.status(), InvitationStatus::Accepted);
    let staff_repo = PgStaffRepository::new(pool.clone());
    let created = staff_repo
        .find_by_email("newbie@example.com")
        .await
        .expect("find created staff")
        .expect("accepted invitation creates the staff user");
    assert_eq!(created.agency_id, inviter.agency_id);
    assert_eq!(created.status, StaffStatus::Active);

    // Accepting twice is rejected: the invitation is no longer pending.
    service
        .accept_invitation(invitation.invitation_id(), &ctx)
        .await
        .expect_err("re-accepting must fail");

    // Revoke a second invitation; revoking is terminal.
    let second = service
        .invite_staff(
            NewStaffInvitation {
                email: "revoked@example.com".to_string(),
                role_ids: vec![RoleId::new()],
                expires_at: Utc::now() + Duration::days(7),
            },
            &ctx,
        )
        .await
        .expect("invite again");
    let revoked = service
        .revoke_invitation(second.invitation_id(), &ctx)
        .await
        .expect("revoke invitation");
    assert_eq!(revoked.status(), InvitationStatus::Revoked);
    service
        .accept_invitation(second.invitation_id(), &ctx)
        .await
        .expect_err("accepting a revoked invitation must fail");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn role_assignment_and_self_assignment_rejected() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let member = make_staff(&pool, "member@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);
    let scope = Scope::Agency {
        agency_id: admin.agency_id,
    };

    // Assign a role to another staff member.
    let assignment = service
        .assign_role(
            NewRoleAssignment {
                staff_id: member.staff_id,
                role_id: RoleId::new(),
                scope,
            },
            &ctx,
        )
        .await
        .expect("assign role");
    assert_eq!(assignment.status(), AssignmentStatus::Active);

    // Listed for the member.
    let assignments = service
        .list_role_assignments(member.staff_id, 10, &ctx)
        .await
        .expect("list assignments");
    assert_eq!(assignments.len(), 1);

    // Self-assignment is rejected at the domain level.
    service
        .assign_role(
            NewRoleAssignment {
                staff_id: admin.staff_id,
                role_id: RoleId::new(),
                scope,
            },
            &ctx,
        )
        .await
        .expect_err("self-assignment must be rejected");

    // Revoke; revocation is idempotent per the domain contract —
    // a second revoke is a no-op so retried commands stay safe.
    let revoked = service
        .revoke_role_assignment(assignment.assignment_id(), &ctx)
        .await
        .expect("revoke assignment");
    assert_eq!(revoked.status(), AssignmentStatus::Revoked);
    let revoked_again = service
        .revoke_role_assignment(assignment.assignment_id(), &ctx)
        .await
        .expect("double revoke is idempotent");
    assert_eq!(revoked_again.status(), AssignmentStatus::Revoked);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn staff_disable_and_reactivate() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let member = make_staff(&pool, "member@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);
    let staff_repo = PgStaffRepository::new(pool.clone());

    service
        .disable_staff(member.staff_id, &ctx)
        .await
        .expect("disable staff");
    let disabled = staff_repo
        .get(member.staff_id)
        .await
        .expect("fetch disabled staff");
    assert_eq!(disabled.status, StaffStatus::Suspended);

    // A staff member cannot disable their own account.
    service
        .disable_staff(admin.staff_id, &ctx)
        .await
        .expect_err("self-disable must be rejected");

    service
        .reactivate_staff(member.staff_id, &ctx)
        .await
        .expect("reactivate staff");
    let reactivated = staff_repo
        .get(member.staff_id)
        .await
        .expect("fetch reactivated staff");
    assert_eq!(reactivated.status, StaffStatus::Active);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn settings_versioning() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    // No settings yet.
    service
        .get_settings(&ctx)
        .await
        .expect_err("no settings version stored yet");

    // First update creates version 1.
    let v1 = service
        .update_settings(
            UpdateAgencySettings {
                display_name: Some("Acme Agency".to_string()),
                support_email: Some(Some("support@acme.example".to_string())),
                session_ttl_secs: Some(43_200),
            },
            &ctx,
        )
        .await
        .expect("create first settings version");
    assert_eq!(v1.version(), 1);
    assert_eq!(v1.agency_id(), admin.agency_id);

    // Partial update bumps the version and keeps untouched fields.
    let v2 = service
        .update_settings(
            UpdateAgencySettings {
                display_name: Some("Acme Agency Ltd".to_string()),
                support_email: None,
                session_ttl_secs: None,
            },
            &ctx,
        )
        .await
        .expect("update settings");
    assert_eq!(v2.version(), 2);
    assert_eq!(v2.display_name(), "Acme Agency Ltd");
    assert_eq!(
        v2.support_email(),
        Some("support@acme.example"),
        "untouched fields carry over"
    );

    // get_current reads the highest version.
    let current = service.get_settings(&ctx).await.expect("get settings");
    assert_eq!(current.version(), 2);

    // Clearing the support email.
    let v3 = service
        .update_settings(
            UpdateAgencySettings {
                display_name: None,
                support_email: Some(None),
                session_ttl_secs: None,
            },
            &ctx,
        )
        .await
        .expect("clear support email");
    assert_eq!(v3.version(), 3);
    assert_eq!(v3.support_email(), None);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn maintenance_singleton() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    // No row yet: reads as the domain default (disabled).
    let mode = service
        .get_maintenance(&ctx)
        .await
        .expect("default maintenance mode");
    assert!(!mode.is_enabled());

    let enabled = service
        .enable_maintenance("deploying v2".to_string(), &ctx)
        .await
        .expect("enable maintenance");
    assert!(enabled.is_enabled());
    assert_eq!(enabled.reason(), Some("deploying v2"));

    // Enabling without a reason is rejected.
    service
        .enable_maintenance("   ".to_string(), &ctx)
        .await
        .expect_err("blank maintenance reason must be rejected");

    let disabled = service
        .disable_maintenance(&ctx)
        .await
        .expect("disable maintenance");
    assert!(!disabled.is_enabled());

    // The singleton row round-trips through the repository.
    let repo = PgMaintenanceRepository::new(pool.clone());
    let stored = repo.get().await.expect("read singleton row");
    let stored = stored.expect("singleton row present");
    assert!(!stored.is_enabled());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn backup_lifecycle() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    let scheduled = service
        .schedule_backup(&ctx)
        .await
        .expect("schedule backup");
    assert_eq!(scheduled.status(), BackupStatus::Scheduled);

    // Completing a scheduled backup without starting is rejected.
    service
        .complete_backup(scheduled.backup_id(), 1024, &ctx)
        .await
        .expect_err("complete-before-start must fail");

    let started = service
        .start_backup(scheduled.backup_id(), &ctx)
        .await
        .expect("start backup");
    assert_eq!(started.status(), BackupStatus::InProgress);

    let completed = service
        .complete_backup(scheduled.backup_id(), 1_048_576, &ctx)
        .await
        .expect("complete backup");
    assert_eq!(completed.status(), BackupStatus::Completed);
    assert_eq!(completed.size_bytes(), Some(1_048_576));

    // Terminal states lock.
    service
        .fail_backup(scheduled.backup_id(), "too late".to_string(), &ctx)
        .await
        .expect_err("failing a completed backup must fail");

    let backups = service.list_backups(10, &ctx).await.expect("list backups");
    assert_eq!(backups.len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn export_lifecycle() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    let requested = service
        .request_export(
            NewExport {
                client_profile_id: None,
                format: ExportFormat::Csv,
            },
            &ctx,
        )
        .await
        .expect("request export");
    assert_eq!(requested.status(), ExportStatus::Requested);

    let started = service
        .start_export(requested.export_id(), &ctx)
        .await
        .expect("start export");
    assert_eq!(started.status(), ExportStatus::InProgress);

    let ready = service
        .complete_export(requested.export_id(), &ctx)
        .await
        .expect("complete export");
    assert_eq!(ready.status(), ExportStatus::Ready);

    let expired = service
        .expire_export(requested.export_id(), &ctx)
        .await
        .expect("expire export");
    assert_eq!(expired.status(), ExportStatus::Expired);

    // A failed export keeps its failure reason.
    let failed_input = service
        .request_export(
            NewExport {
                client_profile_id: None,
                format: ExportFormat::Json,
            },
            &ctx,
        )
        .await
        .expect("request second export");
    service
        .start_export(failed_input.export_id(), &ctx)
        .await
        .expect("start second export");
    let failed = service
        .fail_export(failed_input.export_id(), "disk full".to_string(), &ctx)
        .await
        .expect("fail export");
    assert_eq!(failed.status(), ExportStatus::Failed);

    let exports = service.list_exports(10, &ctx).await.expect("list exports");
    assert_eq!(exports.len(), 2);

    // The repository get path hydrates through the domain validators.
    let repo = PgExportRepository::new(pool.clone());
    let stored = repo
        .get(requested.export_id())
        .await
        .expect("repository get");
    assert_eq!(stored.status(), ExportStatus::Expired);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn upgrade_lifecycle() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    // Blank or identical versions are rejected at the domain level.
    service
        .schedule_upgrade(
            NewUpgrade {
                from_version: "1.0.0".to_string(),
                to_version: "1.0.0".to_string(),
            },
            &ctx,
        )
        .await
        .expect_err("identical versions must be rejected");

    let scheduled = service
        .schedule_upgrade(
            NewUpgrade {
                from_version: "1.0.0".to_string(),
                to_version: "1.1.0".to_string(),
            },
            &ctx,
        )
        .await
        .expect("schedule upgrade");
    assert_eq!(scheduled.status(), UpgradeStatus::Scheduled);

    service
        .start_upgrade(scheduled.upgrade_id(), &ctx)
        .await
        .expect("start upgrade");
    let failed = service
        .fail_upgrade(
            scheduled.upgrade_id(),
            "migration 42 broke".to_string(),
            &ctx,
        )
        .await
        .expect("fail upgrade");
    assert_eq!(failed.status(), UpgradeStatus::Failed);

    let rolled_back = service
        .rollback_upgrade(scheduled.upgrade_id(), &ctx)
        .await
        .expect("rollback upgrade");
    assert_eq!(rolled_back.status(), UpgradeStatus::RolledBack);

    // A clean run completes.
    let second = service
        .schedule_upgrade(
            NewUpgrade {
                from_version: "1.1.0".to_string(),
                to_version: "1.2.0".to_string(),
            },
            &ctx,
        )
        .await
        .expect("schedule second upgrade");
    service
        .start_upgrade(second.upgrade_id(), &ctx)
        .await
        .expect("start second upgrade");
    let completed = service
        .complete_upgrade(second.upgrade_id(), &ctx)
        .await
        .expect("complete upgrade");
    assert_eq!(completed.status(), UpgradeStatus::Completed);

    let upgrades = service
        .list_upgrades(10, &ctx)
        .await
        .expect("list upgrades");
    assert_eq!(upgrades.len(), 2);

    let repo = PgUpgradeRepository::new(pool.clone());
    let stored = repo.get(second.upgrade_id()).await.expect("repository get");
    assert_eq!(stored.status(), UpgradeStatus::Completed);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn installation_lifecycle() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    // The installer never ran: None, not an error.
    let none = service
        .get_installation(&ctx)
        .await
        .expect("get installation");
    assert!(none.is_none());

    let begun = service
        .begin_installation(&ctx)
        .await
        .expect("begin installation");
    assert_eq!(begun.state(), InstallationState::InProgress);

    // Beginning twice is rejected: the installer runs once.
    service
        .begin_installation(&ctx)
        .await
        .expect_err("double begin must fail");

    let completed = service
        .complete_installation("1.2.0".to_string(), &ctx)
        .await
        .expect("complete installation");
    assert_eq!(completed.state(), InstallationState::Completed);
    assert_eq!(completed.version(), Some("1.2.0"));

    let stored = service
        .get_installation(&ctx)
        .await
        .expect("get installation after complete")
        .expect("installation row exists");
    assert_eq!(stored.state(), InstallationState::Completed);

    // The repository get path hydrates through the domain validators.
    let repo = PgInstallationRepository::new(pool.clone());
    let via_repo = repo.get().await.expect("repository get");
    let via_repo = via_repo.expect("installation row present");
    assert_eq!(via_repo.state(), InstallationState::Completed);

    drop_db(&admin_url, &db_name, pool).await;
}

/// The service's inherent single-get helpers (backing the HTTP `GET`
/// endpoints) fetch through the same agency-scope gate as the trait
/// methods.
#[tokio::test]
async fn service_single_gets() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    let invitation = service
        .invite_staff(
            NewStaffInvitation {
                email: "getme@example.com".to_string(),
                role_ids: vec![RoleId::new()],
                expires_at: Utc::now() + Duration::days(7),
            },
            &ctx,
        )
        .await
        .expect("invite staff");
    let fetched_invitation = service
        .get_invitation(invitation.invitation_id(), &ctx)
        .await
        .expect("service get_invitation");
    assert_eq!(
        fetched_invitation.invitation_id(),
        invitation.invitation_id()
    );

    let backup = service
        .schedule_backup(&ctx)
        .await
        .expect("schedule backup");
    let fetched_backup = service
        .get_backup(backup.backup_id(), &ctx)
        .await
        .expect("service get_backup");
    assert_eq!(fetched_backup.backup_id(), backup.backup_id());

    let export = service
        .request_export(
            NewExport {
                client_profile_id: None,
                format: ExportFormat::Json,
            },
            &ctx,
        )
        .await
        .expect("request export");
    let fetched_export = service
        .get_export(export.export_id(), &ctx)
        .await
        .expect("service get_export");
    assert_eq!(fetched_export.export_id(), export.export_id());

    let upgrade = service
        .schedule_upgrade(
            NewUpgrade {
                from_version: "2.0.0".to_string(),
                to_version: "2.1.0".to_string(),
            },
            &ctx,
        )
        .await
        .expect("schedule upgrade");
    let fetched_upgrade = service
        .get_upgrade(upgrade.upgrade_id(), &ctx)
        .await
        .expect("service get_upgrade");
    assert_eq!(fetched_upgrade.upgrade_id(), upgrade.upgrade_id());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn system_health_folds_reporters() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let ctx = ctx_for(admin.staff_id, None);

    let summary = service
        .get_system_health(&ctx)
        .await
        .expect("system health");
    assert!(summary.is_healthy());
    assert!(summary
        .components()
        .iter()
        .any(|report| report.component == "database" && report.is_healthy()));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn profile_scoped_context_is_rejected() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = service(&pool);
    let admin = make_staff(&pool, "admin@example.com").await;
    let profile_ctx = ctx_for(admin.staff_id, Some(ClientProfileId::new()));

    // Mutations with a client-profile scope are forbidden.
    let error = service
        .invite_staff(
            NewStaffInvitation {
                email: "nope@example.com".to_string(),
                role_ids: vec![RoleId::new()],
                expires_at: Utc::now() + Duration::days(7),
            },
            &profile_ctx,
        )
        .await
        .expect_err("profile-scoped invite must be rejected");
    assert!(
        matches!(error, DomainError::Forbidden(_)),
        "expected Forbidden, got {error:?}"
    );

    // Reads with a client-profile scope are forbidden too.
    let error = service
        .get_system_health(&profile_ctx)
        .await
        .expect_err("profile-scoped health read must be rejected");
    assert!(
        matches!(error, DomainError::Forbidden(_)),
        "expected Forbidden, got {error:?}"
    );

    let error = service
        .list_backups(10, &profile_ctx)
        .await
        .expect_err("profile-scoped list must be rejected");
    assert!(
        matches!(error, DomainError::Forbidden(_)),
        "expected Forbidden, got {error:?}"
    );

    drop_db(&admin_url, &db_name, pool).await;
}
