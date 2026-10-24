// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Contacts endpoints: the vertical slice over the domain contact service.
//!
//! * `POST /api/v1/contacts` — create a contact; honors the
//!   `Idempotency-Key` request header.
//! * `GET /api/v1/contacts` — list contacts for one client profile,
//!   paginated.
//! * `GET /api/v1/contacts/{id}` — fetch one contact.
//!
//! The handlers are generic over the contact service `R`/`A` backend and
//! the idempotency store `I`, so the same routes serve the in-memory
//! vertical slice (unit tests) and the Postgres-backed production server
//! (task 02-DB-02).
//!
//! Errors keep the crate's RFC 7807 `application/problem+json` shape:
//! validation failures → `422`, unknown resources → `404`, duplicate
//! state → `409`.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Extension, Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crm_core::memory::{
    ContactReadModel, ContactServiceImpl, InMemoryAuditRepository, InMemoryContactRepository,
    MemoryAuditService, MemoryContactService,
};
use crm_core::{IdempotencyKey, Pagination};
use crm_db::postgres::{
    PgAuditRepository, PgAuditService, PgContactRepository, PgIdempotencyStore,
};
use crm_domain::contact::Contact;
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, ContactId};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{AuditService, ContactService, NewContact};

use crate::auth::{resolve_context, RequestAuth};
use crate::{problem_response, REQUEST_ID_HEADER};

/// Header carrying the client-generated idempotency key for mutating calls.
pub(crate) const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
/// Default page size for the list endpoint.
const DEFAULT_LIMIT: u32 = 20;
/// Stand-in bound on `offset`; the domain repository port is limit-only,
/// so the handler over-fetches `offset + limit` rows and slices.
const MAX_OFFSET: u32 = 10_000;

/// Postgres-backed contact service composition for the production server.
pub type PgContactService = ContactServiceImpl<PgContactRepository, PgAuditService>;

/// Shared application state, generic over the contact service `S` and the
/// idempotency store `I`.
///
/// The in-memory instantiation ([`AppState::new`]) backs the unit tests;
/// the Postgres instantiation ([`AppState::from_pool`]) serves production
/// traffic.
///
/// `auth` carries the authentication/authorization services. It is `None`
/// for the in-memory test slice (no database, no user store) and `Some`
/// for the Postgres-backed server, where the auth middleware and the
/// per-handler authorization gate are active.
#[derive(Debug, Clone)]
pub struct AppState<S, I> {
    contacts: S,
    idempotency: I,
    pub auth: Option<crate::auth::ApiAuth>,
}

impl<S, I> AppState<S, I> {
    /// Borrows the contact service.
    #[must_use]
    pub fn contact_service(&self) -> &S {
        &self.contacts
    }

    /// Borrows the idempotency store.
    #[must_use]
    pub(crate) fn idempotency_store(&self) -> &I {
        &self.idempotency
    }
}

impl AppState<MemoryContactService, MemoryIdempotencyStore> {
    /// Builds fresh in-memory state. Tests build one per test for isolation.
    ///
    /// Authentication is not configured: the auth middleware is not
    /// installed and handlers fall back to the nil-system-actor context.
    #[must_use]
    pub fn new() -> Self {
        let audit = MemoryAuditService::new(InMemoryAuditRepository::new());
        let contacts = MemoryContactService::new(InMemoryContactRepository::new(), audit);
        Self {
            contacts,
            idempotency: MemoryIdempotencyStore::default(),
            auth: None,
        }
    }
}

impl Default for AppState<MemoryContactService, MemoryIdempotencyStore> {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState<PgContactService, PgIdempotencyStore> {
    /// Builds Postgres-backed state over `pool`.
    ///
    /// Callers run [`crm_db::run_migrations`] first; the server does this
    /// at startup before serving traffic. Authentication and
    /// authorization are configured: the auth middleware gates the
    /// protected routes and handlers authorize per action.
    #[must_use]
    pub fn from_pool(pool: &PgPool) -> Self {
        Self {
            contacts: PgContactService::new(
                PgContactRepository::new(pool.clone()),
                PgAuditService::new(PgAuditRepository::new(pool.clone())),
            ),
            idempotency: PgIdempotencyStore::new(pool.clone()),
            auth: Some(crate::auth::ApiAuth::new(pool)),
        }
    }
}

/// Persistent store for idempotent HTTP responses.
///
/// Maps an [`IdempotencyKey`] to the request fingerprint and response body
/// recorded for it, so `Idempotency-Key` replays survive restarts. A
/// present key with a *different* fingerprint is a
/// [`DomainError::Conflict`], never a silent replay.
///
/// A key can also be *claimed* before its response is recorded (see
/// [`claim`]): a claim row reserves the key so concurrent requests with
/// the same key cannot both reach the write. [`lookup`] reports a
/// claimed-but-unrecorded key as `Ok(None)` — the caller proceeds to
/// [`claim`], which is the atomic step.
pub trait IdempotencyStore: Clone + Send + Sync + 'static {
    /// Returns the stored response body for a seen key whose fingerprint
    /// matches, `Ok(None)` for a fresh key or a claimed-but-unrecorded
    /// key, or [`DomainError::Conflict`] when the key was already used
    /// with a different request body.
    fn lookup(
        &self,
        key: &IdempotencyKey,
        fingerprint: u64,
    ) -> impl Future<Output = Result<Option<serde_json::Value>, DomainError>> + Send;

    /// Atomically claims `key` for this request's fingerprint.
    ///
    /// The claim closes the lookup-then-record race: two concurrent
    /// requests with the same key cannot both win the claim, so at most
    /// one reaches the write.
    ///
    /// Returns `Ok(true)` if this request won the claim (proceed with
    /// the write, then persist the response with [`record`]);
    /// `Ok(false)` if the key was already claimed by a concurrent
    /// request with the same fingerprint (the caller must poll
    /// [`lookup`] for the winner's recorded response instead of
    /// executing a duplicate write); or [`DomainError::Conflict`] when
    /// the key was already used with a different fingerprint.
    fn claim(
        &self,
        key: &IdempotencyKey,
        fingerprint: u64,
    ) -> impl Future<Output = Result<bool, DomainError>> + Send;

    /// Stores a response body for later replay, replacing any previous
    /// entry for the key (including a claim row left by [`claim`]).
    fn record(
        &self,
        key: IdempotencyKey,
        fingerprint: u64,
        body: serde_json::Value,
    ) -> impl Future<Output = Result<(), DomainError>> + Send;
}

/// In-memory idempotency store for the vertical-slice unit tests.
///
/// Entries do not survive restarts; the production server persists them
/// with [`PgIdempotencyStore`].
#[derive(Debug, Clone, Default)]
pub struct MemoryIdempotencyStore {
    inner: Arc<RwLock<HashMap<IdempotencyKey, (u64, serde_json::Value)>>>,
}

impl IdempotencyStore for MemoryIdempotencyStore {
    async fn lookup(
        &self,
        key: &IdempotencyKey,
        fingerprint: u64,
    ) -> Result<Option<serde_json::Value>, DomainError> {
        match self.inner.read().await.get(key) {
            None => Ok(None),
            Some((stored_fingerprint, body)) if *stored_fingerprint == fingerprint => {
                // A JSON null body is a claim row, not a recorded
                // response: the winner has not finished writing yet.
                if body.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(body.clone()))
                }
            }
            Some(_) => Err(DomainError::Conflict(
                "idempotency key was already used with a different request body".to_owned(),
            )),
        }
    }

    async fn claim(&self, key: &IdempotencyKey, fingerprint: u64) -> Result<bool, DomainError> {
        // The write lock spans check-and-insert, so the claim is atomic
        // in-process: two concurrent claims cannot both win.
        let mut map = self.inner.write().await;
        match map.get(key) {
            None => {
                // Claim row: JSON null sentinel means "claimed,
                // response not yet recorded". `lookup` treats it as
                // `Ok(None)`; `record` overwrites it with the real body.
                map.insert(*key, (fingerprint, serde_json::Value::Null));
                Ok(true)
            }
            Some((stored_fingerprint, _)) if *stored_fingerprint != fingerprint => {
                Err(DomainError::Conflict(
                    "idempotency key was already used with a different request body".to_owned(),
                ))
            }
            Some(_) => Ok(false),
        }
    }

    async fn record(
        &self,
        key: IdempotencyKey,
        fingerprint: u64,
        body: serde_json::Value,
    ) -> Result<(), DomainError> {
        self.inner.write().await.insert(key, (fingerprint, body));
        Ok(())
    }
}

impl IdempotencyStore for PgIdempotencyStore {
    async fn lookup(
        &self,
        key: &IdempotencyKey,
        fingerprint: u64,
    ) -> Result<Option<serde_json::Value>, DomainError> {
        match self.get(key).await? {
            None => Ok(None),
            Some((stored_fingerprint, body)) if stored_fingerprint == fingerprint => {
                // A JSON null body is a claim row, not a recorded
                // response: the winner has not finished writing yet.
                if body.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(body))
                }
            }
            Some(_) => Err(DomainError::Conflict(
                "idempotency key was already used with a different request body".to_owned(),
            )),
        }
    }

    async fn claim(&self, key: &IdempotencyKey, fingerprint: u64) -> Result<bool, DomainError> {
        self.claim_key(key, fingerprint).await
    }

    async fn record(
        &self,
        key: IdempotencyKey,
        fingerprint: u64,
        body: serde_json::Value,
    ) -> Result<(), DomainError> {
        self.put(&key, fingerprint, &body).await
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
pub(crate) fn request_id(headers: &HeaderMap) -> (String, Uuid) {
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
pub(crate) fn idempotency_key(
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
pub(crate) fn domain_error_response(error: DomainError, instance: String) -> Response {
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

/// Outcome of attempting to claim an idempotency key.
pub(crate) enum ClaimOutcome {
    /// This request won the claim: execute the write, then persist the
    /// response with [`IdempotencyStore::record`] (which overwrites the
    /// claim row with the real body).
    Won,
    /// Another request already recorded (or recorded during the poll):
    /// return this response instead of executing a duplicate write.
    Replay(Response),
}

/// Atomically claims `key` for this request, or replays the stored or
/// in-flight winner's response.
///
/// Call AFTER authorization (the stored response must only be visible
/// to an authorized caller) and BEFORE the write. The claim closes the
/// lookup-then-record race: two concurrent requests with the same key
/// cannot both reach the write.
///
/// - Already recorded with the same fingerprint: `Replay` immediately.
/// - Fresh key: claims it and returns `Won`.
/// - Claimed but not yet recorded by a concurrent winner: polls
///   `lookup` for up to ~5s (50ms intervals) for the winner's response;
///   replays it if it appears, otherwise 409 (never a duplicate write).
/// - Key used with a different fingerprint: 409.
///
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
pub(crate) async fn claim_or_replay<I>(
    store: &I,
    key: &IdempotencyKey,
    fp: u64,
    status: StatusCode,
    instance: &str,
) -> Result<ClaimOutcome, Box<Response>>
where
    I: IdempotencyStore,
{
    // Fast path: already fully recorded.
    if let Some(stored) = match store.lookup(key, fp).await {
        Ok(stored) => stored,
        Err(error) => return Err(Box::new(domain_error_response(error, instance.to_owned()))),
    } {
        return Ok(ClaimOutcome::Replay((status, Json(stored)).into_response()));
    }
    // Fresh key: atomic claim.
    match store.claim(key, fp).await {
        Ok(true) => Ok(ClaimOutcome::Won),
        Ok(false) => {
            // Lost the race: the winner is mid-write. Poll for its
            // recorded response; never execute a duplicate write.
            const POLL_ATTEMPTS: usize = 100;
            const POLL_INTERVAL_MS: u64 = 50;
            for _ in 0..POLL_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
                match store.lookup(key, fp).await {
                    Ok(Some(stored)) => {
                        return Ok(ClaimOutcome::Replay((status, Json(stored)).into_response()));
                    }
                    // Winner still writing; keep polling.
                    Ok(None) => {}
                    Err(error) => {
                        return Err(Box::new(domain_error_response(error, instance.to_owned())));
                    }
                }
            }
            // Winner never recorded (crashed mid-write, or slower than
            // the window): 409, not a duplicate write. The client
            // retries with a fresh key.
            Err(Box::new(problem_response(
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "the idempotency key is claimed by an unfinished concurrent request".to_owned(),
                instance.to_owned(),
            )))
        }
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// `POST /api/v1/contacts` — creates a contact.
///
/// Returns `201` with the created contact. When `Idempotency-Key` is
/// supplied, a repeated request with the same key and body replays the
/// original `201` without re-executing; the same key with a different
/// body is a `409`.
///
/// Requires the `contact/create` grant within the request's client
/// profile scope (`403` without it); the audit event carries the real
/// actor from the session.
pub async fn create_contact<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<CreateContactBody>, JsonRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _correlation_id) = request_id(&headers);
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
        match state.idempotency.lookup(&key, fp).await {
            Ok(Some(stored)) => return (StatusCode::CREATED, Json(stored)).into_response(),
            Ok(None) => {}
            Err(error) => return domain_error_response(error, instance.clone()),
        }
    }

    let profile_id = ClientProfileId::from(body.client_profile_id);
    let action = Action {
        resource: Resource::Contact,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // Atomic claim AFTER authorization: a failed (unauthorized) attempt
    // must not leave a stuck claim row blocking a later authorized retry
    // with the same key.
    if let Some(key) = &key {
        match claim_or_replay(
            state.idempotency_store(),
            key,
            fp,
            StatusCode::CREATED,
            &instance,
        )
        .await
        {
            Ok(ClaimOutcome::Replay(response)) => return response,
            Ok(ClaimOutcome::Won) => {}
            Err(response) => return *response,
        }
    }
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
        // The contact is already created: failing the request now would
        // lose the 201 the client needs, so a persistence failure is
        // logged loudly instead. The write is guarded by the atomic
        // claim in `claim_or_replay`: concurrent requests with the same
        // key cannot both reach the write, and this `record` overwrites
        // the winner's claim row with the real response.
        if let Err(error) = state.idempotency.record(key, fp, response_value).await {
            tracing::warn!(%error, "idempotency response could not be persisted");
        }
    }
    (StatusCode::CREATED, Json(contact)).into_response()
}

/// `GET /api/v1/contacts/{id}` — fetches one contact.
///
/// `200` with the contact, or `404` `problem+json` for an unknown id.
/// Requires the `contact/read` grant within the contact's client profile
/// scope (`403` without it).
pub async fn get_contact<R, A, I>(
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
    let contact = match state
        .contact_service()
        .repository()
        .get(ContactId::from(id))
        .await
    {
        Ok(contact) => contact,
        Err(error) => return domain_error_response(error, instance),
    };
    // Authorize against the contact's own profile scope: the request
    // carries no tenant parameter on this route.
    let action = Action {
        resource: Resource::Contact,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(
        &state,
        auth,
        &headers,
        &action,
        Some(contact.client_profile_id),
    )
    .await
    {
        return *response;
    }
    Json(contact).into_response()
}

/// `GET /api/v1/contacts` — lists contacts for one client profile.
///
/// Returns `200 {"contacts": [...], "total": n}`. `limit` is validated
/// through [`Pagination`] (default 20, max 100); `offset` skips into the
/// client's contacts.
///
/// Requires the `contact/read` grant within the requested client profile
/// scope (`403` without it).
pub async fn list_contacts<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    query: Result<Query<ListContactsQuery>, QueryRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
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
    let action = Action {
        resource: Resource::Contact,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, Some(profile_id)).await
    {
        return *response;
    }
    let repository = state.contact_service().repository();
    // The domain repository port is limit-only, so the handler
    // over-fetches `offset + limit` rows and slices in memory; `MAX_OFFSET`
    // caps the over-fetch. Keyset pagination in the database is a later
    // task.
    let fetch = u64::from(offset.saturating_add(limit));
    let contacts = match repository.list_for_client(profile_id, fetch).await {
        Ok(contacts) => contacts,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = match repository.count_for_client(profile_id).await {
        Ok(total) => total,
        Err(error) => return domain_error_response(error, instance),
    };
    let contacts: Vec<Contact> = contacts
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Json(ContactListResponse { contacts, total }).into_response()
}

/// Body of `POST /api/v1/contacts/merge`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MergeContactsBody {
    /// The contact that survives, absorbing the other's identity.
    pub survivor_id: Uuid,
    /// The duplicate folded into the survivor (tombstoned, never deleted).
    pub duplicate_id: Uuid,
}

/// `POST /api/v1/contacts/merge` — merges a duplicate into its survivor.
///
/// The survivor absorbs the duplicate's emails, phones, sources and
/// consents (union); the duplicate is archived with `merged_into`
/// pointing at the survivor. Returns `200` with the archived duplicate.
///
/// Requires the `contact/update` grant within the contacts' client
/// profile scope (`403` without it); both contacts must live under the
/// same profile (`403` on cross-profile pairs).
pub async fn merge_contacts<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<MergeContactsBody>, JsonRejection>,
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
    // Fetch the survivor first to anchor the tenant scope, mirroring
    // `get_contact`: the request carries no profile parameter.
    let survivor = match state
        .contact_service()
        .repository()
        .get(ContactId::from(body.survivor_id))
        .await
    {
        Ok(contact) => contact,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::Contact,
        operation: Operation::Update,
    };
    let ctx = match resolve_context(
        &state,
        auth,
        &headers,
        &action,
        Some(survivor.client_profile_id),
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    match state
        .contact_service()
        .merge_contacts(
            ContactId::from(body.survivor_id),
            ContactId::from(body.duplicate_id),
            &ctx,
        )
        .await
    {
        Ok(merged) => Json(merged).into_response(),
        Err(error) => domain_error_response(error, instance),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request};
    use tower::ServiceExt;

    fn test_router() -> axum::Router {
        crate::app(AppState::new(), None)
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
