// SPDX-License-Identifier: GPL-3.0-only
//! Admin and operations endpoints (task 16-ADMIN-02).
//!
//! * `POST /api/v1/admin/invitations` — invites a staff member
//!   (`201`).
//! * `GET /api/v1/admin/invitations` — lists invitations, newest
//!   first (`200`).
//! * `GET /api/v1/admin/invitations/{id}` — fetches one invitation
//!   (`200`).
//! * `POST /api/v1/admin/invitations/{id}/accept` — accepts the
//!   invitation and creates the staff account (`200`).
//! * `POST /api/v1/admin/invitations/{id}/revoke` — revokes a pending
//!   invitation (`200`).
//! * `POST /api/v1/admin/staff/{id}/disable` — suspends a staff
//!   account (`204`).
//! * `POST /api/v1/admin/staff/{id}/reactivate` — reactivates a
//!   suspended account (`204`).
//! * `POST /api/v1/admin/role-assignments` — assigns a role within a
//!   scope (`201`).
//! * `GET /api/v1/admin/role-assignments?staff_id=…` — lists one
//!   staff member's assignments (`200`).
//! * `POST /api/v1/admin/role-assignments/{id}/revoke` — revokes an
//!   assignment (`200`).
//! * `GET /api/v1/admin/settings` — reads the current agency
//!   settings (`200`).
//! * `PUT /api/v1/admin/settings` — stores a new settings version
//!   (`200`).
//! * `GET /api/v1/admin/maintenance` — reads maintenance mode
//!   (`200`).
//! * `PUT /api/v1/admin/maintenance` — enables/disables maintenance
//!   mode (`200`).
//! * `GET /api/v1/admin/health` — the aggregated system health
//!   summary (`200`).
//! * `POST /api/v1/admin/backups` — schedules a backup (`201`);
//!   `GET` lists (`200`); `GET /{id}` fetches (`200`);
//!   `POST /{id}/start|complete|fail` drive the lifecycle (`200`).
//! * `POST /api/v1/admin/exports` — requests a data export (`201`);
//!   `GET` lists (`200`); `GET /{id}` fetches (`200`);
//!   `POST /{id}/start|complete|fail|expire` drive the lifecycle
//!   (`200`).
//! * `POST /api/v1/admin/upgrades` — schedules a system upgrade
//!   (`201`); `GET` lists (`200`); `GET /{id}` fetches (`200`);
//!   `POST /{id}/start|complete|fail|rollback` drive the lifecycle
//!   (`200`).
//! * `GET /api/v1/admin/installation` — the installation record, or
//!   `null` when the installer never ran (`200`).
//! * `POST /api/v1/admin/installation/begin` — begins the first-run
//!   installation (`200`).
//! * `POST /api/v1/admin/installation/complete` — completes the
//!   installation (`200`).
//!
//! ## Authorization
//!
//! Admin is agency-wide by construction: every endpoint authorizes at
//! agency scope (no client-profile scope is ever accepted). Staff
//! administration (invitations, disable/reactivate, role assignments)
//! requires [`Resource::StaffUser`](crm_domain::security::Resource::StaffUser)
//! with [`Operation::Manage`](crm_domain::security::Operation::Manage)
//! (reads: `Read`); agency settings require
//! [`Resource::AgencySettings`](crm_domain::security::Resource::AgencySettings);
//! maintenance, health, backups, exports, upgrades and installation
//! require [`Resource::System`](crm_domain::security::Resource::System).
//! Single-get endpoints are fetch-then-authorize: a missing record is
//! `404` before any grant check.

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crm_core::admin::AdminServiceImpl;
use crm_db::admin::{
    DbHealthReporter, PgAgencySettingsRepository, PgBackupRepository, PgExportRepository,
    PgInstallationRepository, PgMaintenanceRepository, PgRoleAssignmentRepository,
    PgStaffInvitationRepository, PgUpgradeRepository,
};
use crm_db::postgres::{PgAuditService, PgStaffRepository};
use crm_domain::admin::ExportFormat;
use crm_domain::ids::{
    BackupId, ClientProfileId, ExportId, InvitationId, RoleAssignmentId, RoleId, StaffId, UpgradeId,
};
use crm_domain::repositories::{
    BackupRepository, ExportRepository, StaffInvitationRepository, UpgradeRepository,
};
use crm_domain::security::{Action, Operation, Resource, Scope};
use crm_domain::services::{
    ActionContext, AdminService, AuditService, NewExport, NewRoleAssignment, NewStaffInvitation,
    NewUpgrade, UpdateAgencySettings,
};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/admin/invitations`.
#[derive(Debug, Deserialize)]
pub struct CreateInvitationBody {
    /// The email address to invite.
    pub email: String,
    /// Role ids granted when the invitation is accepted.
    pub role_ids: Vec<Uuid>,
    /// When the invitation stops being acceptable.
    pub expires_at: DateTime<Utc>,
}

/// Query of the admin `GET` list endpoints.
#[derive(Debug, Deserialize)]
pub struct ListAdminQuery {
    /// Max items returned (default 20).
    pub limit: Option<u64>,
}

/// Body of `POST /api/v1/admin/role-assignments`.
#[derive(Debug, Deserialize)]
pub struct CreateRoleAssignmentBody {
    /// The staff member receiving the role.
    pub staff_id: Uuid,
    /// The role to assign.
    pub role_id: Uuid,
    /// The scope the role is held within.
    pub scope: Scope,
}

/// Query of `GET /api/v1/admin/role-assignments`.
#[derive(Debug, Deserialize)]
pub struct ListRoleAssignmentsQuery {
    /// The staff member whose assignments to list.
    pub staff_id: Uuid,
    /// Max items returned (default 20).
    pub limit: Option<u64>,
}

/// Body of `PUT /api/v1/admin/settings`.
///
/// `support_email`: missing keeps the current value, `null` clears
/// it, a string sets it.
#[derive(Debug, Deserialize)]
pub struct UpdateSettingsBody {
    /// New display name, when changing it.
    pub display_name: Option<String>,
    /// New support email: `None` keeps, `Some(None)` clears,
    /// `Some(Some(_))` sets.
    #[serde(default)]
    pub support_email: Option<Option<String>>,
    /// New session TTL in seconds, when changing it.
    pub session_ttl_secs: Option<u64>,
}

/// Body of `PUT /api/v1/admin/maintenance`.
#[derive(Debug, Deserialize)]
pub struct SetMaintenanceBody {
    /// Whether maintenance mode should be on.
    pub enabled: bool,
    /// Operator-facing reason; required when enabling.
    pub reason: Option<String>,
}

/// Body of `POST /api/v1/admin/backups/{id}/complete`.
#[derive(Debug, Deserialize)]
pub struct CompleteBackupBody {
    /// The finished backup's byte size.
    pub size_bytes: u64,
}

/// Body of the `.../fail` endpoints.
#[derive(Debug, Deserialize)]
pub struct FailBody {
    /// Human-readable failure reason.
    pub reason: String,
}

/// Body of `POST /api/v1/admin/exports`.
#[derive(Debug, Deserialize)]
pub struct CreateExportBody {
    /// The client profile the extract covers, or `None` for an
    /// agency-wide extract.
    pub client_profile_id: Option<Uuid>,
    /// The export file format.
    pub format: ExportFormat,
}

/// Body of `POST /api/v1/admin/upgrades`.
#[derive(Debug, Deserialize)]
pub struct CreateUpgradeBody {
    /// The version being upgraded from.
    pub from_version: String,
    /// The version being upgraded to.
    pub to_version: String,
}

/// Body of `POST /api/v1/admin/installation/complete`.
#[derive(Debug, Deserialize)]
pub struct CompleteInstallationBody {
    /// The installed version recorded for upgrade bookkeeping.
    pub version: String,
}

/// The concrete admin service type wired by the API.
type ApiAdminService = AdminServiceImpl<
    PgStaffInvitationRepository,
    PgRoleAssignmentRepository,
    PgAgencySettingsRepository,
    PgMaintenanceRepository,
    PgStaffRepository,
    PgBackupRepository,
    PgExportRepository,
    PgUpgradeRepository,
    PgInstallationRepository,
    PgAuditService,
    DbHealthReporter,
>;

/// Builds the admin service from the API auth components.
fn admin_service(api_auth: &crate::auth::ApiAuth) -> ApiAdminService {
    AdminServiceImpl::new(
        api_auth.invitations.clone(),
        api_auth.role_assignments.clone(),
        api_auth.settings.clone(),
        api_auth.maintenance.clone(),
        api_auth.staff.clone(),
        api_auth.backups.clone(),
        api_auth.exports.clone(),
        api_auth.upgrades.clone(),
        api_auth.installation.clone(),
        api_auth.audit_service.clone(),
        vec![DbHealthReporter::new(api_auth.pool.clone())],
    )
}

/// Authorizes `operation` on `resource` at agency scope, or returns
/// the 401/403 response. Admin never accepts a client-profile scope.
async fn authorize_admin<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    resource: Resource,
    operation: Operation,
) -> Result<ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource,
        operation,
    };
    match resolve_context(state, auth, headers, &action, None).await {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// Extracts an invitation id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_invitation_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<InvitationId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(InvitationId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid invitation id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a role-assignment id from the path, or returns a 422.
fn path_assignment_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<RoleAssignmentId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(RoleAssignmentId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid role assignment id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a staff id from the path, or returns a 422.
fn path_staff_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<StaffId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(StaffId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid staff id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a backup id from the path, or returns a 422.
fn path_backup_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<BackupId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(BackupId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid backup id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts an export id from the path, or returns a 422.
fn path_export_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<ExportId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(ExportId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid export id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts an upgrade id from the path, or returns a 422.
fn path_upgrade_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<UpgradeId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(UpgradeId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid upgrade id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Returns the 501 used when the server has no authentication
/// configured (the in-memory slice).
fn not_configured(instance: String) -> Response {
    problem_response(
        StatusCode::NOT_IMPLEMENTED,
        "Not implemented",
        "authentication is not configured on this server".to_owned(),
        instance,
    )
}

/// `POST /api/v1/admin/invitations` — invites a staff member (`201`).
///
/// Requires the `staff_user/manage` grant at agency scope (`403`
/// without it).
pub async fn create_invitation<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateInvitationBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::StaffUser,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let invitation = match service
        .invite_staff(
            NewStaffInvitation {
                email: body.email,
                role_ids: body.role_ids.into_iter().map(RoleId::from).collect(),
                expires_at: body.expires_at,
            },
            &ctx,
        )
        .await
    {
        Ok(invitation) => invitation,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(invitation)).into_response()
}

/// `GET /api/v1/admin/invitations` — lists invitations, newest first
/// (`200`).
///
/// The response is `{invitations, total}`; `total` is the page length.
pub async fn list_invitations<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListAdminQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::StaffUser, Operation::Read).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let invitations = match service
        .list_invitations(query.limit.unwrap_or(20), &ctx)
        .await
    {
        Ok(invitations) => invitations,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = invitations.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "invitations": invitations, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/admin/invitations/{id}` — fetches one invitation
/// (`200`).
///
/// Fetch-then-authorize: a missing invitation is `404` before any
/// grant check.
pub async fn get_invitation<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let invitation_id = match path_invitation_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    // Fetch-then-authorize: the existence check runs before any grant
    // check, directly against the repository.
    let invitation = match api_auth.invitations.get(invitation_id).await {
        Ok(invitation) => invitation,
        Err(error) => return domain_error_response(error, instance),
    };
    if let Err(response) =
        authorize_admin(&state, auth, &headers, Resource::StaffUser, Operation::Read).await
    {
        return *response;
    }
    (StatusCode::OK, Json(invitation)).into_response()
}

/// `POST /api/v1/admin/invitations/{id}/accept` — accepts the
/// invitation, creating the staff account (`200`).
pub async fn accept_invitation<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let invitation_id = match path_invitation_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::StaffUser,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let invitation = match service.accept_invitation(invitation_id, &ctx).await {
        Ok(invitation) => invitation,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(invitation)).into_response()
}

/// `POST /api/v1/admin/invitations/{id}/revoke` — revokes a pending
/// invitation (`200`).
pub async fn revoke_invitation<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let invitation_id = match path_invitation_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::StaffUser,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let invitation = match service.revoke_invitation(invitation_id, &ctx).await {
        Ok(invitation) => invitation,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(invitation)).into_response()
}

/// `POST /api/v1/admin/staff/{id}/disable` — suspends a staff
/// account (`204`). A staff member cannot disable their own account.
pub async fn disable_staff<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let staff_id = match path_staff_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::StaffUser,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    if let Err(error) = service.disable_staff(staff_id, &ctx).await {
        return domain_error_response(error, instance);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v1/admin/staff/{id}/reactivate` — reactivates a
/// suspended staff account (`204`).
pub async fn reactivate_staff<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let staff_id = match path_staff_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::StaffUser,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    if let Err(error) = service.reactivate_staff(staff_id, &ctx).await {
        return domain_error_response(error, instance);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v1/admin/role-assignments` — assigns a role to a staff
/// member within a scope (`201`).
///
/// Requires the `staff_user/manage` grant at agency scope.
pub async fn assign_role<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateRoleAssignmentBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::StaffUser,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let assignment = match service
        .assign_role(
            NewRoleAssignment {
                staff_id: StaffId::from(body.staff_id),
                role_id: RoleId::from(body.role_id),
                scope: body.scope,
            },
            &ctx,
        )
        .await
    {
        Ok(assignment) => assignment,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(assignment)).into_response()
}

/// `GET /api/v1/admin/role-assignments?staff_id=…` — lists one staff
/// member's role assignments, newest first (`200`).
///
/// The response is `{assignments, total}`; `total` is the page length.
pub async fn list_role_assignments<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListRoleAssignmentsQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::StaffUser, Operation::Read).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let assignments = match service
        .list_role_assignments(
            StaffId::from(query.staff_id),
            query.limit.unwrap_or(20),
            &ctx,
        )
        .await
    {
        Ok(assignments) => assignments,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = assignments.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "assignments": assignments, "total": total })),
    )
        .into_response()
}

/// `POST /api/v1/admin/role-assignments/{id}/revoke` — revokes a role
/// assignment (`200`). Revocation is terminal: re-granting creates a
/// new assignment.
pub async fn revoke_role_assignment<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let assignment_id = match path_assignment_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::StaffUser,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let assignment = match service.revoke_role_assignment(assignment_id, &ctx).await {
        Ok(assignment) => assignment,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(assignment)).into_response()
}

/// `GET /api/v1/admin/settings` — reads the current agency settings
/// (`200`). `404` when no settings version was ever stored.
pub async fn get_settings<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::AgencySettings,
        Operation::Read,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let settings = match service.get_settings(&ctx).await {
        Ok(settings) => settings,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(settings)).into_response()
}

/// `PUT /api/v1/admin/settings` — stores a new settings version
/// (`200`). The first call creates version 1; every call after that
/// bumps the version.
pub async fn update_settings<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<UpdateSettingsBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::AgencySettings,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let settings = match service
        .update_settings(
            UpdateAgencySettings {
                display_name: body.display_name,
                support_email: body.support_email,
                session_ttl_secs: body.session_ttl_secs,
            },
            &ctx,
        )
        .await
    {
        Ok(settings) => settings,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(settings)).into_response()
}

/// `GET /api/v1/admin/maintenance` — reads maintenance mode (`200`).
/// Reads as disabled when it was never set.
pub async fn get_maintenance<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let mode = match service.get_maintenance(&ctx).await {
        Ok(mode) => mode,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(mode)).into_response()
}

/// `PUT /api/v1/admin/maintenance` — enables (`enabled: true`,
/// `reason` required) or disables maintenance mode (`200`).
pub async fn set_maintenance<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<SetMaintenanceBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::System, Operation::Manage).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let mode = if body.enabled {
        match service
            .enable_maintenance(body.reason.unwrap_or_default(), &ctx)
            .await
        {
            Ok(mode) => mode,
            Err(error) => return domain_error_response(error, instance),
        }
    } else {
        match service.disable_maintenance(&ctx).await {
            Ok(mode) => mode,
            Err(error) => return domain_error_response(error, instance),
        }
    };
    (StatusCode::OK, Json(mode)).into_response()
}

/// `GET /api/v1/admin/health` — the aggregated system health summary
/// (`200`). Read-only: no audit event.
pub async fn get_system_health<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let summary = match service.get_system_health(&ctx).await {
        Ok(summary) => summary,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(summary)).into_response()
}

/// `POST /api/v1/admin/backups` — schedules a backup (`201`).
pub async fn schedule_backup<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::System, Operation::Manage).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let record = match service.schedule_backup(&ctx).await {
        Ok(record) => record,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(record)).into_response()
}

/// `GET /api/v1/admin/backups` — lists backups, newest first (`200`).
///
/// The response is `{backups, total}`; `total` is the page length.
pub async fn list_backups<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListAdminQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let backups = match service.list_backups(query.limit.unwrap_or(20), &ctx).await {
        Ok(backups) => backups,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = backups.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "backups": backups, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/admin/backups/{id}` — fetches one backup (`200`).
///
/// Fetch-then-authorize: a missing backup is `404` before any grant
/// check.
pub async fn get_backup<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let backup_id = match path_backup_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let record = match api_auth.backups.get(backup_id).await {
        Ok(record) => record,
        Err(error) => return domain_error_response(error, instance),
    };
    if let Err(response) =
        authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        return *response;
    }
    (StatusCode::OK, Json(record)).into_response()
}

/// Drives one backup lifecycle step (`start`, `complete`, `fail`).
async fn backup_step<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    api_auth: &crate::auth::ApiAuth,
    backup_id: BackupId,
    step: BackupStep,
    instance: String,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let ctx = match authorize_admin(state, auth, headers, Resource::System, Operation::Manage).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let record = match step {
        BackupStep::Start => service.start_backup(backup_id, &ctx).await,
        BackupStep::Complete(size_bytes) => {
            service.complete_backup(backup_id, size_bytes, &ctx).await
        }
        BackupStep::Fail(reason) => service.fail_backup(backup_id, reason, &ctx).await,
    };
    match record {
        Ok(record) => (StatusCode::OK, Json(record)).into_response(),
        Err(error) => domain_error_response(error, instance),
    }
}

/// One backup lifecycle step driven by [`backup_step`].
enum BackupStep {
    /// `POST /api/v1/admin/backups/{id}/start`.
    Start,
    /// `POST /api/v1/admin/backups/{id}/complete`.
    Complete(u64),
    /// `POST /api/v1/admin/backups/{id}/fail`.
    Fail(String),
}

/// `POST /api/v1/admin/backups/{id}/start` — starts a scheduled
/// backup (`200`).
pub async fn start_backup<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let backup_id = match path_backup_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    backup_step(
        &state,
        auth,
        &headers,
        api_auth,
        backup_id,
        BackupStep::Start,
        instance,
    )
    .await
}

/// `POST /api/v1/admin/backups/{id}/complete` — completes a running
/// backup (`200`).
pub async fn complete_backup<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<CompleteBackupBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let backup_id = match path_backup_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    backup_step(
        &state,
        auth,
        &headers,
        api_auth,
        backup_id,
        BackupStep::Complete(body.size_bytes),
        instance,
    )
    .await
}

/// `POST /api/v1/admin/backups/{id}/fail` — fails a backup (`200`).
pub async fn fail_backup<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<FailBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let backup_id = match path_backup_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    backup_step(
        &state,
        auth,
        &headers,
        api_auth,
        backup_id,
        BackupStep::Fail(body.reason),
        instance,
    )
    .await
}

/// `POST /api/v1/admin/exports` — requests a data export (`201`).
pub async fn request_export<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateExportBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::System, Operation::Manage).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let export = match service
        .request_export(
            NewExport {
                client_profile_id: body.client_profile_id.map(ClientProfileId::from),
                format: body.format,
            },
            &ctx,
        )
        .await
    {
        Ok(export) => export,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(export)).into_response()
}

/// `GET /api/v1/admin/exports` — lists exports, newest first (`200`).
///
/// The response is `{exports, total}`; `total` is the page length.
pub async fn list_exports<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListAdminQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let exports = match service.list_exports(query.limit.unwrap_or(20), &ctx).await {
        Ok(exports) => exports,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = exports.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "exports": exports, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/admin/exports/{id}` — fetches one export (`200`).
///
/// Fetch-then-authorize: a missing export is `404` before any grant
/// check.
pub async fn get_export<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let export_id = match path_export_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let export = match api_auth.exports.get(export_id).await {
        Ok(export) => export,
        Err(error) => return domain_error_response(error, instance),
    };
    if let Err(response) =
        authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        return *response;
    }
    (StatusCode::OK, Json(export)).into_response()
}

/// Drives one export lifecycle step (`start`, `complete`, `fail`,
/// `expire`).
async fn export_step<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    api_auth: &crate::auth::ApiAuth,
    export_id: ExportId,
    step: ExportStep,
    instance: String,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let ctx = match authorize_admin(state, auth, headers, Resource::System, Operation::Manage).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let export = match step {
        ExportStep::Start => service.start_export(export_id, &ctx).await,
        ExportStep::Complete => service.complete_export(export_id, &ctx).await,
        ExportStep::Fail(reason) => service.fail_export(export_id, reason, &ctx).await,
        ExportStep::Expire => service.expire_export(export_id, &ctx).await,
    };
    match export {
        Ok(export) => (StatusCode::OK, Json(export)).into_response(),
        Err(error) => domain_error_response(error, instance),
    }
}

/// One export lifecycle step driven by [`export_step`].
enum ExportStep {
    /// `POST /api/v1/admin/exports/{id}/start`.
    Start,
    /// `POST /api/v1/admin/exports/{id}/complete`.
    Complete,
    /// `POST /api/v1/admin/exports/{id}/fail`.
    Fail(String),
    /// `POST /api/v1/admin/exports/{id}/expire`.
    Expire,
}

/// `POST /api/v1/admin/exports/{id}/start` — starts producing a
/// requested export (`200`).
pub async fn start_export<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let export_id = match path_export_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    export_step(
        &state,
        auth,
        &headers,
        api_auth,
        export_id,
        ExportStep::Start,
        instance,
    )
    .await
}

/// `POST /api/v1/admin/exports/{id}/complete` — marks an export
/// ready (`200`).
pub async fn complete_export<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let export_id = match path_export_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    export_step(
        &state,
        auth,
        &headers,
        api_auth,
        export_id,
        ExportStep::Complete,
        instance,
    )
    .await
}

/// `POST /api/v1/admin/exports/{id}/fail` — fails an in-progress
/// export (`200`).
pub async fn fail_export<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<FailBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let export_id = match path_export_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    export_step(
        &state,
        auth,
        &headers,
        api_auth,
        export_id,
        ExportStep::Fail(body.reason),
        instance,
    )
    .await
}

/// `POST /api/v1/admin/exports/{id}/expire` — expires a ready export
/// past its retention window (`200`).
pub async fn expire_export<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let export_id = match path_export_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    export_step(
        &state,
        auth,
        &headers,
        api_auth,
        export_id,
        ExportStep::Expire,
        instance,
    )
    .await
}

/// `POST /api/v1/admin/upgrades` — schedules a system upgrade
/// (`201`).
pub async fn schedule_upgrade<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateUpgradeBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::System, Operation::Manage).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let upgrade = match service
        .schedule_upgrade(
            NewUpgrade {
                from_version: body.from_version,
                to_version: body.to_version,
            },
            &ctx,
        )
        .await
    {
        Ok(upgrade) => upgrade,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(upgrade)).into_response()
}

/// `GET /api/v1/admin/upgrades` — lists upgrades, newest first
/// (`200`).
///
/// The response is `{upgrades, total}`; `total` is the page length.
pub async fn list_upgrades<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListAdminQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let upgrades = match service.list_upgrades(query.limit.unwrap_or(20), &ctx).await {
        Ok(upgrades) => upgrades,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = upgrades.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "upgrades": upgrades, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/admin/upgrades/{id}` — fetches one upgrade (`200`).
///
/// Fetch-then-authorize: a missing upgrade is `404` before any grant
/// check.
pub async fn get_upgrade<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let upgrade_id = match path_upgrade_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let upgrade = match api_auth.upgrades.get(upgrade_id).await {
        Ok(upgrade) => upgrade,
        Err(error) => return domain_error_response(error, instance),
    };
    if let Err(response) =
        authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        return *response;
    }
    (StatusCode::OK, Json(upgrade)).into_response()
}

/// Drives one upgrade lifecycle step (`start`, `complete`, `fail`,
/// `rollback`).
async fn upgrade_step<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    api_auth: &crate::auth::ApiAuth,
    upgrade_id: UpgradeId,
    step: UpgradeStep,
    instance: String,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let ctx = match authorize_admin(state, auth, headers, Resource::System, Operation::Manage).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let upgrade = match step {
        UpgradeStep::Start => service.start_upgrade(upgrade_id, &ctx).await,
        UpgradeStep::Complete => service.complete_upgrade(upgrade_id, &ctx).await,
        UpgradeStep::Fail(reason) => service.fail_upgrade(upgrade_id, reason, &ctx).await,
        UpgradeStep::Rollback => service.rollback_upgrade(upgrade_id, &ctx).await,
    };
    match upgrade {
        Ok(upgrade) => (StatusCode::OK, Json(upgrade)).into_response(),
        Err(error) => domain_error_response(error, instance),
    }
}

/// One upgrade lifecycle step driven by [`upgrade_step`].
enum UpgradeStep {
    /// `POST /api/v1/admin/upgrades/{id}/start`.
    Start,
    /// `POST /api/v1/admin/upgrades/{id}/complete`.
    Complete,
    /// `POST /api/v1/admin/upgrades/{id}/fail`.
    Fail(String),
    /// `POST /api/v1/admin/upgrades/{id}/rollback`.
    Rollback,
}

/// `POST /api/v1/admin/upgrades/{id}/start` — starts a scheduled
/// upgrade (`200`).
pub async fn start_upgrade<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let upgrade_id = match path_upgrade_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    upgrade_step(
        &state,
        auth,
        &headers,
        api_auth,
        upgrade_id,
        UpgradeStep::Start,
        instance,
    )
    .await
}

/// `POST /api/v1/admin/upgrades/{id}/complete` — completes a running
/// upgrade (`200`).
pub async fn complete_upgrade<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let upgrade_id = match path_upgrade_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    upgrade_step(
        &state,
        auth,
        &headers,
        api_auth,
        upgrade_id,
        UpgradeStep::Complete,
        instance,
    )
    .await
}

/// `POST /api/v1/admin/upgrades/{id}/fail` — fails a running upgrade
/// (`200`).
pub async fn fail_upgrade<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<FailBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let upgrade_id = match path_upgrade_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    upgrade_step(
        &state,
        auth,
        &headers,
        api_auth,
        upgrade_id,
        UpgradeStep::Fail(body.reason),
        instance,
    )
    .await
}

/// `POST /api/v1/admin/upgrades/{id}/rollback` — rolls back a failed
/// upgrade (`200`).
pub async fn rollback_upgrade<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let upgrade_id = match path_upgrade_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    upgrade_step(
        &state,
        auth,
        &headers,
        api_auth,
        upgrade_id,
        UpgradeStep::Rollback,
        instance,
    )
    .await
}

/// `GET /api/v1/admin/installation` — reads the installation record
/// (`200`); `null` when the installer never ran.
pub async fn get_installation<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx = match authorize_admin(&state, auth, &headers, Resource::System, Operation::Read).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = admin_service(api_auth);
    let installation = match service.get_installation(&ctx).await {
        Ok(installation) => installation,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(installation)).into_response()
}

/// `POST /api/v1/admin/installation/begin` — begins the first-run
/// installation (`200`). Rejected when the installer already ran.
pub async fn begin_installation<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::System, Operation::Manage).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let installation = match service.begin_installation(&ctx).await {
        Ok(installation) => installation,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(installation)).into_response()
}

/// `POST /api/v1/admin/installation/complete` — completes the
/// installation, recording the installed version (`200`).
pub async fn complete_installation<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CompleteInstallationBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    let ctx =
        match authorize_admin(&state, auth, &headers, Resource::System, Operation::Manage).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = admin_service(api_auth);
    let installation = match service.complete_installation(body.version, &ctx).await {
        Ok(installation) => installation,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(installation)).into_response()
}
