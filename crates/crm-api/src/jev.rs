// SPDX-License-Identifier: GPL-3.0-only
//! Jev agent endpoints: run lifecycle, tool calls, handoffs (task 09-JEV-02).
//!
//! * `POST /api/v1/agent-runs` — creates and starts a run (`201`).
//! * `GET /api/v1/agent-runs?client_profile_id=…` — lists runs (`200`).
//! * `GET /api/v1/agent-runs/{id}` — fetches a run (`200`).
//! * `POST /api/v1/agent-runs/{id}/request-approval` — pauses for approval.
//! * `POST /api/v1/agent-runs/{id}/approve` — human approves (`approve` grant).
//! * `POST /api/v1/agent-runs/{id}/deny` — human denies (`approve` grant).
//! * `POST /api/v1/agent-runs/{id}/complete` — completes with output.
//! * `POST /api/v1/agent-runs/{id}/fail` — fails the run.
//! * `POST /api/v1/agent-runs/{id}/cancel` — cancels the run.
//! * `POST /api/v1/agent-runs/{id}/tool-calls` — records a tool call (`201`).
//! * `GET /api/v1/agent-runs/{id}/tool-calls` — lists a run's tool calls.
//! * `POST /api/v1/agent-runs/{id}/handoff` — records a handoff (`201`).
//! * `GET /api/v1/agent-runs/{id}/handoffs` — lists a run's handoffs.
//!
//! ## Authorization
//!
//! All endpoints use the `AgentRun` grants within the run's client
//! profile ([`Resource::AgentRun`](crm_domain::security::Resource::AgentRun)).
//! Approve/deny additionally require the `approve` operation, which the
//! service maps to the `agent_approver` role through
//! [`crm_db::jev::PgRoleMembership`].

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_core::jev::AgentServiceImpl;
use crm_db::jev::{
    PgAgentRunRepository, PgHandoffRepository, PgRoleMembership, PgToolCallRepository,
};
use crm_db::postgres::PgAuditService;
use crm_domain::ids::{AgentRunId, ClientProfileId};
use crm_domain::jev::{AgentRun, FallbackChain};
use crm_domain::repositories::{AgentRunRepository, HandoffRepository, ToolCallRepository};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{AgentService, AuditService, NewAgentRun, NewHandoff, NewToolCall};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/agent-runs`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAgentRunBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Which kind of agent executes the run (non-blank).
    pub agent_kind: String,
    /// Ordered model-adapter names; the first is the primary.
    pub adapter_names: Vec<String>,
    /// Input payload for the run (task description, parameters).
    pub input: serde_json::Value,
}

/// Query of `GET /api/v1/agent-runs`.
#[derive(Debug, Deserialize)]
pub struct ListAgentRunsQuery {
    /// Owning client profile (required: runs are always profile-scoped).
    pub client_profile_id: Uuid,
    /// Filter by status: `pending`, `running`, `awaiting_approval`,
    /// `completed`, `failed`, `cancelled`.
    pub status: Option<String>,
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
}

/// Body of `POST /api/v1/agent-runs/{id}/complete`.
#[derive(Debug, Clone, Deserialize)]
pub struct CompleteRunBody {
    /// Output payload recorded on the run.
    pub output: serde_json::Value,
}

/// Body of `POST /api/v1/agent-runs/{id}/tool-calls`.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordToolCallBody {
    /// Tool name (non-blank).
    pub tool_name: String,
    /// Arguments supplied by the model.
    pub input: serde_json::Value,
    /// Whether the tool requires human approval. Declared by the
    /// caller from the tool definition (a tool registry is a later
    /// task); the service enforces the approval flow from this flag.
    #[serde(default)]
    pub requires_approval: bool,
}

/// Body of `POST /api/v1/agent-runs/{id}/handoff`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateHandoffBody {
    /// Which agent kind receives the work (non-blank).
    pub to_agent_kind: String,
    /// Context for the receiving agent (opaque JSON, never null).
    pub context: serde_json::Value,
}

/// `GET /api/v1/agent-runs/{id}` response: the run plus its tool calls.
#[derive(Debug, Serialize)]
struct AgentRunDetailResponse {
    run: AgentRun,
    tool_calls: Vec<crm_domain::jev::ToolCall>,
}

/// Builds the agent service from the API auth components.
fn agent_service(
    api_auth: &crate::auth::ApiAuth,
) -> AgentServiceImpl<
    PgAgentRunRepository,
    PgToolCallRepository,
    PgHandoffRepository,
    PgRoleMembership,
    PgAuditService,
> {
    AgentServiceImpl::new(
        api_auth.agent_runs.clone(),
        api_auth.tool_calls.clone(),
        api_auth.handoffs.clone(),
        api_auth.roles.clone(),
        api_auth.audit_service.clone(),
    )
}

/// Parses a run status from its string form.
fn parse_run_status(raw: &str) -> Result<crm_domain::jev::AgentRunStatus, String> {
    use crm_domain::jev::AgentRunStatus;
    match raw {
        "pending" => Ok(AgentRunStatus::Pending),
        "running" => Ok(AgentRunStatus::Running),
        "awaiting_approval" => Ok(AgentRunStatus::AwaitingApproval),
        "completed" => Ok(AgentRunStatus::Completed),
        "failed" => Ok(AgentRunStatus::Failed),
        "cancelled" => Ok(AgentRunStatus::Cancelled),
        other => Err(format!("unknown agent run status '{other}'")),
    }
}

/// Extracts an agent-run id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_run_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<AgentRunId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(AgentRunId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid run id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// `POST /api/v1/agent-runs` — creates and starts a run.
///
/// Returns `201` with the running run. Requires the `agent_run/create`
/// grant in the run's client profile (`403` without it).
pub async fn create_agent_run<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<CreateAgentRunBody>, axum::extract::rejection::JsonRejection>,
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
        resource: Resource::AgentRun,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let fallback_chain = match FallbackChain::new(body.adapter_names) {
        Ok(chain) => chain,
        Err(error) => return domain_error_response(error, instance),
    };
    let service = agent_service(api_auth);
    let input = NewAgentRun {
        client_profile_id: profile_id,
        agent_kind: body.agent_kind,
        fallback_chain,
        input: body.input,
    };
    let run = match service.start_run(input, &ctx).await {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(run)).into_response()
}

/// `GET /api/v1/agent-runs` — lists runs for a client profile.
///
/// Requires the `agent_run/read` grant in the profile (`403` without it).
pub async fn list_agent_runs<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListAgentRunsQuery>,
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
        resource: Resource::AgentRun,
        operation: Operation::Read,
    };
    let _ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let limit = query.limit.unwrap_or(20).min(100) as u64;
    let runs = match &query.status {
        Some(raw) => {
            let status = match parse_run_status(raw) {
                Ok(status) => status,
                Err(detail) => {
                    return problem_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "Invalid status",
                        detail,
                        instance,
                    )
                }
            };
            api_auth
                .agent_runs
                .list_by_status(profile_id, status, limit)
                .await
        }
        None => api_auth.agent_runs.list_by_profile(profile_id, limit).await,
    };
    let runs = match runs {
        Ok(runs) => runs,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = runs.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "agent_runs": runs, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/agent-runs/{id}` — fetches a run with its tool calls.
///
/// Fetch-then-authorize: a missing run surfaces as `404` before any
/// grant check. Requires the `agent_run/read` grant in the run's
/// profile.
pub async fn get_agent_run<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
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
    let run = match api_auth.agent_runs.get(run_id).await {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::AgentRun,
        operation: Operation::Read,
    };
    if let Err(response) =
        resolve_context(&state, auth, &headers, &action, Some(run.client_profile_id)).await
    {
        return *response;
    }
    let tool_calls = match api_auth.tool_calls.list_by_run(run_id, 100).await {
        Ok(calls) => calls,
        Err(error) => return domain_error_response(error, instance),
    };
    (
        StatusCode::OK,
        Json(AgentRunDetailResponse { run, tool_calls }),
    )
        .into_response()
}

/// Run lifecycle transitions driven through one fetch-then-authorize
/// helper.
enum AgentRunTransition {
    RequestApproval,
    Approve,
    Deny,
    Complete(serde_json::Value),
    Fail,
    Cancel,
}

/// Runs an agent-run lifecycle transition with fetch-then-authorize.
///
/// Approve/deny require the `approve` operation; the other transitions
/// require `update`.
async fn agent_run_transition<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    run_id: AgentRunId,
    transition: AgentRunTransition,
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
    let run = match api_auth.agent_runs.get(run_id).await {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    let operation = match transition {
        AgentRunTransition::Approve | AgentRunTransition::Deny => Operation::Approve,
        _ => Operation::Update,
    };
    let action = Action {
        resource: Resource::AgentRun,
        operation,
    };
    let ctx =
        match resolve_context(state, auth, headers, &action, Some(run.client_profile_id)).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = agent_service(api_auth);
    let run = match match transition {
        AgentRunTransition::RequestApproval => service.request_approval(run_id, &ctx).await,
        AgentRunTransition::Approve => service.approve_step(run_id, &ctx).await,
        AgentRunTransition::Deny => service.deny_step(run_id, &ctx).await,
        AgentRunTransition::Complete(output) => service.complete_run(run_id, output, &ctx).await,
        AgentRunTransition::Fail => service.fail_run(run_id, &ctx).await,
        AgentRunTransition::Cancel => service.cancel_run(run_id, &ctx).await,
    } {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(run)).into_response()
}

/// `POST /api/v1/agent-runs/{id}/request-approval` — pauses a running
/// run to await a human decision.
pub async fn request_approval<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    agent_run_transition(
        &state,
        auth,
        &headers,
        run_id,
        AgentRunTransition::RequestApproval,
    )
    .await
}

/// `POST /api/v1/agent-runs/{id}/approve` — a human approves the run.
///
/// Requires the `agent_run/approve` grant; the service additionally
/// checks the `agent_approver` role before recording the decision.
pub async fn approve_run<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    agent_run_transition(&state, auth, &headers, run_id, AgentRunTransition::Approve).await
}

/// `POST /api/v1/agent-runs/{id}/deny` — a human denies the run,
/// failing it. Requires the `agent_run/approve` grant.
pub async fn deny_run<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    agent_run_transition(&state, auth, &headers, run_id, AgentRunTransition::Deny).await
}

/// `POST /api/v1/agent-runs/{id}/complete` — completes a running run
/// with its output.
pub async fn complete_run<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<CompleteRunBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let run_id = match path_run_id(path, &instance) {
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
    agent_run_transition(
        &state,
        auth,
        &headers,
        run_id,
        AgentRunTransition::Complete(body.output),
    )
    .await
}

/// `POST /api/v1/agent-runs/{id}/fail` — fails a run (error path).
pub async fn fail_run<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    agent_run_transition(&state, auth, &headers, run_id, AgentRunTransition::Fail).await
}

/// `POST /api/v1/agent-runs/{id}/cancel` — cancels a run from any
/// non-terminal status (idempotent).
pub async fn cancel_run<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    agent_run_transition(&state, auth, &headers, run_id, AgentRunTransition::Cancel).await
}

/// `POST /api/v1/agent-runs/{id}/tool-calls` — records a
/// model-requested tool call.
///
/// Returns `201`. When `requires_approval` is set the call stays
/// `Pending` and the run pauses for a human decision; otherwise the
/// service approves it immediately. Requires the `agent_run/update`
/// grant in the run's profile.
pub async fn record_tool_call<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<RecordToolCallBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let run_id = match path_run_id(path, &instance) {
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
    let run = match api_auth.agent_runs.get(run_id).await {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::AgentRun,
        operation: Operation::Update,
    };
    let ctx =
        match resolve_context(&state, auth, &headers, &action, Some(run.client_profile_id)).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = agent_service(api_auth);
    let input = NewToolCall {
        run_id,
        tool_name: body.tool_name,
        input: body.input,
        requires_approval: body.requires_approval,
    };
    let call = match service.record_tool_call(input, &ctx).await {
        Ok(call) => call,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(call)).into_response()
}

/// `GET /api/v1/agent-runs/{id}/tool-calls` — lists a run's tool
/// calls, oldest first. Requires the `agent_run/read` grant.
pub async fn list_tool_calls<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
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
    let run = match api_auth.agent_runs.get(run_id).await {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::AgentRun,
        operation: Operation::Read,
    };
    if let Err(response) =
        resolve_context(&state, auth, &headers, &action, Some(run.client_profile_id)).await
    {
        return *response;
    }
    let calls = match api_auth.tool_calls.list_by_run(run_id, 100).await {
        Ok(calls) => calls,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = calls.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "tool_calls": calls, "total": total })),
    )
        .into_response()
}

/// `POST /api/v1/agent-runs/{id}/handoff` — records an agent-to-agent
/// context transfer.
///
/// Returns `201` with the handoff. The receiving run is started
/// separately via `POST /api/v1/agent-runs`. Requires the
/// `agent_run/update` grant in the run's profile.
pub async fn create_handoff<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<CreateHandoffBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let run_id = match path_run_id(path, &instance) {
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
    let run = match api_auth.agent_runs.get(run_id).await {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::AgentRun,
        operation: Operation::Update,
    };
    let ctx =
        match resolve_context(&state, auth, &headers, &action, Some(run.client_profile_id)).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let service = agent_service(api_auth);
    let input = NewHandoff {
        from_run_id: run_id,
        to_agent_kind: body.to_agent_kind,
        context: body.context,
    };
    let handoff = match service.handoff(input, &ctx).await {
        Ok(handoff) => handoff,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(handoff)).into_response()
}

/// `GET /api/v1/agent-runs/{id}/handoffs` — lists a run's handoffs,
/// newest first. Requires the `agent_run/read` grant.
pub async fn list_handoffs<R, A, I>(
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
    let run_id = match path_run_id(path, &instance) {
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
    let run = match api_auth.agent_runs.get(run_id).await {
        Ok(run) => run,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::AgentRun,
        operation: Operation::Read,
    };
    if let Err(response) =
        resolve_context(&state, auth, &headers, &action, Some(run.client_profile_id)).await
    {
        return *response;
    }
    let handoffs = match api_auth.handoffs.list_by_run(run_id, 100).await {
        Ok(handoffs) => handoffs,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = handoffs.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "handoffs": handoffs, "total": total })),
    )
        .into_response()
}
