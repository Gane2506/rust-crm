// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Analytics vertical slice (task 15-DASH-02).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.
//! `postgres://postgres:postgres@localhost:5432/crm_test`
//!
//! ```sh
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Each test creates its own database, migrates a fresh schema, and
//! drops it afterwards. The service tests wire the real SQLx
//! repositories with the database pool and a no-op audit service.

mod common;

use std::future::Future;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::dashboard::DashboardServiceImpl;
use crm_db::dashboard::{PgDashboardRepository, PgReportRepository};
use crm_db::run_migrations;
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::dashboard::{
    Dashboard, DashboardWidget, DateRange, MetricAggregation, MetricDefinition, MetricEntity,
    Report, ReportParameter, WidgetData, WidgetPosition, WidgetType,
};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, StaffId};
use crm_domain::repositories::DashboardRepository;
use crm_domain::services::{
    ActionContext, AuditService, DashboardService, NewDashboard, NewReport, UpdateDashboard,
};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (dashboards reference it).
async fn ensure_profile(pool: &PgPool, id: ClientProfileId) {
    sqlx::query(
        "INSERT INTO client_profiles (client_profile_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(id.as_uuid())
    .execute(pool)
    .await
    .expect("insert client profile");
}

/// Inserts a contact row (metrics count over the entity tables).
async fn insert_contact(pool: &PgPool, profile: ClientProfileId) {
    sqlx::query(
        "INSERT INTO contacts (contact_id, client_profile_id, lifecycle, first_source, latest_source) \
         VALUES ($1, $2, 'prospect', 'test', 'test')",
    )
    .bind(Uuid::new_v4())
    .bind(profile.as_uuid())
    .execute(pool)
    .await
    .expect("insert contact");
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

/// Builds the real service over `pool` with a no-op audit service.
fn service(
    pool: &PgPool,
) -> DashboardServiceImpl<PgDashboardRepository, PgReportRepository, NoopAudit> {
    DashboardServiceImpl::new(
        PgDashboardRepository::new(pool.clone()),
        PgReportRepository::new(pool.clone()),
        pool.clone(),
        NoopAudit,
    )
}

/// Test action context scoped to `client_profile_id`.
fn ctx_for(profile_id: Option<ClientProfileId>) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::new(),
        client_profile_id: profile_id,
        correlation_id: Uuid::new_v4(),
    }
}

fn contact_count_metric() -> MetricDefinition {
    MetricDefinition::new(
        MetricEntity::Contact,
        MetricAggregation::count(),
        None,
        DateRange::Last30Days,
    )
    .expect("valid metric")
}

fn count_widget_at(x: u32, y: u32) -> DashboardWidget {
    DashboardWidget::new(
        "Contacts".to_string(),
        WidgetType::Count,
        contact_count_metric(),
        WidgetPosition::new(x, y),
    )
    .expect("valid widget")
}

#[tokio::test]
async fn dashboard_crud_round_trip() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let repos = PgDashboardRepository::new(pool.clone());

    let created = repos
        .save(Dashboard::new("Master".to_string(), Some(profile)).expect("valid"))
        .await
        .expect("save dashboard");
    assert_eq!(created.name(), "Master");
    assert_eq!(created.client_profile_id(), Some(profile));

    let fetched = repos.get(created.dashboard_id()).await.expect("get");
    assert_eq!(fetched, created);

    let listed = repos
        .list(Some(profile), 10)
        .await
        .expect("list dashboards");
    assert_eq!(listed.len(), 1);

    repos.delete(created.dashboard_id()).await.expect("delete");
    assert!(repos.get(created.dashboard_id()).await.is_err());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn dashboard_duplicate_widget_position_rejected() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);

    let dashboard = svc
        .create_dashboard(
            NewDashboard {
                name: "Dupes".to_string(),
                client_profile_id: Some(profile),
            },
            &ctx_for(Some(profile)),
        )
        .await
        .expect("create dashboard");

    // The domain rejects the duplicate position before any write.
    let updated = svc
        .update_dashboard(
            dashboard.dashboard_id(),
            UpdateDashboard {
                name: None,
                widgets_to_add: vec![count_widget_at(0, 0), count_widget_at(0, 0)],
                widget_ids_to_remove: vec![],
            },
            &ctx_for(Some(profile)),
        )
        .await;
    assert!(updated.is_err(), "duplicate grid position must be rejected");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn run_widget_count_returns_real_numbers() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);

    // Three contacts land in the entity table; the metric must count
    // exactly those.
    for _ in 0..3 {
        insert_contact(&pool, profile).await;
    }

    let dashboard = svc
        .create_dashboard(
            NewDashboard {
                name: "Counts".to_string(),
                client_profile_id: Some(profile),
            },
            &ctx_for(Some(profile)),
        )
        .await
        .expect("create dashboard");
    let dashboard = svc
        .update_dashboard(
            dashboard.dashboard_id(),
            UpdateDashboard {
                name: None,
                widgets_to_add: vec![count_widget_at(0, 0)],
                widget_ids_to_remove: vec![],
            },
            &ctx_for(Some(profile)),
        )
        .await
        .expect("add widget");
    let widget_id = dashboard.widgets()[0].widget_id();

    let result = svc
        .run_widget(dashboard.dashboard_id(), widget_id, &ctx_for(Some(profile)))
        .await
        .expect("run widget");
    assert!(
        matches!(result.data, WidgetData::Count(3)),
        "expected 3 contacts, got {:?}",
        result.data
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn run_widget_breakdown_groups_by_lifecycle() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);

    for _ in 0..2 {
        insert_contact(&pool, profile).await;
    }
    sqlx::query("UPDATE contacts SET lifecycle = 'customer' WHERE client_profile_id = $1")
        .bind(profile.as_uuid())
        .execute(&pool)
        .await
        .expect("promote contacts");

    let metric = MetricDefinition::new(
        MetricEntity::Contact,
        MetricAggregation::count(),
        Some("lifecycle".to_string()),
        DateRange::Last30Days,
    )
    .expect("valid metric");
    let widget = DashboardWidget::new(
        "By lifecycle".to_string(),
        WidgetType::Breakdown,
        metric,
        WidgetPosition::new(0, 0),
    )
    .expect("valid widget");

    let dashboard = svc
        .create_dashboard(
            NewDashboard {
                name: "Breakdown".to_string(),
                client_profile_id: Some(profile),
            },
            &ctx_for(Some(profile)),
        )
        .await
        .expect("create dashboard");
    let dashboard = svc
        .update_dashboard(
            dashboard.dashboard_id(),
            UpdateDashboard {
                name: None,
                widgets_to_add: vec![widget],
                widget_ids_to_remove: vec![],
            },
            &ctx_for(Some(profile)),
        )
        .await
        .expect("add widget");
    let widget_id = dashboard.widgets()[0].widget_id();

    let result = svc
        .run_widget(dashboard.dashboard_id(), widget_id, &ctx_for(Some(profile)))
        .await
        .expect("run widget");
    match result.data {
        WidgetData::Breakdown(buckets) => {
            assert_eq!(buckets.len(), 1, "expected one lifecycle group");
            assert_eq!(buckets[0].key, "customer");
            assert_eq!(buckets[0].value, 2.0);
        }
        other => panic!("expected breakdown data, got {other:?}"),
    }

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn dashboards_are_profile_scoped() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;
    let svc = service(&pool);

    let dashboard = svc
        .create_dashboard(
            NewDashboard {
                name: "A only".to_string(),
                client_profile_id: Some(profile_a),
            },
            &ctx_for(Some(profile_a)),
        )
        .await
        .expect("create dashboard");

    // Another profile's context cannot read it.
    assert!(svc
        .get_dashboard(dashboard.dashboard_id(), &ctx_for(Some(profile_b)))
        .await
        .is_err());
    // Listing is scoped too.
    let listed = svc
        .list_dashboards(10, &ctx_for(Some(profile_b)))
        .await
        .expect("list");
    assert!(listed.is_empty());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn report_crud_round_trip() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let svc = service(&pool);

    let report = svc
        .create_report(
            NewReport {
                name: "Weekly".to_string(),
                owner: StaffId::new(),
                metric: contact_count_metric(),
                parameters: vec![ReportParameter::new(
                    "source".to_string(),
                    "referral".to_string(),
                )
                .expect("valid parameter")],
            },
            &ctx_for(None),
        )
        .await
        .expect("create report");
    assert_eq!(report.name(), "Weekly");
    assert_eq!(report.parameters().len(), 1);

    let reports = svc.list_reports(10, &ctx_for(None)).await.expect("list");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].report_id(), report.report_id());

    // Reports are immutable: the repository has no update path, and
    // re-saving the same aggregate would be a new report.
    let again: Report = serde_json::from_value(serde_json::to_value(&report).expect("serializes"))
        .expect("deserializes");
    assert_eq!(again, report);

    drop_db(&admin_url, &db_name, pool).await;
}
