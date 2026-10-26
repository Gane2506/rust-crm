// SPDX-License-Identifier: GPL-3.0-only
//! Security audit log HTTP surface for `crm-api` (task 04-AUD-02).
//!
//! * `GET /api/v1/audit/events` — queries the tamper-evident security
//!   event log with the [`AuditQuery`] filters (actor, action, profile,
//!   time range, cursor, limit). Chain order (oldest first) by default,
//!   so readers can verify each link; `?newest_first=true` returns the
//!   newest events first instead, for timelines like the Dashboard
//!   "Recent audit events" tile. Newest-first is a presentation order
//!   only — storage, hashing and verification never change.
//! * `GET /api/v1/audit/verify` — verifies the whole chain from genesis
//!   and reports [`ChainVerification`].
//!
//! ## Authorization
//!
//! Reading the audit log is itself an RBAC decision: both endpoints
//! require `AuditLog:Read` within the agency scope
//! ([`Resource::AuditLog`], [`Scope::Agency`]). A caller holding only
//! client-profile grants gets `403` — the audit log is an agency-level
//! resource, and profile-scoped readers must not learn whether
//! agency-level events exist.
//!
//! ## Failure tradeoff
//!
//! Recording a security event must never break the request it describes:
//! [`record_security_event`] logs loudly (`tracing::error!`) and lets the
//! request continue when the audit write fails. A failed audit write is
//! a critical operational signal — it pages the operator — but turning a
//! login into a 500 because the audit table hiccuped would turn an
//! observability dependency into an availability dependency, and would
//! let an attacker deny logins by filling the disk. The tradeoff is
//! documented in `docs/operations/security.md`.
//!
//! [`AuditQuery`]: crm_domain::services::AuditQuery
//! [`ChainVerification`]: crm_domain::services::ChainVerification
//! [`Resource::AuditLog`]: crm_domain::security::Resource::AuditLog
//! [`Scope::Agency`]: crm_domain::security::Scope::Agency

use axum::{
    extract::{Extension, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_db::audit_log::PgAuditLogService;
use crm_domain::audit::{ActorKind, AuditEvent, SecurityEvent};
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{ActionContext, AuditLogService, AuditQuery};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{request_id, AppState};
use crate::problem_response;
use axum::http::StatusCode;
use crm_core::memory::{ContactReadModel, ContactServiceImpl};
use crm_domain::services::AuditService;

/// Records one [`SecurityEvent`] without ever failing the request.
///
/// Mints a fresh idempotency key per call (each recording is its own
/// logical operation). On storage failure this logs at `error!` — the
/// loudest non-fatal signal — and returns `None`; the caller continues
/// serving the request. See the module docs for the tradeoff.
pub async fn record_security_event(
    audit_log: &PgAuditLogService,
    ctx: &ActionContext,
    event: &SecurityEvent,
) -> Option<AuditEvent> {
    match audit_log.record(ctx, event, Uuid::new_v4()).await {
        Ok(recorded) => Some(recorded),
        Err(error) => {
            tracing::error!(
                %error,
                action = event.action_name(),
                "security audit write failed; request continues without its audit event"
            );
            None
        }
    }
}

/// Builds the [`ActionContext`] for a failed-login or rate-limit audit
/// event: there is no authenticated actor, so the system actor records
/// it. The email/IP never enters the log — only fingerprints.
pub fn system_action_context(correlation_id: Uuid) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: None,
        correlation_id,
    }
}

/// Builds the [`ActionContext`] for an authenticated staff member's
/// security event (login success, logout).
pub fn staff_action_context(
    actor_id: ActorId,
    client_profile_id: Option<ClientProfileId>,
    correlation_id: Uuid,
) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id,
        client_profile_id,
        correlation_id,
    }
}

/// Query parameters for `GET /api/v1/audit/events`; every filter is
/// optional. `limit` defaults to 50 and is capped at 100 by the service.
#[derive(Debug, Deserialize)]
pub struct AuditEventsQuery {
    /// Only events by this actor.
    pub actor_id: Option<Uuid>,
    /// Only events with this action name (e.g. `"auth.login"`).
    pub action: Option<String>,
    /// Only events attributed to this client profile.
    pub client_profile_id: Option<Uuid>,
    /// Only events at or after this time (RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// Only events before this time (RFC 3339).
    pub to: Option<DateTime<Utc>>,
    /// Cursor: only events after this event id, in chain order — or,
    /// with `newest_first`, only events before this event id in
    /// newest-first order (the cursor is always the last event of the
    /// previous page, whichever direction the page runs).
    pub after: Option<Uuid>,
    /// Maximum events to return.
    pub limit: Option<u64>,
    /// When true, return the newest events first instead of chain
    /// order. Absent (or false) keeps the default chain order, so
    /// existing callers are unaffected.
    #[serde(default)]
    pub newest_first: bool,
}

/// One audit event in a query response: the tamper-evident envelope plus
/// the forensic `SecurityEvent` payload, when the event has one.
#[derive(Debug, Serialize)]
pub struct AuditEventResponse {
    /// The chain-linked envelope.
    #[serde(flatten)]
    pub event: AuditEvent,
    /// The serialized [`SecurityEvent`], if this event carries one.
    pub payload: Option<serde_json::Value>,
}

/// A page of audit events: chain order by default, newest first when
/// the request asked for it (`?newest_first=true`).
#[derive(Debug, Serialize)]
pub struct AuditEventsResponse {
    /// Events in the requested order (chain order unless newest-first).
    pub events: Vec<AuditEventResponse>,
    /// Whether more events match beyond this page.
    pub has_more: bool,
}

/// Chain verification result for `GET /api/v1/audit/verify`.
#[derive(Debug, Serialize)]
pub struct VerifyResponse {
    /// Whether every checked event links correctly from genesis.
    pub valid: bool,
    /// Number of events checked.
    pub events_checked: u64,
    /// Chain hash of the verified head, hex-encoded.
    pub head: Option<String>,
    /// The first event that failed verification, if any.
    pub first_bad_event: Option<Uuid>,
}

/// Hex-encodes 32 bytes (lowercase, no prefix).
fn hex_32(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// `GET /api/v1/audit/events` — queries the security audit log.
///
/// Authenticated via the auth middleware; authorized per the module
/// docs (`AuditLog:Read` in the agency scope). Returns chain-order
/// pages by default; `?newest_first=true` returns the newest events
/// first (the cursor then pages backwards from the previous page's
/// last event). Newest-first is presentation only — the chain itself
/// is untouched, and [`audit_verify`] still verifies from genesis.
pub async fn audit_events<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(params): Query<AuditEventsQuery>,
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
    let action = Action {
        resource: Resource::AuditLog,
        operation: Operation::Read,
    };
    // Agency scope: the audit log is an agency-level resource.
    let ctx = match resolve_context(&state, auth, &headers, &action, None).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let filter = AuditQuery {
        actor_id: params.actor_id.map(ActorId::from),
        action: params.action,
        client_profile_id: params.client_profile_id.map(ClientProfileId::from),
        from: params.from,
        to: params.to,
        after: params.after.map(AuditEventId::from),
        limit: params.limit.unwrap_or(0),
        newest_first: params.newest_first,
    };
    match api_auth.audit_log.query_detailed(&ctx, &filter).await {
        Ok((events, has_more)) => (
            StatusCode::OK,
            Json(AuditEventsResponse {
                events: events
                    .into_iter()
                    .map(|(event, payload)| AuditEventResponse { event, payload })
                    .collect(),
                has_more,
            }),
        )
            .into_response(),
        Err(error) => crate::contacts::domain_error_response(error, instance),
    }
}

/// `GET /api/v1/audit/verify` — verifies the audit chain from genesis.
///
/// Same authorization as [`audit_events`]: `AuditLog:Read` in the agency
/// scope. An invalid chain is a critical finding — the response says so
/// plainly (`valid: false` plus the first bad event).
pub async fn audit_verify<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
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
    let action = Action {
        resource: Resource::AuditLog,
        operation: Operation::Read,
    };
    // The context is only needed for authorization here; verification
    // itself covers the whole log by domain design.
    if let Err(response) = resolve_context(&state, auth, &headers, &action, None).await {
        return *response;
    }
    match api_auth.audit_log.verify_chain().await {
        Ok(verification) => (
            StatusCode::OK,
            Json(VerifyResponse {
                valid: verification.valid,
                events_checked: verification.events_checked,
                head: verification.head.as_ref().map(hex_32),
                first_bad_event: verification.first_bad_event.map(|id| id.as_uuid()),
            }),
        )
            .into_response(),
        Err(error) => crate::contacts::domain_error_response(error, instance),
    }
}
