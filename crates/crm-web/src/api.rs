// SPDX-License-Identifier: GPL-3.0-only
//! Typed HTTP client for the `crm-api` endpoints.
//!
//! All requests target [`API_BASE`] with `Authorization: Bearer <token>`.
//! Every function maps transport failures to [`ApiError::Transport`] and
//! non-2xx responses to [`ApiError::Api`] via the decoded
//! `application/problem+json` body.

use std::fmt;

use crm_domain::asset::Asset;
use crm_domain::audit::AuditEvent;
use crm_domain::contact::Contact;
use crm_domain::ids::{ClientProfileId, ContactId};
use crm_domain::profile::ClientProfile;
use gloo_net::http::Request;
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

/// Builds an [`ApiError::Api`] from a non-2xx response, preferring the
/// decoded `application/problem+json` body and falling back to the HTTP
/// status text when the body is not a problem document.
async fn api_error_from_response(response: &gloo_net::http::Response) -> ApiError {
    let status = response.status();
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
    /// The bearer token (UUID string). Keep in memory; never persist.
    pub token: String,
}

/// Logs in with email + password, returning the bearer token.
///
/// The token is kept in memory (a Leptos signal) for the session and sent
/// as `Authorization: Bearer <token>` on every API call. It is never
/// written to local storage.
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
        Err(api_error_from_response(&response).await)
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
        Err(api_error_from_response(&response).await)
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
    pub total: usize,
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
    pub total: usize,
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

/// Registers an asset's metadata. Returns `201` with the new asset, or
/// `200` with the existing asset when the same bytes were already
/// registered under the profile (upload dedupe).
pub async fn register_asset(
    request: &RegisterAssetRequest,
    token: &str,
) -> Result<Asset, ApiError> {
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
        response
            .json::<Asset>()
            .await
            .map_err(|error| ApiError::Transport(format!("invalid asset response: {error}")))
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

/// `GET /api/v1/audit/events` response: chain-order page.
#[derive(Debug, Clone, Deserialize)]
pub struct AuditEventsResponse {
    /// Events in chain (ascending time) order.
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
}

/// Queries the audit log (agency scope required).
pub async fn fetch_audit_events(
    query: &AuditQuery,
    token: &str,
) -> Result<AuditEventsResponse, ApiError> {
    let mut url = format!("{API_BASE}/api/v1/audit/events?");
    if let Some(action) = &query.action {
        url.push_str(&format!("action={action}&"));
    }
    let limit = query.limit.unwrap_or(50).min(100);
    url.push_str(&format!("limit={limit}"));
    let response = bearer(Request::get(&url), token)
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
