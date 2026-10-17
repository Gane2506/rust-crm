// SPDX-License-Identifier: GPL-3.0-only
//! Form builder endpoints: forms, fields and submissions (task 11-FORM-02).
//!
//! * `POST /api/v1/forms` — creates a draft form (`201`).
//! * `GET /api/v1/forms?client_profile_id=…` — lists forms (`200`).
//! * `GET /api/v1/forms/{id}` — fetches a form with its fields (`200`).
//! * `POST /api/v1/forms/{id}/publish` — publishes (draft → published).
//! * `POST /api/v1/forms/{id}/archive` — archives (idempotent).
//! * `POST /api/v1/forms/{id}/fields` — adds a typed field (`201`).
//! * `GET /api/v1/forms/{id}/fields` — lists the form's fields.
//! * `DELETE /api/v1/forms/{id}/fields/{field_id}` — removes a field.
//! * `POST /api/v1/forms/{id}/submissions` — submits answers (`201`;
//!   validate-then-persist: invalid answers are rejected with `422`).
//! * `GET /api/v1/forms/{id}/submissions` — lists submissions, newest
//!   first.
//! * `POST /api/v1/forms/{id}/submissions/{submission_id}/review` —
//!   marks a submission reviewed (idempotent).
//!
//! ## Authorization
//!
//! All endpoints authorize against
//! [`Resource::Form`](crm_domain::security::Resource::Form),
//! profile-scoped. Fetch-then-authorize: a missing form surfaces as
//! `404` before any grant check. Submission intake is staff-side in
//! this slice (it records through the audit log); anonymous public
//! intake is out of scope.

use axum::{
    extract::{rejection::PathRejection, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crm_core::form::FormServiceImpl;
use crm_db::form::{PgFormFieldRepository, PgFormRepository, PgFormSubmissionRepository};
use crm_domain::form::{Form, FormAnswer, FormFieldType};
use crm_domain::ids::{ClientProfileId, FormFieldId, FormId, FormSubmissionId};
use crm_domain::repositories::{FormFieldRepository, FormRepository, FormSubmissionRepository};
use crm_domain::security::{Action, Operation, Resource};
use crm_domain::services::{AuditService, FormService, NewForm, NewFormField, NewFormSubmission};

use crate::auth::{resolve_context, RequestAuth};
use crate::contacts::{domain_error_response, request_id, AppState, IdempotencyStore};
use crate::problem_response;
use crm_core::memory::ContactServiceImpl;

/// Body of `POST /api/v1/forms`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateFormBody {
    /// Owning client profile.
    pub client_profile_id: Uuid,
    /// Human-readable form name (validated non-blank).
    pub name: String,
}

/// Query of `GET /api/v1/forms`.
#[derive(Debug, Deserialize)]
pub struct ListFormsQuery {
    /// Owning client profile (required: forms are profile-scoped).
    pub client_profile_id: Uuid,
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
}

/// Body of `POST /api/v1/forms/{id}/fields`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddFieldBody {
    /// Field kind: `text`, `email`, `phone`, `number`, `select`,
    /// `checkbox` or `date`.
    pub field_type: String,
    /// Human-readable label (validated non-blank).
    pub label: String,
    /// Whether a submission must answer this field (default false).
    #[serde(default)]
    pub required: bool,
    /// Display ordering within the form (default 0).
    #[serde(default)]
    pub position: u32,
    /// Field configuration (opaque JSON, never null).
    pub config: serde_json::Value,
}

/// One submitted answer in `POST /api/v1/forms/{id}/submissions`.
#[derive(Debug, Clone, Deserialize)]
pub struct SubmitAnswerBody {
    /// The field being answered.
    pub field_id: Uuid,
    /// The submitted value.
    pub value: String,
}

/// Body of `POST /api/v1/forms/{id}/submissions`.
#[derive(Debug, Clone, Deserialize)]
pub struct SubmitFormBody {
    /// The contact this submission is attributed to, when known
    /// (optional).
    pub contact_id: Option<Uuid>,
    /// Human-readable submitter label (validated non-blank).
    pub submitted_by_label: String,
    /// The submitted answers.
    pub answers: Vec<SubmitAnswerBody>,
}

/// `GET /api/v1/forms/{id}` response: the form plus its fields.
#[derive(Debug, Serialize)]
struct FormDetailResponse {
    form: Form,
    fields: Vec<crm_domain::form::FormField>,
}

/// Builds the form service from the API auth components.
fn form_service(
    api_auth: &crate::auth::ApiAuth,
) -> FormServiceImpl<
    PgFormRepository,
    PgFormFieldRepository,
    PgFormSubmissionRepository,
    crm_db::postgres::PgAuditService,
> {
    FormServiceImpl::new(
        api_auth.forms.clone(),
        api_auth.form_fields.clone(),
        api_auth.form_submissions.clone(),
        api_auth.audit_service.clone(),
    )
}

/// Extracts a form id from the path, or returns a 422.
/// The error side is boxed: `axum::http::Response` is a large type and
/// returning it by value trips `clippy::result_large_err`.
fn path_form_id(
    path: Result<Path<Uuid>, PathRejection>,
    instance: &str,
) -> Result<FormId, Box<Response>> {
    match path {
        Ok(Path(id)) => Ok(FormId::from(id)),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid form id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a field id from the path, or returns a 422.
fn path_field_id(
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    instance: &str,
) -> Result<(FormId, FormFieldId), Box<Response>> {
    match path {
        Ok(Path((form_id, field_id))) => Ok((FormId::from(form_id), FormFieldId::from(field_id))),
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid field id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Extracts a submission id from the path, or returns a 422.
fn path_submission_id(
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    instance: &str,
) -> Result<(FormId, FormSubmissionId), Box<Response>> {
    match path {
        Ok(Path((form_id, submission_id))) => {
            Ok((FormId::from(form_id), FormSubmissionId::from(submission_id)))
        }
        Err(rejection) => Err(Box::new(problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid submission id",
            rejection.body_text(),
            instance.to_owned(),
        ))),
    }
}

/// Parses a field type from its string form.
fn parse_field_type(raw: &str) -> Result<FormFieldType, String> {
    match raw {
        "text" => Ok(FormFieldType::TextField),
        "email" => Ok(FormFieldType::EmailField),
        "phone" => Ok(FormFieldType::PhoneField),
        "number" => Ok(FormFieldType::NumberField),
        "select" => Ok(FormFieldType::SelectField),
        "checkbox" => Ok(FormFieldType::CheckboxField),
        "date" => Ok(FormFieldType::DateField),
        other => Err(format!("unknown form field type '{other}'")),
    }
}

/// Fetches a form or returns its 404. Fetch-then-authorize: the
/// existence check runs before any grant check.
async fn fetch_form(
    api_auth: &crate::auth::ApiAuth,
    form_id: FormId,
    instance: &str,
) -> Result<Form, Box<Response>> {
    match api_auth.forms.get(form_id).await {
        Ok(form) => Ok(form),
        Err(error) => Err(Box::new(domain_error_response(error, instance.to_owned()))),
    }
}

/// Authorizes `action` against the form's client profile, or returns
/// the 401/403 response.
async fn authorize_form<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    form: &Form,
    operation: Operation,
) -> Result<crm_domain::services::ActionContext, Box<Response>>
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let action = Action {
        resource: Resource::Form,
        operation,
    };
    match resolve_context(state, auth, headers, &action, Some(form.client_profile_id)).await {
        Ok(ctx) => Ok(ctx),
        Err(response) => Err(response),
    }
}

/// `POST /api/v1/forms` — creates a draft form.
///
/// Returns `201`. Requires the `form/create` grant in the client
/// profile (`403` without it).
pub async fn create_form<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    body: Result<Json<CreateFormBody>, axum::extract::rejection::JsonRejection>,
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
        resource: Resource::Form,
        operation: Operation::Create,
    };
    let ctx = match resolve_context(&state, auth, &headers, &action, Some(profile_id)).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = form_service(api_auth);
    let input = NewForm {
        client_profile_id: profile_id,
        name: body.name,
    };
    let form = match service.create_form(input, &ctx).await {
        Ok(form) => form,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(form)).into_response()
}

/// `GET /api/v1/forms` — lists forms for a client profile.
///
/// Requires the `form/read` grant in the profile (`403` without it).
pub async fn list_forms<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    Query(query): Query<ListFormsQuery>,
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
        resource: Resource::Form,
        operation: Operation::Read,
    };
    if let Err(response) = resolve_context(&state, auth, &headers, &action, Some(profile_id)).await
    {
        return *response;
    }
    let limit = query.limit.unwrap_or(20).min(100) as u64;
    let forms = match api_auth.forms.list_by_profile(profile_id, limit).await {
        Ok(forms) => forms,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = forms.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "forms": forms, "total": total })),
    )
        .into_response()
}

/// `GET /api/v1/forms/{id}` — fetches a form with its fields.
/// Requires the `form/read` grant.
pub async fn get_form<R, A, I>(
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
    let form_id = match path_form_id(path, &instance) {
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    if let Err(response) = authorize_form(&state, auth, &headers, &form, Operation::Read).await {
        return *response;
    }
    let fields = match api_auth.form_fields.list_by_form(form_id, 10_000).await {
        Ok(fields) => fields,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(FormDetailResponse { form, fields })).into_response()
}

/// Form lifecycle transitions driven through one fetch-then-authorize
/// helper.
enum FormTransition {
    Publish,
    Archive,
}

/// Runs a form lifecycle transition with fetch-then-authorize.
/// Both transitions require the `update` operation.
async fn form_transition<R, A, I>(
    state: &AppState<ContactServiceImpl<R, A>, I>,
    auth: Option<Extension<RequestAuth>>,
    headers: &HeaderMap,
    form_id: FormId,
    transition: FormTransition,
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    let ctx = match authorize_form(state, auth, headers, &form, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = form_service(api_auth);
    let form = match match transition {
        FormTransition::Publish => service.publish_form(form_id, &ctx).await,
        FormTransition::Archive => service.archive_form(form_id, &ctx).await,
    } {
        Ok(form) => form,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(form)).into_response()
}

/// `POST /api/v1/forms/{id}/publish` — publishes a draft form so it
/// starts accepting submissions.
pub async fn publish_form<R, A, I>(
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
    let form_id = match path_form_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    form_transition(&state, auth, &headers, form_id, FormTransition::Publish).await
}

/// `POST /api/v1/forms/{id}/archive` — archives a form (idempotent).
pub async fn archive_form<R, A, I>(
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
    let form_id = match path_form_id(path, &instance) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    form_transition(&state, auth, &headers, form_id, FormTransition::Archive).await
}

/// `POST /api/v1/forms/{id}/fields` — adds a typed field.
///
/// Returns `201`. Requires the `form/update` grant.
pub async fn add_field<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<AddFieldBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let form_id = match path_form_id(path, &instance) {
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
    let field_type = match parse_field_type(&body.field_type) {
        Ok(field_type) => field_type,
        Err(detail) => {
            return problem_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid field type",
                detail,
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    let ctx = match authorize_form(&state, auth, &headers, &form, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = form_service(api_auth);
    let input = NewFormField {
        form_id,
        field_type,
        label: body.label,
        required: body.required,
        position: body.position,
        config: body.config,
    };
    let field = match service.add_field(input, &ctx).await {
        Ok(field) => field,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(field)).into_response()
}

/// `GET /api/v1/forms/{id}/fields` — lists the form's fields in
/// display order. Requires the `form/read` grant.
pub async fn list_fields<R, A, I>(
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
    let form_id = match path_form_id(path, &instance) {
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    if let Err(response) = authorize_form(&state, auth, &headers, &form, Operation::Read).await {
        return *response;
    }
    let fields = match api_auth.form_fields.list_by_form(form_id, 10_000).await {
        Ok(fields) => fields,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = fields.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "fields": fields, "total": total })),
    )
        .into_response()
}

/// `DELETE /api/v1/forms/{id}/fields/{field_id}` — removes a field.
/// Requires the `form/delete` grant.
pub async fn remove_field<R, A, I>(
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
    let (form_id, field_id) = match path_field_id(path, &instance) {
        Ok(ids) => ids,
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    // The field must belong to the form in the path; otherwise the
    // caller is addressing the wrong resource.
    let field = match api_auth.form_fields.get(field_id).await {
        Ok(field) => field,
        Err(error) => return domain_error_response(error, instance),
    };
    if field.form_id != form_id {
        return problem_response(
            StatusCode::NOT_FOUND,
            "Not found",
            format!("field {field_id} does not belong to form {form_id}"),
            instance,
        );
    }
    let ctx = match authorize_form(&state, auth, &headers, &form, Operation::Delete).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = form_service(api_auth);
    if let Err(error) = service.remove_field(field_id, &ctx).await {
        return domain_error_response(error, instance);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v1/forms/{id}/submissions` — submits answers to a form.
///
/// Validate-then-persist: answers are checked against the form's
/// fields (required answers, per-type values) before anything is
/// stored; invalid answers are a `422`. Requires the `form/create`
/// grant.
pub async fn submit_form<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<SubmitFormBody>, axum::extract::rejection::JsonRejection>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let form_id = match path_form_id(path, &instance) {
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    let ctx = match authorize_form(&state, auth, &headers, &form, Operation::Create).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = form_service(api_auth);
    let input = NewFormSubmission {
        form_id,
        submitted_by_contact: body.contact_id.map(crm_domain::ids::ContactId::from),
        submitted_by_label: body.submitted_by_label,
        answers: body
            .answers
            .into_iter()
            .map(|answer| FormAnswer {
                field_id: FormFieldId::from(answer.field_id),
                value: answer.value,
            })
            .collect(),
    };
    let submission = match service.submit(input, &ctx).await {
        Ok(submission) => submission,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::CREATED, Json(submission)).into_response()
}

/// Query of `GET /api/v1/forms/{id}/submissions`.
#[derive(Debug, Deserialize)]
pub struct ListSubmissionsQuery {
    /// Max items returned (default 20, max 100).
    pub limit: Option<u32>,
}

/// `GET /api/v1/forms/{id}/submissions` — lists submissions for a
/// form, newest first. Requires the `form/read` grant.
pub async fn list_submissions<R, A, I>(
    State(state): State<AppState<ContactServiceImpl<R, A>, I>>,
    auth: Option<Extension<RequestAuth>>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    Query(query): Query<ListSubmissionsQuery>,
) -> Response
where
    R: crm_core::memory::ContactReadModel + Clone + Send + Sync + 'static,
    A: AuditService + Clone + Send + Sync + 'static,
    I: IdempotencyStore,
{
    let (instance, _) = request_id(&headers);
    let form_id = match path_form_id(path, &instance) {
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    if let Err(response) = authorize_form(&state, auth, &headers, &form, Operation::Read).await {
        return *response;
    }
    let limit = query.limit.unwrap_or(20).min(100) as u64;
    let submissions = match api_auth.form_submissions.list_by_form(form_id, limit).await {
        Ok(submissions) => submissions,
        Err(error) => return domain_error_response(error, instance),
    };
    let total = submissions.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "submissions": submissions, "total": total })),
    )
        .into_response()
}

/// `POST /api/v1/forms/{id}/submissions/{submission_id}/review` —
/// marks a submission reviewed (idempotent). Requires the
/// `form/update` grant.
pub async fn review_submission<R, A, I>(
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
    let (form_id, submission_id) = match path_submission_id(path, &instance) {
        Ok(ids) => ids,
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
    let form = match fetch_form(api_auth, form_id, &instance).await {
        Ok(form) => form,
        Err(response) => return *response,
    };
    // The submission must belong to the form in the path; otherwise
    // the caller is addressing the wrong resource.
    let submission = match api_auth.form_submissions.get(submission_id).await {
        Ok(submission) => submission,
        Err(error) => return domain_error_response(error, instance),
    };
    if submission.form_id != form_id {
        return problem_response(
            StatusCode::NOT_FOUND,
            "Not found",
            format!("submission {submission_id} does not belong to form {form_id}"),
            instance,
        );
    }
    let ctx = match authorize_form(&state, auth, &headers, &form, Operation::Update).await {
        Ok(ctx) => ctx,
        Err(response) => return *response,
    };
    let service = form_service(api_auth);
    let submission = match service.review_submission(submission_id, &ctx).await {
        Ok(submission) => submission,
        Err(error) => return domain_error_response(error, instance),
    };
    (StatusCode::OK, Json(submission)).into_response()
}
