// SPDX-License-Identifier: GPL-3.0-only
//! Branding endpoints (task UI2-03).
//!
//! * `GET /api/v1/branding` — reads the agency's branding theme
//!   (`200`). **Public**: the login page needs the theme before
//!   authentication, so this route lives outside the bearer-auth
//!   middleware (like the email ingest webhook). Returns the empty
//!   theme when branding was never set.
//! * `PUT /api/v1/branding` — stores a new branding version (`200`).
//!   Requires `AgencySettings:Manage` at agency scope, a valid
//!   `Idempotency-Key` UUID header, and records a `branding.updated`
//!   audit event.
//!
//! ## Authorization
//!
//! Branding is agency-wide by construction: the write endpoint
//! authorizes at agency scope (no client-profile scope is ever
//! accepted) via [`Resource::AgencySettings`] with
//! [`Operation::Manage`].

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crm_domain::branding::{BrandingRepository, BrandingTheme, BrandingVersion, BRANDING_UPDATED};
use crm_domain::ids::AgencyId;
use crm_domain::security::{Operation, Resource};
use crm_domain::services::AuditService;

use crate::admin::{authorize_admin, not_configured};
use crate::auth::RequestAuth;
use crate::contacts::{
    domain_error_response, idempotency_key, request_id, AppState, IdempotencyStore,
};
use crm_core::memory::ContactServiceImpl;

/// `PUT /api/v1/branding` request body: the theme fields.
///
/// All fields optional; validated by [`BrandingTheme::new`].
#[derive(Debug, Clone, Deserialize)]
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
    let stored = match api_auth.branding.get(&agency_id).await {
        Ok(stored) => stored,
        Err(error) => return domain_error_response(error, instance),
    };
    let response = match stored {
        Some(v) => BrandingResponse::from(v),
        None => BrandingResponse {
            version: 0,
            theme: BrandingTheme::empty(),
        },
    };
    (StatusCode::OK, Json(response)).into_response()
}

/// `PUT /api/v1/branding` — stores a new branding version (`200`).
///
/// Requires `AgencySettings:Manage` at agency scope and a valid
/// `Idempotency-Key` UUID header. Records a `branding.updated` audit
/// event.
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
    let _ = key; // Reserved for idempotency-store integration (follow-on).

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

    // Validate through the domain constructor (manual Deserialize would
    // also work; the explicit constructor keeps the error mapping clear).
    let theme = match BrandingTheme::new(
        body.logo_url,
        body.primary,
        body.primary_foreground,
        body.ring,
        body.chart_1,
        body.radius,
        body.font_family,
    ) {
        Ok(theme) => theme,
        Err(error) => return domain_error_response(error, instance),
    };

    let agency_id = demo_agency_id();
    let stored = match api_auth.branding.upsert(&agency_id, &theme).await {
        Ok(stored) => stored,
        Err(error) => return domain_error_response(error, instance),
    };

    // Record the audit event (best-effort: branding is stored even if
    // the audit write fails, matching the admin settings pattern).
    let _ = api_auth
        .audit_service
        .record_action(
            &ctx,
            BRANDING_UPDATED,
            "branding",
            &agency_id.to_string(),
            crm_domain::audit::PermissionDecision::Allow,
            crm_domain::audit::ActionResult::Success,
            None,
            Some(&stored.version.to_string()),
        )
        .await;

    (StatusCode::OK, Json(BrandingResponse::from(stored))).into_response()
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
