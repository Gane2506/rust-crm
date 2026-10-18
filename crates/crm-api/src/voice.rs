// SPDX-License-Identifier: GPL-3.0-only
//! Calling endpoints: calls and consent-gated recordings (task
//! 14-VOICE-02).
//!
//! * `POST /api/v1/calls` — places an outbound call (`201`).
//! * `GET /api/v1/calls?client_profile_id=…` — lists calls, newest
//!   first (`200`).
//! * `GET /api/v1/calls/{id}` — fetches a call (`200`).
//! * `POST /api/v1/calls/{id}/answer` — answers a ringing call
//!   (`Ringing → InProgress`).
//! * `POST /api/v1/calls/{id}/end` — ends a call, moving it to the
//!   terminal status the domain allows from its current state.
//! * `POST /api/v1/calls/{id}/recordings` — captures recording
//!   consent and starts recording (consent-gated: only
//!   `granted` is accepted).
//! * `POST /api/v1/calls/{id}/recordings/stop` — stops recording,
//!   attaching the audio asset.
//!
//! ## Authorization
//!
//! Call endpoints authorize against
//! [`Resource::Call`](crm_domain::security::Resource::Call),
//! profile-scoped. Fetch-then-authorize: a missing call surfaces as
//! `404` before any grant check.

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crm_core::voice::{StubVoiceProvider, VoiceServiceImpl};
use crm_db::voice::PgCallRepository;
use crm_domain::ids::{AssetId, CallId, ClientProfileId, ContactId};
use crm_domain::repositories::CallRepository;
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{AuditService, NewCall, NewRecording, VoiceService};
use crm_domain::voice::{Call, CallDirection, PhoneNumber, RecordingConsent};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/calls`.
#[derive(Debug, Deserialize)]
pub struct PlaceCallBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Contact on the other end, when known.
    pub contact_id: Option<Uuid>,
    /// Which way the call goes.
    pub direction: CallDirection,
    /// The dialing party (validated E.164-ish; invalid values are a
    /// `422` via axum's JSON rejection).
    pub from: PhoneNumber,
    /// The dialed party (validated E.164-ish).
    pub to: PhoneNumber,
}

/// Body of `POST /api/v1/calls/{id}/recordings`.
#[derive(Debug, Deserialize)]
pub struct StartRecordingBody {
    /// The called party's consent decision (`granted` only).
    pub consent: RecordingConsent,
}

/// Body of `POST /api/v1/calls/{id}/recordings/stop`.
#[derive(Debug, Deserialize)]
pub struct StopRecordingBody {
    /// The audio asset to attach.
    pub asset_id: Uuid,
}

/// Query of `GET /api/v1/calls`.
#[derive(Debug, Deserialize)]
pub struct ListCallsQuery {
    /// Owning client profile (required).
    pub client_profile_id: Option<Uuid>,
    /// Max items returned (default 20).
    pub limit: Option<u64>,
}

/// Builds the calling service from the API auth components.
fn voice_service(
    api_auth: &crate::auth::ApiAuth,
) -> VoiceServiceImpl<PgCallRepository, StubVoiceProvider, crm_db::postgres::PgAuditService> {
    VoiceServiceImpl::new(
        api_auth.calls.clone(),
        api_auth.voice_provider.clone(),
        api_auth.audit_service.clone(),
    )
}

/// Extracts a call id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_call_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<CallId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(CallId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid call id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Fetches a call or returns its 404. Fetch-then-authorize: the
/// existence check runs before any grant check.
async fn fetch_call(
    api_auth: &crate::auth::ApiAuth,
    call_id: CallId,
    instance: &str,
) -> Result<Call, Box<Response>> {
    match api_auth.calls.get(call_id).await {
        Ok(call) => Ok(call),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Authorizes `operation` against the call's client profile, or
/// returns the 401/403 response.
async fn authorize_call<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    call: &Call,
    operation: Operation,
) -> Result<crm_domain::services::ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource: Resource::Call,
        operation,
    };
    match resolve_context(state, auth, headers, &action, Some(call.client_profile_id)).await {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// Call lifecycle transitions driven through one fetch-then-authorize
/// helper.
enum CallTransition {
    Answer,
    End,
}

/// Runs a call lifecycle transition with fetch-then-authorize.
async fn call_transition<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    call_id: CallId,
    transition: CallTransition,
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
    let call = match fetch_call(api_auth, call_id, &instance).await {
        Ok(call) => call,
        Err(response) => return *response,
    };
    let ctx = match authorize_call(state, auth, headers, &call, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = voice_service(api_auth);
    let call = match match transition {
        CallTransition::Answer => service.answer_call(call_id, &ctx).await,
        CallTransition::End => service.end_call(call_id, &ctx).await,
    } {
        Ok(call) => call,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(call)).into_response()
}

/// `POST /api/v1/calls` — places an outbound call (`201`).
///
/// Requires the `call/create` grant in the profile (`403` without
/// it).
pub async fn place_call<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<PlaceCallBody>,
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
    let profile_id = ClientProfileId::from(body.client_profile_id);
    let action = Action {
        resource: Resource::Call,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = voice_service(api_auth);
    let call = match service
        .place_call(
            NewCall {
                client_profile_id: profile_id,
                contact_id: body.contact_id.map(ContactId::from),
                direction: body.direction,
                from: body.from,
                to: body.to,
            },
            &ctx,
        )
        .await
    {
        Ok(call) => call,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(call)).into_response()
}

/// `GET /api/v1/calls?client_profile_id=…` — lists calls, newest
/// first (`200`).
///
/// Requires the `call/read` grant in the profile (`403` without it).
/// The response is `{calls, total}`; `total` is the page length.
pub async fn list_calls<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListCallsQuery>,
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
    let Some(profile_uuid) = query.client_profile_id else {
        return problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Missing client_profile_id",
            "listing calls requires a client_profile_id query parameter".to_owned(),
            instance,
        );
    };
    let profile_id = ClientProfileId::from(profile_uuid);
    let action = Action {
        resource: Resource::Call,
        operation: Operation::Read,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = voice_service(api_auth);
    let calls = match service.list_calls(query.limit.unwrap_or(20), &ctx).await {
        Ok(calls) => calls,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = calls.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "calls": calls, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/calls/{id}` — fetches a call (`200`).
///
/// Fetch-then-authorize: a missing call is `404` before any grant
/// check.
pub async fn get_call<R, A, I>(
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
    let call_id = match path_call_id(path, &instance) {
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
    let call = match fetch_call(api_auth, call_id, &instance).await {
        Ok(call) => call,
        Err(response) => return *response,
    };
    if let Err(response) = authorize_call(&state, auth, &headers, &call, Operation::Read).await {
        return *response;
    }
    (StatusCode::OK, Json(call)).into_response()
}

/// `POST /api/v1/calls/{id}/answer` — answers a ringing call
/// (`Ringing → InProgress`).
pub async fn answer_call<R, A, I>(
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
    let call_id = match path_call_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    call_transition(&state, auth, &headers, call_id, CallTransition::Answer).await
}

/// `POST /api/v1/calls/{id}/end` — ends a call, moving it to the
/// terminal status the domain allows from its current state.
pub async fn end_call<R, A, I>(
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
    let call_id = match path_call_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    call_transition(&state, auth, &headers, call_id, CallTransition::End).await
}

/// `POST /api/v1/calls/{id}/recordings` — captures recording consent
/// and starts recording.
///
/// Consent-gated: anything but `granted` is a `422` (the domain
/// rejects it), and recording can only start on a connected call.
pub async fn start_recording<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<StartRecordingBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let call_id = match path_call_id(path, &instance) {
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
    let call = match fetch_call(api_auth, call_id, &instance).await {
        Ok(call) => call,
        Err(response) => return *response,
    };
    let ctx = match authorize_call(&state, auth, &headers, &call, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = voice_service(api_auth);
    let call = match service
        .start_recording(
            NewRecording {
                call_id,
                consent: body.consent,
            },
            &ctx,
        )
        .await
    {
        Ok(call) => call,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(call)).into_response()
}

/// `POST /api/v1/calls/{id}/recordings/stop` — stops recording,
/// attaching the audio asset.
pub async fn stop_recording<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<StopRecordingBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let call_id = match path_call_id(path, &instance) {
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
    let call = match fetch_call(api_auth, call_id, &instance).await {
        Ok(call) => call,
        Err(response) => return *response,
    };
    let ctx = match authorize_call(&state, auth, &headers, &call, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = voice_service(api_auth);
    let call = match service
        .stop_recording(call_id, AssetId::from(body.asset_id), &ctx)
        .await
    {
        Ok(call) => call,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(call)).into_response()
}
