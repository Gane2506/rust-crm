// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! `crm-api`: HTTP API server for the Rust CRM.
//!
//! Exposes the versioned JSON API (`/api/v1/...`) plus operational endpoints
//! such as `/healthz`. Every request carries an `x-request-id` correlation id
//! (generated when absent, propagated on the response). Error responses use
//! the RFC 7807 `application/problem+json` format.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{
    extract::{OriginalUri, State},
    http::{header, HeaderMap, HeaderName, StatusCode},
    middleware,
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post, put},
    Router,
};
use serde::Serialize;
use thiserror::Error;
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod assets;
mod audit;
mod auth;
mod campaigns;
mod contacts;
mod emails;
mod feed;
mod forms;
mod jev;
mod observability;
#[cfg(test)]
mod pg_tests;
mod profiles;
mod rate_limit;
mod security_headers;
mod workflows;

use contacts::{request_id, AppState, IdempotencyStore};
use crm_core::memory::{BackendHealth, ContactReadModel, ContactServiceImpl};
use crm_domain::services::AuditService;

/// Header carrying the request/correlation id.
pub(crate) const REQUEST_ID_HEADER: &str = "x-request-id";
/// Environment variable overriding the bind address.
const BIND_ADDR_ENV: &str = "CRM_API_ADDR";
/// Default bind address when [`BIND_ADDR_ENV`] is unset.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";
/// Environment variable pointing at the directory holding the built WASM
/// frontend (`index.html` plus the wasm-bindgen output).
///
/// Build it with:
/// `cargo build -p crm-web --target wasm32-unknown-unknown --release`,
/// then `wasm-bindgen --target web --out-dir <web-dir>
/// target/wasm32-unknown-unknown/release/crm-web.wasm`
/// (the wasm-bindgen CLI version must match the lockfile's 0.2.129),
/// then copy `crates/crm-web/index.html` into `<web-dir>`.
const WEB_DIR_ENV: &str = "CRM_WEB_DIR";
/// Default web directory when [`WEB_DIR_ENV`] is unset, resolved relative
/// to the server's working directory.
const DEFAULT_WEB_DIR: &str = "./web";

/// Errors that can abort server startup.
#[derive(Debug, Error)]
pub enum ApiError {
    /// The configured bind address is not a valid socket address.
    #[error("invalid bind address '{addr}': {source}")]
    InvalidBindAddr {
        /// The rejected address string.
        addr: String,
        /// The underlying parse failure.
        source: std::net::AddrParseError,
    },
    /// The TCP listener or the server task failed.
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Database setup failed: `DATABASE_URL` missing or invalid, the
    /// database unreachable, or migrations failed. The server cannot
    /// start without PostgreSQL.
    #[error("database setup failed: {0}")]
    Database(#[from] crm_db::DbError),
}

/// RFC 7807 `application/problem+json` error body.
///
/// See <https://www.rfc-editor.org/rfc/rfc9457.html>.
#[derive(Debug, Serialize)]
pub struct ProblemDetails {
    /// URI identifying the problem type (`about:blank` for generic problems).
    #[serde(rename = "type")]
    pub problem_type: String,
    /// Short human-readable summary of the problem.
    pub title: String,
    /// HTTP status code.
    pub status: u16,
    /// Detailed explanation of this occurrence.
    pub detail: String,
    /// Request/correlation id of this occurrence.
    pub instance: String,
}

/// Liveness probe body.
#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
}

/// Build metadata body.
#[derive(Debug, Serialize)]
struct VersionInfo {
    version: &'static str,
}

/// Liveness probe: always `200 {"status":"ok"}` when the server is up.
async fn healthz() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Readiness probe body.
#[derive(Debug, Serialize)]
struct Readiness {
    status: &'static str,
    database: &'static str,
}

/// Readiness probe: `200 {"status":"ok","database":"ok"}` when the backing
/// store answers, `503` `problem+json` when it does not.
///
/// `/healthz` stays the process-liveness probe (always 200 while the
/// server runs); `/readyz` is what orchestrators gate traffic on, since
/// the API cannot serve without PostgreSQL.
///
/// The backend check is bounded by [`READYZ_TIMEOUT`]: a hung or dead
/// database must fail the probe fast instead of holding the connection
/// open until the orchestrator gives up.
///
/// Maximum time the readiness probe waits for the backing store.
const READYZ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

async fn readyz<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    headers: HeaderMap,
) -> Response
where
    R: ContactReadModel + BackendHealth + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    // Bound the probe: a dead database must yield 503 fast, not hang the
    // orchestrator's check until its own timeout fires.
    let healthy = tokio::time::timeout(READYZ_TIMEOUT, state.contact_service().backend_healthy())
        .await
        .unwrap_or(false);
    if healthy {
        (
            StatusCode::OK,
            Json(Readiness {
                status: "ok",
                database: "ok",
            }),
        )
            .into_response()
    } else {
        problem_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service unavailable",
            "the database is unreachable".to_owned(),
            instance,
        )
    }
}

/// Returns the crate version of the running server.
async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Fallback for unmatched routes, rendered as RFC 7807 `problem+json`.
///
/// Takes [`OriginalUri`] (not the possibly prefix-stripped request URI) so
/// the reported path is the real request path even when this runs as the
/// nested `/api` fallback.
async fn not_found(original_uri: OriginalUri) -> impl IntoResponse {
    let path = original_uri.0.path().to_owned();
    problem_response(
        StatusCode::NOT_FOUND,
        "Not found",
        format!("no route matches '{path}'"),
        path,
    )
}

/// Render a `problem+json` response for `status`.
pub(crate) fn problem_response(
    status: StatusCode,
    title: &str,
    detail: String,
    instance: String,
) -> Response {
    let body = ProblemDetails {
        problem_type: "about:blank".to_owned(),
        title: title.to_owned(),
        status: status.as_u16(),
        detail,
        instance,
    };
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        Json(body),
    )
        .into_response()
}

/// Build the Axum router with request-id and tracing middleware.
///
/// The router is generic over the contact-service backend `R`/`A` and the
/// idempotency store `I` so the same routes serve the in-memory vertical
/// slice (tests) and the Postgres-backed production server.
///
/// `web_dir` is the directory holding the built WASM frontend; when `Some`,
/// static files are served from it with an SPA fallback to `index.html`
/// (so deep links like `/contacts` work on refresh and direct navigation).
/// When `None` the server runs API-only and unknown non-API paths keep the
/// `problem+json` 404.
///
/// API routes are nested under `/api` with their own `problem+json`
/// fallback, so unknown `/api/*` paths never get swallowed by the SPA
/// fallback. Layer order (outermost first): generate the request id, trace
/// the request, then propagate the id onto the response.
pub(crate) fn app<R, A, I>(
    state: AppState<ContactServiceImpl<R, A>, I>,
    web_dir: Option<PathBuf>,
) -> Router
where
    R: ContactReadModel + BackendHealth + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let header_name = HeaderName::from_static(REQUEST_ID_HEADER);

    let api = Router::new()
        .route("/v1/version", get(version))
        .route(
            "/v1/contacts",
            post(contacts::create_contact::<R, A, I>).get(contacts::list_contacts::<R, A, I>),
        )
        .route("/v1/contacts/{id}", get(contacts::get_contact::<R, A, I>))
        .route(
            "/v1/contacts/merge",
            post(contacts::merge_contacts::<R, A, I>),
        )
        .route(
            "/v1/profiles",
            post(profiles::create_profile::<R, A, I>).get(profiles::list_profiles::<R, A, I>),
        )
        .route("/v1/profiles/{id}", get(profiles::get_profile::<R, A, I>))
        .route(
            "/v1/profiles/{id}/suspend",
            post(profiles::suspend_profile::<R, A, I>),
        )
        .route(
            "/v1/profiles/{id}/reactivate",
            post(profiles::reactivate_profile::<R, A, I>),
        )
        .route(
            "/v1/profiles/{id}/archive",
            post(profiles::archive_profile::<R, A, I>),
        )
        .route(
            "/v1/assets",
            post(assets::register_asset::<R, A, I>).get(assets::list_assets::<R, A, I>),
        )
        .route("/v1/assets/{id}", get(assets::get_asset::<R, A, I>))
        .route(
            "/v1/assets/{id}/archive",
            post(assets::archive_asset::<R, A, I>),
        )
        .route("/v1/emails/send", post(emails::send_email::<R, A, I>))
        .route("/v1/threads", get(emails::list_threads::<R, A, I>))
        .route("/v1/threads/{id}", get(emails::get_thread::<R, A, I>))
        .route(
            "/v1/campaigns",
            post(campaigns::create_campaign::<R, A, I>).get(campaigns::list_campaigns::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}",
            get(campaigns::get_campaign::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}/steps",
            post(campaigns::add_campaign_step::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}/schedule",
            post(campaigns::schedule_campaign::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}/launch",
            post(campaigns::launch_campaign::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}/pause",
            post(campaigns::pause_campaign::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}/resume",
            post(campaigns::resume_campaign::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}/complete",
            post(campaigns::complete_campaign::<R, A, I>),
        )
        .route(
            "/v1/campaigns/{id}/cancel",
            post(campaigns::cancel_campaign::<R, A, I>),
        )
        .route(
            "/v1/prospect-pools",
            post(campaigns::create_pool::<R, A, I>).get(campaigns::list_pools::<R, A, I>),
        )
        .route(
            "/v1/prospect-pools/{id}/members",
            post(campaigns::add_pool_member::<R, A, I>),
        )
        .route(
            "/v1/prospect-pools/{id}/members/{contact_id}",
            delete(campaigns::remove_pool_member::<R, A, I>),
        )
        .route("/v1/opt-outs", post(campaigns::record_opt_out::<R, A, I>))
        .route(
            "/v1/conversions",
            post(campaigns::record_conversion::<R, A, I>),
        )
        .route("/v1/audit/events", get(audit::audit_events::<R, A, I>))
        .route("/v1/audit/verify", get(audit::audit_verify::<R, A, I>))
        .route(
            "/v1/agent-runs",
            post(jev::create_agent_run::<R, A, I>).get(jev::list_agent_runs::<R, A, I>),
        )
        .route("/v1/agent-runs/{id}", get(jev::get_agent_run::<R, A, I>))
        .route(
            "/v1/agent-runs/{id}/request-approval",
            post(jev::request_approval::<R, A, I>),
        )
        .route(
            "/v1/agent-runs/{id}/approve",
            post(jev::approve_run::<R, A, I>),
        )
        .route("/v1/agent-runs/{id}/deny", post(jev::deny_run::<R, A, I>))
        .route(
            "/v1/agent-runs/{id}/complete",
            post(jev::complete_run::<R, A, I>),
        )
        .route("/v1/agent-runs/{id}/fail", post(jev::fail_run::<R, A, I>))
        .route(
            "/v1/agent-runs/{id}/cancel",
            post(jev::cancel_run::<R, A, I>),
        )
        .route(
            "/v1/agent-runs/{id}/tool-calls",
            post(jev::record_tool_call::<R, A, I>).get(jev::list_tool_calls::<R, A, I>),
        )
        .route(
            "/v1/agent-runs/{id}/handoff",
            post(jev::create_handoff::<R, A, I>),
        )
        .route(
            "/v1/agent-runs/{id}/handoffs",
            get(jev::list_handoffs::<R, A, I>),
        )
        .route(
            "/v1/workflows",
            post(workflows::create_workflow::<R, A, I>).get(workflows::list_workflows::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}",
            get(workflows::get_workflow::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/publish",
            post(workflows::publish_workflow::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/archive",
            post(workflows::archive_workflow::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/nodes",
            post(workflows::add_node::<R, A, I>).get(workflows::list_nodes::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/nodes/{node_id}",
            delete(workflows::remove_node::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/edges",
            post(workflows::add_edge::<R, A, I>).get(workflows::list_edges::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/edges/{edge_id}",
            delete(workflows::remove_edge::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/trigger",
            put(workflows::set_trigger::<R, A, I>).get(workflows::get_trigger::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/simulate",
            post(workflows::simulate_workflow::<R, A, I>),
        )
        .route(
            "/v1/workflows/{id}/versions",
            get(workflows::list_versions::<R, A, I>),
        )
        .route(
            "/v1/forms",
            post(forms::create_form::<R, A, I>).get(forms::list_forms::<R, A, I>),
        )
        .route("/v1/forms/{id}", get(forms::get_form::<R, A, I>))
        .route(
            "/v1/forms/{id}/publish",
            post(forms::publish_form::<R, A, I>),
        )
        .route(
            "/v1/forms/{id}/archive",
            post(forms::archive_form::<R, A, I>),
        )
        .route(
            "/v1/forms/{id}/fields",
            post(forms::add_field::<R, A, I>).get(forms::list_fields::<R, A, I>),
        )
        .route(
            "/v1/forms/{id}/fields/{field_id}",
            delete(forms::remove_field::<R, A, I>),
        )
        .route(
            "/v1/forms/{id}/submissions",
            post(forms::submit_form::<R, A, I>).get(forms::list_submissions::<R, A, I>),
        )
        .route(
            "/v1/forms/{id}/submissions/{submission_id}/review",
            post(forms::review_submission::<R, A, I>),
        )
        .route("/v1/feed", get(feed::list_feed::<R, A, I>))
        .route(
            "/v1/feed/{id}/read",
            post(feed::mark_feed_item_read::<R, A, I>),
        )
        .route("/v1/feed/{id}/pin", post(feed::pin_feed_item::<R, A, I>))
        .route("/v1/feed/{id}/hide", post(feed::hide_feed_item::<R, A, I>))
        .fallback(not_found);

    // When authentication is configured (the Postgres-backed server), the
    // login/logout routes are registered and every other `/api/v1/*`
    // route goes through the auth middleware: a missing or invalid
    // bearer token is a `401` before any handler runs. The in-memory test
    // slice carries no `ApiAuth`, so the middleware is not installed and
    // handler unit tests keep working without tokens.
    let api = match state.auth.clone() {
        Some(api_auth) => Router::new()
            .route("/v1/auth/login", post(auth::login::<R, A, I>))
            .route("/v1/auth/logout", post(auth::logout::<R, A, I>))
            // The email ingest webhook is NOT bearer-authenticated
            // (providers cannot hold staff sessions); it is gated by
            // the `CRM_EMAIL_WEBHOOK_SECRET` shared secret inside the
            // handler, so it lives outside the auth middleware.
            .route("/v1/emails/ingest", post(emails::ingest_email::<R, A, I>))
            .merge(api.layer(middleware::from_fn_with_state(
                api_auth,
                auth::auth_middleware,
            ))),
        // No auth configured (in-memory test slice): the ingest route
        // still registers; the handler returns 501 without `ApiAuth`.
        None => api.route("/v1/emails/ingest", post(emails::ingest_email::<R, A, I>)),
    };

    // Defensive security headers on every API response — successes,
    // `problem+json` errors, 401s and 403s alike. Applied after the auth
    // middleware so it is the outermost API layer; see
    // [`security_headers`] for the header set and the deliberate
    // omissions (HSTS, CSP).
    let api = api.layer(middleware::from_fn(security_headers::security_headers));

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz::<R, A, I>))
        .nest("/api", api);

    let router = match web_dir {
        // `fallback` (not `not_found_service`): the SPA entry must come
        // back as 200 so the browser boots the app on deep links.
        // `not_found_service` would force a 404 status onto the body.
        Some(dir) => router
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => router.fallback(not_found),
    };

    router.with_state(state).layer(
        ServiceBuilder::new()
            .layer(SetRequestIdLayer::new(header_name.clone(), MakeRequestUuid))
            .layer(TraceLayer::new_for_http())
            .layer(PropagateRequestIdLayer::new(header_name)),
    )
}

/// Resolve the bind address from [`BIND_ADDR_ENV`], defaulting to
/// [`DEFAULT_BIND_ADDR`].
fn bind_addr() -> Result<SocketAddr, ApiError> {
    let raw = std::env::var(BIND_ADDR_ENV).unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_owned());
    raw.parse()
        .map_err(|source| ApiError::InvalidBindAddr { addr: raw, source })
}

/// Resolve the web directory from [`WEB_DIR_ENV`], defaulting to
/// [`DEFAULT_WEB_DIR`]. A relative value resolves against the server's
/// working directory, so the default serves `./web` from wherever the
/// server is started.
fn web_dir() -> PathBuf {
    PathBuf::from(std::env::var(WEB_DIR_ENV).unwrap_or_else(|_| DEFAULT_WEB_DIR.to_owned()))
}

/// Builds the database pool from `DATABASE_URL` and applies any pending
/// embedded migrations.
///
/// A missing or invalid `DATABASE_URL`, an unreachable database, or a
/// failed migration is a startup error: the server cannot serve without
/// PostgreSQL, so it logs the cause and exits non-zero instead of
/// falling back to non-durable state.
async fn database_pool() -> Result<sqlx::PgPool, ApiError> {
    let pool = crm_db::create_pool().await.map_err(|error| {
        tracing::error!(%error, "database setup failed");
        ApiError::from(error)
    })?;
    crm_db::run_migrations(&pool).await.map_err(|error| {
        tracing::error!(%error, "database migration failed");
        ApiError::from(error)
    })?;
    tracing::info!("database pool ready; migrations applied");
    Ok(pool)
}

/// Install the JSON `tracing` subscriber, honoring `RUST_LOG`.
fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crm_api=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();
}

/// Wait for SIGINT (Ctrl+C) or SIGTERM, then return.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::info!("shutdown signal received");
}

#[tokio::main]
async fn main() -> Result<(), ApiError> {
    init_tracing();

    let pool = database_pool().await?;
    let state = AppState::from_pool(&pool);

    let addr = bind_addr()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "crm-api listening");

    let web_dir = web_dir();
    let web_dir = if web_dir.is_dir() {
        tracing::info!(path = %web_dir.display(), "serving static frontend");
        Some(web_dir)
    } else {
        tracing::warn!(
            path = %web_dir.display(),
            "web directory not found; running in API-only mode"
        );
        None
    };

    // `ConnectInfo` feeds the login rate limiter the TCP peer address;
    // the login handler falls back to `X-Forwarded-For` (trusted proxy)
    // or a shared bucket when it is absent.
    axum::serve(
        listener,
        app(state, web_dir).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    tracing::info!("crm-api shut down cleanly");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request};
    use tower::ServiceExt;

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("test request builds")
    }

    #[tokio::test]
    async fn healthz_returns_200_with_ok_status() {
        let response = app(AppState::new(), None)
            .oneshot(get("/healthz"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn readyz_returns_200_with_memory_backend() {
        // The in-memory backend is always ready; no database needed.
        let response = app(AppState::new(), None)
            .oneshot(get("/readyz"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["database"], "ok");
    }

    #[tokio::test]
    async fn readyz_returns_503_when_database_unreachable() {
        // A lazily-connected pool against a closed port never touches the
        // network until the probe runs, so no database is needed for this
        // test; the probe's `SELECT 1` fails with connection-refused.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://postgres:postgres@127.0.0.1:9/crm_test")
            .expect("lazy pool builds without connecting");
        let response = app(AppState::from_pool(&pool), None)
            .oneshot(get("/readyz"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("content type")
                .to_str()
                .expect("content type parses"),
            "application/problem+json"
        );
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], 503);
        assert_eq!(json["title"], "Service unavailable");
    }

    #[tokio::test]
    async fn version_returns_crate_version() {
        let response = app(AppState::new(), None)
            .oneshot(get("/api/v1/version"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn unknown_route_returns_problem_json() {
        let response = app(AppState::new(), None)
            .oneshot(get("/nope"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header");
        assert_eq!(content_type, "application/problem+json");

        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], 404);
        assert_eq!(json["type"], "about:blank");
    }

    #[tokio::test]
    async fn response_carries_request_id() {
        let response = app(AppState::new(), None)
            .oneshot(get("/healthz"))
            .await
            .expect("oneshot");

        assert!(
            response.headers().contains_key(REQUEST_ID_HEADER),
            "response must propagate x-request-id"
        );
    }

    #[tokio::test]
    async fn api_responses_carry_security_headers() {
        let response = app(AppState::new(), None)
            .oneshot(get("/api/v1/version"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        let as_str = |name: &str| {
            headers
                .get(name)
                .unwrap_or_else(|| panic!("{name} must be set"))
                .to_str()
                .expect("header value is ascii")
        };
        assert_eq!(as_str("x-content-type-options"), "nosniff");
        assert_eq!(as_str("x-frame-options"), "DENY");
        assert_eq!(as_str("referrer-policy"), "no-referrer");
        assert!(
            headers.contains_key("permissions-policy"),
            "permissions-policy must be set"
        );
    }

    #[tokio::test]
    async fn api_error_responses_carry_security_headers() {
        // The headers layer is outermost on the API router, so even the
        // problem+json 404 carries them.
        let response = app(AppState::new(), None)
            .oneshot(get("/api/v1/nope"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response.headers().contains_key("x-content-type-options"),
            "error responses must carry the security headers"
        );
    }

    #[tokio::test]
    async fn bind_addr_rejects_garbage() {
        // Only this test touches CRM_API_ADDR, so process-wide env mutation
        // cannot race with another test here.
        std::env::set_var(BIND_ADDR_ENV, "not-an-address");
        let err = bind_addr().expect_err("garbage address must fail");
        assert!(matches!(err, ApiError::InvalidBindAddr { .. }));
        std::env::remove_var(BIND_ADDR_ENV);
    }

    /// Test-only: build a web directory under the system temp dir holding
    /// the given `(name, content)` files. Callers should remove the
    /// returned directory when done.
    fn test_web_dir(files: &[(&str, &str)]) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("crm-api-web-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create test web dir");
        for (name, content) in files {
            std::fs::write(dir.join(name), content).expect("write test web file");
        }
        dir
    }

    const TEST_INDEX: &str = "<!DOCTYPE html><html><body>spa</body></html>";

    #[tokio::test]
    async fn static_file_served_with_content_type() {
        let dir = test_web_dir(&[("index.html", TEST_INDEX), ("app.css", "body{}")]);

        let response = app(AppState::new(), Some(dir.clone()))
            .oneshot(get("/app.css"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header")
            .to_str()
            .expect("content-type is ASCII");
        assert!(
            content_type.starts_with("text/css"),
            "unexpected content-type: {content_type}"
        );
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(body.as_ref(), b"body{}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn spa_fallback_serves_index_for_app_routes() {
        let dir = test_web_dir(&[("index.html", TEST_INDEX)]);

        let response = app(AppState::new(), Some(dir.clone()))
            .oneshot(get("/contacts"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        assert_eq!(body.as_ref(), TEST_INDEX.as_bytes());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn api_404_not_swallowed_by_spa_fallback() {
        let dir = test_web_dir(&[("index.html", TEST_INDEX)]);

        for method in ["GET", "POST"] {
            let request = Request::builder()
                .method(method)
                .uri("/api/v1/nope")
                .body(Body::empty())
                .expect("test request builds");
            let response = app(AppState::new(), Some(dir.clone()))
                .oneshot(request)
                .await
                .expect("oneshot");

            assert_eq!(response.status(), StatusCode::NOT_FOUND, "method {method}");
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("content-type header");
            assert_eq!(content_type, "application/problem+json", "method {method}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn healthz_unaffected_by_static_serving() {
        let dir = test_web_dir(&[("index.html", TEST_INDEX)]);

        let response = app(AppState::new(), Some(dir.clone()))
            .oneshot(get("/healthz"))
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], "ok");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn missing_web_dir_runs_api_only() {
        // `None` keeps the pre-static behavior: unknown non-API paths get
        // the `problem+json` 404, and the API is unaffected.
        let response = app(AppState::new(), None)
            .oneshot(get("/nope"))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header");
        assert_eq!(content_type, "application/problem+json");

        let response = app(AppState::new(), None)
            .oneshot(get("/healthz"))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
