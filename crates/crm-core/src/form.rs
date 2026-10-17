// SPDX-License-Identifier: GPL-3.0-only
//! Form builder service implementation (task 11-FORM-02).
//!
//! [`FormServiceImpl`] implements
//! [`FormService`](crm_domain::services::FormService) over generic
//! form, field, submission and audit ports — the same composition
//! pattern as [`WorkflowServiceImpl`](crate::workflow::WorkflowServiceImpl).
//! The production wiring (SQLx repositories) is assembled by the API
//! layer.
//!
//! Key behaviors:
//! * [`FormServiceImpl::submit`] runs
//!   [`FormSubmission::validate_against`](crm_domain::form::FormSubmission::validate_against)
//!   **before** persisting (validate-then-persist): invalid answers
//!   never reach storage;
//! * every mutation records an [`AuditEvent`](crm_domain::audit::AuditEvent)
//!   with its pinned action name;
//! * audit failures are swallowed: audit is fail-loud at the HTTP
//!   layer, never fails the operation.

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::form::{Form, FormField, FormSubmission};
use crm_domain::ids::{ClientProfileId, FormFieldId, FormId, FormSubmissionId};
use crm_domain::repositories::{FormFieldRepository, FormRepository, FormSubmissionRepository};
use crm_domain::services::{
    ActionContext, AuditService, FormService, NewForm, NewFormField, NewFormSubmission,
};

/// Generic [`FormService`] implementation.
///
/// Type parameters: `F` form repository, `D` field repository, `S`
/// submission repository, `A` audit service.
pub struct FormServiceImpl<F, D, S, A> {
    forms: F,
    fields: D,
    submissions: S,
    audit: A,
}

impl<F, D, S, A> FormServiceImpl<F, D, S, A> {
    /// Creates the service over the given ports.
    pub fn new(forms: F, fields: D, submissions: S, audit: A) -> Self {
        Self {
            forms,
            fields,
            submissions,
            audit,
        }
    }

    /// Enforces profile scope: a profile-scoped context may only touch
    /// forms of its own profile; an agency-scoped context (`None`) may
    /// touch any profile's forms (grant checks happen upstream, at the
    /// HTTP layer).
    fn check_scope(
        ctx: &ActionContext,
        client_profile_id: ClientProfileId,
    ) -> Result<(), DomainError> {
        match ctx.client_profile_id {
            Some(ctx_profile) if ctx_profile != client_profile_id => Err(DomainError::Forbidden(
                "operation crosses client-profile scope".to_owned(),
            )),
            _ => Ok(()),
        }
    }

    /// Records an audit event for a form mutation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails
    /// the operation).
    async fn audit_form(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_id: &str,
        result: ActionResult,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                "form",
                target_id,
                PermissionDecision::Allow,
                result,
                None,
                None,
            )
            .await;
    }

    /// Loads a form and enforces the caller's profile scope on it.
    async fn load_scoped_form(
        &self,
        form_id: FormId,
        ctx: &ActionContext,
    ) -> Result<Form, DomainError>
    where
        F: FormRepository + Send + Sync,
    {
        let form = self.forms.get(form_id).await?;
        Self::check_scope(ctx, form.client_profile_id)?;
        Ok(form)
    }
}

impl<F, D, S, A> FormService for FormServiceImpl<F, D, S, A>
where
    F: FormRepository + Send + Sync,
    D: FormFieldRepository + Send + Sync,
    S: FormSubmissionRepository + Send + Sync,
    A: AuditService + Send + Sync,
{
    async fn create_form(&self, input: NewForm, ctx: &ActionContext) -> Result<Form, DomainError> {
        Self::check_scope(ctx, input.client_profile_id)?;
        let form = Form::new(input.client_profile_id, input.name)?;
        let form = self.forms.create(form).await?;
        self.audit_form(
            ctx,
            "form.created",
            &form.form_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(form)
    }

    async fn publish_form(
        &self,
        form_id: FormId,
        ctx: &ActionContext,
    ) -> Result<Form, DomainError> {
        let mut form = self.load_scoped_form(form_id, ctx).await?;
        form.publish()?;
        let form = self.forms.update(form).await?;
        self.audit_form(
            ctx,
            "form.published",
            &form.form_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(form)
    }

    async fn archive_form(
        &self,
        form_id: FormId,
        ctx: &ActionContext,
    ) -> Result<Form, DomainError> {
        let mut form = self.load_scoped_form(form_id, ctx).await?;
        form.archive()?;
        let form = self.forms.update(form).await?;
        self.audit_form(
            ctx,
            "form.archived",
            &form.form_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(form)
    }

    async fn add_field(
        &self,
        input: NewFormField,
        ctx: &ActionContext,
    ) -> Result<FormField, DomainError> {
        let form = self.load_scoped_form(input.form_id, ctx).await?;
        let field = FormField::new(
            form.form_id,
            input.field_type,
            input.label,
            input.required,
            input.position,
            input.config,
        )?;
        let field = self.fields.create(field).await?;
        self.audit_form(
            ctx,
            "form.field_added",
            &field.field_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(field)
    }

    async fn remove_field(
        &self,
        field_id: FormFieldId,
        ctx: &ActionContext,
    ) -> Result<(), DomainError> {
        let field = self.fields.get(field_id).await?;
        self.load_scoped_form(field.form_id, ctx).await?;
        self.fields.delete(field_id).await?;
        self.audit_form(
            ctx,
            "form.field_removed",
            &field_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(())
    }

    async fn submit(
        &self,
        input: NewFormSubmission,
        ctx: &ActionContext,
    ) -> Result<FormSubmission, DomainError> {
        let form = self.load_scoped_form(input.form_id, ctx).await?;
        // Validate-then-persist: the submission must satisfy the
        // form's fields (required answers, per-type values) before
        // anything is stored. This also rejects answers whose fields
        // belong to another form.
        let fields = self.fields.list_by_form(form.form_id, 10_000).await?;
        let submission = FormSubmission::new(
            form.form_id,
            form.client_profile_id,
            input.submitted_by_contact,
            input.submitted_by_label,
            input.answers,
        )?;
        submission.validate_against(&fields)?;
        let submission = self.submissions.create(submission).await?;
        self.audit_form(
            ctx,
            "form.submitted",
            &submission.submission_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(submission)
    }

    async fn list_submissions(
        &self,
        form_id: FormId,
        limit: u64,
        ctx: &ActionContext,
    ) -> Result<Vec<FormSubmission>, DomainError> {
        self.load_scoped_form(form_id, ctx).await?;
        self.submissions.list_by_form(form_id, limit).await
    }

    async fn review_submission(
        &self,
        submission_id: FormSubmissionId,
        ctx: &ActionContext,
    ) -> Result<FormSubmission, DomainError> {
        let mut submission = self.submissions.get(submission_id).await?;
        Self::check_scope(ctx, submission.client_profile_id)?;
        submission.review()?;
        let submission = self.submissions.set_status(submission).await?;
        self.audit_form(
            ctx,
            "form.submission_reviewed",
            &submission.submission_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(submission)
    }
}
