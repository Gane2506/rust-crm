// SPDX-License-Identifier: GPL-3.0-only
//! Billing endpoints: invoices and payment records (task 12-BILL-02).
//!
//! All `POST` endpoints accept the `Idempotency-Key` header (a UUID):
//! a repeated request with the same key and payload replays the
//! original response instead of executing again; the same key with a
//! different payload is a `409`. A key identifies one operation: the
//! route is part of the fingerprint, so reusing a key on a different
//! route is also a `409`, never a silent replay of the other route's
//! response. The replay check runs after authorization, so a stored
//! response is only visible to a caller authorized for the operation.
//!
//! * `POST /api/v1/invoices` — creates a draft invoice (`201`).
//! * `GET /api/v1/invoices?client_profile_id=…` — lists invoices,
//!   newest first (`200`).
//! * `GET /api/v1/invoices/{id}` — fetches an invoice with its lines
//!   (`200`).
//! * `POST /api/v1/invoices/{id}/lines` — adds a line to a draft
//!   invoice (`201`).
//! * `POST /api/v1/invoices/{id}/issue` — issues a draft invoice.
//! * `POST /api/v1/invoices/{id}/void` — voids a draft or issued
//!   invoice (idempotent).
//! * `GET /api/v1/invoices/{id}/balance` — amount still owed (`200`).
//! * `POST /api/v1/invoices/{id}/payments` — records a payment
//!   (`201`); a fully-covering payment transitions the invoice to
//!   `Paid`.
//! * `GET /api/v1/invoices/{id}/payments` — lists payments, oldest
//!   first (`200`).
//! * `POST /api/v1/payments/{id}/reverse` — reverses a payment
//!   (idempotent); reopens a `Paid` invoice left with a balance.
//!
//! ## Authorization
//!
//! All endpoints authorize against
//! [`Resource::Invoice`](crm_domain::security::Resource::Invoice),
//! profile-scoped. Fetch-then-authorize: a missing invoice surfaces
//! as `404` before any grant check.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_core::billing::BillingServiceImpl;
use crm_core::IdempotencyKey;
use crm_db::billing::{PgInvoiceRepository, PgPaymentRepository};
use crm_domain::billing::{Invoice, Money, Payment};
use crm_domain::ids::{ClientProfileId, InvoiceId, PaymentId};
use crm_domain::repositories::{InvoiceRepository, PaymentRepository};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{AuditService, BillingService, NewInvoice, NewInvoiceLine, NewPayment};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{
    domain_error_response, idempotency_key, request_id, AppState, IdempotencyStore,
};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/invoices`.
#[derive(Debug, Deserialize, Serialize)]
pub struct CreateInvoiceBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// ISO 4217 currency code for every line and the total.
    pub currency: String,
}

/// Body of `POST /api/v1/invoices/{id}/lines`.
#[derive(Debug, Deserialize, Serialize)]
pub struct AddLineBody {
    /// Human-readable description (validated non-blank).
    pub description: String,
    /// Quantity billed (positive).
    pub quantity: u32,
    /// Price per unit. Deserializes through `Money::new`, so an
    /// invalid amount or currency is a `422`.
    pub unit_price: Money,
}

/// Body of `POST /api/v1/invoices/{id}/payments`.
#[derive(Debug, Deserialize, Serialize)]
pub struct RecordPaymentBody {
    /// Amount paid. Deserializes through `Money::new`.
    pub amount: Money,
}

/// Query of `GET /api/v1/invoices`.
#[derive(Debug, Deserialize)]
pub struct ListInvoicesQuery {
    /// Owning client profile (required: invoices are profile-scoped).
    pub client_profile_id: Uuid,
    /// Max invoices returned (default 20).
    pub limit: Option<u64>,
}

/// Builds the billing service from the API auth components.
fn billing_service(
    api_auth: &crate::auth::ApiAuth,
) -> BillingServiceImpl<PgInvoiceRepository, PgPaymentRepository, crm_db::postgres::PgAuditService>
{
    BillingServiceImpl::new(
        api_auth.invoices.clone(),
        api_auth.payments.clone(),
        api_auth.audit_service.clone(),
    )
}

/// Fingerprints a billing write request so a reused idempotency key with
/// a *different* payload is rejected instead of replayed.
///
/// The fingerprint includes the route: a key issued for
/// `POST /api/v1/invoices` must never replay against
/// `POST /api/v1/invoices/{id}/payments`.
///
/// The fingerprint is over the parsed-then-reserialized payload, so it
/// is stable across insignificant whitespace differences in the raw
/// JSON.
fn fingerprint(route: &str, payload: &impl Serialize) -> u64 {
    let mut hasher = DefaultHasher::new();
    route.hash(&mut hasher);
    serde_json::to_string(payload)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// Replays the stored response when `key` was already seen with the same
/// fingerprint. Returns `Ok(None)` for a fresh key; a key seen with a
/// different payload is a `409` response, never a silent replay.
async fn replay_if_seen<I>(
    store: &I,
    key: &IdempotencyKey,
    fp: u64,
    status: StatusCode,
    instance: &str,
) -> Result<Option<Response>, Box<Response>>
where
    I: IdempotencyStore,
{
    match store.lookup(key, fp).await {
        Ok(Some(stored)) => Ok(Some((status, Json(stored)).into_response())),
        Ok(None) => Ok(None),
        // The error side is boxed: `axum::http::Response` is a large type
        // and returning it by value trips `clippy::result_large_err`.
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Persists an idempotent response after a successful write. The write
/// already happened, so a persistence failure is logged loudly instead
/// of failing the request the client is waiting on — losing the
/// response would be worse than losing the replay. Making the write
/// and the idempotency record atomic is a later hardening task.
async fn record_response<I>(store: &I, key: IdempotencyKey, fp: u64, body: serde_json::Value)
where
    I: IdempotencyStore,
{
    if let Err(error) = store.record(key, fp, body).await {
        tracing::warn!(%error, "idempotency response could not be persisted");
    }
}

/// Serializes a successful write result for the idempotency store. A
/// serialization failure is a `500`: the value about to be returned to
/// the client must be storable for replay.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn storable_value(
    value: &impl Serialize,
    what: &str,
    instance: &str,
) -> Result<serde_json::Value, Box<Response>> {
    serde_json::to_value(value).map_err(|_| {
        Box::new(problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Serialization failed",
            format!("the created {what} could not be serialized"),
            instance.to_owned(),
        ))
    })
}

/// Extracts an invoice id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_invoice_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<InvoiceId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(InvoiceId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid invoice id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a payment id from the path, or returns a 422.
fn path_payment_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<PaymentId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(PaymentId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid payment id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Fetches an invoice or returns its 404. Fetch-then-authorize: the
/// existence check runs before any grant check.
async fn fetch_invoice(
    api_auth: &crate::auth::ApiAuth,
    invoice_id: InvoiceId,
    instance: &str,
) -> Result<Invoice, Box<Response>> {
    match api_auth.invoices.get(invoice_id).await {
        Ok(invoice) => Ok(invoice),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Fetches a payment or returns its 404.
async fn fetch_payment(
    api_auth: &crate::auth::ApiAuth,
    payment_id: PaymentId,
    instance: &str,
) -> Result<Payment, Box<Response>> {
    match api_auth.payments.get(payment_id).await {
        Ok(payment) => Ok(payment),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Authorizes `operation` against the invoice's client profile, or
/// returns the 401/403 response.
async fn authorize_invoice<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    invoice: &Invoice,
    operation: Operation,
) -> Result<crm_domain::services::ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource: Resource::Invoice,
        operation,
    };
    match resolve_context(
        state,
        auth,
        headers,
        &action,
        Some(invoice.client_profile_id),
    )
    .await
    {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// Invoice lifecycle transitions driven through one
/// fetch-then-authorize helper. Both require the `update` operation.
enum InvoiceTransition {
    Issue,
    Void,
}

/// Runs an invoice lifecycle transition with fetch-then-authorize.
async fn invoice_transition<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    invoice_id: InvoiceId,
    transition: InvoiceTransition,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(headers);
    let key = match idempotency_key(headers, &instance) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let route = match &transition {
        InvoiceTransition::Issue => "POST /api/v1/invoices/{id}/issue",
        InvoiceTransition::Void => "POST /api/v1/invoices/{id}/void",
    };
    let fp = fingerprint(route, &invoice_id.as_uuid());
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let invoice = match fetch_invoice(api_auth, invoice_id, &instance).await {
        Ok(invoice) => invoice,
        Err(response) => return *response,
    };
    let ctx = match authorize_invoice(state, auth, headers, &invoice, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // The idempotency replay runs AFTER authorization: the stored
    // response must only be visible to a caller authorized for
    // this operation.
    if let Some(key) = &key {
        match replay_if_seen(
            state.idempotency_store(),
            key,
            fp,
            StatusCode::OK,
            &instance,
        )
        .await
        {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(response) => return *response,
        }
    }
    let service = billing_service(api_auth);
    let invoice = match match transition {
        InvoiceTransition::Issue => service.issue_invoice(invoice_id, &ctx).await,
        InvoiceTransition::Void => service.void_invoice(invoice_id, &ctx).await,
    } {
        Ok(invoice) => invoice,
        Err(error) => return domain_error_response(error, instance),
    };
    let stored = match storable_value(&invoice, "invoice", &instance) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if let Some(key) = key {
        record_response(state.idempotency_store(), key, fp, stored).await;
    }
    (StatusCode::OK, Json(invoice)).into_response()
}

/// `POST /api/v1/invoices` — creates a draft invoice.
///
/// Returns `201` with the created invoice. When `Idempotency-Key` is
/// supplied, a repeated request with the same key and body replays the
/// original `201` without re-executing; the same key with a different
/// body is a `409`.
///
/// Requires the `invoice/create` grant in the profile (`403`
/// without it).
pub async fn create_invoice<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateInvoiceBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let key = match idempotency_key(&headers, &instance) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let fp = fingerprint("POST /api/v1/invoices", &body);
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
        resource: Resource::Invoice,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // The idempotency replay runs AFTER authorization: the stored
    // response must only be visible to a caller authorized for
    // this operation.
    if let Some(key) = &key {
        match replay_if_seen(
            state.idempotency_store(),
            key,
            fp,
            StatusCode::CREATED,
            &instance,
        )
        .await
        {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(response) => return *response,
        }
    }
    let service = billing_service(api_auth);
    let invoice = match service
        .create_invoice(
            NewInvoice {
                client_profile_id: profile_id,
                currency: body.currency,
            },
            &ctx,
        )
        .await
    {
        Ok(invoice) => invoice,
        Err(error) => return domain_error_response(error, instance),
    };
    let stored = match storable_value(&invoice, "invoice", &instance) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if let Some(key) = key {
        record_response(state.idempotency_store(), key, fp, stored).await;
    }
    (StatusCode::CREATED, Json(invoice)).into_response()
}

/// `GET /api/v1/invoices?client_profile_id=…` — lists invoices for a
/// profile, newest first.
///
/// Requires the `invoice/read` grant in the profile.
pub async fn list_invoices<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListInvoicesQuery>,
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
        resource: Resource::Invoice,
        operation: Operation::Read,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = billing_service(api_auth);
    let invoices = match service
        .list_invoices(query.limit.unwrap_or(20).min(100), &ctx)
        .await
    {
        Ok(invoices) => invoices,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = invoices.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "invoices": invoices, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/invoices/{id}` — fetches an invoice with its lines.
///
/// Requires the `invoice/read` grant in the invoice's profile.
pub async fn get_invoice<R, A, I>(
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
    let invoice_id = match path_invoice_id(path, &instance) {
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
    let invoice = match fetch_invoice(api_auth, invoice_id, &instance).await {
        Ok(invoice) => invoice,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_invoice(&state, auth, &headers, &invoice, Operation::Read).await
    {
        return *response;
    }
    (StatusCode::OK, Json(invoice)).into_response()
}

/// `POST /api/v1/invoices/{id}/lines` — adds a line to a draft
/// invoice.
///
/// Returns `201` with the created line. Supports `Idempotency-Key`
/// like [`create_invoice`]: a repeated request with the same key and
/// body replays the original `201`.
///
/// Requires the `invoice/update` grant in the invoice's profile.
pub async fn add_invoice_line<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<AddLineBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let invoice_id = match path_invoice_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let key = match idempotency_key(&headers, &instance) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let fp = fingerprint(
        "POST /api/v1/invoices/{id}/lines",
        &(invoice_id.as_uuid(), &body),
    );
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let invoice = match fetch_invoice(api_auth, invoice_id, &instance).await {
        Ok(invoice) => invoice,
        Err(response) => return *response,
    };
    let ctx = match authorize_invoice(&state, auth, &headers, &invoice, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // The idempotency replay runs AFTER authorization: the stored
    // response must only be visible to a caller authorized for
    // this operation.
    if let Some(key) = &key {
        match replay_if_seen(
            state.idempotency_store(),
            key,
            fp,
            StatusCode::CREATED,
            &instance,
        )
        .await
        {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(response) => return *response,
        }
    }
    let service = billing_service(api_auth);
    let line = match service
        .add_line(
            NewInvoiceLine {
                invoice_id,
                description: body.description,
                quantity: body.quantity,
                unit_price: body.unit_price,
            },
            &ctx,
        )
        .await
    {
        Ok(line) => line,
        Err(error) => return domain_error_response(error, instance),
    };
    let stored = match storable_value(&line, "invoice line", &instance) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if let Some(key) = key {
        record_response(state.idempotency_store(), key, fp, stored).await;
    }
    (StatusCode::CREATED, Json(line)).into_response()
}

/// `POST /api/v1/invoices/{id}/issue` — issues a draft invoice.
/// Supports `Idempotency-Key` like [`create_invoice`].
/// Requires the `invoice/update` grant.
pub async fn issue_invoice<R, A, I>(
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
    let invoice_id = match path_invoice_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    invoice_transition(&state, auth, &headers, invoice_id, InvoiceTransition::Issue).await
}

/// `POST /api/v1/invoices/{id}/void` — voids a draft or issued
/// invoice (idempotent). Supports `Idempotency-Key` like
/// [`create_invoice`]. Requires the `invoice/update` grant.
pub async fn void_invoice<R, A, I>(
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
    let invoice_id = match path_invoice_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    invoice_transition(&state, auth, &headers, invoice_id, InvoiceTransition::Void).await
}

/// `GET /api/v1/invoices/{id}/balance` — amount still owed.
///
/// Requires the `invoice/read` grant in the invoice's profile.
pub async fn invoice_balance<R, A, I>(
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
    let invoice_id = match path_invoice_id(path, &instance) {
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
    let invoice = match fetch_invoice(api_auth, invoice_id, &instance).await {
        Ok(invoice) => invoice,
        Err(response) => return *response,
    };
    let ctx = match authorize_invoice(&state, auth, &headers, &invoice, Operation::Read).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = billing_service(api_auth);
    let balance = match service.invoice_balance(invoice_id, &ctx).await {
        Ok(balance) => balance,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(balance)).into_response()
}

/// `POST /api/v1/invoices/{id}/payments` — records a payment.
///
/// A fully-covering payment transitions the invoice to `Paid`.
/// Returns `201` with the recorded payment. Supports `Idempotency-Key`
/// like [`create_invoice`]: a repeated request with the same key and
/// body replays the original `201`.
///
/// Requires the `invoice/update` grant in the invoice's profile.
pub async fn record_payment<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<RecordPaymentBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let invoice_id = match path_invoice_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let key = match idempotency_key(&headers, &instance) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let fp = fingerprint(
        "POST /api/v1/invoices/{id}/payments",
        &(invoice_id.as_uuid(), &body),
    );
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    let invoice = match fetch_invoice(api_auth, invoice_id, &instance).await {
        Ok(invoice) => invoice,
        Err(response) => return *response,
    };
    let ctx = match authorize_invoice(&state, auth, &headers, &invoice, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // The idempotency replay runs AFTER authorization: the stored
    // response must only be visible to a caller authorized for
    // this operation.
    if let Some(key) = &key {
        match replay_if_seen(
            state.idempotency_store(),
            key,
            fp,
            StatusCode::CREATED,
            &instance,
        )
        .await
        {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(response) => return *response,
        }
    }
    let service = billing_service(api_auth);
    let payment = match service
        .record_payment(
            NewPayment {
                invoice_id,
                amount: body.amount,
            },
            &ctx,
        )
        .await
    {
        Ok(payment) => payment,
        Err(error) => return domain_error_response(error, instance),
    };
    let stored = match storable_value(&payment, "payment", &instance) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if let Some(key) = key {
        record_response(state.idempotency_store(), key, fp, stored).await;
    }
    (StatusCode::CREATED, Json(payment)).into_response()
}

/// `GET /api/v1/invoices/{id}/payments` — lists payments for an
/// invoice, oldest first.
///
/// Requires the `invoice/read` grant in the invoice's profile.
pub async fn list_payments<R, A, I>(
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
    let invoice_id = match path_invoice_id(path, &instance) {
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
    let invoice = match fetch_invoice(api_auth, invoice_id, &instance).await {
        Ok(invoice) => invoice,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_invoice(&state, auth, &headers, &invoice, Operation::Read).await
    {
        return *response;
    }
    let payments = match api_auth.payments.list_by_invoice(invoice_id).await {
        Ok(payments) => payments,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = payments.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "payments": payments, "total": total })),
    )
        .into_response()
}

/// `POST /api/v1/payments/{id}/reverse` — reverses a payment
/// (idempotent). If the reversal leaves a `Paid` invoice with a
/// balance, the invoice reopens to `Issued`.
///
/// Supports `Idempotency-Key` like [`create_invoice`]: a repeated
/// request with the same key replays the original `200`.
///
/// Requires the `invoice/update` grant in the invoice's profile.
pub async fn reverse_payment<R, A, I>(
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
    let payment_id = match path_payment_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let key = match idempotency_key(&headers, &instance) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let fp = fingerprint("POST /api/v1/payments/{id}/reverse", &payment_id.as_uuid());
    let Some(api_auth) = &state.auth else {
        return problem_response(
            StatusCode::NOT_IMPLEMENTED,
            "Not implemented",
            "authentication is not configured on this server".to_owned(),
            instance,
        );
    };
    // Fetch-then-authorize through the payment's invoice: the grant is
    // on the invoice, not on the payment.
    let payment = match fetch_payment(api_auth, payment_id, &instance).await {
        Ok(payment) => payment,
        Err(response) => return *response,
    };
    let invoice = match fetch_invoice(api_auth, payment.invoice_id, &instance).await {
        Ok(invoice) => invoice,
        Err(response) => return *response,
    };
    let ctx = match authorize_invoice(&state, auth, &headers, &invoice, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    // The idempotency replay runs AFTER authorization: the stored
    // response must only be visible to a caller authorized for
    // this operation.
    if let Some(key) = &key {
        match replay_if_seen(
            state.idempotency_store(),
            key,
            fp,
            StatusCode::OK,
            &instance,
        )
        .await
        {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(response) => return *response,
        }
    }
    let service = billing_service(api_auth);
    let payment = match service.reverse_payment(payment_id, &ctx).await {
        Ok(payment) => payment,
        Err(error) => return domain_error_response(error, instance),
    };
    let stored = match storable_value(&payment, "payment", &instance) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if let Some(key) = key {
        record_response(state.idempotency_store(), key, fp, stored).await;
    }
    (StatusCode::OK, Json(payment)).into_response()
}
