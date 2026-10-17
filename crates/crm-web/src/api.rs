// SPDX-License-Identifier: GPL-3.0-only
//! Typed HTTP client for the `crm-api` contact endpoints.
//!
//! All requests target [`API_BASE`]. The demo tenant
//! [`DEMO_PROFILE_ID_STR`] stands in for real auth/tenancy until task
//! `03-SEC-01` provides the client profile from the session.

use std::fmt;

use crm_domain::contact::Contact;
use crm_domain::ids::{ClientProfileId, ContactId};
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

/// Fetches the first page (`limit=20`) of contacts for the demo tenant,
/// returning the full envelope (page + `total`) so the UI can report the
/// count the server computed.
pub async fn fetch_contacts() -> Result<ContactListResponse, ApiError> {
    let url = format!(
        "{API_BASE}/api/v1/contacts?client_profile_id={}&limit=20",
        demo_profile_id()
    );
    let response = Request::get(&url)
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
pub async fn create_contact(request: &CreateContactRequest) -> Result<Contact, ApiError> {
    let url = format!("{API_BASE}/api/v1/contacts");
    let body = serde_json::to_string(request)
        .map_err(|error| ApiError::Transport(format!("invalid create-contact request: {error}")))?;
    let response = Request::post(&url)
        .header("Idempotency-Key", &new_idempotency_key())
        .header("Content-Type", "application/json")
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
