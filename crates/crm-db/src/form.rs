// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Form builder vertical slice (task 11-FORM-02).
//!
//! [`PgFormRepository`], [`PgFormFieldRepository`] and
//! [`PgFormSubmissionRepository`] implement the
//! [`crm_domain::repositories`] form traits over the `forms`,
//! `form_fields` and `form_submissions` tables (migration 0010).
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::error::DomainError;
use crm_domain::form::{
    Form, FormAnswer, FormField, FormFieldType, FormStatus, FormSubmission, SubmissionStatus,
};
use crm_domain::ids::{ClientProfileId, ContactId, FormFieldId, FormId, FormSubmissionId};
use crm_domain::repositories::{FormFieldRepository, FormRepository, FormSubmissionRepository};

use crate::postgres::{ensure_client_profile, map_storage_error};

/// Renders a [`FormStatus`] as its stable database string.
fn form_status_as_str(status: FormStatus) -> &'static str {
    match status {
        FormStatus::Draft => "draft",
        FormStatus::Published => "published",
        FormStatus::Archived => "archived",
    }
}

/// Parses a [`FormStatus`] from its database string.
fn form_status_from_str(raw: &str) -> Result<FormStatus, DomainError> {
    match raw {
        "draft" => Ok(FormStatus::Draft),
        "published" => Ok(FormStatus::Published),
        "archived" => Ok(FormStatus::Archived),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown form status '{other}'"
        ))),
    }
}

/// Renders a [`FormFieldType`] as its stable database string.
fn field_type_as_str(field_type: FormFieldType) -> &'static str {
    match field_type {
        FormFieldType::TextField => "text",
        FormFieldType::EmailField => "email",
        FormFieldType::PhoneField => "phone",
        FormFieldType::NumberField => "number",
        FormFieldType::SelectField => "select",
        FormFieldType::CheckboxField => "checkbox",
        FormFieldType::DateField => "date",
    }
}

/// Parses a [`FormFieldType`] from its database string.
fn field_type_from_str(raw: &str) -> Result<FormFieldType, DomainError> {
    match raw {
        "text" => Ok(FormFieldType::TextField),
        "email" => Ok(FormFieldType::EmailField),
        "phone" => Ok(FormFieldType::PhoneField),
        "number" => Ok(FormFieldType::NumberField),
        "select" => Ok(FormFieldType::SelectField),
        "checkbox" => Ok(FormFieldType::CheckboxField),
        "date" => Ok(FormFieldType::DateField),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown form field type '{other}'"
        ))),
    }
}

/// Renders a [`SubmissionStatus`] as its stable database string.
fn submission_status_as_str(status: SubmissionStatus) -> &'static str {
    match status {
        SubmissionStatus::New => "new",
        SubmissionStatus::Reviewed => "reviewed",
        SubmissionStatus::Archived => "archived",
    }
}

/// Parses a [`SubmissionStatus`] from its database string.
fn submission_status_from_str(raw: &str) -> Result<SubmissionStatus, DomainError> {
    match raw {
        "new" => Ok(SubmissionStatus::New),
        "reviewed" => Ok(SubmissionStatus::Reviewed),
        "archived" => Ok(SubmissionStatus::Archived),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown submission status '{other}'"
        ))),
    }
}

/// Columns selected for a [`Form`] row.
const FORM_COLUMNS: &str = "form_id, client_profile_id, name, status, created_at, updated_at";

/// Columns selected for a [`FormField`] row.
const FIELD_COLUMNS: &str = "field_id, form_id, field_type, label, required, position, config";

/// Columns selected for a [`FormSubmission`] row.
const SUBMISSION_COLUMNS: &str =
    "submission_id, form_id, client_profile_id, contact_id, submitted_by_label, status, answers, submitted_at";

/// Hydrates a `forms` row into a [`Form`].
fn row_to_form(row: &sqlx::postgres::PgRow) -> Result<Form, DomainError> {
    Ok(Form {
        form_id: FormId::from(
            row.try_get::<Uuid, _>("form_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        name: row.try_get("name").map_err(map_storage_error)?,
        status: form_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates a `form_fields` row into a [`FormField`].
fn row_to_field(row: &sqlx::postgres::PgRow) -> Result<FormField, DomainError> {
    Ok(FormField {
        field_id: FormFieldId::from(
            row.try_get::<Uuid, _>("field_id")
                .map_err(map_storage_error)?,
        ),
        form_id: FormId::from(
            row.try_get::<Uuid, _>("form_id")
                .map_err(map_storage_error)?,
        ),
        field_type: field_type_from_str(
            row.try_get::<String, _>("field_type")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        label: row.try_get("label").map_err(map_storage_error)?,
        required: row.try_get("required").map_err(map_storage_error)?,
        position: u32::try_from(
            row.try_get::<i32, _>("position")
                .map_err(map_storage_error)?,
        )
        .map_err(|_| DomainError::Validation("storage error: negative position".to_owned()))?,
        config: row.try_get("config").map_err(map_storage_error)?,
    })
}

/// Hydrates a `form_submissions` row into a [`FormSubmission`].
fn row_to_submission(row: &sqlx::postgres::PgRow) -> Result<FormSubmission, DomainError> {
    let answers: serde_json::Value = row.try_get("answers").map_err(map_storage_error)?;
    let answers: Vec<FormAnswer> = serde_json::from_value(answers)
        .map_err(|_| DomainError::Validation("storage error: malformed answers JSON".to_owned()))?;
    Ok(FormSubmission {
        submission_id: FormSubmissionId::from(
            row.try_get::<Uuid, _>("submission_id")
                .map_err(map_storage_error)?,
        ),
        form_id: FormId::from(
            row.try_get::<Uuid, _>("form_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        submitted_by_contact: row
            .try_get::<Option<Uuid>, _>("contact_id")
            .map_err(map_storage_error)?
            .map(ContactId::from),
        submitted_by_label: row
            .try_get("submitted_by_label")
            .map_err(map_storage_error)?,
        answers,
        status: submission_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        submitted_at: row
            .try_get::<DateTime<Utc>, _>("submitted_at")
            .map_err(map_storage_error)?,
    })
}

/// SQLx implementation of [`FormRepository`].
#[derive(Debug, Clone)]
pub struct PgFormRepository {
    pool: PgPool,
}

impl PgFormRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl FormRepository for PgFormRepository {
    async fn create(&self, form: Form) -> Result<Form, DomainError> {
        ensure_client_profile(&self.pool, form.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO forms (form_id, client_profile_id, name, status, created_at, updated_at)\n             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(form.form_id.as_uuid())
        .bind(form.client_profile_id.as_uuid())
        .bind(&form.name)
        .bind(form_status_as_str(form.status))
        .bind(form.created_at)
        .bind(form.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(form)
    }

    async fn get(&self, form_id: FormId) -> Result<Form, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {FORM_COLUMNS} FROM forms WHERE form_id = $1"
        ))
        .bind(form_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_form(&row),
            None => Err(DomainError::NotFound {
                kind: "form".to_owned(),
                id: form_id.to_string(),
            }),
        }
    }

    async fn update(&self, form: Form) -> Result<Form, DomainError> {
        let result = sqlx::query(
            "UPDATE forms\n             SET name = $2, status = $3, updated_at = $4\n             WHERE form_id = $1",
        )
        .bind(form.form_id.as_uuid())
        .bind(&form.name)
        .bind(form_status_as_str(form.status))
        .bind(form.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "form".to_owned(),
                id: form.form_id.to_string(),
            });
        }
        Ok(form)
    }

    async fn list_by_profile(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Form>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {FORM_COLUMNS} FROM forms\n             WHERE client_profile_id = $1\n             ORDER BY created_at DESC, form_id DESC\n             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_form).collect()
    }
}

/// SQLx implementation of [`FormFieldRepository`].
///
/// Append-mostly: there is deliberately no update method — fields are
/// deleted and recreated.
#[derive(Debug, Clone)]
pub struct PgFormFieldRepository {
    pool: PgPool,
}

impl PgFormFieldRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl FormFieldRepository for PgFormFieldRepository {
    async fn create(&self, field: FormField) -> Result<FormField, DomainError> {
        sqlx::query(
            "INSERT INTO form_fields (field_id, form_id, field_type, label, required, position, \
             config)\n             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(field.field_id.as_uuid())
        .bind(field.form_id.as_uuid())
        .bind(field_type_as_str(field.field_type))
        .bind(&field.label)
        .bind(field.required)
        .bind(i32::try_from(field.position).unwrap_or(i32::MAX))
        .bind(&field.config)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(field)
    }

    async fn get(&self, field_id: FormFieldId) -> Result<FormField, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {FIELD_COLUMNS} FROM form_fields WHERE field_id = $1"
        ))
        .bind(field_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_field(&row),
            None => Err(DomainError::NotFound {
                kind: "form_field".to_owned(),
                id: field_id.to_string(),
            }),
        }
    }

    async fn delete(&self, field_id: FormFieldId) -> Result<(), DomainError> {
        let result = sqlx::query("DELETE FROM form_fields WHERE field_id = $1")
            .bind(field_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "form_field".to_owned(),
                id: field_id.to_string(),
            });
        }
        Ok(())
    }

    async fn list_by_form(
        &self,
        form_id: FormId,
        limit: u64,
    ) -> Result<Vec<FormField>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {FIELD_COLUMNS} FROM form_fields\n             WHERE form_id = $1\n             ORDER BY position ASC, field_id ASC\n             LIMIT $2"
        ))
        .bind(form_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_field).collect()
    }
}

/// SQLx implementation of [`FormSubmissionRepository`].
///
/// Append-mostly: answers never change after submission, so only the
/// status is updatable.
#[derive(Debug, Clone)]
pub struct PgFormSubmissionRepository {
    pool: PgPool,
}

impl PgFormSubmissionRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl FormSubmissionRepository for PgFormSubmissionRepository {
    async fn create(&self, submission: FormSubmission) -> Result<FormSubmission, DomainError> {
        ensure_client_profile(&self.pool, submission.client_profile_id).await?;
        let answers = serde_json::to_value(&submission.answers).map_err(|_| {
            DomainError::Validation("storage error: answers are not JSON-serializable".to_owned())
        })?;
        sqlx::query(
            "INSERT INTO form_submissions (submission_id, form_id, client_profile_id, contact_id, \
             submitted_by_label, status, answers, submitted_at)\n             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(submission.submission_id.as_uuid())
        .bind(submission.form_id.as_uuid())
        .bind(submission.client_profile_id.as_uuid())
        .bind(submission.submitted_by_contact.map(|id| id.as_uuid()))
        .bind(&submission.submitted_by_label)
        .bind(submission_status_as_str(submission.status))
        .bind(&answers)
        .bind(submission.submitted_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(submission)
    }

    async fn get(&self, submission_id: FormSubmissionId) -> Result<FormSubmission, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {SUBMISSION_COLUMNS} FROM form_submissions WHERE submission_id = $1"
        ))
        .bind(submission_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_submission(&row),
            None => Err(DomainError::NotFound {
                kind: "form_submission".to_owned(),
                id: submission_id.to_string(),
            }),
        }
    }

    async fn set_status(&self, submission: FormSubmission) -> Result<FormSubmission, DomainError> {
        let result = sqlx::query(
            "UPDATE form_submissions\n             SET status = $2, updated_at = now()\n             WHERE submission_id = $1",
        )
        .bind(submission.submission_id.as_uuid())
        .bind(submission_status_as_str(submission.status))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "form_submission".to_owned(),
                id: submission.submission_id.to_string(),
            });
        }
        Ok(submission)
    }

    async fn list_by_form(
        &self,
        form_id: FormId,
        limit: u64,
    ) -> Result<Vec<FormSubmission>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {SUBMISSION_COLUMNS} FROM form_submissions\n             WHERE form_id = $1\n             ORDER BY submitted_at DESC, submission_id DESC\n             LIMIT $2"
        ))
        .bind(form_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_submission).collect()
    }
}
