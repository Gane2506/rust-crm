// SPDX-License-Identifier: GPL-3.0-only
//! Asset endpoints: file/media attachment metadata (task 05-CRM-02).
//!
//! * `POST /api/v1/assets` — registers an uploaded asset's metadata;
//!   honors the `Idempotency-Key` request header. Uploads are deduped
//!   by content hash within the profile: re-registering the same bytes
//!   returns the existing asset (`200`) instead of a duplicate.
//! * `GET /api/v1/assets` — lists assets for one client profile.
//! * `GET /api/v1/assets/{id}` — fetches one asset's metadata.
//! * `POST /api/v1/assets/{id}/archive` — archives an asset (terminal).
//!
//! ## Authorization
//!
//! Assets are profile-scoped: every endpoint requires the matching
//! `Asset` grant within the asset's client profile
//! ([`Resource::Asset`], [`Scope::ClientProfile`]).
//!
//! ## Scope note
//!
//! These endpoints register and describe asset *metadata*. The byte
//! store itself (upload/download of file contents) is out of scope for
//! this task — the `sha256` is supplied by the uploader (e.g. computed
//! client-side or by the agency's ingestion pipeline) and the API
//! treats it as the content address for dedupe and integrity.
//!
//! [`Resource::Asset`]: crm_domain::security::Resource::Asset
//! [`Scope::ClientProfile`]: crm_domain::security::Scope::ClientProfile

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_domain::asset::Asset;
use crm_domain::ids::{AssetId, ClientProfileId};
use crm_domain::repositories::AssetRepository;
use crm_domain::security::{Action, Operation, Resource};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{
    domain_error_response, idempotency_key, request_id, AppState, IdempotencyStore,
};
use crate::problem_response;
use crm_core::memory::{ContactReadModel, ContactServiceImpl};
use crm_domain::services::AuditService;

/// Body of `POST /api/v1/assets`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegisterAssetBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Original file name.
    pub file_name: String,
    /// MIME type supplied at upload.
    pub mime_type: String,
    /// Size in bytes; must be greater than zero.
    pub size_bytes: u64,
    /// SHA-256 of the file bytes, hex-encoded (64 chars).
    pub sha256_hex: String,
}

/// Query of `GET /api/v1/assets`.
#[derive(Debug, Deserialize)]
pub struct ListAssetsQuery {
    /// Owning client profile (required tenant scope).
    pub client_profile_id: Uuid,
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
}

/// Body of `GET /api/v1/assets`.
#[derive(Debug, Serialize)]
struct AssetListResponse {
    assets: Vec<Asset>,
    total: usize,
}

/// Decodes the hex SHA-256 from the request body.
fn decode_sha256(hex: &str, instance: &str) -> Result<[u8; 32], Box<Response>> {
    let hex = hex.trim();
    let invalid = |detail: &str| {
        Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid SHA-256",
            detail.to_owned(),
            instance.to_owned(),
        ))
    };
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("sha256_hex must be 64 hex characters"));
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hex_pair =
            std::str::from_utf8(chunk).map_err(|_| invalid("sha256_hex must be ASCII hex"))?;
        bytes[i] = u8::from_str_radix(hex_pair, 16)
            .map_err(|_| invalid("sha256_hex must be valid hexadecimal"))?;
    }
    Ok(bytes)
}

/// `POST /api/v1/assets` — registers an asset's metadata.
///
/// Returns `201` with the asset, or `200` with the existing asset when
/// the same bytes were already registered under the profile (upload
/// dedupe by content hash). Idempotent via `Idempotency-Key`.
///
/// Requires the `asset/create` grant within the asset's client profile
/// scope (`403` without it).
pub async fn register_asset<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<RegisterAssetBody>, axum::extract::rejection::JsonRejection>,
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
    let profile_id = ClientProfileId::from(body.client_profile_id);
    let action = Action {
        resource: Resource::Asset,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let sha256 = match decode_sha256(&body.sha256_hex, &instance) {
        Ok(sha256) => sha256,
        Err(response) => return *response,
    };
    // Upload dedupe: the same bytes under this profile return the
    // existing asset instead of a duplicate row.
    if let Ok(Some(existing)) = api_auth.assets.find_by_sha256(profile_id, &sha256).await {
        return (StatusCode::OK, Json(existing)).into_response();
    }
    let asset = match Asset::new(
        profile_id,
        ctx.actor_id,
        body.file_name,
        body.mime_type,
        body.size_bytes,
        sha256,
    ) {
        Ok(asset) => asset,
        Err(error) => return domain_error_response(error, instance),
    };
    let asset = match api_auth.assets.create(asset).await {
        Ok(asset) => asset,
        Err(error) => return domain_error_response(error, instance),
    };
    let response_value = match serde_json::to_value(&asset) {
        Ok(value) => value,
        Err(_) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Serialization failed",
                "the registered asset could not be serialized".to_owned(),
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
    (StatusCode::CREATED, Json(asset)).into_response()
}

/// `GET /api/v1/assets` — lists assets for one client profile.
///
/// Requires the `asset/read` grant within the requested profile scope.
pub async fn list_assets<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    query: Result<Query<ListAssetsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let query = match query {
        Ok(Query(query)) => query,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid query",
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
    let profile_id = ClientProfileId::from(query.client_profile_id);
    let action = Action {
        resource: Resource::Asset,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, Some(profile_id)).await
    {
        return *response;
    }
    let limit = query.limit.unwrap_or(20).min(100) as u64;
    match api_auth.assets.list_for_client(profile_id, limit).await {
        Ok(assets) => {
            let total = assets.len();
            (StatusCode::OK, Json(AssetListResponse { assets, total })).into_response()
        }
        Err(error) => domain_error_response(error, instance),
    }
}

/// `GET /api/v1/assets/{id}` — fetches one asset's metadata.
///
/// Authorizes against the asset's own profile scope. Requires the
/// `asset/read` grant.
pub async fn get_asset<R, A, I>(
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
    let id = match path {
        Ok(Path(id)) => id,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid asset id",
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
    let asset = match api_auth.assets.get(AssetId::from(id)).await {
        Ok(asset) => asset,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::Asset,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(
        &state,
        auth,
        &headers,
        &action,
        Some(asset.client_profile_id),
    )
    .await
    {
        return *response;
    }
    Json(asset).into_response()
}

/// `POST /api/v1/assets/{id}/archive` — archives an asset (terminal).
///
/// The row is never deleted. Requires the `asset/update` grant within
/// the asset's profile scope.
pub async fn archive_asset<R, A, I>(
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
    let id = match path {
        Ok(Path(id)) => id,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid asset id",
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
    let mut asset = match api_auth.assets.get(AssetId::from(id)).await {
        Ok(asset) => asset,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::Asset,
        operation: Operation::Update,
    };
    if let Err(response) = resolve_context(
        &state,
        auth,
        &headers,
        &action,
        Some(asset.client_profile_id),
    )
    .await
    {
        return *response;
    }
    if let Err(error) = asset.archive() {
        return domain_error_response(error, instance);
    }
    match api_auth.assets.update(asset).await {
        Ok(asset) => Json(asset).into_response(),
        Err(error) => domain_error_response(error, instance),
    }
}
