// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Jev agents vertical slice (task 09-JEV-02).
//!
//! [`PgAgentRunRepository`], [`PgToolCallRepository`] and
//! [`PgHandoffRepository`] implement the
//! [`crm_domain::repositories`] agent traits over the `agent_runs`,
//! `tool_calls` and `handoffs` tables (migration 0008).
//! [`PgRoleMembership`] implements
//! [`crm_domain::services::RoleMembership`] over the `grants` table.
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AgentRunId, ClientProfileId, HandoffId, StaffId, ToolCallId};
use crm_domain::jev::{AgentRun, AgentRunStatus, FallbackChain, Handoff, ToolCall, ToolCallStatus};
use crm_domain::repositories::{AgentRunRepository, HandoffRepository, ToolCallRepository};
use crm_domain::services::RoleMembership;

use crate::postgres::map_storage_error;

/// Renders an [`AgentRunStatus`] as its stable database string.
fn run_status_as_str(status: AgentRunStatus) -> &'static str {
    match status {
        AgentRunStatus::Pending => "pending",
        AgentRunStatus::Running => "running",
        AgentRunStatus::AwaitingApproval => "awaiting_approval",
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        AgentRunStatus::Cancelled => "cancelled",
    }
}

/// Parses an [`AgentRunStatus`] from its database string.
fn run_status_from_str(raw: &str) -> Result<AgentRunStatus, DomainError> {
    match raw {
        "pending" => Ok(AgentRunStatus::Pending),
        "running" => Ok(AgentRunStatus::Running),
        "awaiting_approval" => Ok(AgentRunStatus::AwaitingApproval),
        "completed" => Ok(AgentRunStatus::Completed),
        "failed" => Ok(AgentRunStatus::Failed),
        "cancelled" => Ok(AgentRunStatus::Cancelled),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown agent run status '{other}'"
        ))),
    }
}

/// Renders a [`ToolCallStatus`] as its stable database string.
fn call_status_as_str(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::Approved => "approved",
        ToolCallStatus::Denied => "denied",
        ToolCallStatus::Executed => "executed",
        ToolCallStatus::Failed => "failed",
    }
}

/// Parses a [`ToolCallStatus`] from its database string.
fn call_status_from_str(raw: &str) -> Result<ToolCallStatus, DomainError> {
    match raw {
        "pending" => Ok(ToolCallStatus::Pending),
        "approved" => Ok(ToolCallStatus::Approved),
        "denied" => Ok(ToolCallStatus::Denied),
        "executed" => Ok(ToolCallStatus::Executed),
        "failed" => Ok(ToolCallStatus::Failed),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown tool call status '{other}'"
        ))),
    }
}

/// Columns selected for an [`AgentRun`] row.
const RUN_COLUMNS: &str = "run_id, client_profile_id, agent_kind, status, adapter_name, \
     fallback_chain, input, output, created_at, updated_at";

/// Columns selected for a [`ToolCall`] row.
const CALL_COLUMNS: &str = "call_id, run_id, tool_name, input, status, output, \
     requires_approval, created_at, updated_at";

/// Columns selected for a [`Handoff`] row.
const HANDOFF_COLUMNS: &str = "handoff_id, from_run_id, to_agent_kind, context, created_at";

/// Hydrates an `agent_runs` row into an [`AgentRun`].
fn row_to_run(row: &sqlx::postgres::PgRow) -> Result<AgentRun, DomainError> {
    let fallback_chain_value: serde_json::Value =
        row.try_get("fallback_chain").map_err(map_storage_error)?;
    let fallback_chain: FallbackChain =
        serde_json::from_value(fallback_chain_value).map_err(|error| {
            DomainError::Validation(format!("storage error: invalid fallback_chain: {error}"))
        })?;
    let output: Option<serde_json::Value> = row.try_get("output").map_err(map_storage_error)?;
    Ok(AgentRun {
        run_id: AgentRunId::from(
            row.try_get::<Uuid, _>("run_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        agent_kind: row.try_get("agent_kind").map_err(map_storage_error)?,
        status: run_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        adapter_name: row.try_get("adapter_name").map_err(map_storage_error)?,
        fallback_chain,
        input: row.try_get("input").map_err(map_storage_error)?,
        output,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates a `tool_calls` row into a [`ToolCall`].
fn row_to_call(row: &sqlx::postgres::PgRow) -> Result<ToolCall, DomainError> {
    let output: Option<serde_json::Value> = row.try_get("output").map_err(map_storage_error)?;
    Ok(ToolCall {
        call_id: ToolCallId::from(
            row.try_get::<Uuid, _>("call_id")
                .map_err(map_storage_error)?,
        ),
        run_id: AgentRunId::from(
            row.try_get::<Uuid, _>("run_id")
                .map_err(map_storage_error)?,
        ),
        tool_name: row.try_get("tool_name").map_err(map_storage_error)?,
        input: row.try_get("input").map_err(map_storage_error)?,
        status: call_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        requires_approval: row
            .try_get("requires_approval")
            .map_err(map_storage_error)?,
        output,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates a `handoffs` row into a [`Handoff`].
fn row_to_handoff(row: &sqlx::postgres::PgRow) -> Result<Handoff, DomainError> {
    Ok(Handoff {
        handoff_id: HandoffId::from(
            row.try_get::<Uuid, _>("handoff_id")
                .map_err(map_storage_error)?,
        ),
        from_run_id: AgentRunId::from(
            row.try_get::<Uuid, _>("from_run_id")
                .map_err(map_storage_error)?,
        ),
        to_agent_kind: row.try_get("to_agent_kind").map_err(map_storage_error)?,
        context: row.try_get("context").map_err(map_storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// SQLx implementation of [`AgentRunRepository`].
#[derive(Debug, Clone)]
pub struct PgAgentRunRepository {
    pool: PgPool,
}

impl PgAgentRunRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl AgentRunRepository for PgAgentRunRepository {
    async fn create(&self, run: AgentRun) -> Result<AgentRun, DomainError> {
        crate::postgres::ensure_client_profile(&self.pool, run.client_profile_id).await?;
        let fallback_chain = serde_json::to_value(&run.fallback_chain).map_err(|error| {
            DomainError::Validation(format!("cannot serialize fallback chain: {error}"))
        })?;
        sqlx::query(
            "INSERT INTO agent_runs (run_id, client_profile_id, agent_kind, status, adapter_name, \
             fallback_chain, input, output, created_at, updated_at)\n             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(run.run_id.as_uuid())
        .bind(run.client_profile_id.as_uuid())
        .bind(&run.agent_kind)
        .bind(run_status_as_str(run.status))
        .bind(&run.adapter_name)
        .bind(fallback_chain)
        .bind(&run.input)
        .bind(&run.output)
        .bind(run.created_at)
        .bind(run.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(run)
    }

    async fn get(&self, run_id: AgentRunId) -> Result<AgentRun, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM agent_runs WHERE run_id = $1"
        ))
        .bind(run_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_run(&row),
            None => Err(DomainError::NotFound {
                kind: "agent_run".to_owned(),
                id: run_id.to_string(),
            }),
        }
    }

    async fn update(&self, run: AgentRun) -> Result<AgentRun, DomainError> {
        let fallback_chain = serde_json::to_value(&run.fallback_chain).map_err(|error| {
            DomainError::Validation(format!("cannot serialize fallback chain: {error}"))
        })?;
        let result = sqlx::query(
            "UPDATE agent_runs\n             SET client_profile_id = $2, agent_kind = $3, status = $4, \
             adapter_name = $5, fallback_chain = $6, input = $7, output = $8, updated_at = $9\n             WHERE run_id = $1",
        )
        .bind(run.run_id.as_uuid())
        .bind(run.client_profile_id.as_uuid())
        .bind(&run.agent_kind)
        .bind(run_status_as_str(run.status))
        .bind(&run.adapter_name)
        .bind(fallback_chain)
        .bind(&run.input)
        .bind(&run.output)
        .bind(run.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "agent_run".to_owned(),
                id: run.run_id.to_string(),
            });
        }
        Ok(run)
    }

    async fn list_by_profile(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<AgentRun>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM agent_runs\n             WHERE client_profile_id = $1\n             ORDER BY created_at DESC, run_id DESC\n             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_run).collect()
    }

    async fn list_by_status(
        &self,
        client_profile_id: ClientProfileId,
        status: AgentRunStatus,
        limit: u64,
    ) -> Result<Vec<AgentRun>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {RUN_COLUMNS} FROM agent_runs\n             WHERE client_profile_id = $1 AND status = $2\n             ORDER BY created_at DESC, run_id DESC\n             LIMIT $3"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(run_status_as_str(status))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_run).collect()
    }
}

/// SQLx implementation of [`ToolCallRepository`].
#[derive(Debug, Clone)]
pub struct PgToolCallRepository {
    pool: PgPool,
}

impl PgToolCallRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl ToolCallRepository for PgToolCallRepository {
    async fn create(&self, call: ToolCall) -> Result<ToolCall, DomainError> {
        sqlx::query(
            "INSERT INTO tool_calls (call_id, run_id, tool_name, input, status, output, \
             requires_approval, created_at, updated_at)\n             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(call.call_id.as_uuid())
        .bind(call.run_id.as_uuid())
        .bind(&call.tool_name)
        .bind(&call.input)
        .bind(call_status_as_str(call.status))
        .bind(&call.output)
        .bind(call.requires_approval)
        .bind(call.created_at)
        .bind(call.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(call)
    }

    async fn get(&self, call_id: ToolCallId) -> Result<ToolCall, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {CALL_COLUMNS} FROM tool_calls WHERE call_id = $1"
        ))
        .bind(call_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_call(&row),
            None => Err(DomainError::NotFound {
                kind: "tool_call".to_owned(),
                id: call_id.to_string(),
            }),
        }
    }

    async fn update(&self, call: ToolCall) -> Result<ToolCall, DomainError> {
        let result = sqlx::query(
            "UPDATE tool_calls\n             SET tool_name = $2, input = $3, status = $4, output = $5, \
             requires_approval = $6, updated_at = $7\n             WHERE call_id = $1",
        )
        .bind(call.call_id.as_uuid())
        .bind(&call.tool_name)
        .bind(&call.input)
        .bind(call_status_as_str(call.status))
        .bind(&call.output)
        .bind(call.requires_approval)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "tool_call".to_owned(),
                id: call.call_id.to_string(),
            });
        }
        Ok(call)
    }

    async fn list_by_run(
        &self,
        run_id: AgentRunId,
        limit: u64,
    ) -> Result<Vec<ToolCall>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {CALL_COLUMNS} FROM tool_calls\n             WHERE run_id = $1\n             ORDER BY created_at ASC, call_id ASC\n             LIMIT $2"
        ))
        .bind(run_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_call).collect()
    }
}

/// SQLx implementation of [`HandoffRepository`].
#[derive(Debug, Clone)]
pub struct PgHandoffRepository {
    pool: PgPool,
}

impl PgHandoffRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl HandoffRepository for PgHandoffRepository {
    async fn create(&self, handoff: Handoff) -> Result<Handoff, DomainError> {
        sqlx::query(
            "INSERT INTO handoffs (handoff_id, from_run_id, to_agent_kind, context, created_at)\n             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(handoff.handoff_id.as_uuid())
        .bind(handoff.from_run_id.as_uuid())
        .bind(&handoff.to_agent_kind)
        .bind(&handoff.context)
        .bind(handoff.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(handoff)
    }

    async fn get(&self, handoff_id: HandoffId) -> Result<Handoff, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {HANDOFF_COLUMNS} FROM handoffs WHERE handoff_id = $1"
        ))
        .bind(handoff_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_handoff(&row),
            None => Err(DomainError::NotFound {
                kind: "handoff".to_owned(),
                id: handoff_id.to_string(),
            }),
        }
    }

    async fn list_by_run(
        &self,
        from_run_id: AgentRunId,
        limit: u64,
    ) -> Result<Vec<Handoff>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {HANDOFF_COLUMNS} FROM handoffs\n             WHERE from_run_id = $1\n             ORDER BY created_at DESC, handoff_id DESC\n             LIMIT $2"
        ))
        .bind(from_run_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_handoff).collect()
    }
}

/// SQLx [`RoleMembership`] backed by the `grants` table.
///
/// [`crm_domain::jev::APPROVER_ROLE`] is resolved to the
/// `agent_run`/`approve` (or `manage`) grant covering the profile, as
/// described on the constant. Unknown roles report no membership. A
/// database failure fails the check — the agent service treats it as a
/// denial (fail closed).
#[derive(Debug, Clone)]
pub struct PgRoleMembership {
    pool: PgPool,
}

impl PgRoleMembership {
    /// Creates the checker backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl RoleMembership for PgRoleMembership {
    async fn has_role(
        &self,
        actor: ActorId,
        role: &str,
        client_profile_id: ClientProfileId,
    ) -> Result<bool, DomainError> {
        if role != crm_domain::jev::APPROVER_ROLE {
            return Ok(false);
        }
        // Actor ids for staff sessions are the staff id; anything else
        // (agent principals, system ids) matches no grants and reports
        // no membership — fail closed.
        let staff_id = StaffId::from(actor.as_uuid());
        let holds: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                 FROM grants g
                 JOIN client_profiles cp ON cp.client_profile_id = $2
                 WHERE g.staff_id = $1
                   AND g.revoked_at IS NULL
                   AND g.resource = 'agent_run'
                   AND g.operation IN ('approve', 'manage')
                   AND (
                       (g.scope_kind = 'agency' AND g.agency_id = cp.agency_id)
                       OR (g.scope_kind = 'client_profile' AND g.client_profile_id = $2)
                   )
             )",
        )
        .bind(staff_id.as_uuid())
        .bind(client_profile_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(holds)
    }
}
