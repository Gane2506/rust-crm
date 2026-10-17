// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Form builder vertical slice (task 11-FORM-02).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.:
//! `postgres://postgres:postgres@localhost:5432/crm_test`
//!
//! ```sh
//! docker compose up -d            # ephemeral Postgres 16 on :5432
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Each test creates its own database, migrates a fresh schema, and
//! drops it afterwards. The service tests wire the real SQLx
//! repositories with a no-op audit service.

mod common;

use chrono::{SubsecRound, Utc};
use serde_json::json;
use sqlx::PgPool;

use common::{drop_db, fresh_db_raw};
use crm_core::form::FormServiceImpl;
use crm_db::form::{PgFormFieldRepository, PgFormRepository, PgFormSubmissionRepository};
use crm_db::run_migrations;
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::form::{Form, FormAnswer, FormField, FormFieldType, FormStatus, SubmissionStatus};
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, FormId};
use crm_domain::repositories::{FormFieldRepository, FormRepository, FormSubmissionRepository};
use crm_domain::services::{
    ActionContext, AuditService, FormService, NewForm, NewFormField, NewFormSubmission,
};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (forms reference it).
async fn ensure_profile(pool: &PgPool, id: ClientProfileId) {
    sqlx::query(
        "INSERT INTO client_profiles (client_profile_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(id.as_uuid())
    .execute(pool)
    .await
    .expect("insert client profile");
}

/// No-op [`AuditService`] for service tests: audit must never fail the
/// operation under test.
#[derive(Debug, Clone, Default)]
struct NoopAudit;

impl AuditService for NoopAudit {
    async fn record_action(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_kind: &str,
        target_id: &str,
        permission_decision: PermissionDecision,
        result: ActionResult,
        _before_ref: Option<&str>,
        _after_ref: Option<&str>,
    ) -> Result<AuditEvent, DomainError> {
        Ok(AuditEvent {
            event_id: AuditEventId::new(),
            occurred_at: Utc::now().round_subsecs(6),
            actor_kind: ActorKind::System,
            actor_id: ctx.actor_id,
            client_profile_id: ctx.client_profile_id,
            action: action.to_owned(),
            target_kind: target_kind.to_owned(),
            target_id: target_id.to_owned(),
            correlation_id: ctx.correlation_id,
            permission_decision,
            result,
            before_ref: None,
            after_ref: None,
            chain_hash: [0u8; 32],
        })
    }
}

/// Builds the three form repositories over `pool`.
fn repos(
    pool: &PgPool,
) -> (
    PgFormRepository,
    PgFormFieldRepository,
    PgFormSubmissionRepository,
) {
    (
        PgFormRepository::new(pool.clone()),
        PgFormFieldRepository::new(pool.clone()),
        PgFormSubmissionRepository::new(pool.clone()),
    )
}

/// Builds the real service over `pool` with a no-op audit service.
fn service(
    pool: &PgPool,
) -> FormServiceImpl<PgFormRepository, PgFormFieldRepository, PgFormSubmissionRepository, NoopAudit>
{
    let (forms, fields, submissions) = repos(pool);
    FormServiceImpl::new(forms, fields, submissions, NoopAudit)
}

/// Test action context scoped to `client_profile_id`.
fn ctx_for(profile_id: ClientProfileId) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::new(),
        client_profile_id: Some(profile_id),
        correlation_id: uuid::Uuid::new_v4(),
    }
}

#[tokio::test]
async fn form_repo_crud_round_trips() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let (forms, _, _) = repos(&pool);

    // Create + get.
    let form = forms
        .create(Form::new(profile, "Lead capture".to_string()).expect("valid form"))
        .await
        .expect("create form");
    let fetched = forms.get(form.form_id).await.expect("get form");
    assert_eq!(fetched.name, "Lead capture");
    assert_eq!(fetched.status, FormStatus::Draft);

    // Update persists the lifecycle change.
    let mut published = fetched.clone();
    published.publish().expect("publish");
    let updated = forms.update(published).await.expect("update form");
    assert_eq!(updated.status, FormStatus::Published);

    // Missing id surfaces as NotFound.
    let missing = forms.get(FormId::new()).await;
    assert!(matches!(missing, Err(DomainError::NotFound { .. })));

    // List by profile, newest first.
    forms
        .create(Form::new(profile, "Older".to_string()).expect("valid"))
        .await
        .expect("create older");
    let listed = forms.list_by_profile(profile, 10).await.expect("list");
    assert!(listed.len() >= 2);
    let names: Vec<&str> = listed.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"Lead capture"));
    assert!(names.contains(&"Older"));

    // Another profile's form is invisible.
    let other = ClientProfileId::new();
    ensure_profile(&pool, other).await;
    let listed_other = forms.list_by_profile(other, 10).await.expect("list other");
    assert!(listed_other.is_empty());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn field_repo_crud_round_trips() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let (forms, fields, _) = repos(&pool);

    let form = forms
        .create(Form::new(profile, "Intake".to_string()).expect("valid"))
        .await
        .expect("create form");

    // Create + get round-trips every column.
    let field = fields
        .create(
            FormField::new(
                form.form_id,
                FormFieldType::EmailField,
                "Work email".to_string(),
                true,
                1,
                json!({}),
            )
            .expect("valid field"),
        )
        .await
        .expect("create field");
    let fetched = fields.get(field.field_id).await.expect("get field");
    assert_eq!(fetched, field);
    assert_eq!(fetched.field_type, FormFieldType::EmailField);
    assert!(fetched.required);
    assert_eq!(fetched.position, 1);

    // Delete + NotFound.
    fields.delete(field.field_id).await.expect("delete field");
    let missing = fields.get(field.field_id).await;
    assert!(matches!(missing, Err(DomainError::NotFound { .. })));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_full_lifecycle_with_validation() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let service = service(&pool);
    let ctx = ctx_for(profile);

    // Create → publish.
    let form = service
        .create_form(
            NewForm {
                client_profile_id: profile,
                name: "Lead capture".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create form");
    let form = service
        .publish_form(form.form_id, &ctx)
        .await
        .expect("publish form");
    assert_eq!(form.status, FormStatus::Published);

    // Add typed fields.
    let name = service
        .add_field(
            NewFormField {
                form_id: form.form_id,
                field_type: FormFieldType::TextField,
                label: "Full name".to_string(),
                required: true,
                position: 0,
                config: json!({}),
            },
            &ctx,
        )
        .await
        .expect("add text field");
    let email = service
        .add_field(
            NewFormField {
                form_id: form.form_id,
                field_type: FormFieldType::EmailField,
                label: "Work email".to_string(),
                required: false,
                position: 1,
                config: json!({}),
            },
            &ctx,
        )
        .await
        .expect("add email field");

    // Missing required answer → rejected before persisting.
    let invalid = service
        .submit(
            NewFormSubmission {
                form_id: form.form_id,
                submitted_by_contact: None,
                submitted_by_label: "Web form".to_string(),
                answers: vec![FormAnswer {
                    field_id: email.field_id,
                    value: "ada@example.com".to_string(),
                }],
            },
            &ctx,
        )
        .await;
    assert!(matches!(invalid, Err(DomainError::Validation(_))));

    // Invalid per-type value → rejected.
    let bad_email = service
        .submit(
            NewFormSubmission {
                form_id: form.form_id,
                submitted_by_contact: None,
                submitted_by_label: "Web form".to_string(),
                answers: vec![
                    FormAnswer {
                        field_id: name.field_id,
                        value: "Ada".to_string(),
                    },
                    FormAnswer {
                        field_id: email.field_id,
                        value: "not-an-email".to_string(),
                    },
                ],
            },
            &ctx,
        )
        .await;
    assert!(matches!(bad_email, Err(DomainError::Validation(_))));

    // Valid submission → persisted with answers intact.
    let submission = service
        .submit(
            NewFormSubmission {
                form_id: form.form_id,
                submitted_by_contact: None,
                submitted_by_label: "Ada Lovelace".to_string(),
                answers: vec![
                    FormAnswer {
                        field_id: name.field_id,
                        value: "Ada".to_string(),
                    },
                    FormAnswer {
                        field_id: email.field_id,
                        value: "ada@example.com".to_string(),
                    },
                ],
            },
            &ctx,
        )
        .await
        .expect("submit form");
    assert_eq!(submission.status, SubmissionStatus::New);
    assert_eq!(submission.answers.len(), 2);

    // Answers survive the Postgres round-trip.
    let listed = service
        .list_submissions(form.form_id, 10, &ctx)
        .await
        .expect("list submissions");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].answers.len(), 2);
    assert_eq!(listed[0].submitted_by_label, "Ada Lovelace");

    // Review lifecycle.
    let reviewed = service
        .review_submission(submission.submission_id, &ctx)
        .await
        .expect("review submission");
    assert_eq!(reviewed.status, SubmissionStatus::Reviewed);

    // Remove a field.
    service
        .remove_field(email.field_id, &ctx)
        .await
        .expect("remove field");

    // Archive is idempotent.
    let archived = service
        .archive_form(form.form_id, &ctx)
        .await
        .expect("archive form");
    assert_eq!(archived.status, FormStatus::Archived);
    let archived_again = service
        .archive_form(form.form_id, &ctx)
        .await
        .expect("archive again");
    assert_eq!(archived_again.status, FormStatus::Archived);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn submit_rejects_answers_for_other_forms() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let service = service(&pool);
    let ctx = ctx_for(profile);

    // Form A with a required field.
    let form_a = service
        .create_form(
            NewForm {
                client_profile_id: profile,
                name: "Form A".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create form A");
    let field_a = service
        .add_field(
            NewFormField {
                form_id: form_a.form_id,
                field_type: FormFieldType::TextField,
                label: "Name".to_string(),
                required: true,
                position: 0,
                config: json!({}),
            },
            &ctx,
        )
        .await
        .expect("add field A");

    // Form B with its own field.
    let form_b = service
        .create_form(
            NewForm {
                client_profile_id: profile,
                name: "Form B".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create form B");
    let field_b = service
        .add_field(
            NewFormField {
                form_id: form_b.form_id,
                field_type: FormFieldType::TextField,
                label: "Other".to_string(),
                required: false,
                position: 0,
                config: json!({}),
            },
            &ctx,
        )
        .await
        .expect("add field B");

    // Submitting to form B with form A's required field answered →
    // form B's validation must not leak across forms.
    let cross = service
        .submit(
            NewFormSubmission {
                form_id: form_b.form_id,
                submitted_by_contact: None,
                submitted_by_label: "Web form".to_string(),
                answers: vec![
                    FormAnswer {
                        field_id: field_b.field_id,
                        value: "x".to_string(),
                    },
                    FormAnswer {
                        field_id: field_a.field_id,
                        value: "Ada".to_string(),
                    },
                ],
            },
            &ctx,
        )
        .await;
    assert!(matches!(cross, Err(DomainError::Validation(_))));

    // Sanity: a valid submission to form A works.
    let ok = service
        .submit(
            NewFormSubmission {
                form_id: form_a.form_id,
                submitted_by_contact: None,
                submitted_by_label: "Web form".to_string(),
                answers: vec![FormAnswer {
                    field_id: field_a.field_id,
                    value: "Ada".to_string(),
                }],
            },
            &ctx,
        )
        .await;
    assert!(ok.is_ok());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_enforces_scope_isolation() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;
    let service = service(&pool);
    let ctx_a = ctx_for(profile_a);
    let ctx_b = ctx_for(profile_b);

    let form = service
        .create_form(
            NewForm {
                client_profile_id: profile_a,
                name: "Private".to_string(),
            },
            &ctx_a,
        )
        .await
        .expect("create form");

    // A context scoped to another profile cannot touch the form.
    let denied = service.publish_form(form.form_id, &ctx_b).await;
    assert!(matches!(denied, Err(DomainError::Forbidden(_))));
    let denied = service
        .add_field(
            NewFormField {
                form_id: form.form_id,
                field_type: FormFieldType::TextField,
                label: "X".to_string(),
                required: false,
                position: 0,
                config: json!({}),
            },
            &ctx_b,
        )
        .await;
    assert!(matches!(denied, Err(DomainError::Forbidden(_))));
    let denied = service
        .submit(
            NewFormSubmission {
                form_id: form.form_id,
                submitted_by_contact: None,
                submitted_by_label: "Web form".to_string(),
                answers: vec![],
            },
            &ctx_b,
        )
        .await;
    assert!(matches!(denied, Err(DomainError::Forbidden(_))));

    // An agency-scoped context (None) may operate (grant checks happen
    // upstream, at the HTTP layer).
    let agency_ctx = ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::new(),
        client_profile_id: None,
        correlation_id: uuid::Uuid::new_v4(),
    };
    let published = service
        .publish_form(form.form_id, &agency_ctx)
        .await
        .expect("agency ctx may publish");
    assert_eq!(published.status, FormStatus::Published);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn submission_contact_attribution_persists() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let (_, _, submissions) = repos(&pool);
    let service = service(&pool);
    let ctx = ctx_for(profile);

    let form = service
        .create_form(
            NewForm {
                client_profile_id: profile,
                name: "Attributed".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create form");

    // Insert a contact to attribute the submission to.
    let contact_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO contacts (client_profile_id, contact_id, lifecycle, first_source, \
         latest_source) VALUES ($1, gen_random_uuid(), 'prospect', 'manual', 'manual') RETURNING \
         contact_id",
    )
    .bind(profile.as_uuid())
    .fetch_one(&pool)
    .await
    .expect("insert contact");

    let submission = service
        .submit(
            NewFormSubmission {
                form_id: form.form_id,
                submitted_by_contact: Some(crm_domain::ids::ContactId::from(contact_id)),
                submitted_by_label: "Ada Lovelace".to_string(),
                answers: vec![],
            },
            &ctx,
        )
        .await
        .expect("submit");
    let fetched = submissions
        .get(submission.submission_id)
        .await
        .expect("get submission");
    assert_eq!(
        fetched.submitted_by_contact,
        Some(crm_domain::ids::ContactId::from(contact_id))
    );

    drop_db(&admin_url, &db_name, pool).await;
}
