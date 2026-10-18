// SPDX-License-Identifier: GPL-3.0-only
//! Analytics endpoints: saved dashboards and reports (task
//! 15-DASH-02).
//!
//! * `POST /api/v1/dashboards` — creates a draft dashboard (`201`).
//! * `GET /api/v1/dashboards?client_profile_id=…` — lists
//!   dashboards for one owner scope, newest first (`200`).
//! * `GET /api/v1/dashboards/{id}` — fetches a dashboard (`200`).
//! * `PATCH /api/v1/dashboards/{id}` — applies draft-only mutations
//!   (rename, add/remove widgets) (`200`).
//! * `DELETE /api/v1/dashboards/{id}` — deletes a dashboard (`204`).
//! * `POST /api/v1/dashboards/{id}/widgets/{widget_id}/run` —
//!   executes one widget's metric, returning the computed
//!   [`WidgetResult`](crm_domain::dashboard::WidgetResult) (`200`).
//!   Read-only: no audit event.
//! * `POST /api/v1/reports` — saves a parameterized report (`201`).
//! * `GET /api/v1/reports` — lists reports, newest first (`200`).
//! * `GET /api/v1/reports/{id}` — fetches a report (`200`).
//!
//! ## Authorization
//!
//! Dashboard endpoints authorize against
//! [`Resource::Dashboard`](crm_domain::security::Resource::Dashboard),
//! profile-scoped to the dashboard's owner scope (agency-level
//! dashboards authorize at agency scope). Fetch-then-authorize: a
//! missing dashboard surfaces as `404` before any grant check.
//!
//! Report endpoints authorize against
//! [`Resource::Report`](crm_domain::security::Resource::Report) at
//! agency scope — reports are staff-owned and carry no client
//! profile.

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crm_core::dashboard::DashboardServiceImpl;
use crm_db::dashboard::{PgDashboardRepository, PgReportRepository};
use crm_domain::dashboard::{Dashboard, DashboardWidget, MetricDefinition, ReportParameter};
use crm_domain::ids::{ClientProfileId, DashboardId, ReportId, StaffId, WidgetId};
use crm_domain::repositories::{DashboardRepository, ReportRepository};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{
    ActionContext, AuditService, DashboardService, NewDashboard, NewReport, UpdateDashboard,
};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/dashboards`.
#[derive(Debug, Deserialize)]
pub struct CreateDashboardBody {
    /// Display name.
    pub name: String,
    /// Owner scope: `None` is the agency-level master dashboard.
    pub client_profile_id: Option<Uuid>,
}

/// Body of `PATCH /api/v1/dashboards/{id}`.
#[derive(Debug, Deserialize)]
pub struct UpdateDashboardBody {
    /// New display name, when renaming.
    pub name: Option<String>,
    /// Widgets to add (positions must be free; validated through the
    /// domain's constructors).
    #[serde(default)]
    pub widgets_to_add: Vec<DashboardWidget>,
    /// Ids of widgets to remove.
    #[serde(default)]
    pub widget_ids_to_remove: Vec<Uuid>,
}

/// Body of `POST /api/v1/reports`.
#[derive(Debug, Deserialize)]
pub struct CreateReportBody {
    /// Display name.
    pub name: String,
    /// The staff member saving the report.
    pub owner: Uuid,
    /// The metric the report runs.
    pub metric: MetricDefinition,
    /// The parameter values the metric runs with.
    #[serde(default)]
    pub parameters: Vec<ReportParameter>,
}

/// Query of `GET /api/v1/dashboards`.
#[derive(Debug, Deserialize)]
pub struct ListDashboardsQuery {
    /// Owner scope to list (`None` lists the agency-level
    /// dashboards).
    pub client_profile_id: Option<Uuid>,
    /// Max items returned (default 20).
    pub limit: Option<u64>,
}

/// Query of `GET /api/v1/reports`.
#[derive(Debug, Deserialize)]
pub struct ListReportsQuery {
    /// Max items returned (default 20).
    pub limit: Option<u64>,
}

/// Builds the analytics service from the API auth components.
fn dashboard_service(
    api_auth: &crate::auth::ApiAuth,
) -> DashboardServiceImpl<PgDashboardRepository, PgReportRepository, crm_db::postgres::PgAuditService>
{
    DashboardServiceImpl::new(
        api_auth.dashboards.clone(),
        api_auth.reports.clone(),
        api_auth.pool.clone(),
        api_auth.audit_service.clone(),
    )
}

/// Extracts a dashboard id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_dashboard_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<DashboardId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(DashboardId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid dashboard id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a `(dashboard id, widget id)` pair from the path, or
/// returns a 422.
fn path_dashboard_and_widget_ids(
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    instance: &str,
) -> Result<(DashboardId, WidgetId), Box<Response>> {
    match path {
        Ok(Path((dashboard_id, widget_id))) => {
            Ok((DashboardId::from(dashboard_id), WidgetId::from(widget_id)))
        }
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid dashboard or widget id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a report id from the path, or returns a 422.
fn path_report_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<ReportId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(ReportId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid report id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Fetches a dashboard or returns its 404. Fetch-then-authorize: the
/// existence check runs before any grant check.
async fn fetch_dashboard(
    api_auth: &crate::auth::ApiAuth,
    dashboard_id: DashboardId,
    instance: &str,
) -> Result<Dashboard, Box<Response>> {
    match api_auth.dashboards.get(dashboard_id).await {
        Ok(dashboard) => Ok(dashboard),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Authorizes `operation` against a dashboard's owner scope, or
/// returns the 401/403 response.
async fn authorize_dashboard<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    dashboard: &Dashboard,
    operation: Operation,
) -> Result<ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource: Resource::Dashboard,
        operation,
    };
    match resolve_context(state, auth, headers, &action, dashboard.client_profile_id()).await {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// `POST /api/v1/dashboards` — creates a draft dashboard (`201`).
///
/// Requires the `dashboard/create` grant in the dashboard's owner
/// scope (agency scope for the master dashboard; `403` without it).
pub async fn create_dashboard<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateDashboardBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let scope = body.client_profile_id.map(ClientProfileId::from);
    let action = Action {
        resource: Resource::Dashboard,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, scope).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = dashboard_service(api_auth);
    let dashboard = match service
        .create_dashboard(
            NewDashboard {
                name: body.name,
                client_profile_id: scope,
            },
            &ctx,
        )
        .await
    {
        Ok(dashboard) => dashboard,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(dashboard)).into_response()
}

/// `GET /api/v1/dashboards?client_profile_id=…` — lists dashboards
/// for one owner scope, newest first (`200`).
///
/// The response is `{dashboards, total}`; `total` is the page length.
pub async fn list_dashboards<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListDashboardsQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let scope = query.client_profile_id.map(ClientProfileId::from);
    let action = Action {
        resource: Resource::Dashboard,
        operation: Operation::Read,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, scope).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = dashboard_service(api_auth);
    let dashboards = match service
        .list_dashboards(query.limit.unwrap_or(20), &ctx)
        .await
    {
        Ok(dashboards) => dashboards,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = dashboards.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "dashboards": dashboards, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/dashboards/{id}` — fetches a dashboard (`200`).
///
/// Fetch-then-authorize: a missing dashboard is `404` before any
/// grant check.
pub async fn get_dashboard<R, A, I>(
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
    let dashboard_id = match path_dashboard_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let dashboard = match fetch_dashboard(api_auth, dashboard_id, &instance).await {
        Ok(dashboard) => dashboard,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_dashboard(&state, auth, &headers, &dashboard, Operation::Read).await
    {
        return *response;
    }
    (StatusCode::OK, Json(dashboard)).into_response()
}

/// `PATCH /api/v1/dashboards/{id}` — applies draft-only mutations
/// (rename, add/remove widgets) (`200`).
pub async fn update_dashboard<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<UpdateDashboardBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let dashboard_id = match path_dashboard_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let dashboard = match fetch_dashboard(api_auth, dashboard_id, &instance).await {
        Ok(dashboard) => dashboard,
        Err(response) => return *response,
    };
    let ctx = match authorize_dashboard(&state, auth, &headers, &dashboard, Operation::Update).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = dashboard_service(api_auth);
    let dashboard = match service
        .update_dashboard(
            dashboard_id,
            UpdateDashboard {
                name: body.name,
                widgets_to_add: body.widgets_to_add,
                widget_ids_to_remove: body
                    .widget_ids_to_remove
                    .into_iter()
                    .map(WidgetId::from)
                    .collect(),
            },
            &ctx,
        )
        .await
    {
        Ok(dashboard) => dashboard,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(dashboard)).into_response()
}

/// `DELETE /api/v1/dashboards/{id}` — deletes a dashboard (`204`).
pub async fn delete_dashboard<R, A, I>(
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
    let dashboard_id = match path_dashboard_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let dashboard = match fetch_dashboard(api_auth, dashboard_id, &instance).await {
        Ok(dashboard) => dashboard,
        Err(response) => return *response,
    };
    let ctx = match authorize_dashboard(&state, auth, &headers, &dashboard, Operation::Delete).await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = dashboard_service(api_auth);
    if let Err(error) = service.delete_dashboard(dashboard_id, &ctx).await {
        return domain_error_response(error, instance);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v1/dashboards/{id}/widgets/{widget_id}/run` —
/// executes one widget's metric (`200`).
///
/// Read-only: no audit event. Requires the `dashboard/read` grant in
/// the dashboard's owner scope.
pub async fn run_widget<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let (dashboard_id, widget_id) = match path_dashboard_and_widget_ids(path, &instance) {
        Ok(ids) => ids,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let dashboard = match fetch_dashboard(api_auth, dashboard_id, &instance).await {
        Ok(dashboard) => dashboard,
        Err(response) => return *response,
    };
    let ctx = match authorize_dashboard(&state, auth, &headers, &dashboard, Operation::Read).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = dashboard_service(api_auth);
    let result = match service.run_widget(dashboard_id, widget_id, &ctx).await {
        Ok(result) => result,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(result)).into_response()
}

/// `POST /api/v1/reports` — saves a parameterized report (`201`).
///
/// Reports authorize at agency scope.
pub async fn create_report<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateReportBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let action = Action {
        resource: Resource::Report,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, None).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = dashboard_service(api_auth);
    let report = match service
        .create_report(
            NewReport {
                name: body.name,
                owner: StaffId::from(body.owner),
                metric: body.metric,
                parameters: body.parameters,
            },
            &ctx,
        )
        .await
    {
        Ok(report) => report,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(report)).into_response()
}

/// `GET /api/v1/reports` — lists reports, newest first (`200`).
///
/// The response is `{reports, total}`; `total` is the page length.
pub async fn list_reports<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListReportsQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let action = Action {
        resource: Resource::Report,
        operation: Operation::Read,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, None).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = dashboard_service(api_auth);
    let reports = match service.list_reports(query.limit.unwrap_or(20), &ctx).await {
        Ok(reports) => reports,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = reports.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "reports": reports, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/reports/{id}` — fetches a report (`200`).
///
/// Fetch-then-authorize: a missing report is `404` before any grant
/// check.
pub async fn get_report<R, A, I>(
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
    let report_id = match path_report_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let report = match api_auth.reports.get(report_id).await {
        Ok(report) => report,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::Report,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, None).await {
        return *response;
    }
    (StatusCode::OK, Json(report)).into_response()
}
