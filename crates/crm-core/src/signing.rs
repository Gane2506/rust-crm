// SPDX-License-Identifier: GPL-3.0-only
//! Contract signing service implementation (task 13-SIGN-02).
//!
//! [`SigningServiceImpl`] implements
//! [`SigningService`](crm_domain::services::SigningService) over the
//! PostgreSQL repositories. It renders signing templates into
//! immutable document snapshots, collects signatures with forensic
//! evidence, and drives the
//! [`Contract`](crm_domain::contract::Contract) state machine through
//! the payment gate.
//!
//! Audit failures are swallowed: audit is fail-loud at the HTTP layer
//! and never fails a signing operation (same convention as
//! [`BillingServiceImpl`](crate::billing::BillingServiceImpl)).

use std::future::Future;

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::contract::{Contract, ContractState, Signer};
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, ContractId};
use crm_domain::repositories::{
    ContractDocumentRepository, ContractRepository, SigningTemplateRepository,
};
use crm_domain::services::{
    ActionContext, AuditService, NewContract, NewSignature, NewSigningTemplate, SigningService,
};
use crm_domain::signing::{
    SigningEvidence, SigningTemplate, SIGNING_CONTRACT_CREATED, SIGNING_CONTRACT_EFFECTIVE,
    SIGNING_CONTRACT_SIGNED, SIGNING_CONTRACT_VOIDED, SIGNING_PAYMENT_GATE_SATISFIED,
    SIGNING_SIGNATURE_REQUESTED, SIGNING_TEMPLATE_CREATED,
};

/// PostgreSQL-backed implementation of
/// [`SigningService`](crm_domain::services::SigningService).
///
/// The service owns the signing workflow:
/// * `create_contract` renders the template into an immutable
///   [`ContractDocument`](crm_domain::signing::ContractDocument)
///   snapshot, stored alongside the contract;
/// * `sign_contract` records forensic evidence per signature and
///   advances `Sent → PartiallySigned → SignedAwaitingPayment` as
///   signers sign;
/// * `satisfy_payment_gate` is the reconciliation hook: invoking it
///   asserts the required payment was verified and moves
///   `SignedAwaitingPayment → Effective`.
#[derive(Debug, Clone)]
pub struct SigningServiceImpl<CR, TR, A> {
    contracts: CR,
    templates: TR,
    audit: A,
}

impl<CR, TR, A> SigningServiceImpl<CR, TR, A> {
    /// Creates the service over the contract and template
    /// repositories and the audit service.
    pub fn new(contracts: CR, templates: TR, audit: A) -> Self {
        Self {
            contracts,
            templates,
            audit,
        }
    }

    /// Fetches a contract and enforces that it is visible in the
    /// caller's profile scope, returning [`DomainError::Forbidden`]
    /// on a mismatch. An agency-scoped context (`None`) passes —
    /// grant checks happen upstream, at the HTTP layer.
    async fn scoped_contract(
        &self,
        contract_id: ContractId,
        ctx: &ActionContext,
    ) -> Result<Contract, DomainError>
    where
        CR: ContractRepository,
    {
        let contract = self.contracts.get(contract_id).await?;
        check_scope(ctx, contract.client_profile_id)?;
        Ok(contract)
    }

    /// Records an audit event for a signing operation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails
    /// the operation).
    async fn audit_signing(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_kind: &str,
        target_id: &str,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                target_kind,
                target_id,
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
    }
}

/// Enforces profile scope on a contract: a profile-scoped context
/// may only touch its own profile's contracts; an agency-scoped
/// context (`None`) passes because grant checks happen upstream, at
/// the HTTP layer.
fn check_scope(ctx: &ActionContext, client_profile_id: ClientProfileId) -> Result<(), DomainError> {
    match ctx.client_profile_id {
        Some(ctx_profile) if ctx_profile != client_profile_id => Err(DomainError::Forbidden(
            "contract is not visible in this profile".to_string(),
        )),
        _ => Ok(()),
    }
}

// The service trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl<CR, TR, A> SigningService for SigningServiceImpl<CR, TR, A>
where
    CR: ContractRepository + ContractDocumentRepository + Send + Sync,
    TR: SigningTemplateRepository + Send + Sync,
    A: AuditService + Send + Sync,
{
    fn create_contract<'a>(
        &'a self,
        input: NewContract,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a {
        async move {
            check_scope(ctx, input.client_profile_id)?;
            let template = self.templates.get(input.template_id).await?;
            let document = template.render(&input.variables)?;
            let signers: Vec<Signer> = input
                .signers
                .into_iter()
                .map(|(name, email)| Signer {
                    name,
                    email,
                    signed_at: None,
                })
                .collect();
            if signers.is_empty() {
                return Err(DomainError::Validation(
                    "a contract requires at least one signer".to_string(),
                ));
            }
            let contract = Contract {
                contract_id: ContractId::new(),
                client_profile_id: input.client_profile_id,
                template_revision: format!("{}:v{}", template.name, template.version),
                signers,
                evidence_bundle_ref: None,
                state: ContractState::Draft,
                payment_requirement: input.payment_requirement,
                effective_at: None,
            };
            let contract = self.contracts.create(contract).await?;
            self.contracts
                .store_document(contract.contract_id, &document)
                .await?;
            self.audit_signing(
                ctx,
                SIGNING_CONTRACT_CREATED,
                "contract",
                &contract.contract_id.to_string(),
            )
            .await;
            Ok(contract)
        }
    }

    fn request_signature<'a>(
        &'a self,
        contract_id: ContractId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a {
        async move {
            let mut contract = self.scoped_contract(contract_id, ctx).await?;
            contract.transition_to(ContractState::Sent, false)?;
            let contract = self.contracts.update(contract).await?;
            self.audit_signing(
                ctx,
                SIGNING_SIGNATURE_REQUESTED,
                "contract",
                &contract.contract_id.to_string(),
            )
            .await;
            Ok(contract)
        }
    }

    fn sign_contract<'a>(
        &'a self,
        input: NewSignature,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a {
        async move {
            let mut contract = self.scoped_contract(input.contract_id, ctx).await?;
            // The domain fingerprints the network identifiers; raw
            // values never persist.
            let evidence = SigningEvidence::new(
                input.contract_id,
                input.signer_name,
                input.signer_email.clone(),
                input.ip_address,
                input.user_agent,
                input.method,
            )?;
            self.contracts.record_evidence(&evidence).await?;
            // Mark this signer's slot signed.
            let mut found = false;
            for signer in contract.signers.iter_mut() {
                if signer.email == input.signer_email {
                    signer.signed_at = Some(evidence.signed_at);
                    found = true;
                }
            }
            if !found {
                return Err(DomainError::Validation(format!(
                    "signer {} is not on this contract",
                    input.signer_email
                )));
            }
            // Advance the state machine: all signed →
            // SignedAwaitingPayment, otherwise PartiallySigned.
            // `transition_to` enforces the legal edges, and the machine
            // requires Sent → PartiallySigned → SignedAwaitingPayment,
            // so a contract in Sent with all signers done walks both
            // edges.
            let target = if contract
                .signers
                .iter()
                .all(|signer| signer.signed_at.is_some())
            {
                ContractState::SignedAwaitingPayment
            } else {
                ContractState::PartiallySigned
            };
            if contract.state == ContractState::Sent
                && target == ContractState::SignedAwaitingPayment
            {
                contract.transition_to(ContractState::PartiallySigned, false)?;
            }
            contract.transition_to(target, false)?;
            let contract = self.contracts.update(contract).await?;
            self.audit_signing(
                ctx,
                SIGNING_CONTRACT_SIGNED,
                "contract",
                &contract.contract_id.to_string(),
            )
            .await;
            Ok(contract)
        }
    }

    fn satisfy_payment_gate<'a>(
        &'a self,
        contract_id: ContractId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a {
        async move {
            let mut contract = self.scoped_contract(contract_id, ctx).await?;
            // This method is the reconciliation hook: invoking it
            // asserts the required payment was verified (a future
            // task wires it to billing). `transition_to` rejects the
            // move unless the contract is SignedAwaitingPayment.
            contract.transition_to(ContractState::Effective, true)?;
            let contract = self.contracts.update(contract).await?;
            self.audit_signing(
                ctx,
                SIGNING_PAYMENT_GATE_SATISFIED,
                "contract",
                &contract.contract_id.to_string(),
            )
            .await;
            self.audit_signing(
                ctx,
                SIGNING_CONTRACT_EFFECTIVE,
                "contract",
                &contract.contract_id.to_string(),
            )
            .await;
            Ok(contract)
        }
    }

    fn void_contract<'a>(
        &'a self,
        contract_id: ContractId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Contract, DomainError>> + Send + 'a {
        async move {
            let mut contract = self.scoped_contract(contract_id, ctx).await?;
            contract.transition_to(ContractState::Cancelled, false)?;
            let contract = self.contracts.update(contract).await?;
            self.audit_signing(
                ctx,
                SIGNING_CONTRACT_VOIDED,
                "contract",
                &contract.contract_id.to_string(),
            )
            .await;
            Ok(contract)
        }
    }

    fn list_contracts<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<Contract>, DomainError>> + Send + 'a {
        async move {
            let profile = ctx.client_profile_id.ok_or_else(|| {
                DomainError::Forbidden(
                    "contract listing requires a client-profile scope".to_string(),
                )
            })?;
            self.contracts.list_for_client(profile, limit).await
        }
    }

    fn create_template<'a>(
        &'a self,
        input: NewSigningTemplate,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<SigningTemplate, DomainError>> + Send + 'a {
        async move {
            // Templates are agency-level (no profile scope); the HTTP
            // layer authorizes with an agency-scoped grant.
            let templates = self.templates.list(10_000).await?;
            let version = templates
                .iter()
                .filter(|template| template.name == input.name)
                .map(|template| template.version)
                .max()
                .map(|max| max + 1)
                .unwrap_or(1);
            let template = SigningTemplate::new(input.name, input.body, version)?;
            let template = self.templates.create(template).await?;
            self.audit_signing(
                ctx,
                SIGNING_TEMPLATE_CREATED,
                "signing_template",
                &template.template_id.to_string(),
            )
            .await;
            Ok(template)
        }
    }
}
