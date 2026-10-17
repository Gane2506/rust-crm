// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! HTTP authentication for `crm-api` (task 03-SEC-02).
//!
//! * `POST /api/v1/auth/login {email, password}` → `200 {token}` — the
//!   token is rendered via [`SessionToken::as_uuid`] at this boundary
//!   only; it is never logged and never appears in error messages.
//! * `POST /api/v1/auth/logout` (Bearer) → `204`; revokes the session.
//!   Revocation is idempotent: an unknown token still yields `204` so
//!   logout never leaks whether a token existed.
//!
//! [`auth_middleware`] gates every `/api/v1/*` route except login (plus
//! the non-API `/healthz`/`/readyz`, which never enter the `/api`
//! router): a missing or invalid bearer token is a `401` `problem+json`.
//! Per-action authorization stays in the handlers via
//! [`resolve_context`], which calls the
//! [`AuthorizationService`](crm_domain::services::AuthorizationService):
//! a valid token without a covering grant is a `403`.
//!
//! When the application state carries no [`ApiAuth`] (the in-memory test
//! slice), the middleware is not installed and [`resolve_context`]
//! falls back to the legacy nil-system-actor context so handler unit
//! tests keep working without a database.

use axum::{
    extract::{rejection::JsonRejection, Extension, Request, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crm_db::auth::{PgAuthenticationService, PgAuthorizationService};
use crm_domain::error::DomainError;
use crm_domain::ids::{AgencyId, ClientProfileId};
use crm_domain::security::{Action, Scope, SessionToken};
use crm_domain::services::{ActionContext, AuthenticationService, AuthorizationService};

use crate::contacts::{request_id, AppState};
use crate::problem_response;
use crm_core::memory::{ContactReadModel, ContactServiceImpl};
use crm_domain::services::AuditService;

/// Production authentication + authorization services.
///
/// Built once at startup from the database pool and shared by the auth
/// middleware, the login/logout handlers and the per-handler
/// authorization helper.
#[derive(Debug, Clone)]
pub struct ApiAuth {
    /// Password verification, session issuance, refresh and revocation.
    pub authentication: PgAuthenticationService,
    /// Server-side per-request authorization enforcement.
    pub authorization: PgAuthorizationService,
}

impl ApiAuth {
    /// Builds the auth services over `pool`.
    pub fn new(pool: &PgPool) -> Self {
        Self {
            authentication: PgAuthenticationService::new(pool.clone()),
            authorization: PgAuthorizationService::new(pool.clone()),
        }
    }
}

/// The authenticated caller, resolved by [`auth_middleware`] and carried
/// in the request extensions.
///
/// `token`'s `Debug` is redacted by the domain type, so this never leaks
/// the bearer secret into logs.
#[derive(Debug, Clone)]
pub struct RequestAuth {
    /// The agency the staff account operates under (scope anchor).
    pub agency_id: AgencyId,
    /// The presented bearer token, for per-action authorization.
    pub token: SessionToken,
}

/// Renders a `401` `problem+json` without revealing why authentication
/// failed: "no token", "unknown token" and "expired token" are
/// indistinguishable to the caller.
fn unauthorized(instance: &str) -> Response {
    problem_response(
        StatusCode::UNAUTHORIZED,
        "Unauthorized",
        "authentication required".to_owned(),
        instance.to_owned(),
    )
}

/// Renders a `403` `problem+json`: the caller is authenticated but no
/// grant covers the requested action.
fn forbidden(instance: &str) -> Response {
    problem_response(
        StatusCode::FORBIDDEN,
        "Forbidden",
        "the authenticated caller lacks the grant for this action".to_owned(),
        instance.to_owned(),
    )
}

/// Parses the `Authorization: Bearer <uuid>` header into a [`SessionToken`].
///
/// A missing, malformed or non-Bearer header is an authentication
/// failure; the token value itself never appears in the response.
fn bearer_token(headers: &HeaderMap) -> Result<SessionToken, ()> {
    let raw = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(())?;
    let token = raw.strip_prefix("Bearer ").ok_or(())?;
    let id = token.trim().parse::<uuid::Uuid>().map_err(|_| ())?;
    Ok(SessionToken::from_uuid(id))
}

/// Authentication gate for the protected `/api/v1/*` routes.
///
/// Resolves the bearer token to its live session and active staff
/// account, inserting [`RequestAuth`] into the request extensions.
/// Any failure — missing header, unparsable token, unknown/expired/
/// revoked/idle session, suspended account — is the same `401`, so
/// callers learn nothing about which check failed.
pub async fn auth_middleware(
    State(api_auth): State<ApiAuth>,
    request: Request,
    next: Next,
) -> Response {
    let (instance, _) = request_id(request.headers());
    let token = match bearer_token(request.headers()) {
        Ok(token) => token,
        Err(()) => return unauthorized(&instance),
    };
    let validated = match api_auth.authentication.validate_token(&token).await {
        Ok(validated) => validated,
        Err(_) => return unauthorized(&instance),
    };
    let mut request = request;
    request.extensions_mut().insert(RequestAuth {
        agency_id: validated.staff.agency_id,
        token,
    });
    next.run(request).await
}

/// Body of `POST /api/v1/auth/login`.
///
/// The password crosses this boundary only; it is never stored, never
/// logged, and passed to the authentication service as bytes.
#[derive(Debug, Deserialize)]
pub struct LoginBody {
    /// Staff email (login name).
    pub email: String,
    /// Plaintext password — handled as a secret.
    pub password: String,
}

/// Body of a successful `POST /api/v1/auth/login`.
///
/// The token is the UUID form of the issued [`SessionToken`], rendered
/// here — the one place it leaves the server. Clients keep it in memory
/// and send it as `Authorization: Bearer <token>`.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// The bearer token (UUID string). Handle as a secret.
    pub token: String,
}

/// `POST /api/v1/auth/login` — verifies the credentials and issues a
/// session.
///
/// `200 {token}` on success. Bad credentials are `401` without revealing
/// whether the email exists — a suspended account is indistinguishable
/// from a wrong password.
pub async fn login<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    headers: HeaderMap,
    body: Result<Json<LoginBody>, JsonRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: crate::contacts::IdempotencyStore,
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
    if body.email.trim().is_empty() || body.password.is_empty() {
        return unauthorized(&instance);
    }
    match api_auth
        .authentication
        .authenticate(&body.email, body.password.as_bytes())
        .await
    {
        Ok(issued) => {
            // The one boundary where the token leaves the server, in UUID
            // form. It is never logged.
            let token = issued.token.as_uuid().to_string();
            (StatusCode::OK, Json(LoginResponse { token })).into_response()
        }
        Err(DomainError::Unauthorized) => unauthorized(&instance),
        Err(error) => crate::contacts::domain_error_response(error, instance),
    }
}

/// `POST /api/v1/auth/logout` — revokes the caller's session.
///
/// Always `204`: revocation is idempotent and an unknown token still
/// yields `204`, so logout never leaks whether a token existed. A
/// missing bearer header is `401` (there is nothing to revoke without a
/// credential).
pub async fn logout<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    headers: HeaderMap,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: crate::contacts::IdempotencyStore,
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
    let token = match bearer_token(&headers) {
        Ok(token) => token,
        Err(()) => return unauthorized(&instance),
    };
    match api_auth.authentication.revoke(&token).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => crate::contacts::domain_error_response(error, instance),
    }
}

/// Resolves the [`ActionContext`] for a handler: authenticates (via the
/// middleware-provided [`RequestAuth`]) and authorizes (`action` within
/// the caller's agency-anchored `profile` scope).
///
/// * No [`ApiAuth`] configured (in-memory test slice): returns the legacy
///   nil-system-actor context so handler unit tests work without auth.
/// * Middleware did not run (no `RequestAuth` extension): fails closed
///   with `401`.
/// * Token invalid: `401`; no covering grant: `403`.
///
/// The returned context carries the real actor (staff), so audit events
/// record who acted — replacing the nil-actor attribution.
pub async fn resolve_context<S, I>(
    state: &AppState<S, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    action: &Action,
    profile: Option<ClientProfileId>,
) -> Result<ActionContext, Box<Response>> {
    let (instance, correlation_id) = request_id(headers);
    let Some(api_auth) = &state.auth else {
        return Ok(crm_core::memory::api_action_context(
            profile,
            correlation_id,
        ));
    };
    let Some(Extension(req_auth)) = auth else {
        return Err(Box::new(unauthorized(&instance)));
    };
    let scope = match profile {
        Some(client_profile_id) => Scope::ClientProfile {
            agency_id: req_auth.agency_id,
            client_profile_id,
        },
        None => Scope::Agency {
            agency_id: req_auth.agency_id,
        },
    };
    match api_auth
        .authorization
        .authorize(&req_auth.token, action, &scope, correlation_id)
        .await
    {
        Ok(ctx) => Ok(ctx),
        Err(DomainError::Unauthorized) => Err(Box::new(unauthorized(&instance))),
        Err(DomainError::Forbidden(_)) => Err(Box::new(forbidden(&instance))),
        Err(error) => Err(Box::new(crate::contacts::domain_error_response(
            error, instance,
        ))),
    }
}
