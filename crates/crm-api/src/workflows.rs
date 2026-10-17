// SPDX-License-Identifier: GPL-3.0-only
//! Workflow builder endpoints: workflows, nodes, edges, triggers,
//! versions and simulation (task 10-WORK-02).
//!
//! * `POST /api/v1/workflows` — creates a draft workflow (`201`).
//! * `GET /api/v1/workflows?client_profile_id=…` — lists workflows (`200`).
//! * `GET /api/v1/workflows/{id}` — fetches a workflow with its nodes,
//!   edges and trigger (`200`).
//! * `POST /api/v1/workflows/{id}/publish` — publishes (draft →
//!   published) and freezes an immutable version snapshot.
//! * `POST /api/v1/workflows/{id}/archive` — archives (idempotent).
//! * `POST /api/v1/workflows/{id}/nodes` — adds a typed node (`201`).
//! * `GET /api/v1/workflows/{id}/nodes` — lists the workflow's nodes.
//! * `DELETE /api/v1/workflows/{id}/nodes/{node_id}` — removes a node
//!   and its dangling edges.
//! * `POST /api/v1/workflows/{id}/edges` — adds a typed edge (`201`).
//! * `GET /api/v1/workflows/{id}/edges` — lists the workflow's edges.
//! * `DELETE /api/v1/workflows/{id}/edges/{edge_id}` — removes an edge.
//! * `PUT /api/v1/workflows/{id}/trigger` — sets (or replaces) the
//!   workflow's trigger.
//! * `GET /api/v1/workflows/{id}/trigger` — fetches the trigger (`404`
//!   when unset).
//! * `POST /api/v1/workflows/{id}/simulate` — dry-runs the workflow
//!   (pure; never executes actions).
//! * `GET /api/v1/workflows/{id}/versions` — lists version snapshots,
//!   newest first.
//!
//! ## Authorization
//!
//! All endpoints authorize against
//! [`Resource::Workflow`](crm_domain::security::Resource::Workflow),
//! profile-scoped. Fetch-then-authorize: a missing workflow surfaces
//! as `404` before any grant check.

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_core::workflow::WorkflowServiceImpl;
use crm_db::workflow::{
    PgTriggerRepository, PgWorkflowEdgeRepository, PgWorkflowNodeRepository, PgWorkflowRepository,
    PgWorkflowVersionRepository,
};
use crm_domain::ids::{ClientProfileId, WorkflowEdgeId, WorkflowId, WorkflowNodeId};
use crm_domain::repositories::{
    TriggerRepository, WorkflowEdgeRepository, WorkflowNodeRepository, WorkflowRepository,
    WorkflowVersionRepository,
};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{
    AuditService, NewTrigger, NewWorkflow, NewWorkflowEdge, NewWorkflowNode, WorkflowService,
};
use crm_domain::workflow::{TriggerType, Workflow, WorkflowNodeType};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/workflows`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateWorkflowBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Human-readable workflow name (validated non-blank).
    pub name: String,
}

/// Query of `GET /api/v1/workflows`.
#[derive(Debug, Deserialize)]
pub struct ListWorkflowsQuery {
    /// Owning client profile (required: workflows are profile-scoped).
    pub client_profile_id: Uuid,
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
}

/// Body of `POST /api/v1/workflows/{id}/nodes`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddNodeBody {
    /// Node kind: `trigger`, `action`, `condition` or `delay`.
    pub node_type: String,
    /// Node configuration (opaque JSON, never null).
    pub config: serde_json::Value,
    /// Canvas position in pixels.
    pub position: (i32, i32),
}

/// Body of `POST /api/v1/workflows/{id}/edges`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddEdgeBody {
    /// The node the edge leaves.
    pub from_node: Uuid,
    /// The node the edge enters.
    pub to_node: Uuid,
    /// Optional label (e.g. the condition branch).
    pub label: Option<String>,
}

/// Body of `PUT /api/v1/workflows/{id}/trigger`.
#[derive(Debug, Clone, Deserialize)]
pub struct SetTriggerBody {
    /// Trigger kind: `event`, `webhook`, `schedule` or `manual`.
    pub trigger_type: String,
    /// Trigger configuration (opaque JSON, never null).
    pub config: serde_json::Value,
}

/// Body of `POST /api/v1/workflows/{id}/simulate`.
#[derive(Debug, Clone, Deserialize)]
pub struct SimulateBody {
    /// Input payload for the dry-run (opaque JSON).
    #[serde(default)]
    pub input: serde_json::Value,
}

/// `GET /api/v1/workflows/{id}` response: the workflow plus its graph.
#[derive(Debug, Serialize)]
struct WorkflowDetailResponse {
    workflow: Workflow,
    nodes: Vec<crm_domain::workflow::WorkflowNode>,
    edges: Vec<crm_domain::workflow::WorkflowEdge>,
    trigger: Option<crm_domain::workflow::Trigger>,
}

/// Builds the workflow service from the API auth components.
fn workflow_service(
    api_auth: &crate::auth::ApiAuth,
) -> WorkflowServiceImpl<
    PgWorkflowRepository,
    PgWorkflowNodeRepository,
    PgWorkflowEdgeRepository,
    PgTriggerRepository,
    PgWorkflowVersionRepository,
    crm_db::postgres::PgAuditService,
> {
    WorkflowServiceImpl::new(
        api_auth.workflows.clone(),
        api_auth.workflow_nodes.clone(),
        api_auth.workflow_edges.clone(),
        api_auth.workflow_triggers.clone(),
        api_auth.workflow_versions.clone(),
        api_auth.audit_service.clone(),
    )
}

/// Extracts a workflow id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_workflow_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<WorkflowId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(WorkflowId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid workflow id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a node id from the path, or returns a 422.
fn path_node_id(
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    instance: &str,
) -> Result<(WorkflowId, WorkflowNodeId), Box<Response>> {
    match path {
        Ok(Path((workflow_id, node_id))) => {
            Ok((WorkflowId::from(workflow_id), WorkflowNodeId::from(node_id)))
        }
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid node id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts an edge id from the path, or returns a 422.
fn path_edge_id(
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    instance: &str,
) -> Result<(WorkflowId, WorkflowEdgeId), Box<Response>> {
    match path {
        Ok(Path((workflow_id, edge_id))) => {
            Ok((WorkflowId::from(workflow_id), WorkflowEdgeId::from(edge_id)))
        }
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid edge id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Parses a node type from its string form.
fn parse_node_type(raw: &str) -> Result<WorkflowNodeType, String> {
    match raw {
        "trigger" => Ok(WorkflowNodeType::TriggerNode),
        "action" => Ok(WorkflowNodeType::ActionNode),
        "condition" => Ok(WorkflowNodeType::ConditionNode),
        "delay" => Ok(WorkflowNodeType::DelayNode),
        other => Err(format!("unknown workflow node type '{other}'")),
    }
}

/// Parses a trigger type from its string form.
fn parse_trigger_type(raw: &str) -> Result<TriggerType, String> {
    match raw {
        "event" => Ok(TriggerType::EventTrigger),
        "webhook" => Ok(TriggerType::WebhookTrigger),
        "schedule" => Ok(TriggerType::ScheduleTrigger),
        "manual" => Ok(TriggerType::ManualTrigger),
        other => Err(format!("unknown trigger type '{other}'")),
    }
}

/// Fetches a workflow or returns its 404. Fetch-then-authorize: the
/// existence check runs before any grant check.
async fn fetch_workflow(
    api_auth: &crate::auth::ApiAuth,
    workflow_id: WorkflowId,
    instance: &str,
) -> Result<Workflow, Box<Response>> {
    match api_auth.workflows.get(workflow_id).await {
        Ok(workflow) => Ok(workflow),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Authorizes `action` against the workflow's client profile, or
/// returns the 401/403 response.
async fn authorize_workflow<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    workflow: &Workflow,
    operation: Operation,
) -> Result<crm_domain::services::ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource: Resource::Workflow,
        operation,
    };
    match resolve_context(
        state,
        auth,
        headers,
        &action,
        Some(workflow.client_profile_id),
    )
    .await
    {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// `POST /api/v1/workflows` — creates a draft workflow.
///
/// Returns `201`. Requires the `workflow/create` grant in the client
/// profile (`403` without it).
pub async fn create_workflow<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<CreateWorkflowBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid request body",
                rejection.body_text(),
                instance,
            );
        }
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let profile_id = ClientProfileId::from(body.client_profile_id);
    let action = Action {
        resource: Resource::Workflow,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    let input = NewWorkflow {
        client_profile_id: profile_id,
        name: body.name,
    };
    let workflow = match service.create_workflow(input, &ctx).await {
        Ok(workflow) => workflow,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(workflow)).into_response()
}

/// `GET /api/v1/workflows` — lists workflows for a client profile.
///
/// Requires the `workflow/read` grant in the profile (`403` without it).
pub async fn list_workflows<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListWorkflowsQuery>,
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
    let profile_id = ClientProfileId::from(query.client_profile_id);
    let action = Action {
        resource: Resource::Workflow,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, Some(profile_id)).await
    {
        return *response;
    }
    let limit = query.limit.unwrap_or(20).min(100) as u64;
    let workflows = match api_auth.workflows.list_by_profile(profile_id, limit).await {
        Ok(workflows) => workflows,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = workflows.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "workflows": workflows, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/workflows/{id}` — fetches a workflow with its nodes,
/// edges and trigger. Requires the `workflow/read` grant.
pub async fn get_workflow<R, A, I>(
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
    let workflow_id = match path_workflow_id(path, &instance) {
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
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_workflow(&state, auth, &headers, &workflow, Operation::Read).await
    {
        return *response;
    }
    let nodes = match api_auth
        .workflow_nodes
        .list_by_workflow(workflow_id, 10_000)
        .await
    {
        Ok(nodes) => nodes,
        Err(error) => return domain_error_response(error, instance),
    };
    let edges = match api_auth
        .workflow_edges
        .list_by_workflow(workflow_id, 10_000)
        .await
    {
        Ok(edges) => edges,
        Err(error) => return domain_error_response(error, instance),
    };
    let trigger = match api_auth
        .workflow_triggers
        .get_by_workflow(workflow_id)
        .await
    {
        Ok(trigger) => trigger,
        Err(error) => return domain_error_response(error, instance),
    };
    (
        StatusCode::OK,
        Json(WorkflowDetailResponse {
            workflow,
            nodes,
            edges,
            trigger,
        }),
    )
        .into_response()
}

/// Workflow lifecycle transitions driven through one
/// fetch-then-authorize helper.
enum WorkflowTransition {
    Publish,
    Archive,
}

/// Runs a workflow lifecycle transition with fetch-then-authorize.
/// Both transitions require the `update` operation.
async fn workflow_transition<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    workflow_id: WorkflowId,
    transition: WorkflowTransition,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(headers);
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    let ctx = match authorize_workflow(state, auth, headers, &workflow, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    let workflow = match match transition {
        WorkflowTransition::Publish => service.publish_workflow(workflow_id, &ctx).await,
        WorkflowTransition::Archive => service.archive_workflow(workflow_id, &ctx).await,
    } {
        Ok(workflow) => workflow,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(workflow)).into_response()
}

/// `POST /api/v1/workflows/{id}/publish` — publishes a draft workflow
/// and freezes an immutable version snapshot.
pub async fn publish_workflow<R, A, I>(
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
    let workflow_id = match path_workflow_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    workflow_transition(
        &state,
        auth,
        &headers,
        workflow_id,
        WorkflowTransition::Publish,
    )
    .await
}

/// `POST /api/v1/workflows/{id}/archive` — archives a workflow
/// (idempotent).
pub async fn archive_workflow<R, A, I>(
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
    let workflow_id = match path_workflow_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    workflow_transition(
        &state,
        auth,
        &headers,
        workflow_id,
        WorkflowTransition::Archive,
    )
    .await
}

/// `POST /api/v1/workflows/{id}/nodes` — adds a typed node.
///
/// Returns `201`. Requires the `workflow/update` grant.
pub async fn add_node<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<AddNodeBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let workflow_id = match path_workflow_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid request body",
                rejection.body_text(),
                instance,
            );
        }
    };
    let node_type = match parse_node_type(&body.node_type) {
        Ok(node_type) => node_type,
        Err(detail) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid node type",
                detail,
                instance,
            )
        }
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    let ctx = match authorize_workflow(&state, auth, &headers, &workflow, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    let input = NewWorkflowNode {
        workflow_id,
        node_type,
        config: body.config,
        position: body.position,
    };
    let node = match service.add_node(input, &ctx).await {
        Ok(node) => node,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(node)).into_response()
}

/// `GET /api/v1/workflows/{id}/nodes` — lists the workflow's nodes.
/// Requires the `workflow/read` grant.
pub async fn list_nodes<R, A, I>(
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
    let workflow_id = match path_workflow_id(path, &instance) {
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
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_workflow(&state, auth, &headers, &workflow, Operation::Read).await
    {
        return *response;
    }
    let nodes = match api_auth
        .workflow_nodes
        .list_by_workflow(workflow_id, 10_000)
        .await
    {
        Ok(nodes) => nodes,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = nodes.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "nodes": nodes, "total": total })),
    )
        .into_response()
}

/// `DELETE /api/v1/workflows/{id}/nodes/{node_id}` — removes a node
/// and its dangling edges. Requires the `workflow/delete` grant.
pub async fn remove_node<R, A, I>(
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
    let (workflow_id, node_id) = match path_node_id(path, &instance) {
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
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    // The node must belong to the workflow in the path; otherwise the
    // caller is addressing the wrong resource.
    let node = match api_auth.workflow_nodes.get(node_id).await {
        Ok(node) => node,
        Err(error) => return domain_error_response(error, instance),
    };
    if node.workflow_id != workflow_id {
        return problem_response(
            StatusCode::NOT_FOUND,
            "Not found",
            format!("node {node_id} does not belong to workflow {workflow_id}"),
            instance,
        );
    }
    let ctx = match authorize_workflow(&state, auth, &headers, &workflow, Operation::Delete).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    if let Err(error) = service.remove_node(node_id, &ctx).await {
        return domain_error_response(error, instance);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v1/workflows/{id}/edges` — adds a typed edge.
///
/// Returns `201`. Both endpoints must exist in the workflow;
/// self-loops are rejected. Requires the `workflow/update` grant.
pub async fn add_edge<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<AddEdgeBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let workflow_id = match path_workflow_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid request body",
                rejection.body_text(),
                instance,
            );
        }
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    let ctx = match authorize_workflow(&state, auth, &headers, &workflow, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    let input = NewWorkflowEdge {
        workflow_id,
        from_node: WorkflowNodeId::from(body.from_node),
        to_node: WorkflowNodeId::from(body.to_node),
        label: body.label,
    };
    let edge = match service.add_edge(input, &ctx).await {
        Ok(edge) => edge,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(edge)).into_response()
}

/// `GET /api/v1/workflows/{id}/edges` — lists the workflow's edges.
/// Requires the `workflow/read` grant.
pub async fn list_edges<R, A, I>(
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
    let workflow_id = match path_workflow_id(path, &instance) {
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
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_workflow(&state, auth, &headers, &workflow, Operation::Read).await
    {
        return *response;
    }
    let edges = match api_auth
        .workflow_edges
        .list_by_workflow(workflow_id, 10_000)
        .await
    {
        Ok(edges) => edges,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = edges.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "edges": edges, "total": total })),
    )
        .into_response()
}

/// `DELETE /api/v1/workflows/{id}/edges/{edge_id}` — removes an edge.
/// Requires the `workflow/delete` grant.
pub async fn remove_edge<R, A, I>(
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
    let (workflow_id, edge_id) = match path_edge_id(path, &instance) {
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
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    let edge = match api_auth.workflow_edges.get(edge_id).await {
        Ok(edge) => edge,
        Err(error) => return domain_error_response(error, instance),
    };
    if edge.workflow_id != workflow_id {
        return problem_response(
            StatusCode::NOT_FOUND,
            "Not found",
            format!("edge {edge_id} does not belong to workflow {workflow_id}"),
            instance,
        );
    }
    let ctx = match authorize_workflow(&state, auth, &headers, &workflow, Operation::Delete).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    if let Err(error) = service.remove_edge(edge_id, &ctx).await {
        return domain_error_response(error, instance);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `PUT /api/v1/workflows/{id}/trigger` — sets (or replaces) the
/// workflow's trigger. Exactly one trigger per workflow. Requires the
/// `workflow/update` grant.
pub async fn set_trigger<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<SetTriggerBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let workflow_id = match path_workflow_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid request body",
                rejection.body_text(),
                instance,
            );
        }
    };
    let trigger_type = match parse_trigger_type(&body.trigger_type) {
        Ok(trigger_type) => trigger_type,
        Err(detail) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid trigger type",
                detail,
                instance,
            )
        }
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    let ctx = match authorize_workflow(&state, auth, &headers, &workflow, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    let input = NewTrigger {
        workflow_id,
        trigger_type,
        config: body.config,
    };
    let trigger = match service.set_trigger(input, &ctx).await {
        Ok(trigger) => trigger,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(trigger)).into_response()
}

/// `GET /api/v1/workflows/{id}/trigger` — fetches the workflow's
/// trigger, or `404` when none is set. Requires the `workflow/read`
/// grant.
pub async fn get_trigger<R, A, I>(
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
    let workflow_id = match path_workflow_id(path, &instance) {
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
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_workflow(&state, auth, &headers, &workflow, Operation::Read).await
    {
        return *response;
    }
    match api_auth
        .workflow_triggers
        .get_by_workflow(workflow_id)
        .await
    {
        Ok(Some(trigger)) => (StatusCode::OK, Json(trigger)).into_response(),
        Ok(None) => problem_response(
            StatusCode::NOT_FOUND,
            "Not found",
            format!("workflow {workflow_id} has no trigger"),
            instance,
        ),
        Err(error) => domain_error_response(error, instance),
    }
}

/// `POST /api/v1/workflows/{id}/simulate` — dry-runs the workflow.
///
/// Pure: the simulator walks the graph and reports what *would*
/// happen; it never executes actions and never mutates state.
/// Requires the `workflow/read` grant.
pub async fn simulate_workflow<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<SimulateBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let workflow_id = match path_workflow_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid request body",
                rejection.body_text(),
                instance,
            );
        }
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    let ctx = match authorize_workflow(&state, auth, &headers, &workflow, Operation::Read).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = workflow_service(api_auth);
    let trace = match service.simulate(workflow_id, body.input, &ctx).await {
        Ok(trace) => trace,
        Err(error) => return domain_error_response(error, instance),
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({ "trace": trace, "steps": trace.len() })),
    )
        .into_response()
}

/// `GET /api/v1/workflows/{id}/versions` — lists the workflow's
/// version snapshots, newest first. Requires the `workflow/read`
/// grant.
pub async fn list_versions<R, A, I>(
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
    let workflow_id = match path_workflow_id(path, &instance) {
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
    let workflow = match fetch_workflow(api_auth, workflow_id, &instance).await {
        Ok(workflow) => workflow,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_workflow(&state, auth, &headers, &workflow, Operation::Read).await
    {
        return *response;
    }
    let versions = match api_auth
        .workflow_versions
        .list_by_workflow(workflow_id, 100)
        .await
    {
        Ok(versions) => versions,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = versions.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "versions": versions, "total": total })),
    )
        .into_response()
}
