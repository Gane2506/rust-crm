// SPDX-License-Identifier: GPL-3.0-only
//! Typed HTTP client for the `crm-api` endpoints.
//!
//! All requests target [`API_BASE`] with `Authorization: Bearer <token>`.
//! Every function maps transport failures to [`ApiError::Transport`] and
//! non-2xx responses to [`ApiError::Api`] via the decoded
//! `application/problem+json` body.

use std::cell::Cell;
use std::fmt;

use crm_domain::asset::Asset;
use crm_domain::audit::AuditEvent;
use crm_domain::campaign::{Campaign, CampaignStep, ProspectPool};
use crm_domain::contact::Contact;
use crm_domain::ids::{ClientProfileId, ContactId};
use crm_domain::profile::ClientProfile;
use gloo_net::http::Request;
use leptos::prelude::{RwSignal, Set};
use serde::{Deserialize, Serialize};

/// Base URL of the `crm-api` server.
///
/// NOTE: hardcoded for the 01-FND-02 vertical slice; it becomes
/// configurable (build-time env / runtime config) in a later task.
pub const API_BASE: &str = "http://127.0.0.1:3000";

/// Fixed demo tenant (UUID literal) used by the vertical slice until real
/// auth/tenancy lands. Every contact created or listed here belongs to this
/// client profile.
pub const DEMO_PROFILE_ID_STR: &str = "123e4567-e89b-42d3-a456-426614174000";

/// Returns the demo tenant id parsed from [`DEMO_PROFILE_ID_STR`].
///
/// The literal is fixed and valid; `expect` documents that invariant instead
/// of threading a `Result` through every caller.
#[must_use]
pub fn demo_profile_id() -> ClientProfileId {
    serde_json::from_str::<ClientProfileId>(&format!("\"{DEMO_PROFILE_ID_STR}\""))
        .expect("DEMO_PROFILE_ID_STR is a valid UUID literal")
}

/// `POST /api/v1/contacts` request body.
#[derive(Debug, Clone, Serialize)]
pub struct CreateContactRequest {
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Raw email addresses; the server normalizes them.
    pub emails: Vec<String>,
    /// Raw phone numbers; the server normalizes them.
    pub phones: Vec<String>,
    /// Acquisition source label, e.g. `"manual-entry"`.
    pub source: String,
}

/// `GET /api/v1/contacts` response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ContactListResponse {
    /// The requested page of contacts.
    pub contacts: Vec<Contact>,
    /// Total matching contacts across all pages.
    pub total: u64,
}

/// RFC 7807 `application/problem+json` error body.
///
/// Only `title`/`detail` are surfaced in the UI; both default to empty when
/// the server omits them so decoding never fails on a sparse body.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProblemDetails {
    /// Short human-readable summary of the error.
    #[serde(default)]
    pub title: String,
    /// Longer explanation of this specific occurrence.
    #[serde(default)]
    pub detail: String,
}

/// Message shown when the session expired (401). Single source of truth:
/// [`ApiError::friendly_message`] uses it for the 401 case, and the login
/// form shows it when `App` returns there after an expiry, so the user
/// knows why they were logged out instead of seeing a bare login form.
pub const SESSION_EXPIRED_MESSAGE: &str = "Your session may have expired. Please log in again.";

/// Errors surfaced by the contact API client.
#[derive(Debug, Clone)]
pub enum ApiError {
    /// Network failure or an unreadable response body.
    Transport(String),
    /// HTTP error status, carrying the decoded problem+json fields.
    Api {
        /// HTTP status code.
        status: u16,
        /// Problem `title` (may be empty).
        title: String,
        /// Problem `detail` (may be empty).
        detail: String,
    },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Transport(message) => write!(f, "request failed: {message}"),
            ApiError::Api {
                status,
                title,
                detail,
            } => {
                if detail.is_empty() {
                    write!(f, "server error {status}: {title}")
                } else {
                    write!(f, "server error {status}: {title} — {detail}")
                }
            }
        }
    }
}

impl ApiError {
    /// Human-readable message for display in the UI.
    ///
    /// The [`Display`](fmt::Display) string is server-shaped
    /// ("server error 404: Not found — not found: contact '…'") and can
    /// carry opaque ids, so it must never be rendered. This maps the
    /// common shapes to plain language in the one place every page
    /// shares: `404` → not found, `403` → no access, `422` → the
    /// field-level message with the "validation failed: " server prefix
    /// stripped. Anything unrecognized falls back to a generic message
    /// rather than leaking server text.
    ///
    /// The raw error is still logged to the browser console (via
    /// `leptos::logging::error!`, which is `eprintln!` on the host)
    /// so the detail stays available for debugging.
    #[must_use]
    pub fn friendly_message(&self) -> String {
        leptos::logging::error!("ApiError hidden from UI: {self}");
        match self {
            ApiError::Transport(_) => {
                "We couldn't reach the server. Check your connection and try again."
                    .to_string()
            }
            ApiError::Api {
                status,
                title: _,
                detail,
            } => match status {
                401 => SESSION_EXPIRED_MESSAGE.to_string(),
                403 => {
                    "You don't have access to do that. Ask your agency admin for the right permission."
                        .to_string()
                }
                404 => {
                    "We couldn't find that record. It may have been deleted, or the link may be wrong."
                        .to_string()
                }
                409 => plain_detail(detail, "conflict: ").unwrap_or_else(|| {
                    "That conflicts with an existing record. Please check and try again."
                        .to_string()
                }),
                422 => plain_detail(detail, "validation failed: ").unwrap_or_else(|| {
                    "Please check the details you entered and try again.".to_string()
                }),
                _ => "Something went wrong on our side. Please try again.".to_string(),
            },
        }
    }
}

/// Turns a server `detail` string into a display sentence: strips the
/// known machine prefix (e.g. "validation failed: "), capitalizes the
/// first letter, and ends with a period. Returns `None` when the
/// detail is empty (or only the prefix), so the caller can fall back
/// to a status-level message instead of showing a bare prefix.
fn plain_detail(detail: &str, prefix: &str) -> Option<String> {
    let stripped = detail.strip_prefix(prefix).unwrap_or(detail).trim();
    if stripped.is_empty() {
        return None;
    }
    let mut sentence = String::with_capacity(stripped.len() + 1);
    let mut chars = stripped.chars();
    if let Some(first) = chars.next() {
        sentence.extend(first.to_uppercase());
    }
    sentence.push_str(chars.as_str());
    if !sentence.ends_with(['.', '?', '!']) {
        sentence.push('.');
    }
    Some(sentence)
}

// The signal `App` registers so a 401 from any authenticated call can
// mark the session expired (SESSION-02).
//
// Why registration, not `use_context`: every API call in the app runs
// inside `leptos::task::spawn_local`, which does NOT preserve the
// reactive owner — only `spawn_local_scoped` wraps the future in a
// `ScopedFuture` (reactive_graph 0.2.16) — so `use_context` inside
// these `async fn`s would silently return `None` and the expiry would
// never fire. Registration happens once, synchronously, in `App`
// setup where the owner is live; the signal itself is just a `Copy`
// handle afterwards, so setting it from here needs no context at all.
// The app is single-threaded (WASM), so a `thread_local!` cell is
// enough; on the host test target each test thread has its own cell.
// (Plain `//` comment, not `///`: rustdoc does not document macro
// invocations, and `///` here is a denied `unused_doc_comments` lint.)
thread_local! {
    static SESSION_EXPIRED: Cell<Option<RwSignal<bool>>> = const { Cell::new(None) };
}

/// Registers the session-expired signal with the API client. Called once
/// by `App` at startup. Until registered, 401s are returned as errors
/// only and nothing navigates (the pre-SESSION-02 behavior).
pub fn register_session_expired(signal: RwSignal<bool>) {
    SESSION_EXPIRED.with(|cell| cell.set(Some(signal)));
}

/// Marks the session expired, when one is registered. A no-op before
/// registration, so unit tests and the login page (which never registers
/// — it has no session) never trip over it.
fn notify_session_expired() {
    SESSION_EXPIRED.with(|cell| {
        if let Some(signal) = cell.get() {
            signal.set(true);
        }
    });
}

/// Builds an [`ApiError::Api`] from a non-2xx response, preferring the
/// decoded `application/problem+json` body and falling back to the HTTP
/// status text when the body is not a problem document.
///
/// A `401` also marks the session expired (see
/// [`register_session_expired`]) so `App` drops the dead token and
/// returns to the login form. This is the path every authenticated call
/// takes, so no page has to remember to do it itself.
async fn api_error_from_response(response: &gloo_net::http::Response) -> ApiError {
    api_error_from_response_inner(response, true).await
}

/// Same as [`api_error_from_response`] but never marks the session
/// expired. For the calls where a 401 is an expected outcome, not a dead
/// session: `login` (wrong credentials), `logout` (an already-revoked
/// token — the local token is dropped regardless), and `fetch_branding`
/// (public endpoint, called before login).
async fn api_error_from_response_quiet(response: &gloo_net::http::Response) -> ApiError {
    api_error_from_response_inner(response, false).await
}

/// Shared body of [`api_error_from_response`] and
/// [`api_error_from_response_quiet`]: `notify` selects whether a 401
/// also marks the session expired.
async fn api_error_from_response_inner(
    response: &gloo_net::http::Response,
    notify: bool,
) -> ApiError {
    let status = response.status();
    if notify && status == 401 {
        notify_session_expired();
    }
    match response.json::<ProblemDetails>().await {
        Ok(problem) if !problem.title.is_empty() || !problem.detail.is_empty() => ApiError::Api {
            status,
            title: problem.title,
            detail: problem.detail,
        },
        _ => ApiError::Api {
            status,
            title: response.status_text(),
            detail: String::new(),
        },
    }
}

/// `POST /api/v1/auth/login` response body: the bearer token.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    /// The bearer token (UUID string).
    pub token: String,
}

/// Logs in with email + password, returning the bearer token.
///
/// The token is kept in a Leptos signal for the session, persisted to
/// `sessionStorage` (survives refresh, cleared on tab close), and sent
/// as `Authorization: Bearer <token>` on every API call.
pub async fn login(email: &str, password: &str) -> Result<String, ApiError> {
    let url = format!("{API_BASE}/api/v1/auth/login");
    let body = serde_json::json!({ "email": email, "password": password }).to_string();
    let response = Request::post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .map_err(|error| ApiError::Transport(error.to_string()))?
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        let login: LoginResponse = response
            .json()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid login response: {error}")))?;
        Ok(login.token)
    } else {
        Err(api_error_from_response_quiet(&response).await)
    }
}

/// Logs out: revokes the session server-side (`204`).
///
/// Best-effort from the UI's perspective — the local token is dropped
/// regardless, so a failed call still ends the client session.
pub async fn logout(token: &str) -> Result<(), ApiError> {
    let url = format!("{API_BASE}/api/v1/auth/logout");
    let response = bearer(Request::post(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.status() == 204 || response.ok() {
        Ok(())
    } else {
        Err(api_error_from_response_quiet(&response).await)
    }
}

/// Adds the bearer token to a request builder.
fn bearer(request: gloo_net::http::RequestBuilder, token: &str) -> gloo_net::http::RequestBuilder {
    request.header("Authorization", &format!("Bearer {token}"))
}

/// Fetches the first page (`limit=20`) of contacts for the demo tenant,
/// returning the full envelope (page + `total`) so the UI can report the
/// count the server computed.
pub async fn fetch_contacts(token: &str) -> Result<ContactListResponse, ApiError> {
    let url = format!(
        "{API_BASE}/api/v1/contacts?client_profile_id={}&limit=20",
        demo_profile_id()
    );
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<ContactListResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid contact list response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Creates a contact, minting a fresh `Idempotency-Key` per call so a
/// retried submission never creates a duplicate.
pub async fn create_contact(
    request: &CreateContactRequest,
    token: &str,
) -> Result<Contact, ApiError> {
    let url = format!("{API_BASE}/api/v1/contacts");
    let body = serde_json::to_string(request)
        .map_err(|error| ApiError::Transport(format!("invalid create-contact request: {error}")))?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<Contact>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid contact response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Mints a fresh UUID v4 idempotency key for one form submission.
///
/// `ContactId` is used only as a convenient v4 generator here, not as a
/// reference to any contact.
fn new_idempotency_key() -> String {
    ContactId::new().to_string()
}

// ---------------------------------------------------------------------------
// Client profiles (`POST/GET /api/v1/profiles…`)
// ---------------------------------------------------------------------------

/// `GET /api/v1/profiles` response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileListResponse {
    /// Profiles for the caller's agency.
    pub profiles: Vec<ClientProfile>,
    /// Total profiles.
    pub total: u64,
}

/// `POST /api/v1/profiles` request body.
#[derive(Debug, Clone, Serialize)]
pub struct CreateProfileRequest {
    /// Display name of the client's business.
    pub name: String,
}

/// Lists the caller's agency profiles.
pub async fn fetch_profiles(token: &str) -> Result<ProfileListResponse, ApiError> {
    let url = format!("{API_BASE}/api/v1/profiles");
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<ProfileListResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid profile list response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Creates a profile, minting a fresh `Idempotency-Key` per call.
pub async fn create_profile(name: &str, token: &str) -> Result<ClientProfile, ApiError> {
    let url = format!("{API_BASE}/api/v1/profiles");
    let body = serde_json::to_string(&CreateProfileRequest {
        name: name.to_string(),
    })
    .map_err(|error| ApiError::Transport(format!("invalid create-profile request: {error}")))?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<ClientProfile>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid profile response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Fetches one profile by id.
pub async fn fetch_profile(profile_id: &str, token: &str) -> Result<ClientProfile, ApiError> {
    let url = format!("{API_BASE}/api/v1/profiles/{profile_id}");
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<ClientProfile>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid profile response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Runs a profile lifecycle transition (`suspend`, `reactivate`, `archive`).
pub async fn transition_profile(
    profile_id: &str,
    transition: &str,
    token: &str,
) -> Result<ClientProfile, ApiError> {
    let url = format!("{API_BASE}/api/v1/profiles/{profile_id}/{transition}");
    let response = bearer(Request::post(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<ClientProfile>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid profile response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

// ---------------------------------------------------------------------------
// Assets (`POST/GET /api/v1/assets…`)
// ---------------------------------------------------------------------------

/// `GET /api/v1/assets` response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct AssetListResponse {
    /// Assets for the requested profile.
    pub assets: Vec<Asset>,
    /// Total assets.
    pub total: u64,
}

/// `POST /api/v1/assets` request body: asset metadata. The byte store is
/// out of scope — `sha256_hex` is the content address supplied by the
/// uploader and doubles as the upload-dedupe key.
#[derive(Debug, Clone, Serialize)]
pub struct RegisterAssetRequest {
    /// Owning client profile (UUID string).
    pub client_profile_id: String,
    /// Original file name.
    pub file_name: String,
    /// MIME type supplied at upload.
    pub mime_type: String,
    /// Size in bytes; must be greater than zero.
    pub size_bytes: u64,
    /// SHA-256 of the file bytes, hex-encoded (64 chars).
    pub sha256_hex: String,
}

/// Lists assets for one client profile.
pub async fn fetch_assets(profile_id: &str, token: &str) -> Result<AssetListResponse, ApiError> {
    let url = format!("{API_BASE}/api/v1/assets?client_profile_id={profile_id}&limit=50");
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<AssetListResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid asset list response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Outcome of [`register_asset`]: the stored asset plus whether the
/// server deduplicated the upload.
#[derive(Debug, Clone)]
pub struct RegisterAssetResult {
    /// The registered (or already-existing) asset.
    pub asset: Asset,
    /// True when the server returned `200 OK` (the same bytes were already
    /// registered under the profile); false on `201 Created`.
    pub deduplicated: bool,
}

/// Registers an asset's metadata. Returns `201` with the new asset, or
/// `200` with the existing asset when the same bytes were already
/// registered under the profile (upload dedupe).
///
/// The [`RegisterAssetResult::deduplicated`] flag tells the caller which
/// case occurred so the UI can report "already registered" distinctly.
pub async fn register_asset(
    request: &RegisterAssetRequest,
    token: &str,
) -> Result<RegisterAssetResult, ApiError> {
    let url = format!("{API_BASE}/api/v1/assets");
    let body = serde_json::to_string(request)
        .map_err(|error| ApiError::Transport(format!("invalid register-asset request: {error}")))?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        let deduplicated = response.status() == 200;
        let asset = response
            .json::<Asset>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid asset response: {error}")))?;
        Ok(RegisterAssetResult {
            asset,
            deduplicated,
        })
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Archives an asset (terminal).
pub async fn archive_asset(asset_id: &str, token: &str) -> Result<Asset, ApiError> {
    let url = format!("{API_BASE}/api/v1/assets/{asset_id}/archive");
    let response = bearer(Request::post(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<Asset>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid asset response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

// ---------------------------------------------------------------------------
// Contacts: detail + merge (`GET /api/v1/contacts/{id}`,
// `POST /api/v1/contacts/merge`)
// ---------------------------------------------------------------------------

/// Fetches one contact by id.
pub async fn fetch_contact(contact_id: &str, token: &str) -> Result<Contact, ApiError> {
    let url = format!("{API_BASE}/api/v1/contacts/{contact_id}");
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<Contact>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid contact response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// `POST /api/v1/contacts/merge` request body.
#[derive(Debug, Clone, Serialize)]
pub struct MergeContactsRequest {
    /// The contact that survives, absorbing the other's identity (UUID string).
    pub survivor_id: String,
    /// The duplicate folded into the survivor (UUID string).
    pub duplicate_id: String,
}

/// Merges a duplicate into its survivor. Returns the archived duplicate
/// (tombstoned via `merged_into`, never deleted).
pub async fn merge_contacts(
    request: &MergeContactsRequest,
    token: &str,
) -> Result<Contact, ApiError> {
    let url = format!("{API_BASE}/api/v1/contacts/merge");
    let body = serde_json::to_string(request)
        .map_err(|error| ApiError::Transport(format!("invalid merge request: {error}")))?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<Contact>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid merge response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

// ---------------------------------------------------------------------------
// Audit log (`GET /api/v1/audit/events`, `GET /api/v1/audit/verify`)
// ---------------------------------------------------------------------------

/// One audit event in a query response: the chain-linked envelope plus the
/// forensic payload, when the event carries one.
#[derive(Debug, Clone, Deserialize)]
pub struct AuditEventItem {
    /// The chain-linked envelope (flattened).
    #[serde(flatten)]
    pub event: AuditEvent,
    /// The serialized security-event payload, if any.
    pub payload: Option<serde_json::Value>,
}

/// `GET /api/v1/audit/events` response: a page of audit events, in
/// the order the query asked for (chain order by default, newest first
/// with `AuditQuery::newest_first`).
#[derive(Debug, Clone, Deserialize)]
pub struct AuditEventsResponse {
    /// Events in the requested order.
    pub events: Vec<AuditEventItem>,
    /// Whether more events match beyond this page.
    pub has_more: bool,
}

/// Optional filters for the audit query. All are optional; the server
/// applies scope isolation on top.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// Pinned action name, e.g. `"auth.login_failed"`.
    pub action: Option<String>,
    /// Maximum events to return.
    pub limit: Option<u64>,
    /// When true, ask the server for the newest events first instead
    /// of chain order. The Dashboard sets this; the Audit page leaves
    /// it false and reorders the fetched page client-side.
    pub newest_first: bool,
}

/// Query pairs for `GET /api/v1/audit/events`, in request order.
///
/// `newest_first` is only sent when true — absent means chain order,
/// so callers that do not ask for it get exactly today's request.
fn audit_events_params(query: &AuditQuery) -> Vec<(&'static str, String)> {
    let mut params = Vec::new();
    if let Some(action) = &query.action {
        params.push(("action", action.clone()));
    }
    params.push(("limit", query.limit.unwrap_or(50).min(100).to_string()));
    if query.newest_first {
        params.push(("newest_first", "true".to_string()));
    }
    params
}

/// Queries the audit log (agency scope required).
///
/// Query parameters are encoded via gloo-net's query builder
/// (`gloo_net::http::RequestBuilder::query`) so action names with special
/// characters cannot alter the query structure.
pub async fn fetch_audit_events(
    query: &AuditQuery,
    token: &str,
) -> Result<AuditEventsResponse, ApiError> {
    let builder = bearer(
        Request::get(&format!("{API_BASE}/api/v1/audit/events")),
        token,
    );
    let builder = builder.query(audit_events_params(query));
    let response = builder
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<AuditEventsResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid audit response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// `GET /api/v1/audit/verify` response: chain verification result.
#[derive(Debug, Clone, Deserialize)]
pub struct VerifyResponse {
    /// Whether every checked event links correctly from genesis.
    pub valid: bool,
    /// Number of events checked.
    pub events_checked: u64,
    /// Chain hash of the verified head, hex-encoded.
    pub head: Option<String>,
    /// The first event that failed verification, if any (UUID string).
    pub first_bad_event: Option<String>,
}

/// Verifies the audit chain from genesis (agency scope required).
pub async fn verify_audit_chain(token: &str) -> Result<VerifyResponse, ApiError> {
    let url = format!("{API_BASE}/api/v1/audit/verify");
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<VerifyResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid verify response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// `GET /api/v1/branding` response: the current theme version.
///
/// The theme fields mirror `crm_domain::branding::BrandingTheme`; the
/// web copy in `crate::theme` is the render source of truth.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BrandingResponse {
    /// Monotonic version (0 when never set).
    pub version: u64,
    /// The theme (all `None` when never set).
    pub theme: BrandingThemeDto,
}

/// Branding theme DTO (mirrors the domain type).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BrandingThemeDto {
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

/// Fetches the agency branding theme. Public: no token required (the
/// login page calls this before authentication).
pub async fn fetch_branding() -> Result<BrandingResponse, ApiError> {
    let url = format!("{API_BASE}/api/v1/branding");
    let response = Request::get(&url)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<BrandingResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid branding response: {error}")))
    } else {
        Err(api_error_from_response_quiet(&response).await)
    }
}

/// Updates the agency branding theme (requires `AgencySettings:Write`).
pub async fn update_branding(
    theme: &BrandingThemeDto,
    token: &str,
) -> Result<BrandingResponse, ApiError> {
    let url = format!("{API_BASE}/api/v1/branding");
    let body = serde_json::to_string(theme)
        .map_err(|error| ApiError::Transport(format!("invalid branding request: {error}")))?;
    let response = bearer(
        Request::put(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<BrandingResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid branding response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

// ---------------------------------------------------------------------------
// Campaigns (`POST/GET /api/v1/campaigns…`, `/api/v1/prospect-pools…`)
// ---------------------------------------------------------------------------

/// `GET /api/v1/campaigns` response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct CampaignListResponse {
    /// The requested page of campaigns.
    pub campaigns: Vec<Campaign>,
    /// Total matching campaigns across all pages.
    pub total: u64,
}

/// `GET /api/v1/campaigns/{id}` response envelope: the campaign plus its
/// steps in position order.
#[derive(Debug, Clone, Deserialize)]
pub struct CampaignDetailResponse {
    /// The campaign.
    pub campaign: Campaign,
    /// Its steps, in position order.
    pub steps: Vec<CampaignStep>,
}

/// `GET /api/v1/prospect-pools` response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct PoolListResponse {
    /// The requested page of pools.
    pub pools: Vec<ProspectPool>,
}

/// `POST /api/v1/campaigns` request body.
///
/// Field names mirror the API's `CreateCampaignBody` exactly. The
/// optional fields are skipped when `None`: the server's
/// `#[serde(default)]` only applies to *missing* fields — an explicit
/// JSON `null` would fail to deserialize into a `String`.
#[derive(Debug, Clone, Serialize)]
pub struct CreateCampaignRequest {
    /// Campaign name; must be non-empty.
    pub name: String,
    /// `"agency"` or `"client_profile"`.
    pub owner_kind: String,
    /// Owning profile id, required when `owner_kind` is `"client_profile"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_profile_id: Option<String>,
    /// `"email"`, `"call"`, or `"mixed"`.
    pub channel: String,
    /// Prospect pool id (UUID string).
    pub prospect_pool_id: String,
    /// Cron schedule, e.g. `"0 9 * * *"`.
    pub schedule: String,
    /// Suppression policy; server default `""`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppression_policy: Option<String>,
    /// Conversion rules; server default `""`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversion_rules: Option<String>,
    /// Attribution window in days; server default `30`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_window_days: Option<u32>,
}

/// `POST /api/v1/campaigns/{id}/steps` request body.
#[derive(Debug, Clone, Serialize)]
pub struct AddCampaignStepRequest {
    /// `"email"`, `"call"`, or `"wait"`.
    pub kind: String,
    /// 0-based position in the step order.
    pub position: u32,
    /// Template id; required for email/call steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Wait in seconds; required (and > 0) for wait steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_duration_secs: Option<u64>,
}

/// `POST /api/v1/campaigns/{id}/schedule` request body: one-shot launch
/// time, RFC 3339 in UTC (the server rejects a past time).
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleCampaignRequest {
    /// Launch time, e.g. `"2026-11-01T09:00:00Z"`.
    pub schedule_for: String,
}

/// `POST /api/v1/prospect-pools` request body.
#[derive(Debug, Clone, Serialize)]
pub struct CreatePoolRequest {
    /// Owning client profile (UUID string).
    pub client_profile_id: String,
    /// Pool name; must be non-empty.
    pub name: String,
}

/// Lists campaigns: agency-owned when `client_profile_id` is `None`,
/// profile-owned when `Some`. The list response carries no step count —
/// count steps from [`fetch_campaign`] instead (R3-04).
pub async fn fetch_campaigns(
    token: &str,
    client_profile_id: Option<&str>,
) -> Result<CampaignListResponse, ApiError> {
    let url = match client_profile_id {
        Some(profile_id) => {
            format!("{API_BASE}/api/v1/campaigns?client_profile_id={profile_id}&limit=20")
        }
        None => format!("{API_BASE}/api/v1/campaigns?limit=20"),
    };
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<CampaignListResponse>()
            .await
            .map_err(|error| {
                ApiError::Transport(format!("invalid campaign list response: {error}"))
            })
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Fetches one campaign plus its steps.
pub async fn fetch_campaign(
    campaign_id: &str,
    token: &str,
) -> Result<CampaignDetailResponse, ApiError> {
    let url = format!("{API_BASE}/api/v1/campaigns/{campaign_id}");
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<CampaignDetailResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid campaign response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Creates a campaign, minting a fresh `Idempotency-Key` per call.
pub async fn create_campaign(
    request: &CreateCampaignRequest,
    token: &str,
) -> Result<Campaign, ApiError> {
    let url = format!("{API_BASE}/api/v1/campaigns");
    let body = serde_json::to_string(request).map_err(|error| {
        ApiError::Transport(format!("invalid create-campaign request: {error}"))
    })?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<Campaign>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid campaign response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Adds a step to a draft campaign — the server rejects steps once the
/// campaign leaves `Draft` — minting a fresh `Idempotency-Key` per call.
pub async fn add_campaign_step(
    campaign_id: &str,
    request: &AddCampaignStepRequest,
    token: &str,
) -> Result<CampaignStep, ApiError> {
    let url = format!("{API_BASE}/api/v1/campaigns/{campaign_id}/steps");
    let body = serde_json::to_string(request)
        .map_err(|error| ApiError::Transport(format!("invalid add-step request: {error}")))?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<CampaignStep>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid step response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Schedules a one-shot launch for a draft campaign — the server
/// requires a future time and `Draft` state — minting a fresh
/// `Idempotency-Key` per call.
pub async fn schedule_campaign(
    campaign_id: &str,
    request: &ScheduleCampaignRequest,
    token: &str,
) -> Result<Campaign, ApiError> {
    let url = format!("{API_BASE}/api/v1/campaigns/{campaign_id}/schedule");
    let body = serde_json::to_string(request)
        .map_err(|error| ApiError::Transport(format!("invalid schedule request: {error}")))?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<Campaign>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid campaign response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Runs a campaign lifecycle transition (`launch`, `pause`, `resume`,
/// `complete`, `cancel`).
///
/// The server does not read the idempotency key on transitions (they
/// are state-machine operations, not creates); one is still sent so
/// every POST from this client carries a fresh UUID key.
pub async fn transition_campaign(
    campaign_id: &str,
    transition: &str,
    token: &str,
) -> Result<Campaign, ApiError> {
    let url = format!("{API_BASE}/api/v1/campaigns/{campaign_id}/{transition}");
    let response = bearer(
        Request::post(&url).header("Idempotency-Key", &new_idempotency_key()),
        token,
    )
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<Campaign>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid campaign response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Lists a profile's prospect pools (the endpoint requires a profile).
pub async fn fetch_pools(
    client_profile_id: &str,
    token: &str,
) -> Result<PoolListResponse, ApiError> {
    let url =
        format!("{API_BASE}/api/v1/prospect-pools?client_profile_id={client_profile_id}&limit=50");
    let response = bearer(Request::get(&url), token)
        .send()
        .await
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<PoolListResponse>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid pool list response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

/// Creates a prospect pool for a profile, minting a fresh
/// `Idempotency-Key` per call.
pub async fn create_pool(
    request: &CreatePoolRequest,
    token: &str,
) -> Result<ProspectPool, ApiError> {
    let url = format!("{API_BASE}/api/v1/prospect-pools");
    let body = serde_json::to_string(request)
        .map_err(|error| ApiError::Transport(format!("invalid create-pool request: {error}")))?;
    let response = bearer(
        Request::post(&url)
            .header("Idempotency-Key", &new_idempotency_key())
            .header("Content-Type", "application/json"),
        token,
    )
    .body(body)
    .map_err(|error| ApiError::Transport(error.to_string()))?
    .send()
    .await
    .map_err(|error| ApiError::Transport(error.to_string()))?;
    if response.ok() {
        response
            .json::<ProspectPool>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid pool response: {error}")))
    } else {
        Err(api_error_from_response(&response).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an [`ApiError::Api`] the way [`api_error_from_response`]
    /// does, so the mapping is tested against real server shapes.
    fn api(status: u16, title: &str, detail: &str) -> ApiError {
        ApiError::Api {
            status,
            title: title.to_string(),
            detail: detail.to_string(),
        }
    }

    #[test]
    fn not_found_never_leaks_the_id() {
        let message = api(
            404,
            "Not found",
            "not found: contact '00000000-0000-0000-0000-000000000000'",
        )
        .friendly_message();
        assert_eq!(
            message,
            "We couldn't find that record. It may have been deleted, or the link may be wrong."
        );
        assert!(!message.contains("00000000"));
    }

    #[test]
    fn forbidden_becomes_an_access_message() {
        let message = api(
            403,
            "Forbidden",
            "the authenticated caller lacks the grant for this action",
        )
        .friendly_message();
        assert_eq!(
            message,
            "You don't have access to do that. Ask your agency admin for the right permission."
        );
        assert!(!message.contains("grant"));
    }

    #[test]
    fn unauthorized_points_at_the_session() {
        assert_eq!(
            api(401, "Unauthorized", "unauthorized").friendly_message(),
            SESSION_EXPIRED_MESSAGE
        );
    }

    #[test]
    fn session_expiry_notifies_only_when_registered() {
        use leptos::prelude::GetUntracked;

        // No signal registered: notifying is a silent no-op, never a panic.
        notify_session_expired();
        let expired = RwSignal::new(false);
        register_session_expired(expired);
        notify_session_expired();
        assert!(expired.get_untracked());
    }

    #[test]
    fn validation_strips_the_server_prefix() {
        assert_eq!(
            api(
                422,
                "Validation failed",
                "validation failed: emails[0] is not a valid email address"
            )
            .friendly_message(),
            "Emails[0] is not a valid email address."
        );
    }

    #[test]
    fn validation_without_a_detail_falls_back() {
        assert_eq!(
            api(422, "Validation failed", "").friendly_message(),
            "Please check the details you entered and try again."
        );
    }

    #[test]
    fn conflict_strips_its_prefix() {
        assert_eq!(
            api(409, "Conflict", "conflict: asset is already archived").friendly_message(),
            "Asset is already archived."
        );
    }

    #[test]
    fn transport_becomes_a_connection_message() {
        assert_eq!(
            ApiError::Transport("TypeError: NetworkError".to_string()).friendly_message(),
            "We couldn't reach the server. Check your connection and try again."
        );
    }

    #[test]
    fn unknown_status_never_shows_server_text() {
        let message =
            api(500, "Internal Server Error", "database connection refused").friendly_message();
        assert_eq!(
            message,
            "Something went wrong on our side. Please try again."
        );
        assert!(!message.contains("database"));
    }

    #[test]
    fn audit_events_params_default_is_chain_order() {
        // A default query sends only the limit — no `newest_first`,
        // so the server keeps today's chain order.
        assert_eq!(
            audit_events_params(&AuditQuery::default()),
            vec![("limit", "50".to_string())]
        );
    }

    #[test]
    fn audit_events_params_sends_newest_first_only_when_set() {
        let query = AuditQuery {
            action: Some("auth.login".to_string()),
            limit: Some(8),
            newest_first: true,
        };
        assert_eq!(
            audit_events_params(&query),
            vec![
                ("action", "auth.login".to_string()),
                ("limit", "8".to_string()),
                ("newest_first", "true".to_string()),
            ]
        );
        let query = AuditQuery {
            limit: Some(8),
            newest_first: false,
            ..AuditQuery::default()
        };
        assert!(
            audit_events_params(&query)
                .iter()
                .all(|(key, _)| *key != "newest_first"),
            "an explicit false must not send the parameter"
        );
    }

    #[test]
    fn audit_events_params_caps_the_limit() {
        let query = AuditQuery {
            limit: Some(500),
            ..AuditQuery::default()
        };
        assert_eq!(
            audit_events_params(&query),
            vec![("limit", "100".to_string())]
        );
    }

    #[test]
    fn plain_detail_keeps_existing_punctuation() {
        assert_eq!(
            plain_detail(
                "validation failed: name must not be empty.",
                "validation failed: "
            ),
            Some("Name must not be empty.".to_string())
        );
        assert_eq!(plain_detail("", "validation failed: "), None);
        assert_eq!(
            plain_detail("validation failed: ", "validation failed: "),
            None
        );
    }
}
