// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Workflow builder vertical slice (task 10-WORK-02).
//!
//! [`PgWorkflowRepository`], [`PgWorkflowNodeRepository`],
//! [`PgWorkflowEdgeRepository`], [`PgTriggerRepository`] and
//! [`PgWorkflowVersionRepository`] implement the
//! [`crm_domain::repositories`] workflow traits over the `workflows`,
//! `workflow_nodes`, `workflow_edges`, `workflow_triggers` and
//! `workflow_versions` tables (migration 0009).
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::error::DomainError;
use crm_domain::ids::{
    ClientProfileId, TriggerId, WorkflowEdgeId, WorkflowId, WorkflowNodeId, WorkflowVersionId,
};
use crm_domain::repositories::{
    TriggerRepository, WorkflowEdgeRepository, WorkflowNodeRepository, WorkflowRepository,
    WorkflowVersionRepository,
};
use crm_domain::workflow::{
    Trigger, TriggerType, Workflow, WorkflowEdge, WorkflowNode, WorkflowNodeType, WorkflowStatus,
    WorkflowVersion,
};

use crate::postgres::{ensure_client_profile, map_storage_error};

/// Renders a [`WorkflowStatus`] as its stable database string.
fn workflow_status_as_str(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Draft => "draft",
        WorkflowStatus::Published => "published",
        WorkflowStatus::Archived => "archived",
    }
}

/// Parses a [`WorkflowStatus`] from its database string.
fn workflow_status_from_str(raw: &str) -> Result<WorkflowStatus, DomainError> {
    match raw {
        "draft" => Ok(WorkflowStatus::Draft),
        "published" => Ok(WorkflowStatus::Published),
        "archived" => Ok(WorkflowStatus::Archived),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown workflow status '{other}'"
        ))),
    }
}

/// Renders a [`WorkflowNodeType`] as its stable database string.
fn node_type_as_str(node_type: WorkflowNodeType) -> &'static str {
    match node_type {
        WorkflowNodeType::TriggerNode => "trigger",
        WorkflowNodeType::ActionNode => "action",
        WorkflowNodeType::ConditionNode => "condition",
        WorkflowNodeType::DelayNode => "delay",
    }
}

/// Parses a [`WorkflowNodeType`] from its database string.
fn node_type_from_str(raw: &str) -> Result<WorkflowNodeType, DomainError> {
    match raw {
        "trigger" => Ok(WorkflowNodeType::TriggerNode),
        "action" => Ok(WorkflowNodeType::ActionNode),
        "condition" => Ok(WorkflowNodeType::ConditionNode),
        "delay" => Ok(WorkflowNodeType::DelayNode),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown workflow node type '{other}'"
        ))),
    }
}

/// Renders a [`TriggerType`] as its stable database string.
fn trigger_type_as_str(trigger_type: TriggerType) -> &'static str {
    match trigger_type {
        TriggerType::EventTrigger => "event",
        TriggerType::WebhookTrigger => "webhook",
        TriggerType::ScheduleTrigger => "schedule",
        TriggerType::ManualTrigger => "manual",
    }
}

/// Parses a [`TriggerType`] from its database string.
fn trigger_type_from_str(raw: &str) -> Result<TriggerType, DomainError> {
    match raw {
        "event" => Ok(TriggerType::EventTrigger),
        "webhook" => Ok(TriggerType::WebhookTrigger),
        "schedule" => Ok(TriggerType::ScheduleTrigger),
        "manual" => Ok(TriggerType::ManualTrigger),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown trigger type '{other}'"
        ))),
    }
}

/// Columns selected for a [`Workflow`] row.
const WORKFLOW_COLUMNS: &str =
    "workflow_id, client_profile_id, name, status, created_at, updated_at";

/// Columns selected for a [`WorkflowNode`] row.
const NODE_COLUMNS: &str = "node_id, workflow_id, node_type, config, pos_x, pos_y";

/// Columns selected for a [`WorkflowEdge`] row.
const EDGE_COLUMNS: &str = "edge_id, workflow_id, from_node, to_node, label";

/// Columns selected for a [`Trigger`] row.
const TRIGGER_COLUMNS: &str = "trigger_id, workflow_id, trigger_type, config";

/// Columns selected for a [`WorkflowVersion`] row.
const VERSION_COLUMNS: &str = "version_id, workflow_id, version_number, snapshot, created_at";

/// Hydrates a `workflows` row into a [`Workflow`].
fn row_to_workflow(row: &sqlx::postgres::PgRow) -> Result<Workflow, DomainError> {
    Ok(Workflow {
        workflow_id: WorkflowId::from(
            row.try_get::<Uuid, _>("workflow_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        name: row.try_get("name").map_err(map_storage_error)?,
        status: workflow_status_from_str(
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

/// Hydrates a `workflow_nodes` row into a [`WorkflowNode`].
fn row_to_node(row: &sqlx::postgres::PgRow) -> Result<WorkflowNode, DomainError> {
    Ok(WorkflowNode {
        node_id: WorkflowNodeId::from(
            row.try_get::<Uuid, _>("node_id")
                .map_err(map_storage_error)?,
        ),
        workflow_id: WorkflowId::from(
            row.try_get::<Uuid, _>("workflow_id")
                .map_err(map_storage_error)?,
        ),
        node_type: node_type_from_str(
            row.try_get::<String, _>("node_type")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        config: row.try_get("config").map_err(map_storage_error)?,
        position: (
            row.try_get::<i32, _>("pos_x").map_err(map_storage_error)?,
            row.try_get::<i32, _>("pos_y").map_err(map_storage_error)?,
        ),
    })
}

/// Hydrates a `workflow_edges` row into a [`WorkflowEdge`].
fn row_to_edge(row: &sqlx::postgres::PgRow) -> Result<WorkflowEdge, DomainError> {
    Ok(WorkflowEdge {
        edge_id: WorkflowEdgeId::from(
            row.try_get::<Uuid, _>("edge_id")
                .map_err(map_storage_error)?,
        ),
        workflow_id: WorkflowId::from(
            row.try_get::<Uuid, _>("workflow_id")
                .map_err(map_storage_error)?,
        ),
        from_node: WorkflowNodeId::from(
            row.try_get::<Uuid, _>("from_node")
                .map_err(map_storage_error)?,
        ),
        to_node: WorkflowNodeId::from(
            row.try_get::<Uuid, _>("to_node")
                .map_err(map_storage_error)?,
        ),
        label: row.try_get("label").map_err(map_storage_error)?,
    })
}

/// Hydrates a `workflow_triggers` row into a [`Trigger`].
fn row_to_trigger(row: &sqlx::postgres::PgRow) -> Result<Trigger, DomainError> {
    Ok(Trigger {
        trigger_id: TriggerId::from(
            row.try_get::<Uuid, _>("trigger_id")
                .map_err(map_storage_error)?,
        ),
        workflow_id: WorkflowId::from(
            row.try_get::<Uuid, _>("workflow_id")
                .map_err(map_storage_error)?,
        ),
        trigger_type: trigger_type_from_str(
            row.try_get::<String, _>("trigger_type")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        config: row.try_get("config").map_err(map_storage_error)?,
    })
}

/// Hydrates a `workflow_versions` row into a [`WorkflowVersion`].
fn row_to_version(row: &sqlx::postgres::PgRow) -> Result<WorkflowVersion, DomainError> {
    Ok(WorkflowVersion {
        version_id: WorkflowVersionId::from(
            row.try_get::<Uuid, _>("version_id")
                .map_err(map_storage_error)?,
        ),
        workflow_id: WorkflowId::from(
            row.try_get::<Uuid, _>("workflow_id")
                .map_err(map_storage_error)?,
        ),
        version_number: u32::try_from(
            row.try_get::<i32, _>("version_number")
                .map_err(map_storage_error)?,
        )
        .map_err(|_| {
            DomainError::Validation("storage error: negative version_number".to_owned())
        })?,
        snapshot: row.try_get("snapshot").map_err(map_storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// SQLx implementation of [`WorkflowRepository`].
#[derive(Debug, Clone)]
pub struct PgWorkflowRepository {
    pool: PgPool,
}

impl PgWorkflowRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl WorkflowRepository for PgWorkflowRepository {
    async fn create(&self, workflow: Workflow) -> Result<Workflow, DomainError> {
        ensure_client_profile(&self.pool, workflow.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO workflows (workflow_id, client_profile_id, name, status, created_at, \
             updated_at)\n             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(workflow.workflow_id.as_uuid())
        .bind(workflow.client_profile_id.as_uuid())
        .bind(&workflow.name)
        .bind(workflow_status_as_str(workflow.status))
        .bind(workflow.created_at)
        .bind(workflow.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(workflow)
    }

    async fn get(&self, workflow_id: WorkflowId) -> Result<Workflow, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {WORKFLOW_COLUMNS} FROM workflows WHERE workflow_id = $1"
        ))
        .bind(workflow_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_workflow(&row),
            None => Err(DomainError::NotFound {
                kind: "workflow".to_owned(),
                id: workflow_id.to_string(),
            }),
        }
    }

    async fn update(&self, workflow: Workflow) -> Result<Workflow, DomainError> {
        let result = sqlx::query(
            "UPDATE workflows\n             SET name = $2, status = $3, updated_at = $4\n             WHERE workflow_id = $1",
        )
        .bind(workflow.workflow_id.as_uuid())
        .bind(&workflow.name)
        .bind(workflow_status_as_str(workflow.status))
        .bind(workflow.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "workflow".to_owned(),
                id: workflow.workflow_id.to_string(),
            });
        }
        Ok(workflow)
    }

    async fn list_by_profile(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Workflow>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {WORKFLOW_COLUMNS} FROM workflows\n             WHERE client_profile_id = $1\n             ORDER BY created_at DESC, workflow_id DESC\n             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_workflow).collect()
    }
}

/// SQLx implementation of [`WorkflowNodeRepository`].
#[derive(Debug, Clone)]
pub struct PgWorkflowNodeRepository {
    pool: PgPool,
}

impl PgWorkflowNodeRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl WorkflowNodeRepository for PgWorkflowNodeRepository {
    async fn create(&self, node: WorkflowNode) -> Result<WorkflowNode, DomainError> {
        sqlx::query(
            "INSERT INTO workflow_nodes (node_id, workflow_id, node_type, config, pos_x, pos_y)\n             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(node.node_id.as_uuid())
        .bind(node.workflow_id.as_uuid())
        .bind(node_type_as_str(node.node_type))
        .bind(&node.config)
        .bind(node.position.0)
        .bind(node.position.1)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(node)
    }

    async fn get(&self, node_id: WorkflowNodeId) -> Result<WorkflowNode, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {NODE_COLUMNS} FROM workflow_nodes WHERE node_id = $1"
        ))
        .bind(node_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_node(&row),
            None => Err(DomainError::NotFound {
                kind: "workflow_node".to_owned(),
                id: node_id.to_string(),
            }),
        }
    }

    async fn update(&self, node: WorkflowNode) -> Result<WorkflowNode, DomainError> {
        let result = sqlx::query(
            "UPDATE workflow_nodes\n             SET config = $2, pos_x = $3, pos_y = $4\n             WHERE node_id = $1",
        )
        .bind(node.node_id.as_uuid())
        .bind(&node.config)
        .bind(node.position.0)
        .bind(node.position.1)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "workflow_node".to_owned(),
                id: node.node_id.to_string(),
            });
        }
        Ok(node)
    }

    async fn delete(&self, node_id: WorkflowNodeId) -> Result<(), DomainError> {
        let result = sqlx::query("DELETE FROM workflow_nodes WHERE node_id = $1")
            .bind(node_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "workflow_node".to_owned(),
                id: node_id.to_string(),
            });
        }
        Ok(())
    }

    async fn list_by_workflow(
        &self,
        workflow_id: WorkflowId,
        limit: u64,
    ) -> Result<Vec<WorkflowNode>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {NODE_COLUMNS} FROM workflow_nodes\n             WHERE workflow_id = $1\n             ORDER BY created_at ASC, node_id ASC\n             LIMIT $2"
        ))
        .bind(workflow_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_node).collect()
    }
}

/// SQLx implementation of [`WorkflowEdgeRepository`].
#[derive(Debug, Clone)]
pub struct PgWorkflowEdgeRepository {
    pool: PgPool,
}

impl PgWorkflowEdgeRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl WorkflowEdgeRepository for PgWorkflowEdgeRepository {
    async fn create(&self, edge: WorkflowEdge) -> Result<WorkflowEdge, DomainError> {
        sqlx::query(
            "INSERT INTO workflow_edges (edge_id, workflow_id, from_node, to_node, label)\n             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(edge.edge_id.as_uuid())
        .bind(edge.workflow_id.as_uuid())
        .bind(edge.from_node.as_uuid())
        .bind(edge.to_node.as_uuid())
        .bind(&edge.label)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(edge)
    }

    async fn get(&self, edge_id: WorkflowEdgeId) -> Result<WorkflowEdge, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {EDGE_COLUMNS} FROM workflow_edges WHERE edge_id = $1"
        ))
        .bind(edge_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_edge(&row),
            None => Err(DomainError::NotFound {
                kind: "workflow_edge".to_owned(),
                id: edge_id.to_string(),
            }),
        }
    }

    async fn delete(&self, edge_id: WorkflowEdgeId) -> Result<(), DomainError> {
        let result = sqlx::query("DELETE FROM workflow_edges WHERE edge_id = $1")
            .bind(edge_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "workflow_edge".to_owned(),
                id: edge_id.to_string(),
            });
        }
        Ok(())
    }

    async fn list_by_workflow(
        &self,
        workflow_id: WorkflowId,
        limit: u64,
    ) -> Result<Vec<WorkflowEdge>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {EDGE_COLUMNS} FROM workflow_edges\n             WHERE workflow_id = $1\n             ORDER BY created_at ASC, edge_id ASC\n             LIMIT $2"
        ))
        .bind(workflow_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_edge).collect()
    }
}

impl PgWorkflowEdgeRepository {
    /// Deletes every edge touching `node_id` in `workflow_id` (used by
    /// the service before node deletion so no dangling edges remain).
    pub async fn delete_by_node(
        &self,
        workflow_id: WorkflowId,
        node_id: WorkflowNodeId,
    ) -> Result<u64, DomainError> {
        let result = sqlx::query(
            "DELETE FROM workflow_edges\n             WHERE workflow_id = $1 AND (from_node = $2 OR to_node = $2)",
        )
        .bind(workflow_id.as_uuid())
        .bind(node_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(result.rows_affected())
    }
}

/// SQLx implementation of [`TriggerRepository`].
#[derive(Debug, Clone)]
pub struct PgTriggerRepository {
    pool: PgPool,
}

impl PgTriggerRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl TriggerRepository for PgTriggerRepository {
    async fn set(&self, trigger: Trigger) -> Result<Trigger, DomainError> {
        sqlx::query(
            "INSERT INTO workflow_triggers (trigger_id, workflow_id, trigger_type, config)\n             VALUES ($1, $2, $3, $4)\n             ON CONFLICT (workflow_id) DO UPDATE SET\n               trigger_id = EXCLUDED.trigger_id,\n               trigger_type = EXCLUDED.trigger_type,\n               config = EXCLUDED.config,\n               updated_at = now()",
        )
        .bind(trigger.trigger_id.as_uuid())
        .bind(trigger.workflow_id.as_uuid())
        .bind(trigger_type_as_str(trigger.trigger_type))
        .bind(&trigger.config)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(trigger)
    }

    async fn get_by_workflow(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<Option<Trigger>, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {TRIGGER_COLUMNS} FROM workflow_triggers WHERE workflow_id = $1"
        ))
        .bind(workflow_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(|row| row_to_trigger(&row)).transpose()
    }
}

/// SQLx implementation of [`WorkflowVersionRepository`].
///
/// Append-only: there is deliberately no update or delete method.
#[derive(Debug, Clone)]
pub struct PgWorkflowVersionRepository {
    pool: PgPool,
}

impl PgWorkflowVersionRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl WorkflowVersionRepository for PgWorkflowVersionRepository {
    async fn create(&self, version: WorkflowVersion) -> Result<WorkflowVersion, DomainError> {
        sqlx::query(
            "INSERT INTO workflow_versions (version_id, workflow_id, version_number, snapshot, \
             created_at)\n             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(version.version_id.as_uuid())
        .bind(version.workflow_id.as_uuid())
        .bind(i32::try_from(version.version_number).unwrap_or(i32::MAX))
        .bind(&version.snapshot)
        .bind(version.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(version)
    }

    async fn get(&self, version_id: WorkflowVersionId) -> Result<WorkflowVersion, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {VERSION_COLUMNS} FROM workflow_versions WHERE version_id = $1"
        ))
        .bind(version_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_version(&row),
            None => Err(DomainError::NotFound {
                kind: "workflow_version".to_owned(),
                id: version_id.to_string(),
            }),
        }
    }

    async fn list_by_workflow(
        &self,
        workflow_id: WorkflowId,
        limit: u64,
    ) -> Result<Vec<WorkflowVersion>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {VERSION_COLUMNS} FROM workflow_versions\n             WHERE workflow_id = $1\n             ORDER BY version_number DESC\n             LIMIT $2"
        ))
        .bind(workflow_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_version).collect()
    }
}
