// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Workflow builder vertical slice (task 10-WORK-02).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.: <redacted>
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

use chrono::{SubsecRound, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::workflow::WorkflowServiceImpl;
use crm_db::run_migrations;
use crm_db::workflow::{
    PgTriggerRepository, PgWorkflowEdgeRepository, PgWorkflowNodeRepository, PgWorkflowRepository,
    PgWorkflowVersionRepository,
};
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, WorkflowId, WorkflowNodeId};
use crm_domain::repositories::{
    TriggerRepository, WorkflowEdgeRepository, WorkflowNodeRepository, WorkflowRepository,
    WorkflowVersionRepository,
};
use crm_domain::services::{
    ActionContext, AuditService, NewTrigger, NewWorkflow, NewWorkflowEdge, NewWorkflowNode,
    WorkflowService,
};
use crm_domain::workflow::{
    SimulationStepStatus, TriggerType, Workflow, WorkflowNode, WorkflowNodeType, WorkflowStatus,
};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (workflows reference it).
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
            actor_kind: ActorKind::Human,
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

/// Builds the service over real repositories and a no-op audit service.
fn test_service(
    pool: &PgPool,
) -> WorkflowServiceImpl<
    PgWorkflowRepository,
    PgWorkflowNodeRepository,
    PgWorkflowEdgeRepository,
    PgTriggerRepository,
    PgWorkflowVersionRepository,
    NoopAudit,
> {
    WorkflowServiceImpl::new(
        PgWorkflowRepository::new(pool.clone()),
        PgWorkflowNodeRepository::new(pool.clone()),
        PgWorkflowEdgeRepository::new(pool.clone()),
        PgTriggerRepository::new(pool.clone()),
        PgWorkflowVersionRepository::new(pool.clone()),
        NoopAudit,
    )
}

/// Action context for `actor` scoped to `profile`.
fn ctx_for(actor: ActorId, profile: Option<ClientProfileId>) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: actor,
        client_profile_id: profile,
        correlation_id: Uuid::new_v4(),
    }
}

fn new_workflow_input(profile: ClientProfileId) -> NewWorkflow {
    NewWorkflow {
        client_profile_id: profile,
        name: "Welcome drip".to_string(),
    }
}

fn new_node_input(
    workflow_id: WorkflowId,
    node_type: WorkflowNodeType,
    config: serde_json::Value,
) -> NewWorkflowNode {
    NewWorkflowNode {
        workflow_id,
        node_type,
        config,
        position: (10, 20),
    }
}

#[tokio::test]
async fn workflow_repository_crud() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgWorkflowRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let workflow = Workflow::new(profile, "Onboarding".to_string()).expect("valid workflow");
    let workflow_id = workflow.workflow_id;
    let stored = repo.create(workflow).await.expect("create workflow");
    assert_eq!(stored.workflow_id, workflow_id);
    assert_eq!(stored.status, WorkflowStatus::Draft);

    let fetched = repo.get(workflow_id).await.expect("get workflow");
    assert_eq!(fetched.name, "Onboarding");

    let mut updated = fetched;
    updated.publish().expect("publish");
    let updated = repo.update(updated).await.expect("update workflow");
    assert_eq!(updated.status, WorkflowStatus::Published);

    let listed = repo.list_by_profile(profile, 10).await.expect("list");
    assert_eq!(listed.len(), 1);

    // Unknown workflow is NotFound.
    let missing = repo.get(WorkflowId::new()).await.expect_err("missing");
    assert!(matches!(missing, DomainError::NotFound { .. }));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn node_edge_and_trigger_repositories() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let workflows = PgWorkflowRepository::new(pool.clone());
    let nodes = PgWorkflowNodeRepository::new(pool.clone());
    let edges = PgWorkflowEdgeRepository::new(pool.clone());
    let triggers = PgTriggerRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let workflow = workflows
        .create(Workflow::new(profile, "Graph".to_string()).expect("valid"))
        .await
        .expect("create workflow");
    let workflow_id = workflow.workflow_id;

    // Nodes.
    let trigger_node = nodes
        .create(
            WorkflowNode::new(
                workflow_id,
                WorkflowNodeType::TriggerNode,
                json!({"trigger": "manual"}),
                (0, 0),
            )
            .expect("valid"),
        )
        .await
        .expect("create trigger node");
    let action_node = nodes
        .create(
            WorkflowNode::new(
                workflow_id,
                WorkflowNodeType::ActionNode,
                json!({"action": "email.send"}),
                (100, 0),
            )
            .expect("valid"),
        )
        .await
        .expect("create action node");
    assert_eq!(trigger_node.position, (0, 0));

    // Node update (config + position).
    let mut moved = action_node.clone();
    moved.move_to((150, 25));
    let moved = nodes.update(moved).await.expect("update node");
    assert_eq!(moved.position, (150, 25));

    // Edges.
    let edge = edges
        .create(
            crm_domain::workflow::WorkflowEdge::new(
                workflow_id,
                trigger_node.node_id,
                action_node.node_id,
                Some("go".to_string()),
            )
            .expect("valid"),
        )
        .await
        .expect("create edge");
    assert_eq!(edge.label.as_deref(), Some("go"));
    let listed = edges
        .list_by_workflow(workflow_id, 10)
        .await
        .expect("list edges");
    assert_eq!(listed.len(), 1);
    let fetched = edges.get(edge.edge_id).await.expect("get edge");
    assert_eq!(fetched.edge_id, edge.edge_id);
    edges.delete(edge.edge_id).await.expect("delete edge");
    assert!(edges.get(edge.edge_id).await.is_err());

    // Trigger: set replaces.
    let first = triggers
        .set(
            crm_domain::workflow::Trigger::new(workflow_id, TriggerType::ManualTrigger, json!({}))
                .expect("valid"),
        )
        .await
        .expect("set trigger");
    assert_eq!(first.trigger_type, TriggerType::ManualTrigger);
    let second = triggers
        .set(
            crm_domain::workflow::Trigger::new(
                workflow_id,
                TriggerType::ScheduleTrigger,
                json!({"cron": "0 9 * * *"}),
            )
            .expect("valid"),
        )
        .await
        .expect("replace trigger");
    assert_eq!(second.trigger_type, TriggerType::ScheduleTrigger);
    let current = triggers
        .get_by_workflow(workflow_id)
        .await
        .expect("get trigger");
    assert_eq!(current.map(|t| t.trigger_id), Some(second.trigger_id));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn publish_snapshots_immutable_version() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = test_service(&pool);
    let versions = PgWorkflowVersionRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let ctx = ctx_for(ActorId::new(), Some(profile));

    let workflow = service
        .create_workflow(new_workflow_input(profile), &ctx)
        .await
        .expect("create workflow");
    let node = service
        .add_node(
            new_node_input(
                workflow.workflow_id,
                WorkflowNodeType::TriggerNode,
                json!({"trigger": "manual"}),
            ),
            &ctx,
        )
        .await
        .expect("add node");
    service
        .set_trigger(
            NewTrigger {
                workflow_id: workflow.workflow_id,
                trigger_type: TriggerType::ManualTrigger,
                config: json!({}),
            },
            &ctx,
        )
        .await
        .expect("set trigger");

    let published = service
        .publish_workflow(workflow.workflow_id, &ctx)
        .await
        .expect("publish");
    assert_eq!(published.status, WorkflowStatus::Published);

    let listed = versions
        .list_by_workflow(workflow.workflow_id, 10)
        .await
        .expect("list versions");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].version_number, 1);
    // The snapshot froze the definition: node + trigger present.
    let snapshot = &listed[0].snapshot;
    assert!(snapshot.get("nodes").is_some());
    assert!(snapshot.get("trigger").is_some());
    assert_eq!(snapshot["nodes"].as_array().expect("nodes array").len(), 1);
    assert!(
        snapshot.to_string().contains(&node.node_id.to_string()),
        "snapshot embeds the node"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn edge_validation_rejects_self_loops_and_dangling_endpoints() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = test_service(&pool);
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let ctx = ctx_for(ActorId::new(), Some(profile));

    let workflow = service
        .create_workflow(new_workflow_input(profile), &ctx)
        .await
        .expect("create workflow");
    let node = service
        .add_node(
            new_node_input(
                workflow.workflow_id,
                WorkflowNodeType::ActionNode,
                json!({"action": "email.send"}),
            ),
            &ctx,
        )
        .await
        .expect("add node");

    // Self-loop rejected by the domain.
    let self_loop = service
        .add_edge(
            NewWorkflowEdge {
                workflow_id: workflow.workflow_id,
                from_node: node.node_id,
                to_node: node.node_id,
                label: None,
            },
            &ctx,
        )
        .await;
    assert!(self_loop.is_err());

    // Dangling endpoint rejected: the to-node does not exist.
    let dangling = service
        .add_edge(
            NewWorkflowEdge {
                workflow_id: workflow.workflow_id,
                from_node: node.node_id,
                to_node: WorkflowNodeId::new(),
                label: None,
            },
            &ctx,
        )
        .await;
    assert!(matches!(dangling, Err(DomainError::NotFound { .. })));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn remove_node_deletes_dangling_edges() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = test_service(&pool);
    let edges = PgWorkflowEdgeRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let ctx = ctx_for(ActorId::new(), Some(profile));

    let workflow = service
        .create_workflow(new_workflow_input(profile), &ctx)
        .await
        .expect("create workflow");
    let trigger = service
        .add_node(
            new_node_input(
                workflow.workflow_id,
                WorkflowNodeType::TriggerNode,
                json!({"trigger": "manual"}),
            ),
            &ctx,
        )
        .await
        .expect("add trigger");
    let action = service
        .add_node(
            new_node_input(
                workflow.workflow_id,
                WorkflowNodeType::ActionNode,
                json!({"action": "email.send"}),
            ),
            &ctx,
        )
        .await
        .expect("add action");
    service
        .add_edge(
            NewWorkflowEdge {
                workflow_id: workflow.workflow_id,
                from_node: trigger.node_id,
                to_node: action.node_id,
                label: None,
            },
            &ctx,
        )
        .await
        .expect("add edge");

    service
        .remove_node(action.node_id, &ctx)
        .await
        .expect("remove node");
    let remaining = edges
        .list_by_workflow(workflow.workflow_id, 10)
        .await
        .expect("list edges");
    assert!(remaining.is_empty(), "dangling edges are removed");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn simulate_returns_trace_without_executing() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = test_service(&pool);
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let ctx = ctx_for(ActorId::new(), Some(profile));

    let workflow = service
        .create_workflow(new_workflow_input(profile), &ctx)
        .await
        .expect("create workflow");
    let trigger = service
        .add_node(
            new_node_input(
                workflow.workflow_id,
                WorkflowNodeType::TriggerNode,
                json!({"trigger": "manual"}),
            ),
            &ctx,
        )
        .await
        .expect("add trigger");
    let action = service
        .add_node(
            new_node_input(
                workflow.workflow_id,
                WorkflowNodeType::ActionNode,
                json!({"action": "email.send"}),
            ),
            &ctx,
        )
        .await
        .expect("add action");
    service
        .add_edge(
            NewWorkflowEdge {
                workflow_id: workflow.workflow_id,
                from_node: trigger.node_id,
                to_node: action.node_id,
                label: None,
            },
            &ctx,
        )
        .await
        .expect("add edge");

    let trace = service
        .simulate(workflow.workflow_id, json!({}), &ctx)
        .await
        .expect("simulate");
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0].node_id, trigger.node_id);
    assert_eq!(trace[0].status, SimulationStepStatus::WouldRun);
    assert_eq!(trace[1].node_id, action.node_id);
    // Nothing was executed: the workflow is still a draft.
    let nodes = PgWorkflowNodeRepository::new(pool.clone());
    let stored = nodes
        .list_by_workflow(workflow.workflow_id, 10)
        .await
        .expect("nodes unchanged");
    assert_eq!(stored.len(), 2);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn workflow_service_scope_isolation() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = test_service(&pool);
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;

    let ctx_a = ctx_for(ActorId::new(), Some(profile_a));
    let workflow = service
        .create_workflow(new_workflow_input(profile_a), &ctx_a)
        .await
        .expect("create workflow");

    // A context scoped to another profile cannot touch the workflow.
    let ctx_b = ctx_for(ActorId::new(), Some(profile_b));
    let denied = service.publish_workflow(workflow.workflow_id, &ctx_b).await;
    assert!(matches!(denied, Err(DomainError::Forbidden(_))));

    // An agency-scoped context (None) may operate (grant checks happen
    // upstream at the HTTP layer).
    let ctx_agency = ctx_for(ActorId::new(), None);
    service
        .publish_workflow(workflow.workflow_id, &ctx_agency)
        .await
        .expect("agency context publishes");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn workflow_lifecycle_archive_is_idempotent() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let service = test_service(&pool);
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let ctx = ctx_for(ActorId::new(), Some(profile));

    let workflow = service
        .create_workflow(new_workflow_input(profile), &ctx)
        .await
        .expect("create workflow");
    service
        .archive_workflow(workflow.workflow_id, &ctx)
        .await
        .expect("archive");
    let again = service
        .archive_workflow(workflow.workflow_id, &ctx)
        .await
        .expect("archive again");
    assert_eq!(again.status, WorkflowStatus::Archived);
    // Terminal: publish is rejected.
    assert!(service
        .publish_workflow(workflow.workflow_id, &ctx)
        .await
        .is_err());

    drop_db(&admin_url, &db_name, pool).await;
}
