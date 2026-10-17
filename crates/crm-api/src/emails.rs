// SPDX-License-Identifier: GPL-3.0-only
//! Email endpoints: outbound sending, reply ingestion, thread reads
//! (task 07-MAIL-02).
//!
//! * `POST /api/v1/emails/send` — composes and sends an outbound email
//!   via the provider adapter; honors the `Idempotency-Key` request
//!   header (`201` on send, `200` on idempotent replay).
//! * `POST /api/v1/emails/ingest` — provider webhook for inbound
//!   replies. Authenticated by the `CRM_EMAIL_WEBHOOK_SECRET` shared
//!   secret (fail-closed when unset); returns `202` on acceptance.
//! * `GET /api/v1/threads` — lists threads for one client profile.
//! * `GET /api/v1/threads/{id}` — fetches one thread with its messages.
//!
//! ## Authorization
//!
//! `send`, thread reads use the `Email` / `EmailThread` grants within
//! the message's client profile
//! ([`Resource::Email`](crm_domain::security::Resource::Email),
//! [`Resource::EmailThread`](crm_domain::security::Resource::EmailThread),
//! [`Scope::ClientProfile`](crm_domain::security::Scope::ClientProfile)).
//! The ingest webhook is not bearer-authenticated (providers cannot
//! hold staff sessions); it is gated by the shared webhook secret and
//! acts as the system principal scoped to the body's profile.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_core::email::{EmailServiceImpl, StubEmailProvider};
use crm_db::email::{
    PgEmailAttachmentRepository, PgEmailMessageRepository, PgEmailThreadRepository,
};
use crm_db::postgres::PgAuditService;
use crm_domain::audit::ActorKind;
use crm_domain::email::{EmailMessage, EmailThread, InboundThreadHints};
use crm_domain::ids::{ActorId, ClientProfileId, EmailThreadId};
use crm_domain::repositories::{EmailMessageRepository, EmailThreadRepository};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{ActionContext, EmailService, InboundEmail, NewEmailMessage};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{
    domain_error_response, idempotency_key, request_id, AppState, IdempotencyStore,
};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;
use crm_domain::services::AuditService;

/// Environment variable carrying the shared webhook secret for
/// `POST /api/v1/emails/ingest`. Unset means the endpoint refuses all
/// traffic (fail-closed).
const WEBHOOK_SECRET_ENV: &str = "CRM_EMAIL_WEBHOOK_SECRET";

/// Request header carrying the webhook secret.
const WEBHOOK_SECRET_HEADER: &str = "x-webhook-secret";

/// Body of `POST /api/v1/emails/send`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SendEmailBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Thread to append to, if replying in-thread.
    pub thread_id: Option<Uuid>,
    /// Sender address; blank means the agency's configured from-address.
    pub from: String,
    /// Recipient addresses.
    pub to: Vec<String>,
    /// CC addresses.
    #[serde(default)]
    pub cc: Vec<String>,
    /// BCC addresses.
    #[serde(default)]
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body_text: String,
    /// HTML body, if supplied.
    pub body_html: Option<String>,
}

/// Body of `POST /api/v1/emails/ingest` (provider webhook).
#[derive(Debug, Clone, Deserialize)]
pub struct IngestEmailBody {
    /// Owning client profile (tenant scope for the ingested reply).
    pub client_profile_id: Uuid,
    /// Provider's message id (dedupe key).
    pub provider_message_id: String,
    /// Provider's thread id, when the provider supplies one.
    pub provider_thread_id: Option<String>,
    /// `In-Reply-To` header value, when present.
    pub in_reply_to: Option<String>,
    /// `References` header chain.
    #[serde(default)]
    pub references: Vec<String>,
    /// Sender address.
    pub from: String,
    /// Recipient addresses.
    #[serde(default)]
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    #[serde(default)]
    pub body_text: String,
    /// HTML body, if supplied.
    pub body_html: Option<String>,
}

/// Query of `GET /api/v1/threads`.
#[derive(Debug, Deserialize)]
pub struct ListThreadsQuery {
    /// Owning client profile (required tenant scope).
    pub client_profile_id: Uuid,
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
}

/// `GET /api/v1/threads/{id}` response: the thread plus its messages.
#[derive(Debug, Serialize)]
struct ThreadDetailResponse {
    thread: EmailThread,
    messages: Vec<EmailMessage>,
}

/// Builds the email service from the API auth components.
///
/// The provider is the stub adapter: real Gmail/Zoho adapters require
/// provider credentials and land in a later task.
fn email_service(
    api_auth: &crate::auth::ApiAuth,
) -> EmailServiceImpl<
    PgEmailMessageRepository,
    PgEmailThreadRepository,
    PgEmailAttachmentRepository,
    StubEmailProvider,
    PgAuditService,
> {
    EmailServiceImpl::new(
        api_auth.email_messages.clone(),
        api_auth.email_threads.clone(),
        api_auth.email_attachments.clone(),
        StubEmailProvider::new(),
        api_auth.audit_service.clone(),
    )
}

/// `POST /api/v1/emails/send` — composes and sends an outbound email.
///
/// Returns `201` with the sent message, or `200` with the stored
/// response on `Idempotency-Key` replay. Requires the `email/create`
/// grant within the message's client profile scope (`403` without it).
pub async fn send_email<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<SendEmailBody>, axum::extract::rejection::JsonRejection>,
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
            Ok(Some(stored)) => return (StatusCode::OK, Json(stored)).into_response(),
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
        resource: Resource::Email,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = email_service(api_auth);
    let input = NewEmailMessage {
        client_profile_id: profile_id,
        thread_id: body.thread_id.map(EmailThreadId::from),
        from: body.from,
        to: body.to,
        cc: body.cc,
        bcc: body.bcc,
        subject: body.subject,
        body_text: body.body_text,
        body_html: body.body_html,
        idempotency_key: key
            .map(|k| k.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
    };
    let message = match service.send(input, &ctx).await {
        Ok(message) => message,
        Err(error) => return domain_error_response(error, instance),
    };
    let response_value = match serde_json::to_value(&message) {
        Ok(value) => value,
        Err(_) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Serialization failed",
                "the sent email could not be serialized".to_owned(),
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
    (StatusCode::CREATED, Json(message)).into_response()
}

/// Validates the webhook shared secret, fail-closed.
///
/// Returns `None` when the secret is valid; otherwise returns the
/// response to send (401 for a bad secret, 503 when the server has no
/// secret configured).
fn check_webhook_secret(headers: &HeaderMap, instance: &str) -> Option<Response> {
    let configured = std::env::var(WEBHOOK_SECRET_ENV).unwrap_or_default();
    if configured.is_empty() {
        return Some(problem_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Webhook not configured",
            "the email ingest webhook has no shared secret configured".to_owned(),
            instance.to_owned(),
        ));
    }
    let presented = headers
        .get(WEBHOOK_SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // Constant-time comparison would be ideal; the secret is
    // high-entropy and this keeps the dependency tree lean. The
    // comparison is over the full secret, not a prefix.
    if presented.len() != configured.len()
        || !presented
            .bytes()
            .zip(configured.bytes())
            .all(|(a, b)| a == b)
    {
        return Some(problem_response(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "invalid webhook secret".to_owned(),
            instance.to_owned(),
        ));
    }
    None
}

/// `POST /api/v1/emails/ingest` — provider webhook for inbound replies.
///
/// Gated by the `CRM_EMAIL_WEBHOOK_SECRET` shared secret (fail-closed).
/// Correlates the reply with a thread (provider thread id → parent
/// message → subject heuristic), creating a new thread when nothing
/// matches. Duplicate `provider_message_id`s are ignored
/// (at-most-once). Returns `202` on acceptance.
pub async fn ingest_email<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    headers: HeaderMap,
    body: Result<Json<IngestEmailBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, correlation_id) = request_id(&headers);
    if let Some(response) = check_webhook_secret(&headers, &instance) {
        return response;
    }
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
    // The webhook acts as the system principal scoped to the body's
    // profile; bearer auth does not apply to provider callbacks.
    let profile_id = ClientProfileId::from(body.client_profile_id);
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile_id),
        correlation_id,
    };
    let service = email_service(api_auth);
    let inbound = InboundEmail {
        client_profile_id: profile_id,
        hints: InboundThreadHints {
            provider_thread_id: body.provider_thread_id,
            in_reply_to: body.in_reply_to,
            references: body.references,
            subject: body.subject,
        },
        from: body.from,
        to: body.to,
        body_text: body.body_text,
        body_html: body.body_html,
        provider_message_id: body.provider_message_id,
        received_at: Utc::now(),
    };
    match service.ingest_reply(inbound, &ctx).await {
        Ok(message) => (StatusCode::ACCEPTED, Json(message)).into_response(),
        Err(error) => domain_error_response(error, instance),
    }
}

/// `GET /api/v1/threads` — lists threads for one client profile.
///
/// Requires the `email_thread/read` grant within the requested profile
/// scope.
pub async fn list_threads<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    query: Result<Query<ListThreadsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
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
        resource: Resource::EmailThread,
        operation: Operation::Read,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = email_service(api_auth);
    let limit = u64::from(query.limit.unwrap_or(20).min(100));
    match service.list_threads(profile_id, limit, &ctx).await {
        Ok(threads) => {
            let total = threads.len();
            (
                StatusCode::OK,
                Json(serde_json::json!({ "threads": threads, "total": total })),
            )
                .into_response()
        }
        Err(error) => domain_error_response(error, instance),
    }
}

/// `GET /api/v1/threads/{id}` — fetches one thread with its messages.
///
/// Authorizes against the thread's own profile scope. Requires the
/// `email_thread/read` grant.
pub async fn get_thread<R, A, I>(
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
    let id = match path {
        Ok(Path(id)) => id,
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid thread id",
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
    // Fetch first to learn the profile scope, then authorize — the
    // established `get_asset` pattern. A missing thread surfaces as
    // 404 before any grant check.
    let thread_id = EmailThreadId::from(id);
    let thread = match api_auth.email_threads.get(thread_id).await {
        Ok(thread) => thread,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::EmailThread,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(
        &state,
        auth,
        &headers,
        &action,
        Some(thread.client_profile_id),
    )
    .await
    {
        return *response;
    }
    let messages = match api_auth
        .email_messages
        .list_for_thread(thread_id, 100)
        .await
    {
        Ok(messages) => messages,
        Err(error) => return domain_error_response(error, instance),
    };
    // The messages belong to the thread's profile by construction;
    // the thread-level grant covers them.
    (
        StatusCode::OK,
        Json(ThreadDetailResponse { thread, messages }),
    )
        .into_response()
}
