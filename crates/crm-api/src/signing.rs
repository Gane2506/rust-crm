// SPDX-License-Identifier: GPL-3.0-only
//! Contract signing endpoints: templates and contracts (task 13-SIGN-02).
//!
//! * `POST /api/v1/signing/templates` — creates a template version
//!   (`201`). Templates are agency-level (no profile scope).
//! * `GET /api/v1/signing/templates` — lists template versions,
//!   newest first (`200`).
//! * `POST /api/v1/contracts` — creates a draft contract from a
//!   template version, rendering the immutable document snapshot
//!   (`201`).
//! * `GET /api/v1/contracts?client_profile_id=…` — lists contracts,
//!   newest first (`200`).
//! * `GET /api/v1/contracts/{id}` — fetches a contract (`200`).
//! * `GET /api/v1/contracts/{id}/document` — fetches the rendered
//!   document snapshot (`200`).
//! * `GET /api/v1/contracts/{id}/evidence` — lists the signing
//!   evidence, oldest first (`200`).
//! * `POST /api/v1/contracts/{id}/request` — sends a draft for
//!   signature (`Draft → Sent`).
//! * `POST /api/v1/contracts/{id}/sign` — records one signature with
//!   its forensic evidence.
//! * `POST /api/v1/contracts/{id}/satisfy-payment-gate` — asserts the
//!   required payment was verified (`SignedAwaitingPayment →
//!   Effective`).
//! * `POST /api/v1/contracts/{id}/void` — cancels a pre-effective
//!   contract (idempotent).
//!
//! ## Authorization
//!
//! Contract endpoints authorize against
//! [`Resource::Contract`](crm_domain::security::Resource::Contract),
//! profile-scoped. Fetch-then-authorize: a missing contract surfaces
//! as `404` before any grant check. Template endpoints use the same
//! resource at agency scope (no profile).

use std::collections::HashMap;

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crm_core::signing::SigningServiceImpl;
use crm_db::signing::{PgContractRepository, PgSigningTemplateRepository};
use crm_domain::contract::{Contract, PaymentRequirement};
use crm_domain::ids::{ClientProfileId, ContractId, SigningTemplateId};
use crm_domain::repositories::{
    ContractDocumentRepository, ContractRepository, SigningTemplateRepository,
};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{
    AuditService, NewContract, NewSignature, NewSigningTemplate, SigningService,
};
use crm_domain::signing::SignatureMethod;

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/signing/templates`.
#[derive(Debug, Deserialize)]
pub struct CreateTemplateBody {
    /// Human-readable template name (validated non-blank).
    pub name: String,
    /// Template body with `{{placeholder}}` variables.
    pub body: String,
}

/// Body of `POST /api/v1/contracts`.
#[derive(Debug, Deserialize)]
pub struct CreateContractBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Template version to render from.
    pub template_id: Uuid,
    /// Values for the template's `{{placeholder}}` variables.
    pub variables: HashMap<String, String>,
    /// What must be paid before effectiveness.
    pub payment_requirement: PaymentRequirement,
    /// Required signers as (name, email) pairs.
    pub signers: Vec<(String, String)>,
}

/// Body of `POST /api/v1/contracts/{id}/sign`.
#[derive(Debug, Deserialize)]
pub struct SignContractBody {
    /// Signer's display name.
    pub signer_name: String,
    /// Signer's email address.
    pub signer_email: String,
    /// Signer's IP address (hashed by the domain; never persisted
    /// raw).
    pub ip_address: String,
    /// Signer's user agent (hashed by the domain; never persisted
    /// raw).
    pub user_agent: String,
    /// How the signature was applied.
    pub method: SignatureMethod,
}

/// Query of `GET /api/v1/contracts` and `GET
/// /api/v1/signing/templates`.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Owning client profile (required for contracts; ignored for
    /// templates).
    pub client_profile_id: Option<Uuid>,
    /// Max items returned (default 20).
    pub limit: Option<u64>,
}

/// Builds the signing service from the API auth components.
fn signing_service(
    api_auth: &crate::auth::ApiAuth,
) -> SigningServiceImpl<
    PgContractRepository,
    PgSigningTemplateRepository,
    crm_db::postgres::PgAuditService,
> {
    SigningServiceImpl::new(
        api_auth.contracts.clone(),
        api_auth.signing_templates.clone(),
        api_auth.audit_service.clone(),
    )
}

/// Extracts a contract id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_contract_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<ContractId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(ContractId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid contract id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Fetches a contract or returns its 404. Fetch-then-authorize: the
/// existence check runs before any grant check.
async fn fetch_contract(
    api_auth: &crate::auth::ApiAuth,
    contract_id: ContractId,
    instance: &str,
) -> Result<Contract, Box<Response>> {
    match api_auth.contracts.get(contract_id).await {
        Ok(contract) => Ok(contract),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Authorizes `operation` against the contract's client profile, or
/// returns the 401/403 response.
async fn authorize_contract<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    contract: &Contract,
    operation: Operation,
) -> Result<crm_domain::services::ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource: Resource::Contract,
        operation,
    };
    match resolve_context(
        state,
        auth,
        headers,
        &action,
        Some(contract.client_profile_id),
    )
    .await
    {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// Contract lifecycle transitions driven through one
/// fetch-then-authorize helper.
enum ContractTransition {
    Request,
    SatisfyPaymentGate,
    Void,
}

/// Runs a contract lifecycle transition with fetch-then-authorize.
async fn contract_transition<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    contract_id: ContractId,
    transition: ContractTransition,
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
    let contract = match fetch_contract(api_auth, contract_id, &instance).await {
        Ok(contract) => contract,
        Err(response) => return *response,
    };
    let ctx = match authorize_contract(state, auth, headers, &contract, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = signing_service(api_auth);
    let contract = match match transition {
        ContractTransition::Request => service.request_signature(contract_id, &ctx).await,
        ContractTransition::SatisfyPaymentGate => {
            service.satisfy_payment_gate(contract_id, &ctx).await
        }
        ContractTransition::Void => service.void_contract(contract_id, &ctx).await,
    } {
        Ok(contract) => contract,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(contract)).into_response()
}

/// `POST /api/v1/signing/templates` — creates a template version.
///
/// Templates are agency-level: requires the `contract/create` grant
/// at agency scope.
pub async fn create_template<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateTemplateBody>,
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
    let action = Action {
        resource: Resource::Contract,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, None).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = signing_service(api_auth);
    let template = match service
        .create_template(
            NewSigningTemplate {
                name: body.name,
                body: body.body,
            },
            &ctx,
        )
        .await
    {
        Ok(template) => template,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(template)).into_response()
}

/// `GET /api/v1/signing/templates` — lists template versions, newest
/// first.
///
/// Requires the `contract/read` grant at agency scope.
pub async fn list_templates<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
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
    let action = Action {
        resource: Resource::Contract,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, None).await {
        return *response;
    }
    let templates = match api_auth
        .signing_templates
        .list(query.limit.unwrap_or(20).min(100))
        .await
    {
        Ok(templates) => templates,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = templates.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "templates": templates, "total": total })),
    )
        .into_response()
}

/// `POST /api/v1/contracts` — creates a draft contract from a
/// template version.
///
/// Requires the `contract/create` grant in the profile (`403`
/// without it).
pub async fn create_contract<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Json(body): Json<CreateContractBody>,
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
    let profile_id = ClientProfileId::from(body.client_profile_id);
    let action = Action {
        resource: Resource::Contract,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = signing_service(api_auth);
    let contract = match service
        .create_contract(
            NewContract {
                client_profile_id: profile_id,
                template_id: SigningTemplateId::from(body.template_id),
                variables: body.variables,
                payment_requirement: body.payment_requirement,
                signers: body.signers,
            },
            &ctx,
        )
        .await
    {
        Ok(contract) => contract,
        Err(error) => return domain_error_response(error, instance),
    };
    // Return the rendered document snapshot alongside the contract
    // so the caller sees the exact bytes the signer will see.
    let document = match api_auth.contracts.get_document(contract.contract_id).await {
        Ok(document) => document,
        Err(error) => return domain_error_response(error, instance),
    };
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "contract": contract, "document": document })),
    )
        .into_response()
}

/// `GET /api/v1/contracts?client_profile_id=…` — lists contracts for
/// a profile, newest first.
///
/// Requires the `contract/read` grant in the profile.
pub async fn list_contracts<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
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
    let Some(profile_uuid) = query.client_profile_id else {
        return problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Missing client_profile_id",
            "contract listing requires a client_profile_id query parameter".to_owned(),
            instance,
        );
    };
    let profile_id = ClientProfileId::from(profile_uuid);
    let action = Action {
        resource: Resource::Contract,
        operation: Operation::Read,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = signing_service(api_auth);
    let contracts = match service
        .list_contracts(query.limit.unwrap_or(20).min(100), &ctx)
        .await
    {
        Ok(contracts) => contracts,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = contracts.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "contracts": contracts, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/contracts/{id}` — fetches a contract.
///
/// Requires the `contract/read` grant in the contract's profile.
pub async fn get_contract<R, A, I>(
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
    let contract_id = match path_contract_id(path, &instance) {
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
    let contract = match fetch_contract(api_auth, contract_id, &instance).await {
        Ok(contract) => contract,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_contract(&state, auth, &headers, &contract, Operation::Read).await
    {
        return *response;
    }
    (StatusCode::OK, Json(contract)).into_response()
}

/// `GET /api/v1/contracts/{id}/document` — fetches the rendered
/// document snapshot.
///
/// Requires the `contract/read` grant in the contract's profile.
pub async fn get_document<R, A, I>(
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
    let contract_id = match path_contract_id(path, &instance) {
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
    let contract = match fetch_contract(api_auth, contract_id, &instance).await {
        Ok(contract) => contract,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_contract(&state, auth, &headers, &contract, Operation::Read).await
    {
        return *response;
    }
    let document = match api_auth.contracts.get_document(contract_id).await {
        Ok(document) => document,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(document)).into_response()
}

/// `GET /api/v1/contracts/{id}/evidence` — lists the signing
/// evidence, oldest first.
///
/// Requires the `contract/read` grant in the contract's profile.
pub async fn list_evidence<R, A, I>(
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
    let contract_id = match path_contract_id(path, &instance) {
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
    let contract = match fetch_contract(api_auth, contract_id, &instance).await {
        Ok(contract) => contract,
        Err(response) => return *response,
    };
    if let Err(response) =
        authorize_contract(&state, auth, &headers, &contract, Operation::Read).await
    {
        return *response;
    }
    let evidence = match api_auth.contracts.list_evidence(contract_id).await {
        Ok(evidence) => evidence,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(evidence)).into_response()
}

/// `POST /api/v1/contracts/{id}/request` — sends a draft for
/// signature (`Draft → Sent`). Requires the `contract/update` grant.
pub async fn request_signature<R, A, I>(
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
    let contract_id = match path_contract_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    contract_transition(
        &state,
        auth,
        &headers,
        contract_id,
        ContractTransition::Request,
    )
    .await
}

/// `POST /api/v1/contracts/{id}/sign` — records one signature with
/// its forensic evidence.
///
/// Requires the `contract/update` grant in the contract's profile.
pub async fn sign_contract<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Json(body): Json<SignContractBody>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let contract_id = match path_contract_id(path, &instance) {
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
    let contract = match fetch_contract(api_auth, contract_id, &instance).await {
        Ok(contract) => contract,
        Err(response) => return *response,
    };
    let ctx = match authorize_contract(&state, auth, &headers, &contract, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = signing_service(api_auth);
    let contract = match service
        .sign_contract(
            NewSignature {
                contract_id,
                signer_name: body.signer_name,
                signer_email: body.signer_email,
                ip_address: body.ip_address,
                user_agent: body.user_agent,
                method: body.method,
            },
            &ctx,
        )
        .await
    {
        Ok(contract) => contract,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(contract)).into_response()
}

/// `POST /api/v1/contracts/{id}/satisfy-payment-gate` — asserts the
/// required payment was verified (`SignedAwaitingPayment →
/// Effective`). Requires the `contract/update` grant.
pub async fn satisfy_payment_gate<R, A, I>(
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
    let contract_id = match path_contract_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    contract_transition(
        &state,
        auth,
        &headers,
        contract_id,
        ContractTransition::SatisfyPaymentGate,
    )
    .await
}

/// `POST /api/v1/contracts/{id}/void` — cancels a pre-effective
/// contract (idempotent). Requires the `contract/update` grant.
pub async fn void_contract<R, A, I>(
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
    let contract_id = match path_contract_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    contract_transition(
        &state,
        auth,
        &headers,
        contract_id,
        ContractTransition::Void,
    )
    .await
}
