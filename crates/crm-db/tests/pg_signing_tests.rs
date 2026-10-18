// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Contract signing vertical slice (task
//! 13-SIGN-02).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.: <redacted>
//! `postgres://postgres:postgres@localhost:5432/crm_test`
//!
//! ```sh
//! docker compose up -d            # ephemeral Postgres 16 on :5432
//! DATABASE_URL=<redacted>
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Each test creates its own database, migrates a fresh schema, and
//! drops it afterwards. The service tests wire the real SQLx
//! repositories with a no-op audit service.

mod common;

use std::collections::HashMap;
use std::future::Future;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::signing::SigningServiceImpl;
use crm_db::run_migrations;
use crm_db::signing::{PgContractRepository, PgSigningTemplateRepository};
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::contract::{ContractState, PaymentRequirement};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, ContractId};
use crm_domain::repositories::{ContractDocumentRepository, SigningTemplateRepository};
use crm_domain::services::{
    ActionContext, AuditService, NewContract, NewSignature, NewSigningTemplate, SigningService,
};
use crm_domain::signing::SignatureMethod;

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (contracts reference it).
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
    // The trait declares `fn -> impl Future` (not `async fn`), so the
    // impl must keep that exact form.
    #[allow(clippy::manual_async_fn)]
    fn record_action<'a>(
        &'a self,
        ctx: &'a ActionContext,
        action: &'a str,
        target_kind: &'a str,
        target_id: &'a str,
        permission_decision: PermissionDecision,
        result: ActionResult,
        _before_ref: Option<&'a str>,
        _after_ref: Option<&'a str>,
    ) -> impl Future<Output = Result<AuditEvent, DomainError>> + Send + 'a {
        async move {
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
}

/// Builds the signing repositories over `pool`.
fn repos(pool: &PgPool) -> (PgContractRepository, PgSigningTemplateRepository) {
    (
        PgContractRepository::new(pool.clone()),
        PgSigningTemplateRepository::new(pool.clone()),
    )
}

/// Builds the real service over `pool` with a no-op audit service.
fn service(
    pool: &PgPool,
) -> SigningServiceImpl<PgContractRepository, PgSigningTemplateRepository, NoopAudit> {
    let (contracts, templates) = repos(pool);
    SigningServiceImpl::new(contracts, templates, NoopAudit)
}

/// Test action context scoped to `client_profile_id`.
fn ctx_for(profile_id: ClientProfileId) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::new(),
        client_profile_id: Some(profile_id),
        correlation_id: Uuid::new_v4(),
    }
}

fn template_input() -> NewSigningTemplate {
    NewSigningTemplate {
        name: "MSA".to_string(),
        body: "Dear {{client_name}}, your {{plan}} plan starts {{start_date}}.".to_string(),
    }
}

fn variables() -> HashMap<String, String> {
    HashMap::from([
        ("client_name".to_string(), "Acme Corp".to_string()),
        ("plan".to_string(), "Pro".to_string()),
        ("start_date".to_string(), "2026-11-01".to_string()),
    ])
}

fn contract_input(
    profile: ClientProfileId,
    template_id: crm_domain::ids::SigningTemplateId,
) -> NewContract {
    NewContract {
        client_profile_id: profile,
        template_id,
        variables: variables(),
        payment_requirement: PaymentRequirement::None,
        signers: vec![("Ada Lovelace".to_string(), "ada@example.com".to_string())],
    }
}

fn signature_input(contract_id: ContractId) -> NewSignature {
    NewSignature {
        contract_id,
        signer_name: "Ada Lovelace".to_string(),
        signer_email: "ada@example.com".to_string(),
        ip_address: "203.0.113.7".to_string(),
        user_agent: "TestAgent/1.0".to_string(),
        method: SignatureMethod::TypedSignature,
    }
}

#[tokio::test]
async fn template_repo_crud_round_trip() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let (_, templates) = repos(&pool);

    let created = templates
        .create(
            crm_domain::signing::SigningTemplate::new(
                "MSA".to_string(),
                "Hello {{name}}.".to_string(),
                1,
            )
            .expect("valid template"),
        )
        .await
        .expect("create template");
    let fetched = templates
        .get(created.template_id)
        .await
        .expect("get template");
    assert_eq!(fetched.template_id, created.template_id);
    assert_eq!(fetched.version, 1);

    let listed = templates.list(10).await.expect("list templates");
    assert_eq!(listed.len(), 1);

    let missing = templates
        .get(crm_domain::ids::SigningTemplateId::new())
        .await;
    assert!(matches!(missing, Err(DomainError::NotFound { .. })));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn template_versions_are_append_only() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let svc = service(&pool);
    let ctx = ctx_for(ClientProfileId::new());

    let v1 = svc
        .create_template(template_input(), &ctx)
        .await
        .expect("create v1");
    assert_eq!(v1.version, 1);
    let v2 = svc
        .create_template(template_input(), &ctx)
        .await
        .expect("create v2");
    assert_eq!(v2.version, 2);
    assert_ne!(v1.template_id, v2.template_id);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn create_contract_renders_document_snapshot() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    let template = svc
        .create_template(template_input(), &ctx)
        .await
        .expect("create template");
    let contract = svc
        .create_contract(contract_input(profile, template.template_id), &ctx)
        .await
        .expect("create contract");
    assert_eq!(contract.state, ContractState::Draft);
    assert_eq!(contract.signers.len(), 1);
    assert!(contract.signers[0].signed_at.is_none());

    // The rendered snapshot is stored and retrievable.
    let (contracts, _) = repos(&pool);
    let document = contracts
        .get_document(contract.contract_id)
        .await
        .expect("get document");
    assert_eq!(
        document.rendered_body,
        "Dear Acme Corp, your Pro plan starts 2026-11-01."
    );
    assert_eq!(document.content_hash.len(), 64);
    assert_eq!(document.template_version, 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn full_lifecycle_to_effective() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    let template = svc
        .create_template(template_input(), &ctx)
        .await
        .expect("create template");
    let contract = svc
        .create_contract(contract_input(profile, template.template_id), &ctx)
        .await
        .expect("create contract");

    // Draft → Sent.
    let contract = svc
        .request_signature(contract.contract_id, &ctx)
        .await
        .expect("request signature");
    assert_eq!(contract.state, ContractState::Sent);

    // Sign → SignedAwaitingPayment (single signer).
    let contract = svc
        .sign_contract(signature_input(contract.contract_id), &ctx)
        .await
        .expect("sign");
    assert_eq!(contract.state, ContractState::SignedAwaitingPayment);
    assert!(contract.signers[0].signed_at.is_some());
    assert!(!contract.is_effective());

    // Payment gate → Effective.
    let contract = svc
        .satisfy_payment_gate(contract.contract_id, &ctx)
        .await
        .expect("satisfy payment gate");
    assert_eq!(contract.state, ContractState::Effective);
    assert!(contract.is_effective());
    assert!(contract.effective_at.is_some());

    // Evidence was recorded with fingerprints, not raw values.
    let (contracts, _) = repos(&pool);
    let evidence = contracts
        .list_evidence(contract.contract_id)
        .await
        .expect("list evidence");
    assert_eq!(evidence.len(), 1);
    assert!(!evidence[0].ip_hash.contains("203.0.113.7"));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn sign_contract_rejects_unknown_signer() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    let template = svc
        .create_template(template_input(), &ctx)
        .await
        .expect("create template");
    let contract = svc
        .create_contract(contract_input(profile, template.template_id), &ctx)
        .await
        .expect("create contract");
    svc.request_signature(contract.contract_id, &ctx)
        .await
        .expect("request signature");

    let mut bad = signature_input(contract.contract_id);
    bad.signer_email = "mallory@example.com".to_string();
    let err = svc
        .sign_contract(bad, &ctx)
        .await
        .expect_err("unknown signer");
    assert!(matches!(err, DomainError::Validation(_)));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn void_contract_rules() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    let template = svc
        .create_template(template_input(), &ctx)
        .await
        .expect("create template");

    // Void from Draft.
    let draft = svc
        .create_contract(contract_input(profile, template.template_id), &ctx)
        .await
        .expect("create contract");
    let voided = svc
        .void_contract(draft.contract_id, &ctx)
        .await
        .expect("void draft");
    assert_eq!(voided.state, ContractState::Cancelled);

    // Void is idempotent on Cancelled.
    let voided_again = svc
        .void_contract(draft.contract_id, &ctx)
        .await
        .expect("void is idempotent");
    assert_eq!(voided_again.state, ContractState::Cancelled);

    // An Effective contract cannot be voided.
    let contract = svc
        .create_contract(contract_input(profile, template.template_id), &ctx)
        .await
        .expect("create contract");
    svc.request_signature(contract.contract_id, &ctx)
        .await
        .expect("request");
    svc.sign_contract(signature_input(contract.contract_id), &ctx)
        .await
        .expect("sign");
    svc.satisfy_payment_gate(contract.contract_id, &ctx)
        .await
        .expect("effective");
    let err = svc
        .void_contract(contract.contract_id, &ctx)
        .await
        .expect_err("void effective must fail");
    assert!(matches!(err, DomainError::Validation(_)));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn contracts_are_scope_isolated() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;
    let svc = service(&pool);

    let template = svc
        .create_template(template_input(), &ctx_for(profile_a))
        .await
        .expect("create template");
    let contract = svc
        .create_contract(
            contract_input(profile_a, template.template_id),
            &ctx_for(profile_a),
        )
        .await
        .expect("create contract");

    // Cross-profile access is forbidden.
    let err = svc
        .request_signature(contract.contract_id, &ctx_for(profile_b))
        .await
        .expect_err("cross-profile must fail");
    assert!(matches!(err, DomainError::Forbidden(_)));

    // Each profile lists only its own contracts.
    let listed_a = svc
        .list_contracts(100, &ctx_for(profile_a))
        .await
        .expect("list A");
    assert_eq!(listed_a.len(), 1);
    let listed_b = svc
        .list_contracts(100, &ctx_for(profile_b))
        .await
        .expect("list B");
    assert!(listed_b.is_empty());

    drop_db(&admin_url, &db_name, pool).await;
}
