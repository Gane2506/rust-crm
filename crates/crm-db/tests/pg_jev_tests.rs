// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Jev agents vertical slice (task 09-JEV-02).
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
//! repositories with a no-op audit service; the role-membership checks
//! run against real `grants` rows.

mod common;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::jev::AgentServiceImpl;
use crm_db::jev::{
    PgAgentRunRepository, PgHandoffRepository, PgRoleMembership, PgToolCallRepository,
};
use crm_db::postgres::{PgGrantRepository, PgStaffRepository};
use crm_db::run_migrations;
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{
    ActorId, AgencyId, AgentRunId, AuditEventId, ClientProfileId, GrantId, StaffId,
};
use crm_domain::jev::{AgentRun, AgentRunStatus, FallbackChain, ToolCallStatus};
use crm_domain::repositories::{
    AgentRunRepository, GrantRepository, HandoffRepository, StaffRepository, ToolCallRepository,
};
use crm_domain::security::{Action, Grant, Operation, Resource, Scope, StaffStatus, StaffUser};
use crm_domain::services::{
    ActionContext, AgentService, AuditService, NewAgentRun, NewHandoff, NewToolCall, RoleMembership,
};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (agent runs reference it).
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
) -> AgentServiceImpl<
    PgAgentRunRepository,
    PgToolCallRepository,
    PgHandoffRepository,
    PgRoleMembership,
    NoopAudit,
> {
    AgentServiceImpl::new(
        PgAgentRunRepository::new(pool.clone()),
        PgToolCallRepository::new(pool.clone()),
        PgHandoffRepository::new(pool.clone()),
        PgRoleMembership::new(pool.clone()),
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

/// Creates a staff user and returns it.
async fn make_staff(pool: &PgPool, email: &str) -> StaffUser {
    let repo = PgStaffRepository::new(pool.clone());
    let now = Utc::now().round_subsecs(6);
    repo.create(StaffUser {
        staff_id: StaffId::new(),
        agency_id: AgencyId::new(),
        email: email.to_owned(),
        display_name: "Test Approver".to_owned(),
        status: StaffStatus::Active,
        credential: None,
        created_at: now,
        updated_at: now,
    })
    .await
    .expect("create staff")
}

/// Assigns an `agent_run`/`operation` grant on `profile` to `staff`.
async fn grant_agent_run(
    pool: &PgPool,
    staff: StaffId,
    agency: AgencyId,
    profile: ClientProfileId,
    operation: Operation,
) {
    let repo = PgGrantRepository::new(pool.clone());
    repo.assign(
        staff,
        Grant {
            grant_id: GrantId::new(),
            action: Action {
                resource: Resource::AgentRun,
                operation,
            },
            scope: Scope::ClientProfile {
                agency_id: agency,
                client_profile_id: profile,
            },
        },
    )
    .await
    .expect("assign grant");
}

/// Links the profile to the staff member's agency so agency-scoped
/// grants resolve.
async fn link_profile_agency(pool: &PgPool, profile: ClientProfileId, agency: AgencyId) {
    sqlx::query("UPDATE client_profiles SET agency_id = $2 WHERE client_profile_id = $1")
        .bind(profile.as_uuid())
        .bind(agency.as_uuid())
        .execute(pool)
        .await
        .expect("link profile agency");
}

fn test_chain() -> FallbackChain {
    FallbackChain::new(vec!["stub".to_string()]).expect("valid chain")
}

fn new_run_input(profile: ClientProfileId) -> NewAgentRun {
    NewAgentRun {
        client_profile_id: profile,
        agent_kind: "triage".to_string(),
        fallback_chain: test_chain(),
        input: serde_json::json!({"task": "triage inbox"}),
    }
}

#[tokio::test]
async fn agent_run_repository_crud() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgAgentRunRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let run = AgentRun::new(
        profile,
        "triage".to_string(),
        test_chain(),
        serde_json::json!({"task": "triage"}),
    )
    .expect("valid run");
    let run_id = run.run_id;
    let stored = repo.create(run).await.expect("create run");
    assert_eq!(stored.run_id, run_id);
    assert_eq!(stored.status, AgentRunStatus::Pending);
    // The chain round-trips through JSONB.
    assert_eq!(stored.fallback_chain, test_chain());
    assert_eq!(stored.adapter_name, "stub");

    let fetched = repo.get(run_id).await.expect("get run");
    assert_eq!(fetched.agent_kind, "triage");

    let mut updated = fetched;
    updated.start().expect("start");
    let updated = repo.update(updated).await.expect("update run");
    assert_eq!(updated.status, AgentRunStatus::Running);

    let listed = repo
        .list_by_profile(profile, 10)
        .await
        .expect("list by profile");
    assert_eq!(listed.len(), 1);

    let running = repo
        .list_by_status(profile, AgentRunStatus::Running, 10)
        .await
        .expect("list by status");
    assert_eq!(running.len(), 1);
    let pending = repo
        .list_by_status(profile, AgentRunStatus::Pending, 10)
        .await
        .expect("list by status");
    assert!(pending.is_empty());

    // Unknown run is NotFound.
    let missing = repo.get(AgentRunId::new()).await.expect_err("missing run");
    assert!(matches!(missing, DomainError::NotFound { .. }));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn tool_call_and_handoff_repositories() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let runs = PgAgentRunRepository::new(pool.clone());
    let calls = PgToolCallRepository::new(pool.clone());
    let handoffs = PgHandoffRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let run = runs
        .create(
            AgentRun::new(
                profile,
                "triage".to_string(),
                test_chain(),
                serde_json::json!({}),
            )
            .expect("valid run"),
        )
        .await
        .expect("create run");

    // Tool calls.
    let service = test_service(&pool);
    let ctx = ctx_for(ActorId::new(), Some(profile));
    let call = service
        .record_tool_call(
            NewToolCall {
                run_id: run.run_id,
                tool_name: "crm.create_contact".to_string(),
                input: serde_json::json!({"email": "a@example.com"}),
                requires_approval: false,
            },
            &ctx,
        )
        .await
        .expect("record tool call");
    // No approval required: the service approves immediately.
    assert_eq!(call.status, ToolCallStatus::Approved);
    assert!(!call.requires_approval);

    let fetched = calls.get(call.call_id).await.expect("get call");
    assert_eq!(fetched.tool_name, "crm.create_contact");
    let listed = calls.list_by_run(run.run_id, 10).await.expect("list calls");
    assert_eq!(listed.len(), 1);

    // Handoffs (append-only).
    let handoff = service
        .handoff(
            NewHandoff {
                from_run_id: run.run_id,
                to_agent_kind: "drafter".to_string(),
                context: serde_json::json!({"summary": "done"}),
            },
            &ctx,
        )
        .await
        .expect("handoff");
    let fetched = handoffs.get(handoff.handoff_id).await.expect("get handoff");
    assert_eq!(fetched.to_agent_kind, "drafter");
    let listed = handoffs
        .list_by_run(run.run_id, 10)
        .await
        .expect("list handoffs");
    assert_eq!(listed.len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn agent_service_full_lifecycle_with_approval() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    // Approver: staff with the agent_run/approve grant on the profile.
    let staff = make_staff(&pool, "approver@example.com").await;
    link_profile_agency(&pool, profile, staff.agency_id).await;
    grant_agent_run(
        &pool,
        staff.staff_id,
        staff.agency_id,
        profile,
        Operation::Approve,
    )
    .await;
    // The run also needs create/update grants for the lifecycle; the
    // service checks scope, the HTTP layer checks grants — here we
    // exercise the service directly with a scoped context.
    let ctx = ctx_for(ActorId::from(staff.staff_id), Some(profile));

    let service = test_service(&pool);
    let run = service
        .start_run(new_run_input(profile), &ctx)
        .await
        .expect("start run");
    assert_eq!(run.status, AgentRunStatus::Running);

    let run = service
        .request_approval(run.run_id, &ctx)
        .await
        .expect("request approval");
    assert_eq!(run.status, AgentRunStatus::AwaitingApproval);

    // The approver holds the agent_approver role via the approve grant.
    let roles = PgRoleMembership::new(pool.clone());
    assert!(
        roles
            .has_role(
                ActorId::from(staff.staff_id),
                crm_domain::jev::APPROVER_ROLE,
                profile
            )
            .await
            .expect("role check runs"),
        "grant-backed role membership"
    );

    let run = service
        .approve_step(run.run_id, &ctx)
        .await
        .expect("approve");
    assert_eq!(run.status, AgentRunStatus::Running);

    let run = service
        .complete_run(run.run_id, serde_json::json!({"done": true}), &ctx)
        .await
        .expect("complete");
    assert_eq!(run.status, AgentRunStatus::Completed);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn agent_service_approval_gate_persists_decision() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let staff = make_staff(&pool, "gatekeeper@example.com").await;
    link_profile_agency(&pool, profile, staff.agency_id).await;
    grant_agent_run(
        &pool,
        staff.staff_id,
        staff.agency_id,
        profile,
        Operation::Approve,
    )
    .await;
    let actor = ActorId::from(staff.staff_id);
    let ctx = ctx_for(actor, Some(profile));
    let service = test_service(&pool);
    let runs = PgAgentRunRepository::new(pool.clone());

    // Approve path: the decided gate (decision, decider, time) must
    // survive a round-trip through the database.
    let run = service
        .start_run(new_run_input(profile), &ctx)
        .await
        .expect("start run");
    let run = service
        .request_approval(run.run_id, &ctx)
        .await
        .expect("request approval");
    assert!(run.approval_gate.is_none(), "no gate before decision");

    let before = Utc::now().round_subsecs(6);
    let run = service
        .approve_step(run.run_id, &ctx)
        .await
        .expect("approve");
    let gate = run
        .approval_gate
        .as_ref()
        .expect("gate persisted on approve");
    assert_eq!(
        gate.decision,
        Some(crm_domain::jev::ApprovalDecision::Approved)
    );
    assert_eq!(gate.decided_by, Some(actor));
    let decided_at = gate.decided_at.expect("decided_at set");
    assert!(decided_at >= before, "decided_at is the decision time");

    // Read it back from the database: the gate must round-trip.
    let fetched = runs.get(run.run_id).await.expect("get run");
    let gate = fetched
        .approval_gate
        .as_ref()
        .expect("gate survives reload");
    assert_eq!(
        gate.decision,
        Some(crm_domain::jev::ApprovalDecision::Approved)
    );
    assert_eq!(gate.decided_by, Some(actor));
    assert_eq!(gate.decided_at, Some(decided_at));

    // Deny path: same persistence guarantees.
    let run = service
        .start_run(new_run_input(profile), &ctx)
        .await
        .expect("start run");
    let run = service
        .request_approval(run.run_id, &ctx)
        .await
        .expect("request approval");
    let run = service.deny_step(run.run_id, &ctx).await.expect("deny");
    let gate = run.approval_gate.as_ref().expect("gate persisted on deny");
    assert_eq!(
        gate.decision,
        Some(crm_domain::jev::ApprovalDecision::Denied)
    );
    assert_eq!(gate.decided_by, Some(actor));
    assert!(gate.decided_at.is_some());
    assert!(gate.reason.is_some(), "deny carries a reason");

    let fetched = runs.get(run.run_id).await.expect("get run");
    assert_eq!(
        fetched
            .approval_gate
            .as_ref()
            .expect("deny gate survives reload")
            .decision,
        Some(crm_domain::jev::ApprovalDecision::Denied)
    );

    // Re-requesting approval clears the previous decision: a run
    // awaiting a fresh decision must not expose the old gate.
    let run = service
        .start_run(new_run_input(profile), &ctx)
        .await
        .expect("start run");
    let run = service
        .request_approval(run.run_id, &ctx)
        .await
        .expect("request approval");
    let run = service
        .approve_step(run.run_id, &ctx)
        .await
        .expect("approve");
    assert!(run.approval_gate.is_some(), "gate set after approve");
    let run = service
        .request_approval(run.run_id, &ctx)
        .await
        .expect("re-request approval");
    assert!(
        run.approval_gate.is_none(),
        "re-request clears the previous decision"
    );
    let fetched = runs.get(run.run_id).await.expect("get run");
    assert!(
        fetched.approval_gate.is_none(),
        "cleared gate persists as cleared"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn agent_service_wrong_actor_approval_rejected() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    // One staff member CAN approve; another CANNOT.
    let approver = make_staff(&pool, "approver2@example.com").await;
    link_profile_agency(&pool, profile, approver.agency_id).await;
    grant_agent_run(
        &pool,
        approver.staff_id,
        approver.agency_id,
        profile,
        Operation::Approve,
    )
    .await;
    let outsider = make_staff(&pool, "outsider@example.com").await;

    let service = test_service(&pool);
    let ctx = ctx_for(ActorId::from(approver.staff_id), Some(profile));
    let run = service
        .start_run(new_run_input(profile), &ctx)
        .await
        .expect("start run");
    let run = service
        .request_approval(run.run_id, &ctx)
        .await
        .expect("request approval");

    // The outsider holds no approve grant: the role check fails closed.
    let outsider_ctx = ctx_for(ActorId::from(outsider.staff_id), Some(profile));
    let denied = service
        .approve_step(run.run_id, &outsider_ctx)
        .await
        .expect_err("outsider approval must fail");
    assert!(
        matches!(denied, DomainError::Unauthorized),
        "expected Unauthorized, got {denied:?}"
    );
    // The run is still awaiting approval.
    let runs = PgAgentRunRepository::new(pool.clone());
    let fetched = runs.get(run.run_id).await.expect("get run");
    assert_eq!(fetched.status, AgentRunStatus::AwaitingApproval);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn agent_service_scope_isolation() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;

    let service = test_service(&pool);
    let ctx_a = ctx_for(ActorId::new(), Some(profile_a));
    let run = service
        .start_run(new_run_input(profile_a), &ctx_a)
        .await
        .expect("start run");

    // A context scoped to another profile cannot touch the run.
    let ctx_b = ctx_for(ActorId::new(), Some(profile_b));
    let forbidden = service
        .cancel_run(run.run_id, &ctx_b)
        .await
        .expect_err("cross-profile must fail");
    assert!(
        matches!(forbidden, DomainError::Forbidden(_)),
        "expected Forbidden, got {forbidden:?}"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn agent_service_approval_gated_tool_call_pauses_run() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let staff = make_staff(&pool, "approver3@example.com").await;
    link_profile_agency(&pool, profile, staff.agency_id).await;
    grant_agent_run(
        &pool,
        staff.staff_id,
        staff.agency_id,
        profile,
        Operation::Approve,
    )
    .await;
    let ctx = ctx_for(ActorId::from(staff.staff_id), Some(profile));

    let service = test_service(&pool);
    let run = service
        .start_run(new_run_input(profile), &ctx)
        .await
        .expect("start run");

    // An approval-gated tool call pauses the run.
    let call = service
        .record_tool_call(
            NewToolCall {
                run_id: run.run_id,
                tool_name: "crm.delete_contact".to_string(),
                input: serde_json::json!({"id": "x"}),
                requires_approval: true,
            },
            &ctx,
        )
        .await
        .expect("record gated call");
    assert_eq!(call.status, ToolCallStatus::Pending);
    assert!(call.requires_approval);

    let runs = PgAgentRunRepository::new(pool.clone());
    let paused = runs.get(run.run_id).await.expect("get run");
    assert_eq!(paused.status, AgentRunStatus::AwaitingApproval);

    // Approving the run approves its pending gated calls.
    let run = service
        .approve_step(run.run_id, &ctx)
        .await
        .expect("approve");
    assert_eq!(run.status, AgentRunStatus::Running);
    let calls = PgToolCallRepository::new(pool.clone());
    let approved = calls.get(call.call_id).await.expect("get call");
    assert_eq!(approved.status, ToolCallStatus::Approved);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn agent_service_deny_fails_run() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let staff = make_staff(&pool, "approver4@example.com").await;
    link_profile_agency(&pool, profile, staff.agency_id).await;
    grant_agent_run(
        &pool,
        staff.staff_id,
        staff.agency_id,
        profile,
        Operation::Approve,
    )
    .await;
    let ctx = ctx_for(ActorId::from(staff.staff_id), Some(profile));

    let service = test_service(&pool);
    let run = service
        .start_run(new_run_input(profile), &ctx)
        .await
        .expect("start run");
    let run = service
        .request_approval(run.run_id, &ctx)
        .await
        .expect("request approval");
    let run = service.deny_step(run.run_id, &ctx).await.expect("deny");
    assert_eq!(run.status, AgentRunStatus::Failed);
    assert!(run.status.is_terminal());

    drop_db(&admin_url, &db_name, pool).await;
}
