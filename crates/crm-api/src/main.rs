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
    extract::OriginalUri,
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
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

mod contacts;

use contacts::AppState;

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
pub(crate) fn app(state: AppState, web_dir: Option<PathBuf>) -> Router {
    let header_name = HeaderName::from_static(REQUEST_ID_HEADER);

    let api = Router::new()
        .route("/v1/version", get(version))
        .route(
            "/v1/contacts",
            post(contacts::create_contact).get(contacts::list_contacts),
        )
        .route("/v1/contacts/{id}", get(contacts::get_contact))
        .fallback(not_found);

    let router = Router::new()
        .route("/healthz", get(healthz))
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

    axum::serve(listener, app(AppState::new(), web_dir))
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
