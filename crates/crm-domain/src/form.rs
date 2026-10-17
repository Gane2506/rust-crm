// SPDX-License-Identifier: GPL-3.0-only
//! Form builder domain: typed intake/lead forms, field validation and
//! submissions.
//!
//! Agency staff compose forms (lead capture, intake, feedback) from
//! typed fields; end-clients submit answers through published forms.
//! Every form belongs to one client profile — the tenant boundary.
//!
//! ## Authorization
//!
//! Form operations authorize against
//! [`Resource::Form`](crate::security::Resource::Form),
//! profile-scoped (the variant landed in task 11-FORM-02 together
//! with its `"form"` database-string mapping in `crates/crm-db`).
//!
//! ## Audit
//!
//! Form mutations are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s by the service
//! implementation, with pinned action names documented on
//! [`FormService`](crate::services::FormService). They are
//! deliberately **not** [`SecurityEvent`](crate::audit::SecurityEvent)
//! variants: that taxonomy covers security-relevant events (auth,
//! RBAC, sessions, rate limits), while forms are business operations
//! audited like contacts and campaigns.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::email::is_valid_email;
use crate::error::DomainError;
use crate::ids::{ClientProfileId, ContactId, FormFieldId, FormId, FormSubmissionId};

/// Lifecycle of a form definition.
///
/// The happy path is `Draft → Published`. `Archived` is terminal and
/// reachable from both `Draft` and `Published`; archiving is
/// idempotent so repeated archive calls are safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FormStatus {
    /// Being edited; not accepting submissions.
    Draft,
    /// Accepting submissions.
    Published,
    /// Retired; terminal.
    Archived,
}

impl FormStatus {
    /// Whether the status is terminal (no further transitions allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, FormStatus::Archived)
    }
}

/// A named form owned by one client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form {
    /// Stable identifier of this form.
    pub form_id: FormId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Human-readable name, validated non-blank.
    pub name: String,
    /// Lifecycle status.
    pub status: FormStatus,
    /// When the form was created.
    pub created_at: DateTime<Utc>,
    /// When the form was last changed.
    pub updated_at: DateTime<Utc>,
}

impl Form {
    /// Creates a form in `Draft` status.
    ///
    /// Rejects: blank `name`.
    ///
    /// ```
    /// use crm_domain::form::{Form, FormStatus};
    /// use crm_domain::ids::ClientProfileId;
    ///
    /// let form = Form::new(ClientProfileId::new(), "Lead capture".to_string())
    ///     .expect("valid form");
    /// assert_eq!(form.status, FormStatus::Draft);
    /// ```
    pub fn new(client_profile_id: ClientProfileId, name: String) -> Result<Self, DomainError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "form name must not be blank".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            form_id: FormId::new(),
            client_profile_id,
            name,
            status: FormStatus::Draft,
            created_at: now,
            updated_at: now,
        })
    }

    /// Publishes a draft form so it starts accepting submissions.
    ///
    /// Only `Draft → Published` is allowed.
    pub fn publish(&mut self) -> Result<(), DomainError> {
        if self.status != FormStatus::Draft {
            return Err(DomainError::Validation(format!(
                "cannot publish a form in status {:?}",
                self.status
            )));
        }
        self.status = FormStatus::Published;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Archives a form. Idempotent: archiving an already-archived
    /// form succeeds without changing it.
    pub fn archive(&mut self) -> Result<(), DomainError> {
        if self.status == FormStatus::Archived {
            return Ok(());
        }
        self.status = FormStatus::Archived;
        self.updated_at = Utc::now();
        Ok(())
    }
}

/// The kind of a [`FormField`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FormFieldType {
    /// Free text.
    TextField,
    /// Email address (validated structurally).
    EmailField,
    /// Phone number (validated structurally: digits only check).
    PhoneField,
    /// Decimal number.
    NumberField,
    /// One of a fixed set of options from config `options`.
    SelectField,
    /// Boolean: `"true"` / `"false"` (case-insensitive).
    CheckboxField,
    /// Calendar date in ISO `YYYY-MM-DD` form.
    DateField,
}

/// One typed field on a form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormField {
    /// Stable identifier of this field.
    pub field_id: FormFieldId,
    /// The form this field belongs to.
    pub form_id: FormId,
    /// What kind of input this is.
    pub field_type: FormFieldType,
    /// Human-readable label, validated non-blank.
    pub label: String,
    /// Whether a submission must answer this field.
    pub required: bool,
    /// Display ordering within the form.
    pub position: u32,
    /// Field configuration (opaque JSON, never null). A
    /// [`FormFieldType::SelectField`] must carry a non-empty
    /// `options` string array.
    pub config: Value,
}

impl FormField {
    /// Creates a field.
    ///
    /// Rejects: a blank `label`; a null `config`; a
    /// [`FormFieldType::SelectField`] whose config lacks a
    /// non-empty `options` string array.
    ///
    /// ```
    /// use crm_domain::form::{FormField, FormFieldType};
    /// use crm_domain::ids::FormId;
    /// use serde_json::json;
    ///
    /// let field = FormField::new(
    ///     FormId::new(),
    ///     FormFieldType::EmailField,
    ///     "Work email".to_string(),
    ///     true,
    ///     0,
    ///     json!({}),
    /// )
    /// .expect("valid field");
    /// assert!(field.required);
    /// ```
    pub fn new(
        form_id: FormId,
        field_type: FormFieldType,
        label: String,
        required: bool,
        position: u32,
        config: Value,
    ) -> Result<Self, DomainError> {
        let label = label.trim().to_owned();
        if label.is_empty() {
            return Err(DomainError::Validation(
                "field label must not be blank".to_string(),
            ));
        }
        if config.is_null() {
            return Err(DomainError::Validation(
                "field config must not be null".to_string(),
            ));
        }
        if field_type == FormFieldType::SelectField {
            let options_ok =
                config
                    .get("options")
                    .and_then(Value::as_array)
                    .is_some_and(|options| {
                        !options.is_empty()
                            && options
                                .iter()
                                .all(|option| option.as_str().is_some_and(|s| !s.trim().is_empty()))
                    });
            if !options_ok {
                return Err(DomainError::Validation(
                    "select fields require a non-empty options string array in config".to_string(),
                ));
            }
        }
        Ok(Self {
            field_id: FormFieldId::new(),
            form_id,
            field_type,
            label,
            required,
            position,
            config,
        })
    }

    /// Validates one submitted value against this field.
    ///
    /// A blank value is accepted for optional fields and rejected for
    /// required ones. Non-blank values are checked by type: email
    /// structure, phone digits, number parse, select membership,
    /// checkbox true/false, ISO date.
    pub fn validate_value(&self, value: &str) -> Result<(), DomainError> {
        let value = value.trim();
        if value.is_empty() {
            return if self.required {
                Err(DomainError::Validation(format!(
                    "field '{}' is required",
                    self.label
                )))
            } else {
                Ok(())
            };
        }
        let invalid =
            |reason: &str| DomainError::Validation(format!("field '{}': {}", self.label, reason));
        match self.field_type {
            FormFieldType::TextField => Ok(()),
            FormFieldType::EmailField => {
                if is_valid_email(value) {
                    Ok(())
                } else {
                    Err(invalid("must be a valid email address"))
                }
            }
            FormFieldType::PhoneField => {
                // Structural check only: the domain stores phones
                // trim-deduped, never E.164-normalized.
                if value.chars().filter(char::is_ascii_digit).count() >= 7 {
                    Ok(())
                } else {
                    Err(invalid("must contain at least 7 digits"))
                }
            }
            FormFieldType::NumberField => {
                if value.parse::<f64>().is_ok() {
                    Ok(())
                } else {
                    Err(invalid("must be a number"))
                }
            }
            FormFieldType::SelectField => {
                let allowed = self
                    .config
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|options| {
                        options
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<&str>>()
                    })
                    .unwrap_or_default();
                if allowed.contains(&value) {
                    Ok(())
                } else {
                    Err(invalid("is not one of the allowed options"))
                }
            }
            FormFieldType::CheckboxField => {
                if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
                    Ok(())
                } else {
                    Err(invalid("must be true or false"))
                }
            }
            FormFieldType::DateField => {
                if NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok() {
                    Ok(())
                } else {
                    Err(invalid("must be a date in YYYY-MM-DD form"))
                }
            }
        }
    }
}

/// One submitted answer: the value given for one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormAnswer {
    /// The field being answered.
    pub field_id: FormFieldId,
    /// The submitted value (raw; trimmed during validation).
    pub value: String,
}

/// Lifecycle of a [`FormSubmission`].
///
/// Forward-only: `New → Reviewed → Archived`. `Archived` is terminal;
/// reviewing an already-reviewed submission is idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubmissionStatus {
    /// Just received; not yet triaged by staff.
    New,
    /// Seen and triaged by staff.
    Reviewed,
    /// Retired; terminal.
    Archived,
}

/// One submission of a form's answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormSubmission {
    /// Stable identifier of this submission.
    pub submission_id: FormSubmissionId,
    /// The form that was submitted.
    pub form_id: FormId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// The contact this submission is attributed to, when known.
    /// Anonymous web-form fills leave this `None`.
    pub submitted_by_contact: Option<ContactId>,
    /// Human-readable submitter label (e.g. a name or "Web form").
    /// Validated non-blank.
    pub submitted_by_label: String,
    /// The submitted answers.
    pub answers: Vec<FormAnswer>,
    /// Lifecycle status.
    pub status: SubmissionStatus,
    /// When the submission was received.
    pub submitted_at: DateTime<Utc>,
}

impl FormSubmission {
    /// Records a submission in `New` status.
    ///
    /// Rejects: a blank `submitted_by_label`. Answer validity is
    /// checked separately via
    /// [`validate_against`](FormSubmission::validate_against), which
    /// needs the form's fields — the service performs that check.
    pub fn new(
        form_id: FormId,
        client_profile_id: ClientProfileId,
        submitted_by_contact: Option<ContactId>,
        submitted_by_label: String,
        answers: Vec<FormAnswer>,
    ) -> Result<Self, DomainError> {
        let submitted_by_label = submitted_by_label.trim().to_owned();
        if submitted_by_label.is_empty() {
            return Err(DomainError::Validation(
                "submitter label must not be blank".to_string(),
            ));
        }
        Ok(Self {
            submission_id: FormSubmissionId::new(),
            form_id,
            client_profile_id,
            submitted_by_contact,
            submitted_by_label,
            answers,
            status: SubmissionStatus::New,
            submitted_at: Utc::now(),
        })
    }

    /// Marks the submission reviewed. Only `New → Reviewed` is
    /// allowed; reviewing an already-reviewed submission is
    /// idempotent.
    pub fn review(&mut self) -> Result<(), DomainError> {
        match self.status {
            SubmissionStatus::New => {
                self.status = SubmissionStatus::Reviewed;
                Ok(())
            }
            SubmissionStatus::Reviewed => Ok(()),
            SubmissionStatus::Archived => Err(DomainError::Validation(
                "cannot review an archived submission".to_string(),
            )),
        }
    }

    /// Archives a reviewed submission. `Archived` is terminal and
    /// idempotent; only `Reviewed → Archived` is allowed.
    pub fn archive(&mut self) -> Result<(), DomainError> {
        match self.status {
            SubmissionStatus::Reviewed => {
                self.status = SubmissionStatus::Archived;
                Ok(())
            }
            SubmissionStatus::Archived => Ok(()),
            SubmissionStatus::New => Err(DomainError::Validation(
                "cannot archive a submission that has not been reviewed".to_string(),
            )),
        }
    }

    /// Validates the submission's answers against a form's fields.
    ///
    /// Checks: every field belongs to this submission's form; every
    /// required field has an answer; every answer references a known
    /// field and its value validates against that field's type.
    pub fn validate_against(&self, fields: &[FormField]) -> Result<(), DomainError> {
        for field in fields {
            if field.form_id != self.form_id {
                return Err(DomainError::Validation(format!(
                    "field '{}' does not belong to this form",
                    field.label
                )));
            }
        }
        for answer in &self.answers {
            let Some(field) = fields.iter().find(|f| f.field_id == answer.field_id) else {
                return Err(DomainError::Validation(
                    "answer references an unknown field".to_string(),
                ));
            };
            field.validate_value(&answer.value)?;
        }
        for field in fields {
            if field.required
                && !self.answers.iter().any(|answer| {
                    answer.field_id == field.field_id && !answer.value.trim().is_empty()
                })
            {
                return Err(DomainError::Validation(format!(
                    "field '{}' is required",
                    field.label
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn draft_form() -> Form {
        Form::new(ClientProfileId::new(), "Lead capture".to_string()).expect("valid form")
    }

    fn text_field(form_id: FormId, label: &str, required: bool) -> FormField {
        FormField::new(
            form_id,
            FormFieldType::TextField,
            label.to_string(),
            required,
            0,
            json!({}),
        )
        .expect("valid field")
    }

    fn new_submission(form_id: FormId) -> FormSubmission {
        FormSubmission::new(
            form_id,
            ClientProfileId::new(),
            None,
            "Web form".to_string(),
            vec![],
        )
        .expect("valid submission")
    }

    #[test]
    fn form_lifecycle_is_forward_only() {
        let mut form = draft_form();
        // Cannot archive before publishing is fine (archive from
        // Draft is allowed).
        assert!(form.archive().is_ok());
        assert_eq!(form.status, FormStatus::Archived);
        assert!(form.status.is_terminal());
        // Terminal: nothing moves.
        assert!(form.publish().is_err());
    }

    #[test]
    fn form_archive_is_idempotent() {
        let mut form = draft_form();
        form.archive().expect("archive");
        form.archive().expect("archive again");
        assert_eq!(form.status, FormStatus::Archived);
    }

    #[test]
    fn form_publish_requires_draft() {
        let mut form = draft_form();
        form.publish().expect("publish");
        assert_eq!(form.status, FormStatus::Published);
        assert!(!form.status.is_terminal());
        // Cannot publish twice.
        assert!(form.publish().is_err());
        // Published archives cleanly.
        form.archive().expect("archive published");
        assert_eq!(form.status, FormStatus::Archived);
    }

    #[test]
    fn form_rejects_blank_name() {
        assert!(Form::new(ClientProfileId::new(), "  ".to_string()).is_err());
    }

    #[test]
    fn field_rejects_blank_label_and_null_config() {
        let form_id = FormId::new();
        assert!(FormField::new(
            form_id,
            FormFieldType::TextField,
            "  ".to_string(),
            false,
            0,
            json!({}),
        )
        .is_err());
        assert!(FormField::new(
            form_id,
            FormFieldType::TextField,
            "Name".to_string(),
            false,
            0,
            Value::Null,
        )
        .is_err());
    }

    #[test]
    fn select_field_requires_non_empty_options() {
        let form_id = FormId::new();
        // Missing options rejected.
        assert!(FormField::new(
            form_id,
            FormFieldType::SelectField,
            "Plan".to_string(),
            false,
            0,
            json!({}),
        )
        .is_err());
        // Empty options rejected.
        assert!(FormField::new(
            form_id,
            FormFieldType::SelectField,
            "Plan".to_string(),
            false,
            0,
            json!({"options": []}),
        )
        .is_err());
        // Non-string options rejected.
        assert!(FormField::new(
            form_id,
            FormFieldType::SelectField,
            "Plan".to_string(),
            false,
            0,
            json!({"options": ["basic", 3]}),
        )
        .is_err());
        let field = FormField::new(
            form_id,
            FormFieldType::SelectField,
            "Plan".to_string(),
            false,
            0,
            json!({"options": ["basic", "pro"]}),
        )
        .expect("valid select");
        assert!(field.validate_value("basic").is_ok());
        assert!(field.validate_value("enterprise").is_err());
        // Optional blank values pass.
        assert!(field.validate_value("  ").is_ok());
    }

    #[test]
    fn required_fields_reject_blank_values() {
        let form_id = FormId::new();
        let field = text_field(form_id, "Name", true);
        assert!(field.validate_value("  ").is_err());
        assert!(field.validate_value("Ada").is_ok());
        // Optional blank passes.
        let optional = text_field(form_id, "Notes", false);
        assert!(optional.validate_value("").is_ok());
    }

    #[test]
    fn email_field_validates_structure() {
        let form_id = FormId::new();
        let field = FormField::new(
            form_id,
            FormFieldType::EmailField,
            "Email".to_string(),
            true,
            0,
            json!({}),
        )
        .expect("valid field");
        assert!(field.validate_value("ada@example.com").is_ok());
        assert!(field.validate_value("not-an-email").is_err());
        assert!(field.validate_value("ada@localhost").is_err());
    }

    #[test]
    fn phone_field_validates_digits() {
        let form_id = FormId::new();
        let field = FormField::new(
            form_id,
            FormFieldType::PhoneField,
            "Phone".to_string(),
            false,
            0,
            json!({}),
        )
        .expect("valid field");
        // Dashes kept (domain stores phones trim-deduped, never E.164).
        assert!(field.validate_value("+1-555-0199").is_ok());
        assert!(field.validate_value("no digits here").is_err());
    }

    #[test]
    fn number_and_checkbox_and_date_fields_validate() {
        let form_id = FormId::new();
        let number = FormField::new(
            form_id,
            FormFieldType::NumberField,
            "Budget".to_string(),
            true,
            0,
            json!({}),
        )
        .expect("valid field");
        assert!(number.validate_value("42000.50").is_ok());
        assert!(number.validate_value("lots").is_err());

        let checkbox = FormField::new(
            form_id,
            FormFieldType::CheckboxField,
            "Consent".to_string(),
            true,
            1,
            json!({}),
        )
        .expect("valid field");
        assert!(checkbox.validate_value("true").is_ok());
        assert!(checkbox.validate_value("FALSE").is_ok());
        assert!(checkbox.validate_value("yes").is_err());

        let date = FormField::new(
            form_id,
            FormFieldType::DateField,
            "Start".to_string(),
            true,
            2,
            json!({}),
        )
        .expect("valid field");
        assert!(date.validate_value("2026-10-17").is_ok());
        assert!(date.validate_value("17/10/2026").is_err());
        assert!(date.validate_value("2026-02-30").is_err());
    }

    #[test]
    fn submission_lifecycle_is_forward_only() {
        let mut submission = new_submission(FormId::new());
        // Cannot archive before review.
        assert!(submission.archive().is_err());
        submission.review().expect("review");
        assert_eq!(submission.status, SubmissionStatus::Reviewed);
        // Review is idempotent.
        submission.review().expect("review again");
        assert_eq!(submission.status, SubmissionStatus::Reviewed);
        submission.archive().expect("archive");
        assert_eq!(submission.status, SubmissionStatus::Archived);
        // Terminal: archive idempotent, review rejected.
        submission.archive().expect("archive again");
        assert!(submission.review().is_err());
    }

    #[test]
    fn submission_rejects_blank_submitter_label() {
        assert!(FormSubmission::new(
            FormId::new(),
            ClientProfileId::new(),
            None,
            "  ".to_string(),
            vec![],
        )
        .is_err());
    }

    #[test]
    fn validate_against_checks_required_and_types() {
        let form_id = FormId::new();
        let name = text_field(form_id, "Name", true);
        let email = FormField::new(
            form_id,
            FormFieldType::EmailField,
            "Email".to_string(),
            false,
            1,
            json!({}),
        )
        .expect("valid field");
        let fields = vec![name.clone(), email.clone()];

        // Missing required field.
        let submission = FormSubmission::new(
            form_id,
            ClientProfileId::new(),
            None,
            "Web form".to_string(),
            vec![FormAnswer {
                field_id: email.field_id,
                value: "ada@example.com".to_string(),
            }],
        )
        .expect("valid submission");
        assert!(submission.validate_against(&fields).is_err());

        // Invalid value for a known field.
        let bad_email = FormSubmission::new(
            form_id,
            ClientProfileId::new(),
            None,
            "Web form".to_string(),
            vec![
                FormAnswer {
                    field_id: name.field_id,
                    value: "Ada".to_string(),
                },
                FormAnswer {
                    field_id: email.field_id,
                    value: "not-an-email".to_string(),
                },
            ],
        )
        .expect("valid submission");
        assert!(bad_email.validate_against(&fields).is_err());

        // Answer for an unknown field.
        let foreign = FormSubmission::new(
            form_id,
            ClientProfileId::new(),
            None,
            "Web form".to_string(),
            vec![FormAnswer {
                field_id: FormFieldId::new(),
                value: "x".to_string(),
            }],
        )
        .expect("valid submission");
        assert!(foreign.validate_against(&fields).is_err());

        // Happy path.
        let good = FormSubmission::new(
            form_id,
            ClientProfileId::new(),
            Some(ContactId::new()),
            "Ada Lovelace".to_string(),
            vec![
                FormAnswer {
                    field_id: name.field_id,
                    value: "Ada".to_string(),
                },
                FormAnswer {
                    field_id: email.field_id,
                    value: "ada@example.com".to_string(),
                },
            ],
        )
        .expect("valid submission");
        assert!(good.validate_against(&fields).is_ok());
    }

    #[test]
    fn validate_against_rejects_fields_from_another_form() {
        let form_id = FormId::new();
        let other_form_id = FormId::new();
        let foreign_field = text_field(other_form_id, "Name", true);
        let submission = new_submission(form_id);
        assert!(submission.validate_against(&[foreign_field]).is_err());
    }
}
