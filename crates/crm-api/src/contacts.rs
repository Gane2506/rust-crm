// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Contacts endpoints: the 01-FND-02 vertical slice over
//! [`crm_core::memory`]'s in-memory services.
//!
//! * `POST /api/v1/contacts` — create a contact; honors the
//!   `Idempotency-Key` request header.
//! * `GET /api/v1/contacts` — list contacts for one client profile,
//!   paginated.
//! * `GET /api/v1/contacts/{id}` — fetch one contact.
//!
//! Errors keep the crate's RFC 7807 `application/problem+json` shape:
//! validation failures → `422`, unknown resources → `404`, duplicate
//! state → `409`.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crm_core::memory::{
    api_action_context, InMemoryAuditRepository, InMemoryContactRepository, MemoryAuditService,
    MemoryContactService,
};
use crm_core::{IdempotencyKey, Pagination};
use crm_domain::contact::Contact;
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, ContactId};
use crm_domain::repositories::ContactRepository;
use crm_domain::services::{ContactService, NewContact};

use crate::{problem_response, REQUEST_ID_HEADER};

/// Header carrying the client-generated idempotency key for mutating calls.
const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
/// Default page size for the list endpoint.
const DEFAULT_LIMIT: u32 = 20;
/// Stand-in bound on `offset`; the SQL implementation in 02-DB pages in
/// the database instead of slicing in memory.
const MAX_OFFSET: u32 = 10_000;

/// Shared application state: the vertical-slice services plus the
/// idempotency-key store.
#[derive(Debug, Clone)]
pub struct AppState {
    contacts: MemoryContactService,
    /// `Idempotency-Key` → (request fingerprint, response body).
    idempotency: Arc<RwLock<HashMap<IdempotencyKey, (u64, serde_json::Value)>>>,
}

impl AppState {
    /// Builds fresh in-memory state. The server builds one; tests build
    /// one per test for isolation.
    #[must_use]
    pub fn new() -> Self {
        let audit = MemoryAuditService::new(InMemoryAuditRepository::new());
        let contacts = MemoryContactService::new(InMemoryContactRepository::new(), audit);
        Self {
            contacts,
            idempotency: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Borrows the contact service.
    #[must_use]
    pub fn contact_service(&self) -> &MemoryContactService {
        &self.contacts
    }

    /// Returns the stored response for a seen idempotency key, or `None`
    /// for a fresh key. A reused key with a *different* request body is a
    /// [`DomainError::Conflict`], never a silent replay.
    async fn idempotent_replay(
        &self,
        key: &IdempotencyKey,
        fingerprint: u64,
    ) -> Result<Option<serde_json::Value>, DomainError> {
        match self.idempotency.read().await.get(key) {
            None => Ok(None),
            Some((stored_fingerprint, body)) if *stored_fingerprint == fingerprint => {
                Ok(Some(body.clone()))
            }
            Some(_) => Err(DomainError::Conflict(
                "idempotency key was already used with a different request body".to_owned(),
            )),
        }
    }

    /// Stores a response for later idempotent replay.
    async fn store_idempotent_response(
        &self,
        key: IdempotencyKey,
        fingerprint: u64,
        body: serde_json::Value,
    ) {
        self.idempotency
            .write()
            .await
            .insert(key, (fingerprint, body));
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Body of `POST /api/v1/contacts`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateContactBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Raw email addresses (normalized by the service).
    pub emails: Vec<String>,
    /// Raw phone numbers (normalized by the service).
    pub phones: Vec<String>,
    /// Acquisition source, e.g. `"web-form"`.
    pub source: String,
}

/// Query of `GET /api/v1/contacts`.
#[derive(Debug, Deserialize)]
pub struct ListContactsQuery {
    /// Owning client profile (required tenant scope).
    pub client_profile_id: Uuid,
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
    /// Items to skip (default 0).
    pub offset: Option<u32>,
}

/// Body of `GET /api/v1/contacts`.
#[derive(Debug, Serialize)]
struct ContactListResponse {
    contacts: Vec<Contact>,
    total: usize,
}

/// Reads the request/correlation id the middleware propagated, generating
/// one when absent (tests bypass the middleware).
fn request_id(headers: &HeaderMap) -> (String, Uuid) {
    let id = headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Uuid>().ok())
        .unwrap_or_else(Uuid::new_v4);
    (id.to_string(), id)
}

/// Parses the `Idempotency-Key` header; a present-but-malformed key is a
/// `422`, not silently ignored.
///
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn idempotency_key(
    headers: &HeaderMap,
    instance: &str,
) -> Result<Option<IdempotencyKey>, Box<Response>> {
    let Some(raw) = headers.get(IDEMPOTENCY_KEY_HEADER) else {
        return Ok(None);
    };
    raw.to_str()
        .ok()
        .and_then(|value| value.parse::<IdempotencyKey>().ok())
        .map(Some)
        .ok_or_else(|| {
            Box::new(problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid Idempotency-Key",
                "the Idempotency-Key header must be a UUID".to_owned(),
                instance.to_owned(),
            ))
        })
}

/// Fingerprints a request body so a reused idempotency key with a
/// *different* payload is rejected instead of replayed.
///
/// The fingerprint is over the parsed-then-reserialized body, so it is
/// stable across insignificant whitespace differences in the raw JSON.
fn fingerprint(body: &CreateContactBody) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(body)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// Validates `POST /api/v1/contacts` semantics beyond the JSON shape.
/// The offending address is never echoed: no PII in error messages.
fn validate_create_body(body: &CreateContactBody) -> Result<(), DomainError> {
    if body.source.trim().is_empty() {
        return Err(DomainError::Validation(
            "source must not be empty".to_owned(),
        ));
    }
    let has_email = body.emails.iter().any(|email| !email.trim().is_empty());
    let has_phone = body.phones.iter().any(|phone| !phone.trim().is_empty());
    if !has_email && !has_phone {
        return Err(DomainError::Validation(
            "at least one email or phone number is required".to_owned(),
        ));
    }
    for (index, email) in body.emails.iter().enumerate() {
        let email = email.trim();
        // Lightweight shape check for the vertical slice; full RFC
        // validation is out of scope.
        if !email.is_empty() && !email.contains('@') {
            return Err(DomainError::Validation(format!(
                "emails[{index}] is not a valid email address"
            )));
        }
    }
    Ok(())
}

/// Maps a [`DomainError`] to an RFC 7807 `problem+json` response,
/// preserving the crate's error shape and never leaking internals.
fn domain_error_response(error: DomainError, instance: String) -> Response {
    let (status, title) = match &error {
        DomainError::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed"),
        DomainError::NotFound { .. } => (StatusCode::NOT_FOUND, "Not found"),
        DomainError::Conflict(_) => (StatusCode::CONFLICT, "Conflict"),
        DomainError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized"),
        DomainError::Forbidden(_) => (StatusCode::FORBIDDEN, "Forbidden"),
        DomainError::ConcurrencyConflict => (StatusCode::CONFLICT, "Concurrency conflict"),
    };
    problem_response(status, title, error.to_string(), instance)
}

/// `POST /api/v1/contacts` — creates a contact.
///
/// Returns `201` with the created contact. When `Idempotency-Key` is
/// supplied, a repeated request with the same key and body replays the
/// original `201` without re-executing; the same key with a different
/// body is a `409`.
pub async fn create_contact(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CreateContactBody>, JsonRejection>,
) -> Response {
    let (instance, correlation_id) = request_id(&headers);
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
    if let Err(error) = validate_create_body(&body) {
        return domain_error_response(error, instance);
    }
    let key = match idempotency_key(&headers, &instance) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let fp = fingerprint(&body);
    if let Some(key) = key {
        match state.idempotent_replay(&key, fp).await {
            Ok(Some(stored)) => return (StatusCode::CREATED, Json(stored)).into_response(),
            Ok(None) => {}
            Err(error) => return domain_error_response(error, instance.clone()),
        }
    }

    let profile_id = ClientProfileId::from(body.client_profile_id);
    let ctx = api_action_context(Some(profile_id), correlation_id);
    let contact = match state
        .contact_service()
        .create_contact(
            NewContact {
                client_profile_id: profile_id,
                emails: body.emails,
                phones: body.phones,
                source: body.source,
            },
            &ctx,
        )
        .await
    {
        Ok(contact) => contact,
        Err(error) => return domain_error_response(error, instance),
    };
    let response_value = match serde_json::to_value(&contact) {
        Ok(value) => value,
        Err(_) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Serialization failed",
                "the created contact could not be serialized".to_owned(),
                instance,
            );
        }
    };
    if let Some(key) = key {
        state
            .store_idempotent_response(key, fp, response_value)
            .await;
    }
    (StatusCode::CREATED, Json(contact)).into_response()
}

/// `GET /api/v1/contacts/{id}` — fetches one contact.
///
/// `200` with the contact, or `404` `problem+json` for an unknown id.
pub async fn get_contact(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let (instance, _correlation_id) = request_id(&headers);
    let id = match path {
        Ok(Path(id)) => id,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid contact id",
                rejection.body_text(),
                instance,
            );
        }
    };
    match state
        .contact_service()
        .repository()
        .get(ContactId::from(id))
        .await
    {
        Ok(contact) => Json(contact).into_response(),
        Err(error) => domain_error_response(error, instance),
    }
}

/// `GET /api/v1/contacts` — lists contacts for one client profile.
///
/// Returns `200 {"contacts": [...], "total": n}`. `limit` is validated
/// through [`Pagination`] (default 20, max 100); `offset` skips into the
/// client's contacts.
pub async fn list_contacts(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<ListContactsQuery>, QueryRejection>,
) -> Response {
    let (instance, _correlation_id) = request_id(&headers);
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
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if Pagination::new(1, limit).is_err() {
        return domain_error_response(
            DomainError::Validation(format!(
                "limit must be within 1..={}",
                Pagination::MAX_PER_PAGE
            )),
            instance,
        );
    }
    let offset = query.offset.unwrap_or(0);
    if offset > MAX_OFFSET {
        return domain_error_response(
            DomainError::Validation(format!("offset must be within 0..={MAX_OFFSET}")),
            instance,
        );
    }

    let profile_id = ClientProfileId::from(query.client_profile_id);
    let repository = state.contact_service().repository();
    // The stand-in repository has no server-side offset; fetch enough rows
    // to cover it and slice. 02-DB pages in the database.
    let fetch = u64::from(offset.saturating_add(limit));
    let contacts = match repository.list_for_client(profile_id, fetch).await {
        Ok(contacts) => contacts,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = repository.count_for_client(profile_id).await;
    let contacts: Vec<Contact> = contacts
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Json(ContactListResponse { contacts, total }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request};
    use tower::ServiceExt;

    fn test_router() -> axum::Router {
        crate::app(AppState::new())
    }

    fn contact_body(profile: Uuid) -> serde_json::Value {
        serde_json::json!({
            "client_profile_id": profile,
            "emails": ["Test@Example.COM "],
            "phones": [" +1-555-0100 "],
            "source": "web-form"
        })
    }

    fn post_contacts(body: serde_json::Value, key: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/v1/contacts")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
        }
        builder
            .body(Body::from(body.to_string()))
            .expect("test request builds")
    }

    fn get(uri: String) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("test request builds")
    }

    async fn read_json(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body reads");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
        (status, json)
    }

    #[tokio::test]
    async fn create_get_list_round_trip() {
        let router = test_router();
        let profile = Uuid::new_v4();

        let (status, created) = read_json(
            router
                .clone()
                .oneshot(post_contacts(contact_body(profile), None))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = created["contact_id"].as_str().expect("contact id");
        assert_eq!(
            created["normalized_emails"],
            serde_json::json!(["test@example.com"])
        );
        assert_eq!(created["lifecycle"], "Prospect");

        let (status, fetched) = read_json(
            router
                .clone()
                .oneshot(get(format!("/api/v1/contacts/{id}")))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched["contact_id"], created["contact_id"]);

        let (status, listed) = read_json(
            router
                .oneshot(get(format!("/api/v1/contacts?client_profile_id={profile}")))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["contacts"].as_array().expect("array").len(), 1);
    }

    #[tokio::test]
    async fn idempotent_post_replays_without_duplicating() {
        let router = test_router();
        let profile = Uuid::new_v4();
        let key = IdempotencyKey::generate().to_string();
        let body = contact_body(profile);

        let (first_status, first) = read_json(
            router
                .clone()
                .oneshot(post_contacts(body.clone(), Some(&key)))
                .await
                .expect("oneshot"),
        )
        .await;
        let (second_status, second) = read_json(
            router
                .clone()
                .oneshot(post_contacts(body, Some(&key)))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(first_status, StatusCode::CREATED);
        assert_eq!(second_status, StatusCode::CREATED);
        assert_eq!(first, second, "replay returns the identical response");

        let (_, listed) = read_json(
            router
                .oneshot(get(format!("/api/v1/contacts?client_profile_id={profile}")))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(listed["total"], 1, "one record despite two posts");
    }

    #[tokio::test]
    async fn idempotency_key_with_different_body_conflicts() {
        let router = test_router();
        let profile = Uuid::new_v4();
        let key = IdempotencyKey::generate().to_string();
        let mut other_body = contact_body(profile);
        other_body["emails"] = serde_json::json!(["other@example.com"]);

        let (status, _) = read_json(
            router
                .clone()
                .oneshot(post_contacts(contact_body(profile), Some(&key)))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, problem) = read_json(
            router
                .oneshot(post_contacts(other_body, Some(&key)))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["status"], 409);
    }

    #[tokio::test]
    async fn unknown_contact_returns_404_problem() {
        let router = test_router();
        let id = Uuid::new_v4();
        let response = router
            .oneshot(get(format!("/api/v1/contacts/{id}")))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("content-type"),
            "application/problem+json"
        );
        let (_, problem) = read_json(response).await;
        assert_eq!(problem["status"], 404);
        assert_eq!(problem["title"], "Not found");
    }

    #[tokio::test]
    async fn malformed_body_returns_422_problem() {
        let router = test_router();
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/contacts")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not json"))
            .expect("test request builds");

        let (status, problem) = read_json(router.oneshot(request).await.expect("oneshot")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["status"], 422);
    }

    #[tokio::test]
    async fn empty_source_returns_422() {
        let router = test_router();
        let mut body = contact_body(Uuid::new_v4());
        body["source"] = serde_json::json!("  ");

        let (status, problem) = read_json(
            router
                .oneshot(post_contacts(body, None))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["title"], "Validation failed");
    }

    #[tokio::test]
    async fn invalid_idempotency_key_returns_422() {
        let router = test_router();
        let (status, problem) = read_json(
            router
                .oneshot(post_contacts(
                    contact_body(Uuid::new_v4()),
                    Some("not-a-uuid"),
                ))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["title"], "Invalid Idempotency-Key");
    }

    #[tokio::test]
    async fn duplicate_email_returns_409() {
        let router = test_router();
        let profile = Uuid::new_v4();
        let body = contact_body(profile);

        let (status, _) = read_json(
            router
                .clone()
                .oneshot(post_contacts(body.clone(), None))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, problem) = read_json(
            router
                .oneshot(post_contacts(body, None))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["title"], "Conflict");
        assert_eq!(problem["status"], 409);
    }

    #[tokio::test]
    async fn pagination_limit_is_respected() {
        let router = test_router();
        let profile = Uuid::new_v4();
        for index in 0..3 {
            let mut body = contact_body(profile);
            body["emails"] = serde_json::json!([format!("user{index}@example.com")]);
            let (status, _) = read_json(
                router
                    .clone()
                    .oneshot(post_contacts(body, None))
                    .await
                    .expect("oneshot"),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, listed) = read_json(
            router
                .clone()
                .oneshot(get(format!(
                    "/api/v1/contacts?client_profile_id={profile}&limit=2"
                )))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["total"], 3);
        assert_eq!(listed["contacts"].as_array().expect("array").len(), 2);

        let (status, _) = read_json(
            router
                .oneshot(get(format!(
                    "/api/v1/contacts?client_profile_id={profile}&limit=2&offset=2"
                )))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, problem) = read_json(
            test_router()
                .oneshot(get(format!(
                    "/api/v1/contacts?client_profile_id={profile}&limit=101"
                )))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["title"], "Validation failed");
    }

    #[tokio::test]
    async fn missing_client_profile_id_returns_422() {
        let router = test_router();
        let (status, problem) = read_json(
            router
                .oneshot(get("/api/v1/contacts".to_owned()))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["status"], 422);
    }

    #[tokio::test]
    async fn malformed_contact_id_returns_422() {
        let router = test_router();
        let (status, problem) = read_json(
            router
                .oneshot(get("/api/v1/contacts/not-a-uuid".to_owned()))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["title"], "Invalid contact id");
    }
}
