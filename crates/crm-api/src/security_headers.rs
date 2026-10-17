// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Security headers for API responses (task 03-SEC-03).
//!
//! [`security_headers`] is a thin axum middleware applied to the `/api`
//! router: every API response — success, `problem+json` error, 401/403 —
//! carries the same defensive defaults.
//!
//! ## Headers set
//!
//! * `X-Content-Type-Options: nosniff` — stops MIME-sniffing of API
//!   payloads.
//! * `X-Frame-Options: DENY` — API responses are never framed.
//! * `Referrer-Policy: no-referrer` — the API never needs a referrer and
//!   must not leak request paths (which can contain UUIDs).
//! * `Permissions-Policy: camera=(), microphone=(), geolocation=(),
//!   payment=()` — the API uses none of these capabilities.
//!
//! ## Deliberately not set here
//!
//! * `Strict-Transport-Security` — the server binds loopback and never
//!   terminates TLS; HSTS is the reverse proxy's job (see
//!   `docs/operations/threat-model.md`). Emitting it over plaintext
//!   would be ineffective.
//! * `Content-Security-Policy` — API JSON responses gain nothing from a
//!   CSP, and the SPA's framing policy belongs to the UI workstream
//!   (06-UI), which owns the document the policy would constrain.

use axum::{
    extract::Request,
    http::{
        header::{HeaderName, HeaderValue},
        HeaderMap,
    },
    middleware::Next,
    response::Response,
};

/// Header names applied to every API response.
const HEADER_NAMES: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=()",
    ),
];

/// Builds the static security-header map shared by every response.
///
/// All names and values are `'static` constants, so construction cannot
/// fail.
fn header_map() -> HeaderMap {
    let mut map = HeaderMap::with_capacity(HEADER_NAMES.len());
    for (name, value) in HEADER_NAMES {
        let name = HeaderName::from_static(name);
        let value = HeaderValue::from_static(value);
        map.insert(name, value);
    }
    map
}

/// Inserts the defensive security headers into the response.
///
/// Existing values are overwritten: the middleware is the authority on
/// these headers for API responses, so a handler cannot accidentally
/// weaken them.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().extend(header_map());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::StatusCode, routing::get, Router};
    use tower::ServiceExt;

    async fn ok() -> &'static str {
        "ok"
    }

    fn router() -> Router {
        Router::new()
            .route("/ping", get(ok))
            .layer(axum::middleware::from_fn(security_headers))
    }

    #[tokio::test]
    async fn sets_all_defensive_headers() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/ping")
                    .body(Body::empty())
                    .expect("test request builds"),
            )
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
        assert_eq!(
            as_str("permissions-policy"),
            "camera=(), microphone=(), geolocation=(), payment=()",
        );
    }

    #[tokio::test]
    async fn headers_apply_to_error_responses_too() {
        async fn boom() -> (StatusCode, &'static str) {
            (StatusCode::IM_A_TEAPOT, "boom")
        }
        let router = Router::new()
            .route("/boom", get(boom))
            .layer(axum::middleware::from_fn(security_headers));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/boom")
                    .body(Body::empty())
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot");

        assert_eq!(response.status(), StatusCode::IM_A_TEAPOT);
        assert!(
            response.headers().contains_key("x-content-type-options"),
            "error responses must carry the headers as well"
        );
    }
}
