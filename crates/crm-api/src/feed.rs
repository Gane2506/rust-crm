// SPDX-License-Identifier: GPL-3.0-only
//! Feed endpoints: the per-client-profile activity timeline (task 12-FEED-02).
//!
//! * `GET /api/v1/feed?client_profile_id=…` — lists feed items,
//!   pinned first then newest-first (`200`).
//! * `POST /api/v1/feed/{id}/read` — marks an item read (idempotent).
//! * `POST /api/v1/feed/{id}/pin` — pins an item to the top of reads.
//! * `POST /api/v1/feed/{id}/hide` — hides an item (sticky).
//!
//! ## Authorization
//!
//! All endpoints authorize against
//! [`Resource::FeedItem`](crm_domain::security::Resource::FeedItem),
//! profile-scoped. Fetch-then-authorize: a missing item surfaces as
//! `404` before any grant check.

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crm_core::feed::FeedServiceImpl;
use crm_db::feed::PgFeedItemRepository;
use crm_domain::feed::{FeedItem, FeedQuery};
use crm_domain::ids::{ClientProfileId, FeedItemId};
use crm_domain::repositories::FeedItemRepository;
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{AuditService, FeedService};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Query of `GET /api/v1/feed`.
#[derive(Debug, Deserialize)]
pub struct ListFeedQuery {
    /// Owning client profile (required: the feed is profile-scoped).
    pub client_profile_id: Uuid,
    /// Max items returned (default 20; validated and capped by
    /// `FeedQuery::new` — an out-of-range value is a `422`).
    pub limit: Option<u64>,
    /// Include hidden items (default false).
    #[serde(default)]
    pub include_hidden: bool,
    /// Only unread items (default false).
    #[serde(default)]
    pub unread_only: bool,
}

/// Builds the feed service from the API auth components.
fn feed_service(
    api_auth: &crate::auth::ApiAuth,
) -> FeedServiceImpl<PgFeedItemRepository, crm_db::postgres::PgAuditService> {
    FeedServiceImpl::new(api_auth.feed_items.clone(), api_auth.audit_service.clone())
}

/// Extracts a feed item id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_feed_item_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<FeedItemId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(FeedItemId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid feed item id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Fetches a feed item or returns its 404. Fetch-then-authorize: the
/// existence check runs before any grant check.
async fn fetch_feed_item(
    api_auth: &crate::auth::ApiAuth,
    feed_item_id: FeedItemId,
    instance: &str,
) -> Result<FeedItem, Box<Response>> {
    match api_auth.feed_items.get(feed_item_id).await {
        Ok(item) => Ok(item),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Authorizes `action` against the feed item's client profile, or
/// returns the 401/403 response.
async fn authorize_feed_item<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    item: &FeedItem,
    operation: Operation,
) -> Result<crm_domain::services::ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource: Resource::FeedItem,
        operation,
    };
    match resolve_context(state, auth, headers, &action, Some(item.client_profile_id)).await {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// Feed item annotations driven through one fetch-then-authorize
/// helper. Read/pin/hide all require the `update` operation.
enum FeedItemAnnotation {
    Read,
    Pin,
    Hide,
}

/// Runs a feed item annotation with fetch-then-authorize.
async fn annotate_feed_item<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    feed_item_id: FeedItemId,
    annotation: FeedItemAnnotation,
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
    let item = match fetch_feed_item(api_auth, feed_item_id, &instance).await {
        Ok(item) => item,
        Err(response) => return *response,
    };
    let ctx = match authorize_feed_item(state, auth, headers, &item, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = feed_service(api_auth);
    let item = match match annotation {
        FeedItemAnnotation::Read => service.mark_read(feed_item_id, &ctx).await,
        FeedItemAnnotation::Pin => service.pin_item(feed_item_id, &ctx).await,
        FeedItemAnnotation::Hide => service.hide_item(feed_item_id, &ctx).await,
    } {
        Ok(item) => item,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(item)).into_response()
}

/// `GET /api/v1/feed` — lists feed items for a client profile,
/// pinned first then newest-first.
///
/// Requires the `feed_item/read` grant in the profile (`403` without
/// it). An out-of-range `limit` is a `422`.
pub async fn list_feed<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListFeedQuery>,
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
    let profile_id = ClientProfileId::from(query.client_profile_id);
    let action = Action {
        resource: Resource::FeedItem,
        operation: Operation::Read,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // `FeedQuery::new` validates and caps the limit; a deserialized
    // query goes through the same constructor, so the invariant holds
    // for HTTP input too.
    let feed_query = match FeedQuery::new(
        query.limit.unwrap_or(20).min(100),
        query.include_hidden,
        query.unread_only,
    ) {
        Ok(feed_query) => feed_query,
        Err(error) => return domain_error_response(error, instance),
    };
    let service = feed_service(api_auth);
    let items = match service.list_feed(feed_query, &ctx).await {
        Ok(items) => items,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = items.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "items": items, "total": total })),
    )
        .into_response()
}

/// `POST /api/v1/feed/{id}/read` — marks a feed item read
/// (idempotent). Requires the `feed_item/update` grant.
pub async fn mark_feed_item_read<R, A, I>(
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
    let feed_item_id = match path_feed_item_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    annotate_feed_item(
        &state,
        auth,
        &headers,
        feed_item_id,
        FeedItemAnnotation::Read,
    )
    .await
}

/// `POST /api/v1/feed/{id}/pin` — pins a feed item so it floats to
/// the top of reads (idempotent; rejects hidden items). Requires the
/// `feed_item/update` grant.
pub async fn pin_feed_item<R, A, I>(
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
    let feed_item_id = match path_feed_item_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    annotate_feed_item(
        &state,
        auth,
        &headers,
        feed_item_id,
        FeedItemAnnotation::Pin,
    )
    .await
}

/// `POST /api/v1/feed/{id}/hide` — hides a feed item (sticky: there
/// is no unhide). Requires the `feed_item/update` grant.
pub async fn hide_feed_item<R, A, I>(
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
    let feed_item_id = match path_feed_item_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    annotate_feed_item(
        &state,
        auth,
        &headers,
        feed_item_id,
        FeedItemAnnotation::Hide,
    )
    .await
}
