// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! `crm-api`: HTTP API server for the Rust CRM.
//!
//! Exposes the versioned JSON API (`/api/v1/...`) plus operational endpoints
//! such as `/healthz`. Every request carries an `x-request-id` correlation id
//! (generated when absent, propagated on the response). Error responses use
//! the RFC 7807 `application/problem+json` format.

use std::net::SocketAddr;

use axum::{
    http::{header, HeaderName, StatusCode, Uri},
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::Serialize;
use thiserror::Error;
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Header carrying the request/correlation id.
const REQUEST_ID_HEADER: &str = "x-request-id";
/// Environment variable overriding the bind address.
const BIND_ADDR_ENV: &str = "CRM_API_ADDR";
/// Default bind address when [`BIND_ADDR_ENV`] is unset.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";

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
async fn not_found(uri: Uri) -> impl IntoResponse {
    problem_response(
        StatusCode::NOT_FOUND,
        "Not found",
        format!("no route matches '{}'", uri.path()),
        uri.path().to_owned(),
    )
}

/// Render a `problem+json` response for `status`.
fn problem_response(status: StatusCode, title: &str, detail: String, instance: String) -> Response {
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
/// Layer order (outermost first): generate the request id, trace the request,
/// then propagate the id onto the response.
fn app() -> Router {
    let header_name = HeaderName::from_static(REQUEST_ID_HEADER);
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/version", get(version))
        .fallback(not_found)
        .layer(
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

    axum::serve(listener, app())
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
        let response = app().oneshot(get("/healthz")).await.expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn version_returns_crate_version() {
        let response = app()
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
        let response = app().oneshot(get("/nope")).await.expect("oneshot");

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
        let response = app().oneshot(get("/healthz")).await.expect("oneshot");

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
}
