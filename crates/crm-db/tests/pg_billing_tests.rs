// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Billing vertical slice (task 12-BILL-02).
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

use std::future::Future;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::billing::BillingServiceImpl;
use crm_db::billing::{PgInvoiceRepository, PgPaymentRepository};
use crm_db::run_migrations;
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::billing::{Invoice, InvoiceStatus, Money, PaymentStatus};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, InvoiceId};
use crm_domain::repositories::{InvoiceRepository, PaymentRepository};
use crm_domain::services::{
    ActionContext, AuditService, BillingService, NewInvoice, NewInvoiceLine, NewPayment,
};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (invoices reference it).
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

/// Builds the billing repositories over `pool`.
fn repos(pool: &PgPool) -> (PgInvoiceRepository, PgPaymentRepository) {
    (
        PgInvoiceRepository::new(pool.clone()),
        PgPaymentRepository::new(pool.clone()),
    )
}

/// Builds the real service over `pool` with a no-op audit service.
fn service(
    pool: &PgPool,
) -> BillingServiceImpl<PgInvoiceRepository, PgPaymentRepository, NoopAudit> {
    let (invoices, payments) = repos(pool);
    BillingServiceImpl::new(invoices, payments, NoopAudit)
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

fn usd(amount: i64) -> Money {
    Money::new(amount, "USD".to_string()).expect("valid money")
}

#[tokio::test]
async fn invoice_repo_crud_round_trip() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let (invoices, _) = repos(&pool);

    // Create + get.
    let created = invoices
        .create(Invoice::new(profile, "USD".to_string()).expect("valid"))
        .await
        .expect("create invoice");
    assert_eq!(created.status, InvoiceStatus::Draft);
    let fetched = invoices.get(created.invoice_id).await.expect("get");
    assert_eq!(fetched.invoice_id, created.invoice_id);
    assert!(fetched.lines.is_empty());

    // Update persists a status transition.
    let mut issued = fetched;
    issued
        .add_line("Work".to_string(), 1, usd(100))
        .expect("add line");
    issued.issue().expect("issue");
    let updated = invoices.update(issued).await.expect("update");
    assert_eq!(updated.status, InvoiceStatus::Issued);
    let refetched = invoices.get(created.invoice_id).await.expect("get");
    assert_eq!(refetched.status, InvoiceStatus::Issued);
    assert_eq!(refetched.lines.len(), 1);
    assert_eq!(refetched.lines[0].description, "Work");

    // Unknown id is a 404-shaped error.
    let missing = invoices.get(InvoiceId::new()).await;
    assert!(matches!(missing, Err(DomainError::NotFound { .. })));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn invoice_list_is_profile_scoped() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;
    let (invoices, _) = repos(&pool);

    invoices
        .create(Invoice::new(profile_a, "USD".to_string()).expect("valid"))
        .await
        .expect("create A");
    invoices
        .create(Invoice::new(profile_b, "EUR".to_string()).expect("valid"))
        .await
        .expect("create B");

    let listed_a = invoices
        .list_by_profile(profile_a, 100)
        .await
        .expect("list");
    assert_eq!(listed_a.len(), 1);
    assert_eq!(listed_a[0].client_profile_id, profile_a);
    let listed_b = invoices
        .list_by_profile(profile_b, 100)
        .await
        .expect("list");
    assert_eq!(listed_b.len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_full_lifecycle_to_paid() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    // Create → add a line (total 10_000) → issue.
    let invoice = svc
        .create_invoice(
            NewInvoice {
                client_profile_id: profile,
                currency: "USD".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create");
    svc.add_line(
        NewInvoiceLine {
            invoice_id: invoice.invoice_id,
            description: "Retainer".to_string(),
            quantity: 1,
            unit_price: usd(10_000),
        },
        &ctx,
    )
    .await
    .expect("add line");
    let issued = svc
        .issue_invoice(invoice.invoice_id, &ctx)
        .await
        .expect("issue");
    assert_eq!(issued.status, InvoiceStatus::Issued);

    // Partial payment: balance drops, status stays Issued.
    svc.record_payment(
        NewPayment {
            invoice_id: invoice.invoice_id,
            amount: usd(6_000),
        },
        &ctx,
    )
    .await
    .expect("partial payment");
    let balance = svc
        .invoice_balance(invoice.invoice_id, &ctx)
        .await
        .expect("balance");
    assert_eq!(balance.amount(), 4_000);

    // Full coverage: transitions to Paid.
    svc.record_payment(
        NewPayment {
            invoice_id: invoice.invoice_id,
            amount: usd(4_000),
        },
        &ctx,
    )
    .await
    .expect("final payment");
    let balance = svc
        .invoice_balance(invoice.invoice_id, &ctx)
        .await
        .expect("balance");
    assert_eq!(balance.amount(), 0);

    // Payments are listed oldest-first.
    let (_, payments) = repos(&pool);
    let listed = payments
        .list_by_invoice(invoice.invoice_id)
        .await
        .expect("list payments");
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|p| p.status == PaymentStatus::Recorded));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_rejects_overpayment() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    let invoice = svc
        .create_invoice(
            NewInvoice {
                client_profile_id: profile,
                currency: "USD".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create");
    svc.add_line(
        NewInvoiceLine {
            invoice_id: invoice.invoice_id,
            description: "Work".to_string(),
            quantity: 1,
            unit_price: usd(1_000),
        },
        &ctx,
    )
    .await
    .expect("add line");
    svc.issue_invoice(invoice.invoice_id, &ctx)
        .await
        .expect("issue");

    // Paying more than the total is rejected; nothing is recorded.
    let err = svc
        .record_payment(
            NewPayment {
                invoice_id: invoice.invoice_id,
                amount: usd(1_001),
            },
            &ctx,
        )
        .await
        .expect_err("overpayment must be rejected");
    assert!(matches!(err, DomainError::Validation(_)));
    let (_, payments) = repos(&pool);
    assert!(payments
        .list_by_invoice(invoice.invoice_id)
        .await
        .expect("list")
        .is_empty());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_reversal_reopens_paid_invoice() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    let invoice = svc
        .create_invoice(
            NewInvoice {
                client_profile_id: profile,
                currency: "USD".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create");
    svc.add_line(
        NewInvoiceLine {
            invoice_id: invoice.invoice_id,
            description: "Work".to_string(),
            quantity: 1,
            unit_price: usd(5_000),
        },
        &ctx,
    )
    .await
    .expect("add line");
    svc.issue_invoice(invoice.invoice_id, &ctx)
        .await
        .expect("issue");
    let payment = svc
        .record_payment(
            NewPayment {
                invoice_id: invoice.invoice_id,
                amount: usd(5_000),
            },
            &ctx,
        )
        .await
        .expect("full payment");

    // Reversing the covering payment reopens the invoice.
    let reversed = svc
        .reverse_payment(payment.payment_id, &ctx)
        .await
        .expect("reverse");
    assert_eq!(reversed.status, PaymentStatus::Reversed);
    // Reversal is idempotent.
    let again = svc
        .reverse_payment(payment.payment_id, &ctx)
        .await
        .expect("reverse again");
    assert_eq!(again.status, PaymentStatus::Reversed);

    let balance = svc
        .invoice_balance(invoice.invoice_id, &ctx)
        .await
        .expect("balance");
    assert_eq!(balance.amount(), 5_000);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_void_rules() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let ctx = ctx_for(profile);

    // Void from draft is idempotent.
    let draft = svc
        .create_invoice(
            NewInvoice {
                client_profile_id: profile,
                currency: "USD".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create");
    let voided = svc
        .void_invoice(draft.invoice_id, &ctx)
        .await
        .expect("void");
    assert_eq!(voided.status, crm_domain::billing::InvoiceStatus::Void);
    let voided_again = svc
        .void_invoice(draft.invoice_id, &ctx)
        .await
        .expect("void is idempotent");
    assert_eq!(
        voided_again.status,
        crm_domain::billing::InvoiceStatus::Void
    );

    // A paid invoice cannot be voided.
    let invoice = svc
        .create_invoice(
            NewInvoice {
                client_profile_id: profile,
                currency: "USD".to_string(),
            },
            &ctx,
        )
        .await
        .expect("create");
    svc.add_line(
        NewInvoiceLine {
            invoice_id: invoice.invoice_id,
            description: "Work".to_string(),
            quantity: 1,
            unit_price: usd(100),
        },
        &ctx,
    )
    .await
    .expect("add line");
    svc.issue_invoice(invoice.invoice_id, &ctx)
        .await
        .expect("issue");
    svc.record_payment(
        NewPayment {
            invoice_id: invoice.invoice_id,
            amount: usd(100),
        },
        &ctx,
    )
    .await
    .expect("pay in full");
    let err = svc
        .void_invoice(invoice.invoice_id, &ctx)
        .await
        .expect_err("paid invoice must not be voided");
    assert!(matches!(err, DomainError::Validation(_)));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_enforces_scope_isolation() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;
    let svc = service(&pool);
    let ctx_a = ctx_for(profile_a);
    let ctx_b = ctx_for(profile_b);

    let invoice = svc
        .create_invoice(
            NewInvoice {
                client_profile_id: profile_a,
                currency: "USD".to_string(),
            },
            &ctx_a,
        )
        .await
        .expect("create");

    // Another profile cannot touch the invoice.
    let denied = svc
        .issue_invoice(invoice.invoice_id, &ctx_b)
        .await
        .expect_err("cross-profile issue must be forbidden");
    assert!(matches!(denied, DomainError::Forbidden(_)));
    let denied = svc
        .record_payment(
            NewPayment {
                invoice_id: invoice.invoice_id,
                amount: usd(10),
            },
            &ctx_b,
        )
        .await
        .expect_err("cross-profile payment must be forbidden");
    assert!(matches!(denied, DomainError::Forbidden(_)));

    // An agency-scoped context (None) may operate (grant checks happen
    // upstream, at the HTTP layer).
    let agency_ctx = ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::new(),
        client_profile_id: None,
        correlation_id: Uuid::new_v4(),
    };
    svc.add_line(
        NewInvoiceLine {
            invoice_id: invoice.invoice_id,
            description: "Agency line".to_string(),
            quantity: 1,
            unit_price: usd(10),
        },
        &agency_ctx,
    )
    .await
    .expect("agency ctx may add a line");

    drop_db(&admin_url, &db_name, pool).await;
}
