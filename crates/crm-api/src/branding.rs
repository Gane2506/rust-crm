// SPDX-License-Identifier: GPL-3.0-only
//! Branding endpoints (task UI2-03, idempotency in HARD-02).
//!
//! * `GET /api/v1/branding` — reads the agency's branding theme
//!   (`200`). **Public**: the login page needs the theme before
//!   authentication, so this route lives outside the bearer-auth
//!   middleware (like the email ingest webhook). Returns the empty
//!   theme when branding was never set.
//! * `PUT /api/v1/branding` — stores a new branding version (`200`).
//!   Requires `AgencySettings:Manage` at agency scope, a valid
//!   `Idempotency-Key` UUID header, and records a `branding.updated`
//!   audit event. Replays are idempotent: a repeated request with the
//!   same key and body returns the stored response without bumping the
//!   version again (atomic claim, HARD-01 pattern).
//!
//! ## Authorization
//!
//! Branding is agency-wide by construction: the write endpoint
//! authorizes at agency scope (no client-profile scope is ever
//! accepted) via [`Resource::AgencySettings`] with
//! [`Operation::Manage`].

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crm_core::branding::BrandingServiceImpl;
use crm_domain::branding::{BrandingService, BrandingTheme, BrandingVersion};
use crm_domain::ids::AgencyId;
use crm_domain::security::{Operation, Resource};

use crate::admin::{authorize_admin, not_configured};
use crate::auth::{ApiAuth, RequestAuth};
use crate::billing::record_response;
use crate::contacts::{
    claim_or_replay, domain_error_response, idempotency_key, request_id, AppState, ClaimOutcome,
    IdempotencyStore,
};
use crm_core::memory::ContactServiceImpl;

/// `PUT /api/v1/branding` request body: the theme fields.
///
/// All fields optional; validated by [`BrandingTheme::new`].
/// `Serialize` supports the idempotency fingerprint (parsed-then-
/// reserialized payload, billing pattern).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateBrandingBody {
    /// Logo image URL (https only).
    #[serde(default)]
    pub logo_url: Option<String>,
    /// Overrides `--primary`.
    #[serde(default)]
    pub primary: Option<String>,
    /// Overrides `--primary-foreground`.
    #[serde(default)]
    pub primary_foreground: Option<String>,
    /// Overrides `--ring`.
    #[serde(default)]
    pub ring: Option<String>,
    /// Overrides `--chart-1`.
    #[serde(default)]
    pub chart_1: Option<String>,
    /// Overrides `--radius`.
    #[serde(default)]
    pub radius: Option<String>,
    /// Overrides `--font-sans`.
    #[serde(default)]
    pub font_family: Option<String>,
    /// UI style id (one of the 16 style ids); empty/absent = Bento default.
    #[serde(default)]
    pub ui_style: Option<String>,
}

/// `GET /api/v1/branding` response: the current theme version.
#[derive(Debug, Clone, Serialize)]
pub struct BrandingResponse {
    /// Monotonic version (0 when never set).
    pub version: u64,
    /// The theme (empty when never set).
    pub theme: BrandingTheme,
}

impl From<BrandingVersion> for BrandingResponse {
    fn from(v: BrandingVersion) -> Self {
        Self {
            version: v.version,
            theme: v.theme,
        }
    }
}

/// Builds the branding service over the API auth's repository and
/// audit service (billing-service pattern: constructed per request
/// from `ApiAuth`, no new `ApiAuth` field).
fn branding_service(
    api_auth: &ApiAuth,
) -> BrandingServiceImpl<crm_db::branding::PgBrandingRepository, crm_db::postgres::PgAuditService> {
    BrandingServiceImpl::new(api_auth.branding.clone(), api_auth.audit_service.clone())
}

/// Fingerprints a branding write request so a reused idempotency key
/// with a *different* payload is rejected instead of replayed.
///
/// Mirrors billing's fingerprint: route + parsed-then-reserialized
/// payload, stable across insignificant whitespace differences.
fn fingerprint(route: &str, payload: &impl Serialize) -> u64 {
    let mut hasher = DefaultHasher::new();
    route.hash(&mut hasher);
    serde_json::to_string(payload)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// `GET /api/v1/branding` — reads the agency's branding theme (`200`).
///
/// Public: no authentication required. Returns the empty theme with
/// version 0 when branding was never configured.
pub async fn get_branding<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: crm_domain::services::AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&HeaderMap::new());
    let Some(api_auth) = &state.auth else {
        // No auth configured (in-memory test slice): return the empty theme.
        return (
            StatusCode::OK,
            Json(BrandingResponse {
                version: 0,
                theme: BrandingTheme::empty(),
            }),
        )
            .into_response();
    };
    // The demo agency is the single tenant; branding is agency-scoped.
    // In production the agency comes from the request context; the
    // public endpoint uses the well-known demo agency id.
    let agency_id = demo_agency_id();
    let stored = match branding_service(api_auth).get_branding(&agency_id).await {
        Ok(stored) => stored,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(BrandingResponse::from(stored))).into_response()
}

/// `PUT /api/v1/branding` — stores a new branding version (`200`).
///
/// Requires `AgencySettings:Manage` at agency scope and a valid
/// `Idempotency-Key` UUID header. Records a `branding.updated` audit
/// event (in the service). Replays are idempotent: the same key and
/// body returns the stored response without bumping the version again.
pub async fn update_branding<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<UpdateBrandingBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: crm_domain::services::AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let Some(api_auth) = &state.auth else {
        return not_configured(instance);
    };
    // Authorization FIRST: the idempotency replay/claim must stay after
    // authorization (FIX-02 lesson) so an unauthorized caller cannot
    // see stored responses or leave a stuck claim.
    let ctx = match authorize_admin(
        &state,
        auth,
        &headers,
        Resource::AgencySettings,
        Operation::Manage,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // Idempotency-Key is required for branding writes.
    let key = match idempotency_key(&headers, &instance) {
        Ok(Some(key)) => key,
        Ok(None) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "title": "Missing Idempotency-Key",
                    "detail": "the Idempotency-Key header is required for branding updates",
                    "instance": instance,
                })),
            )
                .into_response();
        }
        Err(response) => return *response,
    };

    // Validate through the domain constructor (manual Deserialize would
    // also work; the explicit constructor keeps the error mapping clear).
    // Fingerprint first: the constructor moves the body fields.
    let fp = fingerprint("PUT /api/v1/branding", &body);
    let theme = match BrandingTheme::new(
        body.logo_url,
        body.primary,
        body.primary_foreground,
        body.ring,
        body.chart_1,
        body.radius,
        body.font_family,
        body.ui_style,
    ) {
        Ok(theme) => theme,
        Err(error) => return domain_error_response(error, instance),
    };

    // Atomic claim AFTER authorization: winner proceeds to the write;
    // loser replays the winner's stored response (never a duplicate
    // version bump).
    match claim_or_replay(
        state.idempotency_store(),
        &key,
        fp,
        StatusCode::OK,
        &instance,
    )
    .await
    {
        Ok(ClaimOutcome::Replay(response)) => return response,
        Ok(ClaimOutcome::Won) => {}
        Err(response) => return *response,
    }

    let agency_id = demo_agency_id();
    let stored = match branding_service(api_auth)
        .update_branding(&agency_id, theme, &ctx)
        .await
    {
        Ok(stored) => stored,
        Err(error) => return domain_error_response(error, instance),
    };

    let response = BrandingResponse::from(stored);
    let storable = match serde_json::to_value(&response) {
        Ok(value) => value,
        // The value about to be returned must be storable for replay
        // (billing pattern): a serialization failure is a 500.
        Err(_) => {
            return crate::problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Serialization failed",
                "the stored branding could not be serialized".to_owned(),
                instance,
            );
        }
    };
    record_response(state.idempotency_store(), key, fp, storable).await;
    (StatusCode::OK, Json(response)).into_response()
}

/// The demo agency id (single tenant).
///
/// In production this comes from the request's agency scope; the
/// branding endpoints use the well-known demo agency until multi-agency
/// routing lands.
fn demo_agency_id() -> AgencyId {
    // Fixed nil UUID as the demo agency marker: the branding table is
    // keyed by agency, and the demo deployment is single-tenant.
    AgencyId::from(uuid::Uuid::nil())
}
