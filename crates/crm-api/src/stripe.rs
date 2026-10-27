// SPDX-License-Identifier: GPL-3.0-only
//! Stripe adapter (STRIPE-01): Checkout sessions + webhook-confirmed
//! payments.
//!
//! Two surfaces:
//!
//! - `POST /v1/invoices/{id}/stripe-checkout` (authenticated, in
//!   [`crate::billing`]): creates a Stripe Checkout Session for an
//!   issued invoice, amount = remaining balance, currency = invoice
//!   currency. The caller pays on Stripe's hosted page; this server
//!   never sees card details.
//! - `POST /v1/stripe/webhook` (NOT bearer-authenticated, registered
//!   outside the auth middleware like the email ingest webhook): a
//!   `checkout.session.completed` event records the payment against
//!   the invoice through the existing guarded payment path, so a
//!   webhook payment can never over-apply.
//!
//! Configuration is environment only: `STRIPE_SECRET_KEY` (the
//! Stripe secret key), `STRIPE_WEBHOOK_SECRET` (the endpoint's
//! signing secret, `whsec_…`). Missing keys are not a startup error:
//! the server starts, everything else works, and the checkout
//! endpoint answers `503 "Stripe not configured"`. Key material is
//! never logged and never returned in a response; logs say only
//! configured/unconfigured.
//!
//! The webhook signature is the load-bearing control: a missing or
//! bad `Stripe-Signature` is a `400` and the payload is never
//! processed. Verification is HMAC-SHA256 over `{timestamp}.{raw
//! body}` with a 300-second timestamp tolerance (Stripe's scheme),
//! compared in constant time against every `v1=` value in the
//! header (Stripe sends two during a secret rotation).
//!
//! Idempotency has two layers. The event claim
//! (`stripe_webhook_events`, one row per Stripe event id, claimed
//! atomically before recording) makes redelivery safe: the loser
//! looks up the payment the winner recorded instead of recording
//! again. The recorded payment also carries the Stripe Checkout
//! Session id as `external_reference`, so a winner that recorded
//! the payment but crashed before linking it is still found by a
//! redelivery. A claim is held only while the winner is actively
//! processing: every exit that does not record a payment releases
//! it, and a claim older than five minutes with no payment is
//! treated as stale (the HARD-04 pattern) so a redelivery can
//! reclaim it.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;

use crm_domain::audit::ActorKind;
use crm_domain::billing::Money;
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, InvoiceId, PaymentId};
use crm_domain::repositories::{InvoiceRepository, PaymentRepository};
use crm_domain::services::{ActionContext, AuditService, BillingService, NewPayment};

use crate::auth::ApiAuth;
use crate::contacts::{request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Environment variable holding the Stripe secret key (`sk_…`).
pub(crate) const STRIPE_SECRET_KEY_ENV: &str = "STRIPE_SECRET_KEY";
/// Environment variable holding the Stripe webhook signing secret
/// (`whsec_…`).
pub(crate) const STRIPE_WEBHOOK_SECRET_ENV: &str = "STRIPE_WEBHOOK_SECRET";
/// Test-only override for the Stripe API base URL. Production always
/// uses [`DEFAULT_STRIPE_API_BASE_URL`]; tests point it at a mock
/// server so the suite never touches Stripe.
pub(crate) const STRIPE_API_BASE_URL_ENV: &str = "STRIPE_API_BASE_URL";
/// Environment variable overriding the Checkout success redirect.
pub(crate) const STRIPE_SUCCESS_URL_ENV: &str = "STRIPE_SUCCESS_URL";
/// Environment variable overriding the Checkout cancel redirect.
pub(crate) const STRIPE_CANCEL_URL_ENV: &str = "STRIPE_CANCEL_URL";

/// Stripe API base URL when [`STRIPE_API_BASE_URL_ENV`] is unset.
const DEFAULT_STRIPE_API_BASE_URL: &str = "https://api.stripe.com";
/// Default Checkout return URL for an invoice: its detail page.
/// Both the success and the cancel redirect land here — the detail
/// page shows the current state on load and does not poll, so the
/// payment appears once the webhook lands.
fn default_stripe_return_url(invoice_id: InvoiceId) -> String {
    format!("http://127.0.0.1:3000/billing/{}", invoice_id.as_uuid())
}

/// Stripe's webhook timestamp tolerance, in seconds: a signature
/// older than this is rejected even when the HMAC matches, so a
/// captured webhook cannot be replayed hours later.
const STRIPE_SIGNATURE_TOLERANCE_SECS: u64 = 300;

/// How long a webhook event claim may sit with no payment before a
/// redelivery treats it as stale (the winner died mid-flight) and
/// reclaims it. Mirrors the five-minute idempotency claim TTL
/// (HARD-04).
const STRIPE_EVENT_CLAIM_TTL: chrono::Duration = chrono::Duration::minutes(5);

/// Stripe configuration, read from the environment.
///
/// The secret key and webhook secret are `Option`: missing keys are
/// a configuration state, not an error — the server starts without
/// Stripe. `Debug` is implemented by hand so the key material is
/// never printed: it renders as `<redacted>`.
#[derive(Clone)]
pub(crate) struct StripeConfig {
    /// Stripe secret key (`sk_…`), or `None` when unconfigured.
    pub secret_key: Option<String>,
    /// Stripe webhook signing secret (`whsec_…`), or `None`.
    pub webhook_secret: Option<String>,
    /// Stripe API base URL (override for tests only).
    pub api_base_url: String,
    /// `STRIPE_SUCCESS_URL` override, or `None` when unset: the
    /// per-invoice default (the invoice detail page) applies then.
    pub success_url: Option<String>,
    /// `STRIPE_CANCEL_URL` override, or `None` when unset: the
    /// per-invoice default (the invoice detail page) applies then.
    pub cancel_url: Option<String>,
}

impl std::fmt::Debug for StripeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripeConfig")
            .field(
                "secret_key",
                &self.secret_key.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "webhook_secret",
                &self.webhook_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("api_base_url", &self.api_base_url)
            .field("success_url", &self.success_url)
            .field("cancel_url", &self.cancel_url)
            .finish()
    }
}

impl StripeConfig {
    /// Reads the Stripe configuration from the environment. Missing
    /// keys are `None`, never an error: the server starts without
    /// Stripe and the checkout endpoint answers `503`.
    pub(crate) fn from_env() -> Self {
        Self {
            secret_key: std::env::var(STRIPE_SECRET_KEY_ENV).ok(),
            webhook_secret: std::env::var(STRIPE_WEBHOOK_SECRET_ENV).ok(),
            api_base_url: std::env::var(STRIPE_API_BASE_URL_ENV)
                .unwrap_or_else(|_| DEFAULT_STRIPE_API_BASE_URL.to_owned()),
            success_url: std::env::var(STRIPE_SUCCESS_URL_ENV).ok(),
            cancel_url: std::env::var(STRIPE_CANCEL_URL_ENV).ok(),
        }
    }

    /// URL Stripe redirects to after a successful Checkout for
    /// `invoice_id`: the `STRIPE_SUCCESS_URL` override when set,
    /// else the invoice detail page. The payer lands back on the
    /// invoice they just paid, where the payment appears once the
    /// webhook lands.
    #[must_use]
    pub(crate) fn success_url_for(&self, invoice_id: InvoiceId) -> String {
        self.success_url
            .clone()
            .unwrap_or_else(|| default_stripe_return_url(invoice_id))
    }

    /// URL Stripe redirects to when the customer cancels Checkout
    /// for `invoice_id`: the `STRIPE_CANCEL_URL` override when set,
    /// else the invoice detail page.
    #[must_use]
    pub(crate) fn cancel_url_for(&self, invoice_id: InvoiceId) -> String {
        self.cancel_url
            .clone()
            .unwrap_or_else(|| default_stripe_return_url(invoice_id))
    }

    /// Whether a Checkout Session can be created: a secret key is
    /// configured.
    #[must_use]
    pub(crate) fn is_configured(&self) -> bool {
        self.secret_key.is_some()
    }
}

/// What can go wrong creating a Stripe Checkout Session.
#[derive(Debug, Error)]
pub(crate) enum StripeError {
    /// No `STRIPE_SECRET_KEY` is configured.
    #[error("Stripe is not configured")]
    NotConfigured,
    /// The HTTP call to Stripe failed (network, TLS, timeout).
    #[error("Stripe request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// Stripe answered with a non-success status. The body is not
    /// kept: it can contain request ids and error detail that must
    /// not leak into our logs or responses.
    #[error("Stripe returned an unexpected status {status}")]
    UnexpectedStatus {
        /// The HTTP status Stripe returned.
        status: u16,
    },
    /// Stripe answered `200` but the session could not be parsed, or
    /// the session has no checkout URL.
    #[error("Stripe response could not be parsed")]
    BadResponse,
}

/// A Stripe Checkout Session, as returned by
/// `POST /v1/checkout/sessions`. No `id` field: the session id is
/// never read (the webhook reconciles on `metadata[invoice_id]`),
/// and serde ignores the extra field Stripe sends.
#[derive(Debug, Deserialize)]
pub(crate) struct StripeCheckoutSession {
    /// Hosted Checkout URL the payer is redirected to.
    pub url: Option<String>,
}

/// Builds the form-encoded body for `POST /v1/checkout/sessions`.
///
/// Pure (no I/O) so tests can assert the exact parameters without
/// touching Stripe. The amount is a single line item priced from
/// the invoice balance (`price_data.unit_amount`, minor units) —
/// `amount_total` is a response field, not a request parameter.
/// `metadata[invoice_id]` is what the webhook reconciles on.
/// `success_url` / `cancel_url` are the env overrides when set,
/// else the invoice detail page for this invoice.
fn checkout_session_form(
    config: &StripeConfig,
    invoice_id: InvoiceId,
    amount: Money,
) -> Vec<(String, String)> {
    vec![
        ("mode".to_owned(), "payment".to_owned()),
        (
            "line_items[0][price_data][currency]".to_owned(),
            amount.currency().to_lowercase(),
        ),
        (
            "line_items[0][price_data][product_data][name]".to_owned(),
            format!("Invoice {}", invoice_id.as_uuid()),
        ),
        (
            "line_items[0][price_data][unit_amount]".to_owned(),
            amount.amount().to_string(),
        ),
        ("line_items[0][quantity]".to_owned(), "1".to_owned()),
        ("success_url".to_owned(), config.success_url_for(invoice_id)),
        ("cancel_url".to_owned(), config.cancel_url_for(invoice_id)),
        (
            "metadata[invoice_id]".to_owned(),
            invoice_id.as_uuid().to_string(),
        ),
    ]
}

/// Minimal client for the Stripe API (form-encoded, no SDK).
#[derive(Debug, Clone)]
pub(crate) struct StripeClient {
    client: reqwest::Client,
    config: StripeConfig,
}

impl StripeClient {
    /// Builds a client from `config`. The HTTP client carries a
    /// 10-second timeout so a hung Stripe call fails inside the
    /// server-wide 30-second request timeout instead of holding the
    /// request open.
    pub(crate) fn new(config: StripeConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client builds with the rustls-tls feature");
        Self { client, config }
    }

    /// Creates a Checkout Session for `invoice_id` for `amount`.
    ///
    /// When `idempotency_key` is `Some` (the caller's
    /// `Idempotency-Key` header), it is passed to Stripe too, so a
    /// retried checkout creation returns the same Stripe session
    /// instead of creating a duplicate.
    pub(crate) async fn create_checkout_session(
        &self,
        invoice_id: InvoiceId,
        amount: Money,
        idempotency_key: Option<&str>,
    ) -> Result<StripeCheckoutSession, StripeError> {
        let Some(secret_key) = self.config.secret_key.as_deref() else {
            return Err(StripeError::NotConfigured);
        };
        let mut request = self
            .client
            .post(format!("{}/v1/checkout/sessions", self.config.api_base_url))
            .header("Authorization", format!("Bearer {secret_key}"))
            .form(&checkout_session_form(&self.config, invoice_id, amount));
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(StripeError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }
        let session: StripeCheckoutSession = response
            .json()
            .await
            .map_err(|_| StripeError::BadResponse)?;
        if session.url.is_none() {
            return Err(StripeError::BadResponse);
        }
        Ok(session)
    }
}

/// What can go wrong verifying a Stripe webhook signature.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum StripeSignatureError {
    /// The `Stripe-Signature` header has no usable `t=` timestamp.
    #[error("the Stripe-Signature header has no usable timestamp")]
    MissingTimestamp,
    /// The `Stripe-Signature` header has no `v1=` signature.
    #[error("the Stripe-Signature header has no v1 signature")]
    MissingSignature,
    /// The signature's timestamp is outside the 300-second
    /// tolerance: a captured webhook must not be replayable.
    #[error("the webhook timestamp is outside the 300-second tolerance")]
    TimestampTooOld,
    /// No `v1=` value matches the payload.
    #[error("no v1 signature matches the payload")]
    SignatureMismatch,
    /// The webhook secret cannot initialise an HMAC (unreachable for
    /// HMAC-SHA256, which accepts any key length — kept so the
    /// constructor's error has somewhere to go).
    #[error("the webhook secret cannot be used for signing")]
    InvalidSecret,
}

/// Verifies a Stripe webhook signature.
///
/// `header` is the raw `Stripe-Signature` header
/// (`t=<timestamp>,v1=<hex>[,v1=<hex>]`), `payload` is the raw
/// request body — signature verification is over the exact bytes
/// Stripe sent, never a re-serialized parse. The signed payload is
/// `{timestamp}.{raw body}`, HMAC-SHA256 with `secret`, compared in
/// constant time against every `v1=` value (Stripe sends two during
/// a secret rotation). A signature older than
/// [`STRIPE_SIGNATURE_TOLERANCE_SECS`] is rejected even when the
/// HMAC matches.
pub(crate) fn verify_stripe_signature(
    secret: &str,
    header: &str,
    payload: &[u8],
    now_unix_secs: u64,
) -> Result<(), StripeSignatureError> {
    let mut timestamp: Option<u64> = None;
    let mut signatures: Vec<&str> = Vec::new();
    for part in header.split(',') {
        if let Some(value) = part.strip_prefix("t=") {
            timestamp = value.parse::<u64>().ok();
        } else if let Some(value) = part.strip_prefix("v1=") {
            signatures.push(value);
        }
        // `v0=` (legacy scheme) is ignored: `v1=` is current.
    }
    let timestamp = timestamp.ok_or(StripeSignatureError::MissingTimestamp)?;
    if now_unix_secs.abs_diff(timestamp) > STRIPE_SIGNATURE_TOLERANCE_SECS {
        return Err(StripeSignatureError::TimestampTooOld);
    }
    if signatures.is_empty() {
        return Err(StripeSignatureError::MissingSignature);
    }
    let mut signed_payload = format!("{timestamp}.").into_bytes();
    signed_payload.extend_from_slice(payload);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| StripeSignatureError::InvalidSecret)?;
    mac.update(&signed_payload);
    let expected = mac.finalize().into_bytes();
    for signature in signatures {
        let Ok(bytes) = hex::decode(signature) else {
            continue;
        };
        if bytes.len() == expected.len() && bool::from(expected.as_slice().ct_eq(bytes.as_slice()))
        {
            return Ok(());
        }
    }
    Err(StripeSignatureError::SignatureMismatch)
}

/// A Stripe event envelope. Only the fields the webhook needs are
/// modelled; `data.object` stays untyped here because its shape
/// depends on the event type and is parsed per type below.
#[derive(Debug, Deserialize)]
struct StripeEvent {
    /// Stripe event id (`evt_…`) — the dedupe key.
    id: String,
    /// Event type, e.g. `checkout.session.completed`.
    #[serde(rename = "type")]
    event_type: String,
    /// Event payload.
    data: StripeEventData,
}

/// The `data` member of a Stripe event.
#[derive(Debug, Deserialize)]
struct StripeEventData {
    /// The object the event is about (a Checkout Session here).
    object: serde_json::Value,
}

/// A Checkout Session object, as carried by
/// `checkout.session.completed`.
#[derive(Debug, Deserialize)]
struct CheckoutSession {
    /// Session id (`cs_test_…` / `cs_live_…`).
    id: String,
    /// `paid` when the customer paid; other values (`unpaid`, …)
    /// mean the session completed without payment.
    #[serde(default)]
    payment_status: Option<String>,
    /// Amount paid, in minor units.
    #[serde(default)]
    amount_total: Option<i64>,
    /// Lowercase ISO 4217 currency, e.g. `usd`.
    #[serde(default)]
    currency: Option<String>,
    /// Session metadata — carries `invoice_id` for reconciliation.
    #[serde(default)]
    metadata: HashMap<String, String>,
}

/// What a lost event claim means for a redelivery.
enum ClaimState {
    /// The event already produced this payment.
    Recorded(PaymentId),
    /// The winning delivery is still processing; Stripe should retry.
    InFlight,
    /// The claim is older than the TTL with no payment: the winner
    /// died mid-flight. The caller may release it and claim again.
    Stale,
}

/// Resolves a lost claim: has this event already produced a payment,
/// is the winner still processing, or is the claim stale?
async fn claim_state(
    api_auth: &ApiAuth,
    event_id: &str,
    session_id: &str,
    invoice_id: InvoiceId,
) -> Result<ClaimState, DomainError> {
    if let Some(payment_id) = api_auth.stripe_events.payment_for_event(event_id).await? {
        return Ok(ClaimState::Recorded(payment_id));
    }
    // Crash-recovery: the winner may have recorded the payment
    // before linking it to the event. The session id on the payment
    // (`external_reference`) identifies it.
    let payments = api_auth.payments.list_by_invoice(invoice_id).await?;
    if let Some(payment) = payments
        .iter()
        .find(|payment| payment.external_reference.as_deref() == Some(session_id))
    {
        // Link it now so the next redelivery takes the fast path.
        // A failure here is harmless: the next redelivery finds the
        // payment the same way.
        let _ = api_auth
            .stripe_events
            .link_payment(event_id, payment.payment_id)
            .await;
        return Ok(ClaimState::Recorded(payment.payment_id));
    }
    match api_auth.stripe_events.received_at(event_id).await? {
        Some(at) if Utc::now().signed_duration_since(at) > STRIPE_EVENT_CLAIM_TTL => {
            Ok(ClaimState::Stale)
        }
        _ => Ok(ClaimState::InFlight),
    }
}

/// `POST /v1/stripe/webhook` — Stripe webhook endpoint.
///
/// NOT bearer-authenticated (Stripe cannot hold a staff session);
/// it is gated by the `STRIPE_WEBHOOK_SECRET` signature inside the
/// handler, so it lives outside the auth middleware like the email
/// ingest webhook. A missing or bad signature is a `400` and the
/// payload is never processed.
///
/// Only `checkout.session.completed` with `payment_status == "paid"`
/// records a payment. Every other outcome is acknowledged with
/// `200` (Stripe stops retrying a `200`): unknown event types,
/// unpaid sessions, and permanently unrecordable payments (unknown
/// invoice, bad amount, invoice no longer `Issued`). A `409` means
/// "already being processed, retry later"; a `500` means a storage
/// error, also retryable. The payment goes through the existing
/// guarded path, so a webhook payment can never over-apply.
pub(crate) async fn stripe_webhook<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
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
    let config = StripeConfig::from_env();
    let Some(webhook_secret) = config.webhook_secret.as_deref() else {
        tracing::warn!("stripe webhook received but STRIPE_WEBHOOK_SECRET is not configured");
        return problem_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Webhook not configured",
            "the Stripe webhook secret is not configured on this server".to_owned(),
            instance,
        );
    };
    let Some(signature) = headers
        .get("stripe-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "Invalid signature",
            "missing Stripe-Signature header".to_owned(),
            instance,
        );
    };
    let now_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if let Err(error) = verify_stripe_signature(webhook_secret, signature, &body, now_unix_secs) {
        tracing::warn!(%error, "stripe webhook signature verification failed");
        return problem_response(
            StatusCode::BAD_REQUEST,
            "Invalid signature",
            error.to_string(),
            instance,
        );
    }
    let event: StripeEvent = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(_) => {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "Invalid event",
                "the webhook body is not a Stripe event".to_owned(),
                instance,
            );
        }
    };
    if event.event_type != "checkout.session.completed" {
        return (StatusCode::OK, Json(json!({"received": true}))).into_response();
    }
    process_checkout_completed(api_auth, &event, &instance, correlation_id).await
}

/// Records the payment for a verified `checkout.session.completed`
/// event. `instance` is the request id for problem responses.
async fn process_checkout_completed(
    api_auth: &ApiAuth,
    event: &StripeEvent,
    instance: &str,
    correlation_id: Uuid,
) -> Response {
    let session: CheckoutSession = match serde_json::from_value(event.data.object.clone()) {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%error, event_id = %event.id, "stripe webhook session object is unparseable; acknowledged without recording");
            return (StatusCode::OK, Json(json!({"received": true}))).into_response();
        }
    };
    if session.payment_status.as_deref() != Some("paid") {
        return (StatusCode::OK, Json(json!({"received": true}))).into_response();
    }
    let Some(invoice_id) = session
        .metadata
        .get("invoice_id")
        .and_then(|value| value.parse::<Uuid>().ok())
        .map(InvoiceId::from)
    else {
        tracing::warn!(event_id = %event.id, "stripe checkout session has no invoice_id metadata; acknowledged without recording");
        return (StatusCode::OK, Json(json!({"received": true}))).into_response();
    };
    let invoice = match api_auth.invoices.get(invoice_id).await {
        Ok(invoice) => invoice,
        Err(DomainError::NotFound { .. }) => {
            tracing::warn!(event_id = %event.id, invoice_id = %invoice_id.as_uuid(), "stripe webhook references an unknown invoice; acknowledged without recording");
            return (StatusCode::OK, Json(json!({"received": true}))).into_response();
        }
        Err(error) => {
            tracing::warn!(%error, "stripe webhook invoice lookup failed");
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Storage error",
                "the invoice could not be read".to_owned(),
                instance.to_owned(),
            );
        }
    };
    // Claim the event; at most one retry after releasing a stale
    // claim. A lost claim means another delivery is processing this
    // event (409) or already recorded its payment (200).
    let mut claimed = false;
    for _ in 0..2 {
        match api_auth
            .stripe_events
            .claim(&event.id, &event.event_type, Some(invoice_id))
            .await
        {
            Ok(true) => {
                claimed = true;
                break;
            }
            Ok(false) => match claim_state(api_auth, &event.id, &session.id, invoice_id).await {
                Ok(ClaimState::Recorded(payment_id)) => {
                    return (
                        StatusCode::OK,
                        Json(json!({
                            "received": true,
                            "payment_id": payment_id.to_string(),
                        })),
                    )
                        .into_response();
                }
                Ok(ClaimState::InFlight) => {
                    return problem_response(
                        StatusCode::CONFLICT,
                        "Already processing",
                        "this webhook event is already being processed; retry later".to_owned(),
                        instance.to_owned(),
                    );
                }
                Ok(ClaimState::Stale) => {
                    if let Err(error) = api_auth.stripe_events.release(&event.id).await {
                        tracing::warn!(%error, "could not release a stale stripe event claim");
                        return problem_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Storage error",
                            "the webhook event claim could not be released".to_owned(),
                            instance.to_owned(),
                        );
                    }
                    // Loop: claim again now the stale row is gone.
                }
                Err(error) => {
                    tracing::warn!(%error, "stripe webhook claim lookup failed");
                    return problem_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Storage error",
                        "the webhook event claim could not be read".to_owned(),
                        instance.to_owned(),
                    );
                }
            },
            Err(error) => {
                tracing::warn!(%error, "stripe webhook event claim failed");
                return problem_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Storage error",
                    "the webhook event could not be claimed".to_owned(),
                    instance.to_owned(),
                );
            }
        }
    }
    if !claimed {
        return problem_response(
            StatusCode::CONFLICT,
            "Already processing",
            "this webhook event is already being processed; retry later".to_owned(),
            instance.to_owned(),
        );
    }
    let amount = match (session.amount_total, session.currency.as_deref()) {
        (Some(total), Some(currency)) => match Money::new(total, currency.to_uppercase()) {
            Ok(amount) => amount,
            Err(error) => {
                tracing::warn!(%error, event_id = %event.id, "stripe webhook session has an invalid amount or currency; acknowledged without recording");
                let _ = api_auth.stripe_events.release(&event.id).await;
                return (StatusCode::OK, Json(json!({"received": true}))).into_response();
            }
        },
        _ => {
            tracing::warn!(event_id = %event.id, "stripe webhook session has no amount or currency; acknowledged without recording");
            let _ = api_auth.stripe_events.release(&event.id).await;
            return (StatusCode::OK, Json(json!({"received": true}))).into_response();
        }
    };
    // The webhook acts as the System principal, scoped to the
    // invoice's profile — the ingest webhook's pattern. The payment
    // goes through the guarded path: coverage is re-checked against
    // the latest payments under the invoice lock, so a webhook
    // payment can never over-apply even if a manual payment landed
    // after the Checkout Session was created.
    let ctx = ActionContext {
        actor_id: ActorId::from(Uuid::nil()),
        actor_kind: ActorKind::System,
        client_profile_id: Some(invoice.client_profile_id),
        correlation_id,
    };
    let service = crate::billing::billing_service(api_auth);
    let payment = match service
        .record_payment(
            NewPayment {
                invoice_id,
                amount,
                external_reference: Some(session.id.clone()),
            },
            &ctx,
        )
        .await
    {
        Ok(payment) => payment,
        Err(DomainError::ConcurrencyConflict) => {
            // Transient: another write is in flight. Release the
            // claim so the retry re-claims and records.
            let _ = api_auth.stripe_events.release(&event.id).await;
            return problem_response(
                StatusCode::CONFLICT,
                "Concurrency conflict",
                "the payment could not be recorded because a concurrent write is in progress; retry".to_owned(),
                instance.to_owned(),
            );
        }
        Err(error @ (DomainError::Validation(_) | DomainError::NotFound { .. })) => {
            // Permanent: the invoice is no longer payable (voided,
            // already paid, amount exceeds the remaining balance).
            // Release the claim and acknowledge — retrying the same
            // event would fail the same way; the operator reconciles
            // in the Stripe dashboard.
            let _ = api_auth.stripe_events.release(&event.id).await;
            tracing::warn!(%error, event_id = %event.id, "stripe webhook payment is permanently unrecordable; acknowledged without recording");
            return (StatusCode::OK, Json(json!({"received": true}))).into_response();
        }
        Err(error) => {
            let _ = api_auth.stripe_events.release(&event.id).await;
            tracing::warn!(%error, "stripe webhook payment recording failed");
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Storage error",
                "the payment could not be recorded".to_owned(),
                instance.to_owned(),
            );
        }
    };
    if let Err(error) = api_auth
        .stripe_events
        .link_payment(&event.id, payment.payment_id)
        .await
    {
        // The payment IS recorded; a redelivery recovers through the
        // external-reference lookup, so this is a loud warn, not a
        // failure.
        tracing::warn!(%error, event_id = %event.id, payment_id = %payment.payment_id, "stripe webhook event could not be linked to its payment");
    }
    (
        StatusCode::OK,
        Json(json!({
            "received": true,
            "payment_id": payment.payment_id.to_string(),
        })),
    )
        .into_response()
}

/// Response body of `POST /v1/invoices/{id}/stripe-checkout`
/// (built in [`crate::billing`]).
#[derive(Debug, Serialize)]
pub(crate) struct CheckoutSessionResponse {
    /// Hosted Checkout URL the payer is redirected to.
    pub checkout_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed dummy key material used by EVERY Stripe test (unit and
    /// pg): env vars are process-global and tests run in parallel, so
    /// a test that set a *different* value would race. Never a real
    /// key — these values only ever reach the mock server.
    const TEST_SECRET_KEY: &str = "sk_test_dummy";
    const TEST_WEBHOOK_SECRET: &str = "whsec_test_dummy";

    /// Signs `payload` the way Stripe does, for the tests below.
    fn sign(secret: &str, timestamp: u64, payload: &[u8]) -> String {
        let mut signed = format!("{timestamp}.").into_bytes();
        signed.extend_from_slice(payload);
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("test hmac key");
        mac.update(&signed);
        format!(
            "t={timestamp},v1={}",
            hex::encode(mac.finalize().into_bytes())
        )
    }

    #[test]
    fn signature_verifies_for_a_correctly_signed_payload() {
        let payload = br#"{"id":"evt_1","type":"checkout.session.completed"}"#;
        let now = 1_800_000_000u64;
        let header = sign(TEST_WEBHOOK_SECRET, now, payload);
        assert!(verify_stripe_signature(TEST_WEBHOOK_SECRET, &header, payload, now).is_ok());
    }

    #[test]
    fn signature_rejects_a_tampered_payload() {
        let payload = br#"{"id":"evt_1","type":"checkout.session.completed"}"#;
        let now = 1_800_000_000u64;
        let header = sign(TEST_WEBHOOK_SECRET, now, payload);
        let tampered = br#"{"id":"evt_2","type":"checkout.session.completed"}"#;
        assert_eq!(
            verify_stripe_signature(TEST_WEBHOOK_SECRET, &header, tampered, now),
            Err(StripeSignatureError::SignatureMismatch)
        );
    }

    #[test]
    fn signature_rejects_a_stale_timestamp() {
        let payload = br#"{"id":"evt_1"}"#;
        let now = 1_800_000_000u64;
        // 301 seconds old: one second past the tolerance.
        let header = sign(TEST_WEBHOOK_SECRET, now - 301, payload);
        assert_eq!(
            verify_stripe_signature(TEST_WEBHOOK_SECRET, &header, payload, now),
            Err(StripeSignatureError::TimestampTooOld)
        );
    }

    #[test]
    fn signature_accepts_a_timestamp_within_tolerance() {
        let payload = br#"{"id":"evt_1"}"#;
        let now = 1_800_000_000u64;
        // 300 seconds old: exactly at the tolerance.
        let header = sign(TEST_WEBHOOK_SECRET, now - 300, payload);
        assert!(verify_stripe_signature(TEST_WEBHOOK_SECRET, &header, payload, now).is_ok());
        // A slightly future timestamp (clock skew) is also accepted.
        let header = sign(TEST_WEBHOOK_SECRET, now + 60, payload);
        assert!(verify_stripe_signature(TEST_WEBHOOK_SECRET, &header, payload, now).is_ok());
    }

    #[test]
    fn signature_rejects_a_header_without_a_v1_value() {
        let payload = br#"{"id":"evt_1"}"#;
        let now = 1_800_000_000u64;
        assert_eq!(
            verify_stripe_signature(TEST_WEBHOOK_SECRET, &format!("t={now}"), payload, now),
            Err(StripeSignatureError::MissingSignature)
        );
    }

    #[test]
    fn signature_rejects_a_header_without_a_timestamp() {
        let payload = br#"{"id":"evt_1"}"#;
        let now = 1_800_000_000u64;
        assert_eq!(
            verify_stripe_signature(TEST_WEBHOOK_SECRET, "v1=deadbeef", payload, now),
            Err(StripeSignatureError::MissingTimestamp)
        );
    }

    #[test]
    fn signature_accepts_any_matching_v1_value() {
        // Stripe sends two v1 values during a secret rotation; the
        // old one must not break verification of the new one.
        let payload = br#"{"id":"evt_1"}"#;
        let now = 1_800_000_000u64;
        let good = sign(TEST_WEBHOOK_SECRET, now, payload);
        let good_v1 = good.split("v1=").nth(1).expect("signed header has a v1");
        let header = format!("t={now},v1=deadbeef,v1={good_v1}");
        assert!(verify_stripe_signature(TEST_WEBHOOK_SECRET, &header, payload, now).is_ok());
    }

    #[test]
    fn signature_rejects_a_non_hex_v1_value() {
        let payload = br#"{"id":"evt_1"}"#;
        let now = 1_800_000_000u64;
        let header = format!("t={now},v1=not-hex-at-all");
        assert_eq!(
            verify_stripe_signature(TEST_WEBHOOK_SECRET, &header, payload, now),
            Err(StripeSignatureError::SignatureMismatch)
        );
    }

    #[test]
    fn checkout_session_form_uses_line_items_for_the_balance() {
        let config = StripeConfig {
            secret_key: Some(TEST_SECRET_KEY.to_owned()),
            webhook_secret: None,
            api_base_url: DEFAULT_STRIPE_API_BASE_URL.to_owned(),
            success_url: None,
            cancel_url: None,
        };
        let invoice_id =
            InvoiceId::from(Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0));
        let amount = Money::new(4_000, "USD".to_owned()).expect("valid money");
        let form = checkout_session_form(&config, invoice_id, amount);
        let get = |key: &str| {
            form.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(get("mode").as_deref(), Some("payment"));
        assert_eq!(
            get("line_items[0][price_data][unit_amount]").as_deref(),
            Some("4000")
        );
        assert_eq!(
            get("line_items[0][price_data][currency]").as_deref(),
            Some("usd")
        );
        assert_eq!(
            get("line_items[0][price_data][product_data][name]").as_deref(),
            Some(format!("Invoice {}", invoice_id.as_uuid()).as_str())
        );
        assert_eq!(get("line_items[0][quantity]").as_deref(), Some("1"));
        assert_eq!(
            get("metadata[invoice_id]").as_deref(),
            Some(invoice_id.as_uuid().to_string().as_str())
        );
        assert_eq!(
            get("success_url").as_deref(),
            Some(format!("http://127.0.0.1:3000/billing/{}", invoice_id.as_uuid()).as_str())
        );
        assert_eq!(
            get("cancel_url").as_deref(),
            Some(format!("http://127.0.0.1:3000/billing/{}", invoice_id.as_uuid()).as_str())
        );
        assert!(
            !form.iter().any(|(name, _)| name == "amount_total"),
            "amount_total is a Stripe response field, not a request parameter"
        );
    }

    #[test]
    fn checkout_session_form_uses_url_overrides_when_set() {
        let config = StripeConfig {
            secret_key: Some(TEST_SECRET_KEY.to_owned()),
            webhook_secret: Some(TEST_WEBHOOK_SECRET.to_owned()),
            api_base_url: DEFAULT_STRIPE_API_BASE_URL.to_owned(),
            success_url: Some("https://example.test/checkout/ok".to_owned()),
            cancel_url: Some("https://example.test/checkout/cancel".to_owned()),
        };
        let invoice_id =
            InvoiceId::from(Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0));
        let amount = Money::new(4_000, "USD".to_owned()).expect("valid money");
        let form = checkout_session_form(&config, invoice_id, amount);
        let get = |key: &str| {
            form.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            get("success_url").as_deref(),
            Some("https://example.test/checkout/ok")
        );
        assert_eq!(
            get("cancel_url").as_deref(),
            Some("https://example.test/checkout/cancel")
        );
    }

    #[test]
    fn stripe_config_reads_the_test_env_values() {
        // Same values as every other Stripe test (see the consts
        // above): never remove them, never use different ones.
        std::env::set_var(STRIPE_SECRET_KEY_ENV, TEST_SECRET_KEY);
        std::env::set_var(STRIPE_WEBHOOK_SECRET_ENV, TEST_WEBHOOK_SECRET);
        let config = StripeConfig::from_env();
        assert!(config.is_configured());
        assert_eq!(config.secret_key.as_deref(), Some(TEST_SECRET_KEY));
        assert_eq!(config.webhook_secret.as_deref(), Some(TEST_WEBHOOK_SECRET));
    }

    #[test]
    fn stripe_config_debug_never_prints_key_material() {
        let config = StripeConfig {
            secret_key: Some("sk_live_abc".to_owned()),
            webhook_secret: Some("whsec_xyz".to_owned()),
            api_base_url: DEFAULT_STRIPE_API_BASE_URL.to_owned(),
            success_url: None,
            cancel_url: None,
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk_live_abc"));
        assert!(!debug.contains("whsec_xyz"));
        assert!(debug.contains("<redacted>"));
    }
}
