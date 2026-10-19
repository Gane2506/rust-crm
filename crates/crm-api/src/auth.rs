// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! HTTP authentication for `crm-api` (task 03-SEC-02).
//!
//! * `POST /api/v1/auth/login {email, password}` → `200 {token}` — the
//!   token is rendered via [`SessionToken::as_uuid`] at this boundary
//!   only; it is never logged and never appears in error messages.
//!   Login attempts are rate-limited per client IP and per normalized
//!   email (see [`crate::rate_limit`]); an exhausted bucket is `429`
//!   `problem+json` with `Retry-After`.
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

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, ConnectInfo, Extension, Request, State},
    http::{header::AUTHORIZATION, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crm_db::auth::{PgAuthenticationService, PgAuthorizationService};
use crm_domain::audit::{fingerprint, RateLimitBucket, SecurityEvent};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AgencyId, ClientProfileId};
use crm_domain::observability::{AnomalySignal, AnomalySink, TraceEvent, Tracer};
use crm_domain::security::{Action, Scope, SessionToken};
use crm_domain::services::{ActionContext, AuthenticationService, AuthorizationService};

use crate::contacts::{request_id, AppState};
use crate::observability::{DenialTracker, DenialTrackerConfig, LoggingAnomalySink, TracingTracer};
use crate::problem_response;
use crate::rate_limit::{
    LoginRateLimitConfig, LoginRateLimiter, WriteRateLimitConfig, WriteRateLimiter,
};
use crm_core::memory::{ContactReadModel, ContactServiceImpl};
use crm_db::admin::{
    PgAgencySettingsRepository, PgBackupRepository, PgExportRepository, PgInstallationRepository,
    PgMaintenanceRepository, PgRoleAssignmentRepository, PgStaffInvitationRepository,
    PgUpgradeRepository,
};
use crm_db::audit_log::PgAuditLogService;
use crm_db::billing::{PgInvoiceRepository, PgPaymentRepository};
use crm_db::campaign::{
    PgCampaignRepository, PgConversionRepository, PgOptOutRepository, PgProspectPoolRepository,
};
use crm_db::core_crm::{PgAssetRepository, PgClientProfileRepository};
use crm_db::dashboard::{PgDashboardRepository, PgReportRepository};
use crm_db::email::{
    PgEmailAttachmentRepository, PgEmailMessageRepository, PgEmailThreadRepository,
};
use crm_db::feed::PgFeedItemRepository;
use crm_db::form::{PgFormFieldRepository, PgFormRepository, PgFormSubmissionRepository};
use crm_db::jev::{
    PgAgentRunRepository, PgHandoffRepository, PgRoleMembership, PgToolCallRepository,
};
use crm_db::postgres::{PgAuditService, PgContactRepository, PgStaffRepository};
use crm_db::signing::{PgContractRepository, PgSigningTemplateRepository};
use crm_db::voice::PgCallRepository;
use crm_db::workflow::{
    PgTriggerRepository, PgWorkflowEdgeRepository, PgWorkflowNodeRepository, PgWorkflowRepository,
    PgWorkflowVersionRepository,
};
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
    /// Brute-force mitigation for the login endpoint (shared, in-memory).
    pub login_limiter: Arc<LoginRateLimiter>,
    /// Flood mitigation for mutating endpoints: every `POST`/`PUT`/
    /// `PATCH`/`DELETE` on `/api/v1/*` consumes from a per-IP bucket
    /// (shared, in-memory). Reads are unlimited.
    pub write_limiter: Arc<WriteRateLimiter>,
    /// Tamper-evident security audit log.
    pub audit_log: PgAuditLogService,
    /// Structured trace emitter (fire-and-forget).
    pub tracer: TracingTracer,
    /// Anomaly signal sink (fire-and-forget).
    pub anomaly_sink: LoggingAnomalySink,
    /// Per-actor authorization-denial counter for probing detection.
    pub denial_tracker: Arc<DenialTracker>,
    /// Client-profile repository for the profile endpoints.
    pub profiles: PgClientProfileRepository,
    /// Asset repository for the asset endpoints.
    pub assets: PgAssetRepository,
    /// Email message repository for the email endpoints.
    pub email_messages: PgEmailMessageRepository,
    /// Email thread repository for the email endpoints.
    pub email_threads: PgEmailThreadRepository,
    /// Email attachment repository for the email endpoints.
    pub email_attachments: PgEmailAttachmentRepository,
    /// Business-action audit service for email send/receive events.
    pub audit_service: PgAuditService,
    /// Campaign repository for the campaign endpoints.
    pub campaigns: PgCampaignRepository,
    /// Prospect-pool repository for the campaign endpoints.
    pub prospect_pools: PgProspectPoolRepository,
    /// Opt-out repository for the campaign endpoints.
    pub opt_outs: PgOptOutRepository,
    /// Conversion repository for the campaign endpoints.
    pub conversions: PgConversionRepository,
    /// Contact repository for campaign bulk sends.
    pub contacts: PgContactRepository,
    /// Agent-run repository for the Jev agent endpoints.
    pub agent_runs: PgAgentRunRepository,
    /// Tool-call repository for the Jev agent endpoints.
    pub tool_calls: PgToolCallRepository,
    /// Handoff repository for the Jev agent endpoints.
    pub handoffs: PgHandoffRepository,
    /// Role-membership checker for agent-run approvals.
    pub roles: PgRoleMembership,
    /// Workflow repository for the workflow endpoints.
    pub workflows: PgWorkflowRepository,
    /// Workflow-node repository for the workflow endpoints.
    pub workflow_nodes: PgWorkflowNodeRepository,
    /// Workflow-edge repository for the workflow endpoints.
    pub workflow_edges: PgWorkflowEdgeRepository,
    /// Workflow-trigger repository for the workflow endpoints.
    pub workflow_triggers: PgTriggerRepository,
    /// Workflow-version repository for the workflow endpoints.
    pub workflow_versions: PgWorkflowVersionRepository,
    /// Form repository for the form endpoints.
    pub forms: PgFormRepository,
    /// Form-field repository for the form endpoints.
    pub form_fields: PgFormFieldRepository,
    /// Form-submission repository for the form endpoints.
    pub form_submissions: PgFormSubmissionRepository,
    /// Feed-item repository for the feed endpoints.
    pub feed_items: PgFeedItemRepository,
    /// Invoice repository for the billing endpoints.
    pub invoices: PgInvoiceRepository,
    /// Payment repository for the billing endpoints.
    pub payments: PgPaymentRepository,
    /// Contract repository for the signing endpoints.
    pub contracts: PgContractRepository,
    /// Signing-template repository for the signing endpoints.
    pub signing_templates: PgSigningTemplateRepository,
    /// Call repository for the calling endpoints.
    pub calls: PgCallRepository,
    /// Voice provider for the calling endpoints (stub until the
    /// Telnyx adapter lands).
    pub voice_provider: crm_core::voice::StubVoiceProvider,
    /// Dashboard repository for the analytics endpoints.
    pub dashboards: PgDashboardRepository,
    /// Report repository for the analytics endpoints.
    pub reports: PgReportRepository,
    /// Staff-invitation repository for the admin endpoints.
    pub invitations: PgStaffInvitationRepository,
    /// Role-assignment repository for the admin endpoints.
    pub role_assignments: PgRoleAssignmentRepository,
    /// Agency-settings repository for the admin endpoints.
    pub settings: PgAgencySettingsRepository,
    /// Maintenance-mode repository for the admin endpoints.
    pub maintenance: PgMaintenanceRepository,
    /// Staff repository for the admin endpoints (disable/reactivate,
    /// invitation acceptance).
    pub staff: PgStaffRepository,
    /// Backup repository for the admin endpoints.
    pub backups: PgBackupRepository,
    /// Export repository for the admin endpoints.
    pub exports: PgExportRepository,
    /// Upgrade repository for the admin endpoints.
    pub upgrades: PgUpgradeRepository,
    /// Installation repository for the admin endpoints.
    pub installation: PgInstallationRepository,
    /// Branding repository for the branding studio endpoints (UI2-03).
    pub branding: crm_db::branding::PgBrandingRepository,
    /// Database pool for read-only metric execution (widgets query
    /// the entity tables directly; the service builds SQL only from
    /// a hardcoded allowlist).
    pub pool: PgPool,
}

impl ApiAuth {
    /// Builds the auth services over `pool`, with login and write rate
    /// limits and denial-tracker tuning read from the environment (see
    /// [`LoginRateLimitConfig::from_env`],
    /// [`WriteRateLimitConfig::from_env`] and
    /// [`DenialTrackerConfig::from_env`]).
    pub fn new(pool: &PgPool) -> Self {
        Self::with_rate_limits(pool, LoginRateLimitConfig::from_env())
    }

    /// Builds the auth services over `pool` with explicit login rate
    /// limits. Production uses [`ApiAuth::new`]; tests use this to trip
    /// the limiter quickly.
    pub fn with_rate_limits(pool: &PgPool, config: LoginRateLimitConfig) -> Self {
        Self::with_rate_limit_configs(pool, config, WriteRateLimitConfig::from_env())
    }

    /// Builds the auth services over `pool` with explicit login and
    /// write rate limits. Production uses [`ApiAuth::new`]; tests use
    /// this to trip either limiter quickly.
    pub fn with_rate_limit_configs(
        pool: &PgPool,
        login_config: LoginRateLimitConfig,
        write_config: WriteRateLimitConfig,
    ) -> Self {
        Self {
            authentication: PgAuthenticationService::new(pool.clone()),
            authorization: PgAuthorizationService::new(pool.clone()),
            login_limiter: Arc::new(LoginRateLimiter::new(login_config)),
            write_limiter: Arc::new(WriteRateLimiter::new(write_config)),
            audit_log: PgAuditLogService::new(pool.clone()),
            tracer: TracingTracer,
            anomaly_sink: LoggingAnomalySink,
            denial_tracker: Arc::new(DenialTracker::with_config(DenialTrackerConfig::from_env())),
            profiles: PgClientProfileRepository::new(pool.clone()),
            assets: PgAssetRepository::new(pool.clone()),
            email_messages: PgEmailMessageRepository::new(pool.clone()),
            email_threads: PgEmailThreadRepository::new(pool.clone()),
            email_attachments: PgEmailAttachmentRepository::new(pool.clone()),
            audit_service: PgAuditService::new(crm_db::postgres::PgAuditRepository::new(
                pool.clone(),
            )),
            campaigns: PgCampaignRepository::new(pool.clone()),
            prospect_pools: PgProspectPoolRepository::new(pool.clone()),
            opt_outs: PgOptOutRepository::new(pool.clone()),
            conversions: PgConversionRepository::new(pool.clone()),
            contacts: PgContactRepository::new(pool.clone()),
            agent_runs: PgAgentRunRepository::new(pool.clone()),
            tool_calls: PgToolCallRepository::new(pool.clone()),
            handoffs: PgHandoffRepository::new(pool.clone()),
            roles: PgRoleMembership::new(pool.clone()),
            workflows: PgWorkflowRepository::new(pool.clone()),
            workflow_nodes: PgWorkflowNodeRepository::new(pool.clone()),
            workflow_edges: PgWorkflowEdgeRepository::new(pool.clone()),
            workflow_triggers: PgTriggerRepository::new(pool.clone()),
            workflow_versions: PgWorkflowVersionRepository::new(pool.clone()),
            forms: PgFormRepository::new(pool.clone()),
            form_fields: PgFormFieldRepository::new(pool.clone()),
            form_submissions: PgFormSubmissionRepository::new(pool.clone()),
            feed_items: PgFeedItemRepository::new(pool.clone()),
            invoices: PgInvoiceRepository::new(pool.clone()),
            payments: PgPaymentRepository::new(pool.clone()),
            contracts: PgContractRepository::new(pool.clone()),
            signing_templates: PgSigningTemplateRepository::new(pool.clone()),
            calls: PgCallRepository::new(pool.clone()),
            voice_provider: crm_core::voice::StubVoiceProvider::new(),
            dashboards: PgDashboardRepository::new(pool.clone()),
            reports: PgReportRepository::new(pool.clone()),
            invitations: PgStaffInvitationRepository::new(pool.clone()),
            role_assignments: PgRoleAssignmentRepository::new(pool.clone()),
            settings: PgAgencySettingsRepository::new(pool.clone()),
            maintenance: PgMaintenanceRepository::new(pool.clone()),
            staff: PgStaffRepository::new(pool.clone()),
            backups: PgBackupRepository::new(pool.clone()),
            exports: PgExportRepository::new(pool.clone()),
            upgrades: PgUpgradeRepository::new(pool.clone()),
            installation: PgInstallationRepository::new(pool.clone()),
            branding: crm_db::branding::PgBrandingRepository::new(pool.clone()),
            pool: pool.clone(),
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

/// Optional peer address that never rejects: `ConnectInfo` is only
/// present when the server runs with
/// `into_make_service_with_connect_info` (absent in `oneshot` tests),
/// and `Option<ConnectInfo<_>>` is not a valid axum extractor, so this
/// wrapper reads the extension directly.
pub(crate) struct MaybeConnectInfo(Option<SocketAddr>);

impl<S> axum::extract::FromRequestParts<S> for MaybeConnectInfo
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0),
        ))
    }
}

/// Derives the rate-limit key for the caller.
///
/// Prefers the TCP peer address (available when the server runs with
/// `into_make_service_with_connect_info`); behind the documented
/// trusted reverse proxy, honors the leftmost `X-Forwarded-For` entry.
/// Falls back to a single shared bucket when neither is present (e.g.
/// in-process tests), which is the conservative choice.
pub(crate) fn client_ip_key(peer: Option<SocketAddr>, headers: &HeaderMap) -> String {
    if let Some(addr) = peer {
        return addr.ip().to_string();
    }
    if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = forwarded
            .split(',')
            .next()
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            return format!("xff:{first}");
        }
    }
    "unknown".to_owned()
}

/// Renders a `429` `problem+json` with a `Retry-After` hint in seconds.
pub(crate) fn rate_limited(instance: &str, detail: &str, retry_after_secs: u64) -> Response {
    let mut response = problem_response(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many requests",
        detail.to_owned(),
        instance.to_owned(),
    );
    let value = retry_after_secs
        .to_string()
        .parse()
        .expect("retry-after seconds are a valid header value");
    response
        .headers_mut()
        .insert(axum::http::header::RETRY_AFTER, value);
    response
}

/// Flood mitigation for mutating API endpoints (task 17-PERF-02).
///
/// Applied as middleware to the whole `/api` router, *outside* the auth
/// middleware so floods are rejected before session lookups. Every
/// `POST`/`PUT`/`PATCH`/`DELETE` consumes from the per-client-IP write
/// bucket ([`WriteRateLimiter`], same key derivation as the login
/// limiter); reads pass through untouched. A tripped bucket is `429`
/// `problem+json` with a `Retry-After` hint and records an
/// `auth.rate_limit_tripped` security event with a fingerprinted (never
/// raw) bucket key, mirroring the login limiter's audit behavior.
pub(crate) async fn write_rate_limit_middleware(
    State(api_auth): State<ApiAuth>,
    connect_info: MaybeConnectInfo,
    request: Request,
    next: Next,
) -> Response {
    const WRITE_METHODS: [Method; 4] = [Method::POST, Method::PUT, Method::PATCH, Method::DELETE];
    if !WRITE_METHODS.contains(request.method()) {
        return next.run(request).await;
    }
    let (instance, correlation_id) = request_id(request.headers());
    let ip_key = client_ip_key(connect_info.0, request.headers());
    if let Err(retry_after) = api_auth.write_limiter.check(&ip_key) {
        let sys_ctx = crate::audit::system_action_context(correlation_id);
        crate::audit::record_security_event(
            &api_auth.audit_log,
            &sys_ctx,
            &SecurityEvent::rate_limit_tripped(RateLimitBucket::Ip, &ip_key),
        )
        .await;
        return rate_limited(
            &instance,
            "write rate limit exceeded; try again later",
            retry_after,
        );
    }
    next.run(request).await
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
///
/// Every attempt consumes from the per-IP and per-email rate-limit
/// buckets; an exhausted bucket is `429` `problem+json` with a
/// `Retry-After` hint. The limit applies before credential verification
/// so attackers cannot probe around it.
pub async fn login<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    body: Result<Json<LoginBody>, JsonRejection>,
) -> Response
where
    R: ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: crate::contacts::IdempotencyStore,
{
    let (instance, correlation_id) = request_id(&headers);
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
    // Brute-force mitigation runs before any credential work: the email
    // is normalized the same way the service layer normalizes it so
    // `Op@Example.com` and `op@example.com` share one bucket.
    let email_key = body.email.trim().to_lowercase();
    let ip_key = client_ip_key(connect_info.0, &headers);
    if let Err((bucket, retry_after)) = api_auth.login_limiter.check(&ip_key, &email_key) {
        // The rate-limit trip is itself a security event: record it with
        // a fingerprinted bucket key (never the raw IP or email), then
        // emit the login-velocity anomaly signal for the email bucket.
        let sys_ctx = crate::audit::system_action_context(correlation_id);
        let bucket_key = match bucket {
            RateLimitBucket::Ip => &ip_key,
            RateLimitBucket::Email => &email_key,
        };
        crate::audit::record_security_event(
            &api_auth.audit_log,
            &sys_ctx,
            &SecurityEvent::rate_limit_tripped(bucket, bucket_key),
        )
        .await;
        if bucket == RateLimitBucket::Email {
            api_auth
                .anomaly_sink
                .emit(AnomalySignal::LoginVelocity {
                    email_fingerprint: fingerprint(&email_key),
                    attempts: api_auth.login_limiter.config().max_attempts_per_email,
                    window_secs: api_auth.login_limiter.config().email_window.as_secs(),
                })
                .await;
        }
        api_auth
            .tracer
            .emit(TraceEvent::now(
                "auth.login_attempt",
                correlation_id,
                None,
                vec![("result".to_string(), "rate_limited".to_string())],
            ))
            .await;
        return rate_limited(
            &instance,
            "login rate limit exceeded; try again later",
            retry_after,
        );
    }
    if body.email.trim().is_empty() || body.password.is_empty() {
        return unauthorized(&instance);
    }
    match api_auth
        .authentication
        .authenticate(&body.email, body.password.as_bytes())
        .await
    {
        Ok(issued) => {
            // Record the successful login against the staff actor, then
            // trace it. The audit write never fails the login (see
            // `record_security_event`).
            let staff_id = issued.session.staff_id;
            let ctx =
                crate::audit::staff_action_context(ActorId::from(staff_id), None, correlation_id);
            crate::audit::record_security_event(
                &api_auth.audit_log,
                &ctx,
                &SecurityEvent::LoginSucceeded { staff_id },
            )
            .await;
            api_auth
                .tracer
                .emit(TraceEvent::now(
                    "auth.login_attempt",
                    correlation_id,
                    Some(ActorId::from(staff_id)),
                    vec![("result".to_string(), "allow".to_string())],
                ))
                .await;
            // The one boundary where the token leaves the server, in UUID
            // form. It is never logged.
            let token = issued.token.as_uuid().to_string();
            (StatusCode::OK, Json(LoginResponse { token })).into_response()
        }
        Err(DomainError::Unauthorized) => {
            // Failed login: record with a fingerprinted email (never the
            // raw address). Suspended accounts are indistinguishable from
            // wrong passwords here by design.
            let sys_ctx = crate::audit::system_action_context(correlation_id);
            crate::audit::record_security_event(
                &api_auth.audit_log,
                &sys_ctx,
                &SecurityEvent::login_failed(&body.email),
            )
            .await;
            api_auth
                .tracer
                .emit(TraceEvent::now(
                    "auth.login_attempt",
                    correlation_id,
                    None,
                    vec![("result".to_string(), "deny".to_string())],
                ))
                .await;
            unauthorized(&instance)
        }
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
    let (instance, correlation_id) = request_id(&headers);
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
    // Resolve the session so the logout is attributed to the staff
    // member; an unresolvable token still revokes idempotently (204)
    // without an audit event, since there is no actor to attribute.
    if let Ok(validated) = api_auth.authentication.validate_token(&token).await {
        let staff_id = validated.staff.staff_id;
        let ctx = crate::audit::staff_action_context(ActorId::from(staff_id), None, correlation_id);
        crate::audit::record_security_event(
            &api_auth.audit_log,
            &ctx,
            &SecurityEvent::Logout {
                staff_id,
                session_id: validated.session.session_id,
            },
        )
        .await;
        api_auth
            .tracer
            .emit(TraceEvent::now(
                "auth.logout",
                correlation_id,
                Some(ActorId::from(staff_id)),
                vec![("result".to_string(), "allow".to_string())],
            ))
            .await;
    }
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
        Err(DomainError::Forbidden(_)) => {
            // Probing signal: a valid token without a covering grant.
            // Resolve the actor for the denial counter; if resolution
            // fails the 403 still stands, just untracked.
            if let Ok(validated) = api_auth
                .authentication
                .validate_token(&req_auth.token)
                .await
            {
                let actor = ActorId::from(validated.staff.staff_id);
                if let Some(signal) = api_auth.denial_tracker.note_denial(actor) {
                    api_auth.anomaly_sink.emit(signal).await;
                }
            }
            api_auth
                .tracer
                .emit(TraceEvent::now(
                    "authz.denied",
                    correlation_id,
                    None,
                    vec![
                        ("resource".to_string(), format!("{:?}", action.resource)),
                        ("operation".to_string(), format!("{:?}", action.operation)),
                    ],
                ))
                .await;
            Err(Box::new(forbidden(&instance)))
        }
        Err(error) => Err(Box::new(crate::contacts::domain_error_response(
            error, instance,
        ))),
    }
}
