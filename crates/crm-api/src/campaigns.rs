// SPDX-License-Identifier: GPL-3.0-only
//! Campaign endpoints: lifecycle, prospect pools, opt-outs, conversions
//! (task 08-CAMP-02).
//!
//! * `POST /api/v1/campaigns` — creates a draft campaign (`201`).
//! * `GET /api/v1/campaigns` — lists campaigns for a profile or the agency.
//! * `POST /api/v1/campaigns/{id}/steps` — appends a step (`201`).
//! * `POST /api/v1/campaigns/{id}/schedule` — schedules a draft campaign.
//! * `POST /api/v1/campaigns/{id}/launch` — launches (bulk-sends email steps).
//! * `POST /api/v1/campaigns/{id}/pause|resume|complete|cancel` — lifecycle.
//! * `POST /api/v1/prospect-pools` — creates a pool (`201`).
//! * `GET /api/v1/prospect-pools` — lists pools for a profile.
//! * `POST /api/v1/prospect-pools/{id}/members` — adds a member.
//! * `DELETE /api/v1/prospect-pools/{id}/members/{contact_id}` — removes a member.
//! * `POST /api/v1/opt-outs` — records an opt-out (`201`, idempotent `200`).
//! * `POST /api/v1/conversions` — records a conversion (`201`).
//!
//! ## Authorization
//!
//! All endpoints use the `Campaign` grants within the campaign's owner
//! scope ([`Resource::Campaign`](crm_domain::security::Resource::Campaign)):
//! profile-scoped for client-profile campaigns, agency-scoped for
//! agency-owned campaigns. Opt-outs and conversions are profile-scoped.

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_core::campaign::CampaignServiceImpl;
use crm_core::email::{EmailServiceImpl, StubEmailProvider};
use crm_db::campaign::{
    PgCampaignRepository, PgConversionRepository, PgOptOutRepository, PgProspectPoolRepository,
};
use crm_db::email::{
    PgEmailAttachmentRepository, PgEmailMessageRepository, PgEmailThreadRepository,
};
use crm_db::postgres::{PgAuditService, PgContactRepository};
use crm_domain::campaign::{
    AttributionModel, Campaign, CampaignStep, Channel, OptOutChannel, OwnerContext, StepKind,
};
use crm_domain::ids::{CampaignId, ClientProfileId, ContactId, ProspectPoolId};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::repositories::{CampaignRepository, OptOutRepository, ProspectPoolRepository};
use crm_domain::services::{
    AuditService, CampaignService, NewCampaign, NewCampaignStep, NewConversion,
};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/campaigns`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCampaignBody {
    /// Human-readable campaign name (non-blank).
    pub name: String,
    /// Owner: `"agency"` or `"client_profile"`.
    pub owner_kind: String,
    /// Owning client profile (required when `owner_kind` is `"client_profile"`).
    pub client_profile_id: Option<Uuid>,
    /// Outreach channel: `"email"`, `"call"`, or `"mixed"`.
    pub channel: String,
    /// Identifier of the prospect pool the campaign draws from.
    pub prospect_pool_id: String,
    /// Cron expression for the delivery schedule.
    pub schedule: String,
    /// Suppression rules reference.
    #[serde(default)]
    pub suppression_policy: String,
    /// How conversions are attributed.
    #[serde(default)]
    pub conversion_rules: String,
    /// Attribution window in days.
    #[serde(default = "default_attribution_window")]
    pub attribution_window_days: u32,
}

fn default_attribution_window() -> u32 {
    30
}

/// Query of `GET /api/v1/campaigns` and `GET /api/v1/prospect-pools`.
#[derive(Debug, Deserialize)]
pub struct ListCampaignsQuery {
    /// Owning client profile. Absent means agency-owned campaigns.
    pub client_profile_id: Option<Uuid>,
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
}

/// Body of `POST /api/v1/campaigns/{id}/steps`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddStepBody {
    /// What the step does: `"email"`, `"call"`, or `"wait"`.
    pub kind: String,
    /// Position in the sequence (0-based).
    pub position: u32,
    /// Template (email) or script (call) reference.
    pub template_id: Option<String>,
    /// Wait duration in seconds (wait steps only).
    pub wait_duration_secs: Option<u64>,
}

/// Body of `POST /api/v1/campaigns/{id}/schedule`.
#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleCampaignBody {
    /// One-shot launch time (must be in the future).
    pub schedule_for: DateTime<Utc>,
}

/// Body of `POST /api/v1/prospect-pools`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePoolBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Human-readable pool name (non-blank).
    pub name: String,
}

/// Body of `POST /api/v1/prospect-pools/{id}/members`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddMemberBody {
    /// Contact to add to the pool.
    pub contact_id: Uuid,
}

/// Body of `POST /api/v1/opt-outs`.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordOptOutBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// The contact who opted out.
    pub contact_id: Uuid,
    /// Channel: `"email"`, `"sms"`, `"call"`, or `"all"`.
    pub channel: String,
    /// Optional reason.
    pub reason: Option<String>,
}

/// Body of `POST /api/v1/conversions`.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordConversionBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// The contact who converted.
    pub contact_id: Uuid,
    /// Candidate touchpoint campaign ids for attribution.
    #[serde(default)]
    pub touchpoint_campaign_ids: Vec<Uuid>,
    /// Attribution model: `"first_touch"` or `"last_touch"`.
    #[serde(default = "default_attribution_model")]
    pub attribution_model: String,
    /// Monetary value in cents, if known.
    pub value_cents: Option<u64>,
    /// When the conversion happened.
    pub occurred_at: DateTime<Utc>,
}

fn default_attribution_model() -> String {
    "last_touch".to_owned()
}

/// `GET /api/v1/campaigns/{id}` response: the campaign plus its steps.
#[derive(Debug, Serialize)]
struct CampaignDetailResponse {
    campaign: Campaign,
    steps: Vec<CampaignStep>,
}

/// Parses an owner context from the request body.
fn parse_owner_context(
    owner_kind: &str,
    client_profile_id: Option<Uuid>,
) -> Result<(OwnerContext, Option<ClientProfileId>), String> {
    match owner_kind {
        "agency" => Ok((OwnerContext::Agency, None)),
        "client_profile" => {
            let profile_id = client_profile_id.ok_or_else(|| {
                "client_profile_id is required for client_profile owner".to_owned()
            })?;
            Ok((
                OwnerContext::ClientProfile(ClientProfileId::from(profile_id)),
                Some(ClientProfileId::from(profile_id)),
            ))
        }
        other => Err(format!(
            "unknown owner_kind '{other}' (expected 'agency' or 'client_profile')"
        )),
    }
}

/// Parses a channel from its string form.
fn parse_channel(raw: &str) -> Result<Channel, String> {
    match raw {
        "email" => Ok(Channel::Email),
        "call" => Ok(Channel::Call),
        "mixed" => Ok(Channel::Mixed),
        other => Err(format!("unknown channel '{other}'")),
    }
}

/// Parses a step kind from its string form.
fn parse_step_kind(raw: &str) -> Result<StepKind, String> {
    match raw {
        "email" => Ok(StepKind::Email),
        "call" => Ok(StepKind::Call),
        "wait" => Ok(StepKind::Wait),
        other => Err(format!("unknown step kind '{other}'")),
    }
}

/// Parses an opt-out channel from its string form.
fn parse_opt_out_channel(raw: &str) -> Result<OptOutChannel, String> {
    match raw {
        "email" => Ok(OptOutChannel::Email),
        "sms" => Ok(OptOutChannel::Sms),
        "call" => Ok(OptOutChannel::Call),
        "all" => Ok(OptOutChannel::All),
        other => Err(format!("unknown opt-out channel '{other}'")),
    }
}

/// Parses an attribution model from its string form.
fn parse_attribution_model(raw: &str) -> Result<AttributionModel, String> {
    match raw {
        "first_touch" => Ok(AttributionModel::FirstTouch),
        "last_touch" => Ok(AttributionModel::LastTouch),
        other => Err(format!("unknown attribution model '{other}'")),
    }
}

/// Builds the campaign service from the API auth components.
#[allow(clippy::type_complexity)]
fn campaign_service(
    api_auth: &crate::auth::ApiAuth,
) -> CampaignServiceImpl<
    PgCampaignRepository,
    PgProspectPoolRepository,
    PgOptOutRepository,
    PgContactRepository,
    PgConversionRepository,
    EmailServiceImpl<
        PgEmailMessageRepository,
        PgEmailThreadRepository,
        PgEmailAttachmentRepository,
        StubEmailProvider,
        PgAuditService,
    >,
    PgAuditService,
> {
    let email_service = EmailServiceImpl::new(
        api_auth.email_messages.clone(),
        api_auth.email_threads.clone(),
        api_auth.email_attachments.clone(),
        StubEmailProvider::new(),
        api_auth.audit_service.clone(),
    );
    CampaignServiceImpl::new(
        api_auth.campaigns.clone(),
        api_auth.prospect_pools.clone(),
        api_auth.opt_outs.clone(),
        api_auth.contacts.clone(),
        api_auth.conversions.clone(),
        email_service,
        api_auth.audit_service.clone(),
    )
}

/// `POST /api/v1/campaigns` — creates a draft campaign.
///
/// Returns `201` with the campaign. Requires the `campaign/create`
/// grant in the campaign's owner scope (`403` without it).
pub async fn create_campaign<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<CreateCampaignBody>, axum::extract::rejection::JsonRejection>,
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
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let (owner_context, profile_id) =
        match parse_owner_context(&body.owner_kind, body.client_profile_id) {
            Ok(parsed) => parsed,
            Err(detail) => {
                return problem_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Invalid owner",
                    detail,
                    instance,
                )
            }
        };
    let channel = match parse_channel(&body.channel) {
        Ok(channel) => channel,
        Err(detail) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid channel",
                detail,
                instance,
            )
        }
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, profile_id).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = campaign_service(api_auth);
    let input = NewCampaign {
        name: body.name,
        owner_context,
        channel,
        prospect_pool_id: body.prospect_pool_id,
        schedule: body.schedule,
        suppression_policy: body.suppression_policy,
        conversion_rules: body.conversion_rules,
        attribution_window_days: body.attribution_window_days,
        idempotency_key: Uuid::new_v4().to_string(),
    };
    let campaign = match service.create_campaign(input, &ctx).await {
        Ok(campaign) => campaign,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(campaign)).into_response()
}

/// `GET /api/v1/campaigns` — lists campaigns.
///
/// `client_profile_id` selects profile-owned campaigns; absent selects
/// agency-owned campaigns. Requires the `campaign/read` grant in the
/// selected scope.
pub async fn list_campaigns<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListCampaignsQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
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
    let profile_id = query.client_profile_id.map(ClientProfileId::from);
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Read,
    };
    let _ctx = match resolve_context(&state, auth, &headers, &action, profile_id).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let limit = query.limit.unwrap_or(20).min(100) as u64;
    let campaigns = match profile_id {
        Some(profile_id) => api_auth.campaigns.list_by_profile(profile_id, limit).await,
        None => {
            api_auth
                .campaigns
                .list_for_owner(&OwnerContext::Agency, limit)
                .await
        }
    };
    let campaigns = match campaigns {
        Ok(campaigns) => campaigns,
        Err(error) => return domain_error_response(error, instance),
    };
    // The total is the page length (matches the threads endpoint's
    // documented behavior).
    let total = campaigns.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "campaigns": campaigns, "total": total })),
    )
        .into_response()
}

/// Extracts a campaign id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_campaign_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<CampaignId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(CampaignId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid campaign id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// `POST /api/v1/campaigns/{id}/steps` — appends a step to a draft campaign.
///
/// Returns `201` with the step. Requires the `campaign/update` grant.
pub async fn add_campaign_step<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<AddStepBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
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
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    // Fetch first to learn the owner scope, then authorize — the
    // established fetch-then-authorize pattern.
    let campaign = match api_auth.campaigns.get(campaign_id).await {
        Ok(campaign) => campaign,
        Err(error) => return domain_error_response(error, instance),
    };
    let profile_id = match campaign.owner_context {
        OwnerContext::Agency => None,
        OwnerContext::ClientProfile(profile_id) => Some(profile_id),
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Update,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, profile_id).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let kind = match parse_step_kind(&body.kind) {
        Ok(kind) => kind,
        Err(detail) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid step kind",
                detail,
                instance,
            )
        }
    };
    let service = campaign_service(api_auth);
    let input = NewCampaignStep {
        campaign_id,
        kind,
        position: body.position,
        template_id: body.template_id,
        wait_duration_secs: body.wait_duration_secs,
    };
    let step = match service.add_step(input, &ctx).await {
        Ok(step) => step,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(step)).into_response()
}

/// The lifecycle transition to run inside [`campaign_transition`].
#[derive(Debug, Clone, Copy)]
enum CampaignTransition {
    Schedule(DateTime<Utc>),
    Launch,
    Pause,
    Resume,
    Complete,
    Cancel,
}

/// Runs a campaign lifecycle transition (`schedule`, `launch`, `pause`,
/// `resume`, `complete`, `cancel`) with fetch-then-authorize.
#[allow(clippy::too_many_arguments)]
async fn campaign_transition<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    campaign_id: CampaignId,
    transition: CampaignTransition,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(headers);
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let campaign = match api_auth.campaigns.get(campaign_id).await {
        Ok(campaign) => campaign,
        Err(error) => return domain_error_response(error, instance),
    };
    let profile_id = match campaign.owner_context {
        OwnerContext::Agency => None,
        OwnerContext::ClientProfile(profile_id) => Some(profile_id),
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Update,
    };
    let ctx = match resolve_context(state, auth, headers, &action, profile_id).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = campaign_service(api_auth);
    let campaign = match match transition {
        CampaignTransition::Schedule(schedule_for) => {
            service.schedule(campaign_id, schedule_for, &ctx).await
        }
        CampaignTransition::Launch => service.launch(campaign_id, &ctx).await,
        CampaignTransition::Pause => service.pause(campaign_id, &ctx).await,
        CampaignTransition::Resume => service.resume(campaign_id, &ctx).await,
        CampaignTransition::Complete => service.complete(campaign_id, &ctx).await,
        CampaignTransition::Cancel => service.cancel(campaign_id, &ctx).await,
    } {
        Ok(campaign) => campaign,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(campaign)).into_response()
}

/// `POST /api/v1/campaigns/{id}/schedule` — schedules a draft campaign.
pub async fn schedule_campaign<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<ScheduleCampaignBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
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
    let schedule_for = body.schedule_for;
    campaign_transition(
        &state,
        auth,
        &headers,
        campaign_id,
        CampaignTransition::Schedule(schedule_for),
    )
    .await
}

/// `POST /api/v1/campaigns/{id}/launch` — launches a campaign.
///
/// Executes email steps as bulk sends, skipping opted-out contacts.
pub async fn launch_campaign<R, A, I>(
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
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    campaign_transition(
        &state,
        auth,
        &headers,
        campaign_id,
        CampaignTransition::Launch,
    )
    .await
}

/// `POST /api/v1/campaigns/{id}/pause` — pauses a running campaign.
pub async fn pause_campaign<R, A, I>(
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
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    campaign_transition(
        &state,
        auth,
        &headers,
        campaign_id,
        CampaignTransition::Pause,
    )
    .await
}

/// `POST /api/v1/campaigns/{id}/resume` — resumes a paused campaign.
pub async fn resume_campaign<R, A, I>(
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
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    campaign_transition(
        &state,
        auth,
        &headers,
        campaign_id,
        CampaignTransition::Resume,
    )
    .await
}

/// `POST /api/v1/campaigns/{id}/complete` — completes a campaign.
pub async fn complete_campaign<R, A, I>(
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
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    campaign_transition(
        &state,
        auth,
        &headers,
        campaign_id,
        CampaignTransition::Complete,
    )
    .await
}

/// `POST /api/v1/campaigns/{id}/cancel` — cancels a campaign.
pub async fn cancel_campaign<R, A, I>(
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
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    campaign_transition(
        &state,
        auth,
        &headers,
        campaign_id,
        CampaignTransition::Cancel,
    )
    .await
}

/// `GET /api/v1/campaigns/{id}` — fetches a campaign with its steps.
pub async fn get_campaign<R, A, I>(
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
    let campaign_id = match path_campaign_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let campaign = match api_auth.campaigns.get(campaign_id).await {
        Ok(campaign) => campaign,
        Err(error) => return domain_error_response(error, instance),
    };
    let profile_id = match campaign.owner_context {
        OwnerContext::Agency => None,
        OwnerContext::ClientProfile(profile_id) => Some(profile_id),
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, profile_id).await {
        return *response;
    }
    let steps = match api_auth.campaigns.list_steps(campaign_id).await {
        Ok(steps) => steps,
        Err(error) => return domain_error_response(error, instance),
    };
    (
        StatusCode::OK,
        Json(CampaignDetailResponse { campaign, steps }),
    )
        .into_response()
}

/// `POST /api/v1/prospect-pools` — creates a prospect pool.
///
/// Returns `201` with the pool. Requires the `campaign/create` grant
/// in the pool's profile scope.
pub async fn create_pool<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<CreatePoolBody>, axum::extract::rejection::JsonRejection>,
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
        resource: Resource::Campaign,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = campaign_service(api_auth);
    let pool = match service.create_pool(profile_id, body.name, &ctx).await {
        Ok(pool) => pool,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(pool)).into_response()
}

/// `GET /api/v1/prospect-pools` — lists pools for a profile.
pub async fn list_pools<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListCampaignsQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
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
    let profile_id = match query.client_profile_id {
        Some(id) => ClientProfileId::from(id),
        None => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Missing client_profile_id",
                "prospect pools are profile-scoped; client_profile_id is required".to_owned(),
                instance,
            )
        }
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, Some(profile_id)).await
    {
        return *response;
    }
    let limit = query.limit.unwrap_or(20).min(100) as u64;
    let pools = match api_auth
        .prospect_pools
        .list_by_profile(profile_id, limit)
        .await
    {
        Ok(pools) => pools,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = pools.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "pools": pools, "total": total })),
    )
        .into_response()
}

/// `POST /api/v1/prospect-pools/{id}/members` — adds a contact to a pool.
pub async fn add_pool_member<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<AddMemberBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let pool_id = match path {
        Ok(Path(id)) => ProspectPoolId::from(id),
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid pool id",
                rejection.body_text(),
                instance,
            )
        }
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
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    // Fetch first to learn the profile scope, then authorize.
    let pool = match api_auth.prospect_pools.get(pool_id).await {
        Ok(pool) => pool,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Update,
    };
    if let Err(response) = resolve_context(
        &state,
        auth,
        &headers,
        &action,
        Some(pool.client_profile_id),
    )
    .await
    {
        return *response;
    }
    let pool = match api_auth
        .prospect_pools
        .add_member(pool_id, ContactId::from(body.contact_id))
        .await
    {
        Ok(pool) => pool,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(pool)).into_response()
}

/// `DELETE /api/v1/prospect-pools/{id}/members/{contact_id}` — removes a member.
pub async fn remove_pool_member<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let (pool_id, contact_id) = match path {
        Ok(Path((pool_id, contact_id))) => {
            (ProspectPoolId::from(pool_id), ContactId::from(contact_id))
        }
        Err(rejection) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid path",
                rejection.body_text(),
                instance,
            )
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
    let pool = match api_auth.prospect_pools.get(pool_id).await {
        Ok(pool) => pool,
        Err(error) => return domain_error_response(error, instance),
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Update,
    };
    if let Err(response) = resolve_context(
        &state,
        auth,
        &headers,
        &action,
        Some(pool.client_profile_id),
    )
    .await
    {
        return *response;
    }
    let pool = match api_auth
        .prospect_pools
        .remove_member(pool_id, contact_id)
        .await
    {
        Ok(pool) => pool,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(pool)).into_response()
}

/// `POST /api/v1/opt-outs` — records a contact's opt-out.
///
/// Idempotent: re-recording the same opt-out returns `200` with the
/// existing record. Requires the `campaign/update` grant in the
/// profile scope.
pub async fn record_opt_out<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<RecordOptOutBody>, axum::extract::rejection::JsonRejection>,
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
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let profile_id = ClientProfileId::from(body.client_profile_id);
    let channel = match parse_opt_out_channel(&body.channel) {
        Ok(channel) => channel,
        Err(detail) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid channel",
                detail,
                instance,
            )
        }
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Update,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = campaign_service(api_auth);
    let contact_id = ContactId::from(body.contact_id);
    // Idempotency: if already opted out on this channel, return the
    // existing record with 200.
    if let Ok(true) = api_auth.opt_outs.is_opted_out(contact_id, channel).await {
        if let Ok(opt_outs) = api_auth.opt_outs.list_by_contact(contact_id).await {
            if let Some(existing) = opt_outs.into_iter().find(|o| o.covers(channel)) {
                return (StatusCode::OK, Json(existing)).into_response();
            }
        }
    }
    let opt_out = match service
        .record_opt_out(profile_id, contact_id, channel, body.reason, &ctx)
        .await
    {
        Ok(opt_out) => opt_out,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(opt_out)).into_response()
}

/// `POST /api/v1/conversions` — records a conversion with attribution.
///
/// Returns `201` with the conversion. Requires the `campaign/create`
/// grant in the profile scope.
pub async fn record_conversion<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<RecordConversionBody>, axum::extract::rejection::JsonRejection>,
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
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let profile_id = ClientProfileId::from(body.client_profile_id);
    let attribution_model = match parse_attribution_model(&body.attribution_model) {
        Ok(model) => model,
        Err(detail) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid attribution model",
                detail,
                instance,
            )
        }
    };
    let action = Action {
        resource: Resource::Campaign,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = campaign_service(api_auth);
    // Build touchpoints from the candidate campaign ids. The occurred_at
    // is the conversion time (the service applies the window); a real
    // implementation would look up actual touch timestamps.
    let touchpoints: Vec<crm_domain::campaign::Touchpoint> = body
        .touchpoint_campaign_ids
        .into_iter()
        .map(|id| crm_domain::campaign::Touchpoint {
            campaign_id: CampaignId::from(id),
            step_id: None,
            occurred_at: body.occurred_at,
        })
        .collect();
    let input = NewConversion {
        client_profile_id: profile_id,
        contact_id: ContactId::from(body.contact_id),
        touchpoints,
        attribution_model,
        value_cents: body.value_cents,
        occurred_at: body.occurred_at,
    };
    let conversion = match service.record_conversion(input, &ctx).await {
        Ok(conversion) => conversion,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(conversion)).into_response()
}
