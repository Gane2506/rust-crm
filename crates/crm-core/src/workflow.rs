// SPDX-License-Identifier: GPL-3.0-only
//! Workflow builder service implementation (task 10-WORK-02).
//!
//! [`WorkflowServiceImpl`] implements
//! [`WorkflowService`](crm_domain::services::WorkflowService) over
//! generic workflow, node, edge, trigger, version and audit ports —
//! the same composition pattern as
//! [`AgentServiceImpl`](crate::jev::AgentServiceImpl). The production
//! wiring (SQLx repositories) is assembled by the API layer.
//!
//! Key behaviors:
//! * [`WorkflowServiceImpl::publish_workflow`] freezes an immutable
//!   [`WorkflowVersion`](crm_domain::workflow::WorkflowVersion)
//!   snapshot (nodes, edges, trigger) with `version_number = max + 1`;
//! * [`WorkflowServiceImpl::add_edge`] validates both endpoints exist
//!   in the workflow (the domain rejects self-loops);
//! * [`WorkflowServiceImpl::remove_node`] deletes dangling edges first;
//! * [`WorkflowServiceImpl::set_trigger`] replaces the workflow's
//!   single trigger;
//! * [`WorkflowServiceImpl::simulate`] runs the pure
//!   [`Simulator`](crm_domain::workflow::Simulator) — never executes
//!   actions, never mutates state.

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, WorkflowEdgeId, WorkflowId, WorkflowNodeId};
use crm_domain::repositories::{
    TriggerRepository, WorkflowEdgeRepository, WorkflowNodeRepository, WorkflowRepository,
    WorkflowVersionRepository,
};
use crm_domain::services::{
    ActionContext, AuditService, NewTrigger, NewWorkflow, NewWorkflowEdge, NewWorkflowNode,
    WorkflowService,
};
use crm_domain::workflow::{Simulator, Trigger, Workflow, WorkflowVersion};

/// Generic [`WorkflowService`] implementation.
///
/// Type parameters: `W` workflow repository, `N` node repository, `E`
/// edge repository, `T` trigger repository, `V` version repository,
/// `A` audit service.
pub struct WorkflowServiceImpl<W, N, E, T, V, A> {
    workflows: W,
    nodes: N,
    edges: E,
    triggers: T,
    versions: V,
    audit: A,
}

impl<W, N, E, T, V, A> WorkflowServiceImpl<W, N, E, T, V, A> {
    /// Creates the service over the given ports.
    pub fn new(workflows: W, nodes: N, edges: E, triggers: T, versions: V, audit: A) -> Self {
        Self {
            workflows,
            nodes,
            edges,
            triggers,
            versions,
            audit,
        }
    }

    /// Enforces profile scope: a profile-scoped context may only touch
    /// workflows of its own profile; an agency-scoped context (`None`)
    /// may touch any profile's workflows (grant checks happen upstream,
    /// at the HTTP layer).
    fn check_scope(
        ctx: &ActionContext,
        client_profile_id: ClientProfileId,
    ) -> Result<(), DomainError> {
        match ctx.client_profile_id {
            Some(ctx_profile) if ctx_profile != client_profile_id => Err(DomainError::Forbidden(
                "operation crosses client-profile scope".to_owned(),
            )),
            _ => Ok(()),
        }
    }

    /// Records an audit event for a workflow mutation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails the
    /// operation).
    async fn audit_workflow(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_id: &str,
        result: ActionResult,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                "workflow",
                target_id,
                PermissionDecision::Allow,
                result,
                None,
                None,
            )
            .await;
    }

    /// Loads a workflow and enforces the caller's profile scope on it.
    async fn load_scoped_workflow(
        &self,
        workflow_id: WorkflowId,
        ctx: &ActionContext,
    ) -> Result<Workflow, DomainError>
    where
        W: WorkflowRepository + Send + Sync,
    {
        let workflow = self.workflows.get(workflow_id).await?;
        Self::check_scope(ctx, workflow.client_profile_id)?;
        Ok(workflow)
    }
}

impl<W, N, E, T, V, A> WorkflowService for WorkflowServiceImpl<W, N, E, T, V, A>
where
    W: WorkflowRepository + Send + Sync,
    N: WorkflowNodeRepository + Send + Sync,
    E: WorkflowEdgeRepository + Send + Sync,
    T: TriggerRepository + Send + Sync,
    V: WorkflowVersionRepository + Send + Sync,
    A: AuditService + Send + Sync,
{
    async fn create_workflow(
        &self,
        input: NewWorkflow,
        ctx: &ActionContext,
    ) -> Result<Workflow, DomainError> {
        Self::check_scope(ctx, input.client_profile_id)?;
        let workflow = Workflow::new(input.client_profile_id, input.name)?;
        let workflow = self.workflows.create(workflow).await?;
        self.audit_workflow(
            ctx,
            "workflow.created",
            &workflow.workflow_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(workflow)
    }

    async fn publish_workflow(
        &self,
        workflow_id: WorkflowId,
        ctx: &ActionContext,
    ) -> Result<Workflow, DomainError> {
        let mut workflow = self.load_scoped_workflow(workflow_id, ctx).await?;
        workflow.publish()?;
        // Freeze an immutable snapshot of the current definition. The
        // snapshot is opaque to the domain; the service builds it from
        // the live nodes, edges and trigger.
        let nodes = self.nodes.list_by_workflow(workflow_id, 10_000).await?;
        let edges = self.edges.list_by_workflow(workflow_id, 10_000).await?;
        let trigger = self.triggers.get_by_workflow(workflow_id).await?;
        let snapshot = serde_json::json!({
            "name": workflow.name,
            "nodes": nodes,
            "edges": edges,
            "trigger": trigger,
        });
        let existing = self.versions.list_by_workflow(workflow_id, 1).await?;
        let next_number = existing
            .first()
            .map_or(1, |latest| latest.version_number.saturating_add(1));
        let version = WorkflowVersion::new(workflow_id, next_number, snapshot)?;
        self.versions.create(version).await?;
        let workflow = self.workflows.update(workflow).await?;
        self.audit_workflow(
            ctx,
            "workflow.published",
            &workflow.workflow_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(workflow)
    }

    async fn archive_workflow(
        &self,
        workflow_id: WorkflowId,
        ctx: &ActionContext,
    ) -> Result<Workflow, DomainError> {
        let mut workflow = self.load_scoped_workflow(workflow_id, ctx).await?;
        workflow.archive()?;
        let workflow = self.workflows.update(workflow).await?;
        self.audit_workflow(
            ctx,
            "workflow.archived",
            &workflow.workflow_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(workflow)
    }

    async fn add_node(
        &self,
        input: NewWorkflowNode,
        ctx: &ActionContext,
    ) -> Result<crm_domain::workflow::WorkflowNode, DomainError> {
        let workflow = self.load_scoped_workflow(input.workflow_id, ctx).await?;
        let node = crm_domain::workflow::WorkflowNode::new(
            workflow.workflow_id,
            input.node_type,
            input.config,
            input.position,
        )?;
        let node = self.nodes.create(node).await?;
        self.audit_workflow(
            ctx,
            "workflow.node_added",
            &node.node_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(node)
    }

    async fn remove_node(
        &self,
        node_id: WorkflowNodeId,
        ctx: &ActionContext,
    ) -> Result<(), DomainError> {
        let node = self.nodes.get(node_id).await?;
        let workflow = self.load_scoped_workflow(node.workflow_id, ctx).await?;
        // Remove dangling edges first so no edge references a deleted
        // node.
        let edges = self
            .edges
            .list_by_workflow(workflow.workflow_id, 10_000)
            .await?;
        for edge in edges {
            if edge.from_node == node_id || edge.to_node == node_id {
                self.edges.delete(edge.edge_id).await?;
            }
        }
        self.nodes.delete(node_id).await?;
        self.audit_workflow(
            ctx,
            "workflow.node_removed",
            &node_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(())
    }

    async fn add_edge(
        &self,
        input: NewWorkflowEdge,
        ctx: &ActionContext,
    ) -> Result<crm_domain::workflow::WorkflowEdge, DomainError> {
        let workflow = self.load_scoped_workflow(input.workflow_id, ctx).await?;
        // Both endpoints must exist in the workflow; the domain
        // rejects self-loops.
        self.nodes.get(input.from_node).await?;
        self.nodes.get(input.to_node).await?;
        let edge = crm_domain::workflow::WorkflowEdge::new(
            workflow.workflow_id,
            input.from_node,
            input.to_node,
            input.label,
        )?;
        let edge = self.edges.create(edge).await?;
        self.audit_workflow(
            ctx,
            "workflow.edge_added",
            &edge.edge_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(edge)
    }

    async fn remove_edge(
        &self,
        edge_id: WorkflowEdgeId,
        ctx: &ActionContext,
    ) -> Result<(), DomainError> {
        let edge = self.edges.get(edge_id).await?;
        self.load_scoped_workflow(edge.workflow_id, ctx).await?;
        self.edges.delete(edge_id).await?;
        self.audit_workflow(
            ctx,
            "workflow.edge_removed",
            &edge_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(())
    }

    async fn set_trigger(
        &self,
        input: NewTrigger,
        ctx: &ActionContext,
    ) -> Result<Trigger, DomainError> {
        let workflow = self.load_scoped_workflow(input.workflow_id, ctx).await?;
        let trigger = Trigger::new(workflow.workflow_id, input.trigger_type, input.config)?;
        // One trigger per workflow: the repository upserts, replacing
        // any existing trigger.
        let trigger = self.triggers.set(trigger).await?;
        self.audit_workflow(
            ctx,
            "workflow.trigger_set",
            &trigger.trigger_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(trigger)
    }

    async fn simulate(
        &self,
        workflow_id: WorkflowId,
        input: serde_json::Value,
        ctx: &ActionContext,
    ) -> Result<crm_domain::workflow::SimulationTrace, DomainError> {
        let workflow = self.load_scoped_workflow(workflow_id, ctx).await?;
        let nodes = self.nodes.list_by_workflow(workflow_id, 10_000).await?;
        let edges = self.edges.list_by_workflow(workflow_id, 10_000).await?;
        // Pure dry-run: the simulator never executes actions and never
        // mutates state.
        let trace = Simulator::simulate(&workflow, &nodes, &edges, &input);
        self.audit_workflow(
            ctx,
            "workflow.simulated",
            &workflow.workflow_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(trace)
    }
}
