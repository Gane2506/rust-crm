// SPDX-License-Identifier: GPL-3.0-only
//! Client-profile endpoints: the agency-managed workspace lifecycle
//! (task 05-CRM-02).
//!
//! * `POST /api/v1/profiles` — creates a profile; honors the
//!   `Idempotency-Key` request header.
//! * `GET /api/v1/profiles` — lists profiles for the caller's agency.
//! * `GET /api/v1/profiles/{id}` — fetches one profile.
//! * `POST /api/v1/profiles/{id}/suspend` — suspends an active profile.
//! * `POST /api/v1/profiles/{id}/reactivate` — reactivates a suspended profile.
//! * `POST /api/v1/profiles/{id}/archive` — archives a profile (terminal).
//!
//! ## Authorization
//!
//! Profiles are agency-level resources: every endpoint requires the
//! matching `ClientProfile` grant within the agency scope
//! ([`Resource::ClientProfile`], [`Scope::Agency`]). The caller's agency
//! comes from the session, never from request parameters. Clients never
//! act on profiles — there is no client login.
//!
//! [`Resource::ClientProfile`]: crm_domain::security::Resource::ClientProfile
//! [`Scope::Agency`]: crm_domain::security::Scope::Agency

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use axum::{
    extract::{rejection::PathRejection, Extension, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_domain::error::DomainError;
use crm_domain::ids::{AgencyId, ClientProfileId};
use crm_domain::profile::ClientProfile;
use crm_domain::repositories::ClientProfileRepository;
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::ActionContext;

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{
    domain_error_response, idempotency_key, request_id, AppState, IdempotencyStore,
};
use crate::problem_response;
use crm_core::memory::{ContactReadModel, ContactServiceImpl};
use crm_domain::services::AuditService;

/// Body of `POST /api/v1/profiles`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateProfileBody {
    /// Display name of the client's business.
    pub name: String,
}

/// Body of `GET /api/v1/profiles`.
#[derive(Debug, Serialize)]
struct ProfileListResponse {
    profiles: Vec<ClientProfile>,
    total: usize,
}

/// Requires the `ClientProfile` grant for `operation` at agency scope.
///
/// The caller's agency comes from the session (`RequestAuth`), never
/// from request parameters. Returns the resolved [`ActionContext`] and
/// the caller's agency id.
async fn authorize_agency<S, I>(
    state: &AppState<S, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    operation: Operation,
    instance: &str,
) -> Result<(ActionContext, AgencyId), Box<Response>>
where
    S: Clone + Send + Sync + 'static,
    I: Clone + Send + Sync + 'static,
{
    let Some(Extension(ref req_auth)) = auth else {
        return Err(Box::new(problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance.to_owned(),
        )));
    };
    let agency_id = req_auth.agency_id;
    let action = Action {
        resource: Resource::ClientProfile,
        operation,
    };
    // `None` profile → agency scope.
    match resolve_context(
        state,
        Some(Extension(req_auth.clone())),
        headers,
        &action,
        None,
    )
    .await
    {
        Ok(ctx) => Ok((ctx, agency_id)),
        Err(response) => Err(response),
    }
}

/// Extracts the profile id from the path, or renders a `422`.
fn profile_id_from_path(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<Uuid, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(id),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid profile id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// `POST /api/v1/profiles` — creates a client profile.
///
/// Returns `201` with the created profile. Idempotent via the
/// `Idempotency-Key` header (same key + body replays the original `201`;
/// same key + different body is a `409`).
///
/// Requires the `client-profile/create` grant at agency scope.
pub async fn create_profile<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<CreateProfileBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
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
    let key = match idempotency_key(&headers, &instance) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let fp = {
        let mut hasher = DefaultHasher::new();
        serde_json::to_string(&body)
            .unwrap_or_default()
            .hash(&mut hasher);
        hasher.finish()
    };
    if let Some(key) = key {
        match state.idempotency_store().lookup(&key, fp).await {
            Ok(Some(stored)) => return (StatusCode::CREATED, Json(stored)).into_response(),
            Ok(None) => {}
            Err(error) => return domain_error_response(error, instance.clone()),
        }
    }

    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let (_ctx, agency_id) =
        match authorize_agency(&state, auth, &headers, Operation::Create, &instance).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let profile = match ClientProfile::new(body.name, agency_id) {
        Ok(profile) => profile,
        Err(error) => return domain_error_response(error, instance),
    };
    let profile = match api_auth.profiles.create(profile).await {
        Ok(profile) => profile,
        Err(error) => return domain_error_response(error, instance),
    };
    let response_value = match serde_json::to_value(&profile) {
        Ok(value) => value,
        Err(_) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Serialization failed",
                "the created profile could not be serialized".to_owned(),
                instance,
            );
        }
    };
    if let Some(key) = key {
        if let Err(error) = state
            .idempotency_store()
            .record(key, fp, response_value)
            .await
        {
            tracing::warn!(%error, "idempotency response could not be persisted");
        }
    }
    (StatusCode::CREATED, Json(profile)).into_response()
}

/// `GET /api/v1/profiles` — lists the caller's agency profiles.
///
/// Requires the `client-profile/read` grant at agency scope.
pub async fn list_profiles<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
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
    let (_ctx, agency_id) =
        match authorize_agency(&state, auth, &headers, Operation::Read, &instance).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    match api_auth.profiles.list_for_agency(agency_id, 100).await {
        Ok(profiles) => {
            let total = profiles.len();
            (
                StatusCode::OK,
                Json(ProfileListResponse { profiles, total }),
            )
                .into_response()
        }
        Err(error) => domain_error_response(error, instance),
    }
}

/// `GET /api/v1/profiles/{id}` — fetches one profile.
///
/// The profile must belong to the caller's agency (`404` otherwise —
/// agency isolation, not an existence leak).
///
/// Requires the `client-profile/read` grant at agency scope.
pub async fn get_profile<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let id = match profile_id_from_path(path, &instance) {
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
    let (_ctx, agency_id) =
        match authorize_agency(&state, auth, &headers, Operation::Read, &instance).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    match api_auth.profiles.get(ClientProfileId::from(id)).await {
        Ok(profile) => {
            if profile.agency_id != agency_id {
                return domain_error_response(
                    DomainError::NotFound {
                        kind: "client_profile".to_owned(),
                        id: id.to_string(),
                    },
                    instance,
                );
            }
            Json(profile).into_response()
        }
        Err(error) => domain_error_response(error, instance),
    }
}

/// Applies a lifecycle transition to a profile. Shared by the
/// suspend/reactivate/archive endpoints.
async fn transition_profile<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    id: Uuid,
    instance: &str,
    transition: fn(&mut ClientProfile) -> Result<(), DomainError>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance.to_owned(),
        );
    };
    let (_ctx, agency_id) =
        match authorize_agency(state, auth, headers, Operation::Update, instance).await {
            Ok(ctx) => ctx,
            Err(response) => return *response,
        };
    let mut profile = match api_auth.profiles.get(ClientProfileId::from(id)).await {
        Ok(profile) => profile,
        Err(error) => return domain_error_response(error, instance.to_owned()),
    };
    if profile.agency_id != agency_id {
        return domain_error_response(
            DomainError::NotFound {
                kind: "client_profile".to_owned(),
                id: id.to_string(),
            },
            instance.to_owned(),
        );
    }
    if let Err(error) = transition(&mut profile) {
        return domain_error_response(error, instance.to_owned());
    }
    match api_auth.profiles.update(profile).await {
        Ok(profile) => Json(profile).into_response(),
        Err(error) => domain_error_response(error, instance.to_owned()),
    }
}

/// Renders a transition endpoint: extracts the path id, then delegates
/// to [`transition_profile`].
async fn transition_endpoint<R, A, I>(
    state: AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    instance: String,
    transition: fn(&mut ClientProfile) -> Result<(), DomainError>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let id = match profile_id_from_path(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    transition_profile(&state, auth, &headers, id, &instance, transition).await
}

/// `POST /api/v1/profiles/{id}/suspend` — suspends an active profile.
///
/// Idempotent; archiving is terminal. Requires the
/// `client-profile/update` grant at agency scope.
pub async fn suspend_profile<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    transition_endpoint(state, auth, headers, path, instance, ClientProfile::suspend).await
}

/// `POST /api/v1/profiles/{id}/reactivate` — reactivates a suspended profile.
///
/// Idempotent. Requires the `client-profile/update` grant at agency scope.
pub async fn reactivate_profile<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    transition_endpoint(
        state,
        auth,
        headers,
        path,
        instance,
        ClientProfile::reactivate,
    )
    .await
}

/// `POST /api/v1/profiles/{id}/archive` — archives a profile (terminal).
///
/// The row is never deleted. Requires the `client-profile/update`
/// grant at agency scope.
pub async fn archive_profile<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    transition_endpoint(state, auth, headers, path, instance, ClientProfile::archive).await
}
