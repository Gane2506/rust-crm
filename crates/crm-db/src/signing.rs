// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Contract signing vertical slice (task
//! 13-SIGN-02).
//!
//! [`PgContractRepository`] implements
//! [`crm_domain::repositories::ContractRepository`] over the
//! `contracts` table (migration 0013), hydrating the
//! [`Contract`](crm_domain::contract::Contract) aggregate from
//! 01-FND-01. [`PgSigningTemplateRepository`] implements
//! [`crm_domain::repositories::SigningTemplateRepository`] over
//! `signing_templates`. Signing evidence lives in `signing_evidence`
//! and is written/read through inherent methods on
//! [`PgContractRepository`] — there is no evidence trait in the
//! domain; evidence is always handled alongside its contract.
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::contract::{Contract, ContractState, PaymentRequirement, Signer};
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, ContractId, SigningTemplateId};
use crm_domain::repositories::{
    ContractDocumentRepository, ContractRepository, SigningTemplateRepository,
};
use crm_domain::signing::{ContractDocument, SignatureMethod, SigningEvidence, SigningTemplate};

use crate::postgres::{ensure_client_profile, map_storage_error};

/// Renders a [`ContractState`] as its stable database string.
fn contract_state_as_str(state: ContractState) -> &'static str {
    match state {
        ContractState::Draft => "draft",
        ContractState::Sent => "sent",
        ContractState::PartiallySigned => "partially_signed",
        ContractState::SignedAwaitingPayment => "signed_awaiting_payment",
        ContractState::Effective => "effective",
        ContractState::Cancelled => "cancelled",
        ContractState::Expired => "expired",
        ContractState::Disputed => "disputed",
    }
}

/// Parses a [`ContractState`] from its database string.
fn contract_state_from_str(raw: &str) -> Result<ContractState, DomainError> {
    match raw {
        "draft" => Ok(ContractState::Draft),
        "sent" => Ok(ContractState::Sent),
        "partially_signed" => Ok(ContractState::PartiallySigned),
        "signed_awaiting_payment" => Ok(ContractState::SignedAwaitingPayment),
        "effective" => Ok(ContractState::Effective),
        "cancelled" => Ok(ContractState::Cancelled),
        "expired" => Ok(ContractState::Expired),
        "disputed" => Ok(ContractState::Disputed),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown contract status '{other}'"
        ))),
    }
}

/// Renders a [`SignatureMethod`] as its stable database string.
fn signature_method_as_str(method: SignatureMethod) -> &'static str {
    method.as_str()
}

/// Parses a [`SignatureMethod`] from its database string.
fn signature_method_from_str(raw: &str) -> Result<SignatureMethod, DomainError> {
    match raw {
        "typed" => Ok(SignatureMethod::TypedSignature),
        "drawn" => Ok(SignatureMethod::DrawnSignature),
        "click_to_sign" => Ok(SignatureMethod::ClickToSign),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown signature method '{other}'"
        ))),
    }
}

/// Deserializes the `signers` JSONB column.
fn signers_from_json(raw: serde_json::Value) -> Result<Vec<Signer>, DomainError> {
    serde_json::from_value(raw).map_err(|error| {
        DomainError::Validation(format!("storage error: invalid signers JSON: {error}"))
    })
}

/// Deserializes the `payment_requirement` JSONB column.
fn payment_requirement_from_json(
    raw: serde_json::Value,
) -> Result<PaymentRequirement, DomainError> {
    serde_json::from_value(raw).map_err(|error| {
        DomainError::Validation(format!(
            "storage error: invalid payment requirement JSON: {error}"
        ))
    })
}

/// Hydrates a `contracts` row into a [`Contract`]. All aggregate
/// fields are `pub`, so hydration builds the struct directly (state
/// transitions stay behind
/// [`Contract::transition_to`](crm_domain::contract::Contract::transition_to)).
fn row_to_contract(row: &sqlx::postgres::PgRow) -> Result<Contract, DomainError> {
    Ok(Contract {
        contract_id: ContractId::from(
            row.try_get::<Uuid, _>("contract_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        template_revision: row
            .try_get("template_revision")
            .map_err(map_storage_error)?,
        signers: signers_from_json(
            row.try_get::<serde_json::Value, _>("signers")
                .map_err(map_storage_error)?,
        )?,
        evidence_bundle_ref: row
            .try_get::<Option<String>, _>("evidence_bundle_ref")
            .map_err(map_storage_error)?,
        state: contract_state_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        payment_requirement: payment_requirement_from_json(
            row.try_get::<serde_json::Value, _>("payment_requirement")
                .map_err(map_storage_error)?,
        )?,
        effective_at: row
            .try_get::<Option<DateTime<Utc>>, _>("effective_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates a `signing_evidence` row into a [`SigningEvidence`]. The
/// domain evidence carries no id; the surrogate `evidence_id` stays in
/// the database.
fn row_to_evidence(row: &sqlx::postgres::PgRow) -> Result<SigningEvidence, DomainError> {
    Ok(SigningEvidence {
        contract_id: ContractId::from(
            row.try_get::<Uuid, _>("contract_id")
                .map_err(map_storage_error)?,
        ),
        signer_name: row.try_get("signer_name").map_err(map_storage_error)?,
        signer_email: row.try_get("signer_email").map_err(map_storage_error)?,
        signed_at: row
            .try_get::<DateTime<Utc>, _>("signed_at")
            .map_err(map_storage_error)?,
        ip_hash: row.try_get("ip_hash").map_err(map_storage_error)?,
        user_agent_hash: row.try_get("user_agent_hash").map_err(map_storage_error)?,
        method: signature_method_from_str(
            row.try_get::<String, _>("method")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
    })
}

/// Hydrates a `signing_templates` row into a [`SigningTemplate`].
fn row_to_template(row: &sqlx::postgres::PgRow) -> Result<SigningTemplate, DomainError> {
    Ok(SigningTemplate {
        template_id: SigningTemplateId::from(
            row.try_get::<Uuid, _>("template_id")
                .map_err(map_storage_error)?,
        ),
        name: row.try_get("name").map_err(map_storage_error)?,
        body: row.try_get("body").map_err(map_storage_error)?,
        version: row
            .try_get::<i32, _>("version")
            .map_err(map_storage_error)?
            .try_into()
            .map_err(|_| {
                DomainError::Validation("storage error: negative template version".to_owned())
            })?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// Columns selected for a `contracts` row.
const CONTRACT_COLUMNS: &str = "contract_id, client_profile_id, template_revision, signers, \
     evidence_bundle_ref, status, payment_requirement, effective_at";

/// SQLx implementation of [`ContractRepository`], plus inherent
/// evidence methods.
#[derive(Debug, Clone)]
pub struct PgContractRepository {
    pool: PgPool,
}

impl PgContractRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl ContractRepository for PgContractRepository {
    async fn create(&self, contract: Contract) -> Result<Contract, DomainError> {
        ensure_client_profile(&self.pool, contract.client_profile_id).await?;
        let signers = serde_json::to_value(&contract.signers).map_err(|error| {
            DomainError::Validation(format!("signers do not serialize: {error}"))
        })?;
        let payment_requirement =
            serde_json::to_value(&contract.payment_requirement).map_err(|error| {
                DomainError::Validation(format!("payment requirement does not serialize: {error}"))
            })?;
        sqlx::query(
            "INSERT INTO contracts (contract_id, client_profile_id, status, template_revision, \
             payment_requirement, signers, evidence_bundle_ref, effective_at)\n             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(contract.contract_id.as_uuid())
        .bind(contract.client_profile_id.as_uuid())
        .bind(contract_state_as_str(contract.state))
        .bind(&contract.template_revision)
        .bind(payment_requirement)
        .bind(signers)
        .bind(contract.evidence_bundle_ref.as_deref())
        .bind(contract.effective_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(contract)
    }

    async fn get(&self, contract_id: ContractId) -> Result<Contract, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {CONTRACT_COLUMNS} FROM contracts WHERE contract_id = $1"
        ))
        .bind(contract_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_contract(&row),
            None => Err(DomainError::NotFound {
                kind: "contract".to_owned(),
                id: contract_id.to_string(),
            }),
        }
    }

    async fn update(&self, contract: Contract) -> Result<Contract, DomainError> {
        let signers = serde_json::to_value(&contract.signers).map_err(|error| {
            DomainError::Validation(format!("signers do not serialize: {error}"))
        })?;
        let payment_requirement =
            serde_json::to_value(&contract.payment_requirement).map_err(|error| {
                DomainError::Validation(format!("payment requirement does not serialize: {error}"))
            })?;
        let result = sqlx::query(
            "UPDATE contracts\n             SET status = $2, signers = $3, evidence_bundle_ref = $4, effective_at = $5, \
             payment_requirement = $6, updated_at = now()\n             WHERE contract_id = $1",
        )
        .bind(contract.contract_id.as_uuid())
        .bind(contract_state_as_str(contract.state))
        .bind(signers)
        .bind(contract.evidence_bundle_ref.as_deref())
        .bind(contract.effective_at)
        .bind(payment_requirement)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "contract".to_owned(),
                id: contract.contract_id.to_string(),
            });
        }
        Ok(contract)
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Contract>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {CONTRACT_COLUMNS} FROM contracts\n             WHERE client_profile_id = $1\n             ORDER BY created_at DESC, contract_id DESC\n             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_contract).collect()
    }
}

impl ContractDocumentRepository for PgContractRepository {
    async fn store_document(
        &self,
        contract_id: ContractId,
        document: &ContractDocument,
    ) -> Result<(), DomainError> {
        let document = serde_json::to_value(document).map_err(|error| {
            DomainError::Validation(format!("document does not serialize: {error}"))
        })?;
        let result = sqlx::query(
            "UPDATE contracts SET document = $2, updated_at = now() WHERE contract_id = $1",
        )
        .bind(contract_id.as_uuid())
        .bind(document)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "contract".to_owned(),
                id: contract_id.to_string(),
            });
        }
        Ok(())
    }

    async fn get_document(&self, contract_id: ContractId) -> Result<ContractDocument, DomainError> {
        let row: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT document FROM contracts WHERE contract_id = $1")
                .bind(contract_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
        match row {
            Some(serde_json::Value::Null) | None => Err(DomainError::NotFound {
                kind: "contract_document".to_owned(),
                id: contract_id.to_string(),
            }),
            Some(document) => serde_json::from_value(document).map_err(|error| {
                DomainError::Validation(format!("storage error: invalid document JSON: {error}"))
            }),
        }
    }

    async fn record_evidence(&self, evidence: &SigningEvidence) -> Result<(), DomainError> {
        sqlx::query(
            "INSERT INTO signing_evidence (evidence_id, contract_id, signer_name, signer_email, \
             signed_at, ip_hash, user_agent_hash, method)\n             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)\n             ON CONFLICT (contract_id, signer_email) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(evidence.contract_id.as_uuid())
        .bind(&evidence.signer_name)
        .bind(&evidence.signer_email)
        .bind(evidence.signed_at)
        .bind(&evidence.ip_hash)
        .bind(&evidence.user_agent_hash)
        .bind(signature_method_as_str(evidence.method))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(())
    }

    async fn list_evidence(
        &self,
        contract_id: ContractId,
    ) -> Result<Vec<SigningEvidence>, DomainError> {
        let rows = sqlx::query(
            "SELECT contract_id, signer_name, signer_email, signed_at, ip_hash, user_agent_hash, \
             method\n             FROM signing_evidence\n             WHERE contract_id = $1\n             ORDER BY signed_at ASC",
        )
        .bind(contract_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_evidence).collect()
    }
}

/// Columns selected for a `signing_templates` row.
const TEMPLATE_COLUMNS: &str = "template_id, name, body, version, created_at";

/// SQLx implementation of [`SigningTemplateRepository`].
#[derive(Debug, Clone)]
pub struct PgSigningTemplateRepository {
    pool: PgPool,
}

impl PgSigningTemplateRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns the highest version number stored for `name`, or
    /// `None` when no version exists yet.
    pub async fn max_version(&self, name: &str) -> Result<Option<u32>, DomainError> {
        let row: Option<i32> =
            sqlx::query_scalar("SELECT MAX(version) FROM signing_templates WHERE name = $1")
                .bind(name)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
        row.map(|v| {
            u32::try_from(v).map_err(|_| {
                DomainError::Validation("storage error: negative template version".to_owned())
            })
        })
        .transpose()
    }
}

impl SigningTemplateRepository for PgSigningTemplateRepository {
    async fn create(&self, template: SigningTemplate) -> Result<SigningTemplate, DomainError> {
        sqlx::query(
            "INSERT INTO signing_templates (template_id, name, body, version, created_at)\n             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(template.template_id.as_uuid())
        .bind(&template.name)
        .bind(&template.body)
        .bind(i32::try_from(template.version).map_err(|_| {
            DomainError::Validation("template version does not fit in i32".to_owned())
        })?)
        .bind(template.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(template)
    }

    async fn get(&self, template_id: SigningTemplateId) -> Result<SigningTemplate, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {TEMPLATE_COLUMNS} FROM signing_templates WHERE template_id = $1"
        ))
        .bind(template_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_template(&row),
            None => Err(DomainError::NotFound {
                kind: "signing_template".to_owned(),
                id: template_id.to_string(),
            }),
        }
    }

    async fn list(&self, limit: u64) -> Result<Vec<SigningTemplate>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {TEMPLATE_COLUMNS} FROM signing_templates\n             ORDER BY created_at DESC, template_id DESC\n             LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_template).collect()
    }
}
