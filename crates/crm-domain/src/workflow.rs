// SPDX-License-Identifier: GPL-3.0-only
//! Workflow builder domain: typed visual workflows, triggers, versioning
//! and a pure simulator.
//!
//! Staff compose automation workflows on a visual canvas: typed nodes
//! (trigger / action / condition / delay) connected by typed edges,
//! started by one trigger (event, webhook, schedule or manual). Every
//! workflow belongs to one client profile — the tenant boundary.
//!
//! ## Authorization (deferred)
//!
//! Workflow operations will authorize against `Resource::Workflow`,
//! profile-scoped. That variant is **not** added here: the `Resource`
//! → database-string mapping lives in `crates/crm-db` (exhaustive
//! match), and this task is domain-only. 10-WORK-02 (the vertical
//! slice) adds the variant together with its `"workflow"` mapping arms
//! atomically, so the workspace keeps compiling.
//!
//! ## Audit
//!
//! Workflow mutations are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s by the service
//! implementation, with pinned action names documented on
//! [`WorkflowService`]. They are deliberately **not**
//! [`SecurityEvent`](crate::audit::SecurityEvent) variants: that
//! taxonomy covers security-relevant events (auth, RBAC, sessions,
//! rate limits), while workflows are business operations audited like
//! contacts and campaigns.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::DomainError;
use crate::ids::{
    ClientProfileId, TriggerId, WorkflowEdgeId, WorkflowId, WorkflowNodeId, WorkflowVersionId,
};

/// Lifecycle of a workflow definition.
///
/// The happy path is `Draft → Published`. `Archived` is terminal and
/// reachable from both `Draft` and `Published`; archiving is
/// idempotent so repeated archive calls are safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowStatus {
    /// Being edited on the canvas; not executable.
    Draft,
    /// Frozen and executable; a [`WorkflowVersion`] snapshot is taken
    /// on publish.
    Published,
    /// Retired; terminal.
    Archived,
}

impl WorkflowStatus {
    /// Whether the status is terminal (no further transitions allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, WorkflowStatus::Archived)
    }
}

/// A named automation workflow owned by one client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    /// Stable identifier of this workflow.
    pub workflow_id: WorkflowId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Human-readable name, validated non-blank.
    pub name: String,
    /// Lifecycle status.
    pub status: WorkflowStatus,
    /// When the workflow was created.
    pub created_at: DateTime<Utc>,
    /// When the workflow was last changed.
    pub updated_at: DateTime<Utc>,
}

impl Workflow {
    /// Creates a workflow in `Draft` status.
    ///
    /// Rejects: blank `name`.
    ///
    /// ```
    /// use crm_domain::ids::ClientProfileId;
    /// use crm_domain::workflow::{Workflow, WorkflowStatus};
    ///
    /// let workflow = Workflow::new(ClientProfileId::new(), "Welcome drip".to_string())
    ///     .expect("valid workflow");
    /// assert_eq!(workflow.status, WorkflowStatus::Draft);
    /// ```
    pub fn new(client_profile_id: ClientProfileId, name: String) -> Result<Self, DomainError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "workflow name must not be blank".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            workflow_id: WorkflowId::new(),
            client_profile_id,
            name,
            status: WorkflowStatus::Draft,
            created_at: now,
            updated_at: now,
        })
    }

    /// Publishes a draft workflow so it becomes executable.
    ///
    /// Only `Draft → Published` is allowed; publishing also freezes a
    /// [`WorkflowVersion`] snapshot (done by the service).
    pub fn publish(&mut self) -> Result<(), DomainError> {
        if self.status != WorkflowStatus::Draft {
            return Err(DomainError::Validation(format!(
                "cannot publish a workflow in status {:?}",
                self.status
            )));
        }
        self.status = WorkflowStatus::Published;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Archives a workflow. Idempotent: archiving an already-archived
    /// workflow succeeds without changing it.
    pub fn archive(&mut self) -> Result<(), DomainError> {
        if self.status == WorkflowStatus::Archived {
            return Ok(());
        }
        self.status = WorkflowStatus::Archived;
        self.updated_at = Utc::now();
        Ok(())
    }
}

/// The kind of a [`WorkflowNode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowNodeType {
    /// Entry point of a run; exactly one per workflow.
    TriggerNode,
    /// Performs one action (e.g. send an email, create a task).
    ActionNode,
    /// Branches the run on a predicate.
    ConditionNode,
    /// Pauses the run for a duration.
    DelayNode,
}

/// One typed step on the workflow canvas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNode {
    /// Stable identifier of this node.
    pub node_id: WorkflowNodeId,
    /// The workflow this node belongs to.
    pub workflow_id: WorkflowId,
    /// What kind of step this is.
    pub node_type: WorkflowNodeType,
    /// Node configuration (opaque JSON, never null). A
    /// [`WorkflowNodeType::DelayNode`] must carry a positive
    /// `duration_secs` number.
    pub config: Value,
    /// Canvas position in pixels: `(x, y)`.
    pub position: (i32, i32),
}

impl WorkflowNode {
    /// Creates a node.
    ///
    /// Rejects: a null `config`; a [`WorkflowNodeType::DelayNode`]
    /// whose config lacks a positive `duration_secs` number.
    ///
    /// ```
    /// use crm_domain::ids::WorkflowId;
    /// use crm_domain::workflow::{WorkflowNode, WorkflowNodeType};
    /// use serde_json::json;
    ///
    /// let node = WorkflowNode::new(
    ///     WorkflowId::new(),
    ///     WorkflowNodeType::ActionNode,
    ///     json!({"action": "email.send"}),
    ///     (120, 80),
    /// )
    /// .expect("valid node");
    /// assert_eq!(node.position, (120, 80));
    /// ```
    pub fn new(
        workflow_id: WorkflowId,
        node_type: WorkflowNodeType,
        config: Value,
        position: (i32, i32),
    ) -> Result<Self, DomainError> {
        if config.is_null() {
            return Err(DomainError::Validation(
                "node config must not be null".to_string(),
            ));
        }
        if node_type == WorkflowNodeType::DelayNode {
            let duration_ok = config
                .get("duration_secs")
                .and_then(Value::as_u64)
                .is_some_and(|secs| secs > 0);
            if !duration_ok {
                return Err(DomainError::Validation(
                    "delay nodes require a positive duration_secs in config".to_string(),
                ));
            }
        }
        Ok(Self {
            node_id: WorkflowNodeId::new(),
            workflow_id,
            node_type,
            config,
            position,
        })
    }

    /// Moves the node on the canvas. Positions are display-only; the
    /// domain places no bounds on them.
    pub fn move_to(&mut self, position: (i32, i32)) {
        self.position = position;
    }
}

/// A typed connection between two [`WorkflowNode`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEdge {
    /// Stable identifier of this edge.
    pub edge_id: WorkflowEdgeId,
    /// The workflow this edge belongs to.
    pub workflow_id: WorkflowId,
    /// The node the edge leaves.
    pub from_node: WorkflowNodeId,
    /// The node the edge enters.
    pub to_node: WorkflowNodeId,
    /// Optional label (e.g. the condition branch: `"yes"` / `"no"`).
    pub label: Option<String>,
}

impl WorkflowEdge {
    /// Creates an edge.
    ///
    /// Rejects: self-loops (`from_node == to_node`). Whether both
    /// endpoints exist in the workflow is checked by the service,
    /// which can see the full node set.
    ///
    /// ```
    /// use crm_domain::ids::{WorkflowId, WorkflowNodeId};
    /// use crm_domain::workflow::WorkflowEdge;
    ///
    /// let edge = WorkflowEdge::new(
    ///     WorkflowId::new(),
    ///     WorkflowNodeId::new(),
    ///     WorkflowNodeId::new(),
    ///     Some("yes".to_string()),
    /// )
    /// .expect("valid edge");
    /// assert_eq!(edge.label.as_deref(), Some("yes"));
    /// ```
    pub fn new(
        workflow_id: WorkflowId,
        from_node: WorkflowNodeId,
        to_node: WorkflowNodeId,
        label: Option<String>,
    ) -> Result<Self, DomainError> {
        if from_node == to_node {
            return Err(DomainError::Validation(
                "workflow edges must not be self-loops".to_string(),
            ));
        }
        let label = label.map(|l| l.trim().to_owned()).filter(|l| !l.is_empty());
        Ok(Self {
            edge_id: WorkflowEdgeId::new(),
            workflow_id,
            from_node,
            to_node,
            label,
        })
    }
}

/// How a workflow run is started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerType {
    /// Fires on a domain event (e.g. `contact.created`).
    EventTrigger,
    /// Fires on an inbound HTTP webhook call.
    WebhookTrigger,
    /// Fires on a cron-style schedule.
    ScheduleTrigger,
    /// Started manually by a staff member.
    ManualTrigger,
}

/// The trigger that starts a workflow's runs.
///
/// Exactly one trigger per workflow; the service enforces the
/// invariant when [`set_trigger`](crate::services::WorkflowService::set_trigger)
/// replaces the existing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// Stable identifier of this trigger.
    pub trigger_id: TriggerId,
    /// The workflow this trigger starts.
    pub workflow_id: WorkflowId,
    /// How runs are started.
    pub trigger_type: TriggerType,
    /// Trigger configuration (opaque JSON, never null).
    pub config: Value,
}

impl Trigger {
    /// Creates a trigger.
    ///
    /// Rejects: a null `config`.
    pub fn new(
        workflow_id: WorkflowId,
        trigger_type: TriggerType,
        config: Value,
    ) -> Result<Self, DomainError> {
        if config.is_null() {
            return Err(DomainError::Validation(
                "trigger config must not be null".to_string(),
            ));
        }
        Ok(Self {
            trigger_id: TriggerId::new(),
            workflow_id,
            trigger_type,
            config,
        })
    }
}

/// An immutable snapshot of a workflow taken at publish time.
///
/// Versions are append-only: there are deliberately no mutators, so a
/// published definition can always be reconstructed exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowVersion {
    /// Stable identifier of this version record.
    pub version_id: WorkflowVersionId,
    /// The workflow this version snapshots.
    pub workflow_id: WorkflowId,
    /// Monotonic version number within the workflow (starts at 1).
    pub version_number: u32,
    /// The frozen definition (nodes, edges, trigger) as JSON.
    pub snapshot: Value,
    /// When the version was published.
    pub created_at: DateTime<Utc>,
}

impl WorkflowVersion {
    /// Snapshots a workflow definition. The snapshot is opaque to the
    /// domain; the service builds it from the current nodes, edges
    /// and trigger at publish time.
    ///
    /// Rejects: a null `snapshot`, a zero `version_number`.
    pub fn new(
        workflow_id: WorkflowId,
        version_number: u32,
        snapshot: Value,
    ) -> Result<Self, DomainError> {
        if version_number == 0 {
            return Err(DomainError::Validation(
                "version numbers start at 1".to_string(),
            ));
        }
        if snapshot.is_null() {
            return Err(DomainError::Validation(
                "version snapshot must not be null".to_string(),
            ));
        }
        Ok(Self {
            version_id: WorkflowVersionId::new(),
            workflow_id,
            version_number,
            snapshot,
            created_at: Utc::now(),
        })
    }
}

/// What the [`Simulator`] concluded about one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SimulationStepStatus {
    /// The node would run in a real execution.
    WouldRun,
    /// The node was reached but its branch is not resolved by the
    /// dry-run (condition branches are followed, not evaluated).
    Skipped,
    /// The node's config is invalid for simulation.
    Failed,
}

/// One step of a [`SimulationTrace`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationStep {
    /// The node this step describes.
    pub node_id: WorkflowNodeId,
    /// The kind of node.
    pub node_type: WorkflowNodeType,
    /// The simulation verdict for the node.
    pub status: SimulationStepStatus,
    /// Human-readable explanation of the verdict.
    pub detail: String,
}

/// The ordered result of a dry-run: one [`SimulationStep`] per node
/// reachable from the trigger, in traversal order.
pub type SimulationTrace = Vec<SimulationStep>;

/// Pure dry-run executor for workflows.
///
/// The simulator walks the graph from the trigger node and records
/// what *would* happen. It never executes actions, never calls
/// providers, never touches the network: it is a pure function of
/// its inputs, deterministic and unit-testable.
///
/// Traversal rules:
/// * starts at the [`WorkflowNodeType::TriggerNode`]; a workflow with
///   no trigger node yields an empty trace;
/// * only nodes and edges belonging to `workflow` are considered;
/// * follows edges breadth-first; each node is visited at most once
///   (cycles terminate);
/// * nodes unreachable from the trigger are absent from the trace;
/// * [`WorkflowNodeType::ConditionNode`] branches are followed but
///   not evaluated (marked `Skipped` with an explanation);
/// * a [`WorkflowNodeType::DelayNode`] with an unreadable duration is
///   marked `Failed` (defensive: [`WorkflowNode::new`] rejects such
///   configs at construction).
pub struct Simulator;

impl Simulator {
    /// Dry-runs `workflow`'s graph against `input` without executing
    /// anything. `input` is accepted for signature symmetry with the
    /// real executor (a later task) and is currently unused.
    #[must_use]
    pub fn simulate(
        workflow: &Workflow,
        nodes: &[WorkflowNode],
        edges: &[WorkflowEdge],
        _input: &Value,
    ) -> SimulationTrace {
        let nodes: Vec<&WorkflowNode> = nodes
            .iter()
            .filter(|node| node.workflow_id == workflow.workflow_id)
            .collect();
        let edges: Vec<&WorkflowEdge> = edges
            .iter()
            .filter(|edge| edge.workflow_id == workflow.workflow_id)
            .collect();
        let Some(trigger) = nodes
            .iter()
            .find(|node| node.node_type == WorkflowNodeType::TriggerNode)
        else {
            return Vec::new();
        };

        let mut trace = Vec::new();
        let mut visited: Vec<WorkflowNodeId> = Vec::new();
        let mut queue = std::collections::VecDeque::from([trigger.node_id]);

        while let Some(node_id) = queue.pop_front() {
            if visited.contains(&node_id) {
                continue;
            }
            visited.push(node_id);
            let Some(node) = nodes.iter().find(|n| n.node_id == node_id) else {
                continue;
            };

            let (status, detail) = match node.node_type {
                WorkflowNodeType::TriggerNode => (
                    SimulationStepStatus::WouldRun,
                    "trigger would fire".to_string(),
                ),
                WorkflowNodeType::ActionNode => (
                    SimulationStepStatus::WouldRun,
                    "action would execute (not executed by the simulator)".to_string(),
                ),
                WorkflowNodeType::ConditionNode => (
                    SimulationStepStatus::Skipped,
                    "condition not evaluated in simulation; branches followed".to_string(),
                ),
                WorkflowNodeType::DelayNode => {
                    match node.config.get("duration_secs").and_then(Value::as_u64) {
                        Some(secs) => (
                            SimulationStepStatus::WouldRun,
                            format!("would wait {secs} seconds"),
                        ),
                        None => (
                            SimulationStepStatus::Failed,
                            "delay node has no readable duration_secs".to_string(),
                        ),
                    }
                }
            };
            trace.push(SimulationStep {
                node_id,
                node_type: node.node_type,
                status,
                detail,
            });

            for edge in edges.iter().filter(|e| e.from_node == node_id) {
                if !visited.contains(&edge.to_node) {
                    queue.push_back(edge.to_node);
                }
            }
        }

        trace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn draft_workflow() -> Workflow {
        Workflow::new(ClientProfileId::new(), "Onboarding".to_string()).expect("valid workflow")
    }

    fn action_node(workflow_id: WorkflowId) -> WorkflowNode {
        WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::ActionNode,
            json!({"action": "email.send"}),
            (0, 0),
        )
        .expect("valid node")
    }

    #[test]
    fn workflow_lifecycle_is_forward_only() {
        let mut workflow = draft_workflow();
        // Cannot archive-publish out of order.
        assert!(workflow.archive().is_ok());
        assert_eq!(workflow.status, WorkflowStatus::Archived);
        assert!(workflow.status.is_terminal());
        // Terminal: nothing moves.
        assert!(workflow.publish().is_err());
    }

    #[test]
    fn workflow_archive_is_idempotent() {
        let mut workflow = draft_workflow();
        workflow.archive().expect("archive");
        workflow.archive().expect("archive again");
        assert_eq!(workflow.status, WorkflowStatus::Archived);
    }

    #[test]
    fn workflow_publish_requires_draft() {
        let mut workflow = draft_workflow();
        workflow.publish().expect("publish");
        assert_eq!(workflow.status, WorkflowStatus::Published);
        assert!(!workflow.status.is_terminal());
        // Cannot publish twice.
        assert!(workflow.publish().is_err());
        // Published archives cleanly.
        workflow.archive().expect("archive published");
        assert_eq!(workflow.status, WorkflowStatus::Archived);
    }

    #[test]
    fn workflow_rejects_blank_name() {
        assert!(Workflow::new(ClientProfileId::new(), "  ".to_string()).is_err());
    }

    #[test]
    fn node_validates_config() {
        let workflow_id = WorkflowId::new();
        // Null config rejected.
        assert!(WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::ActionNode,
            Value::Null,
            (0, 0),
        )
        .is_err());
        // Delay node requires a positive duration_secs.
        assert!(
            WorkflowNode::new(workflow_id, WorkflowNodeType::DelayNode, json!({}), (0, 0),)
                .is_err()
        );
        assert!(WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::DelayNode,
            json!({"duration_secs": 0}),
            (0, 0),
        )
        .is_err());
        let delay = WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::DelayNode,
            json!({"duration_secs": 300}),
            (10, 20),
        )
        .expect("valid delay");
        assert_eq!(delay.position, (10, 20));
        // Moving the node only changes its canvas position.
        let mut movable = action_node(workflow_id);
        movable.move_to((5, 5));
        assert_eq!(movable.position, (5, 5));
    }

    #[test]
    fn edge_rejects_self_loops() {
        let workflow_id = WorkflowId::new();
        let node = WorkflowNodeId::new();
        assert!(WorkflowEdge::new(workflow_id, node, node, None).is_err());
        let edge =
            WorkflowEdge::new(workflow_id, node, WorkflowNodeId::new(), None).expect("valid edge");
        assert!(edge.label.is_none());
    }

    #[test]
    fn version_is_immutable() {
        let version = WorkflowVersion::new(WorkflowId::new(), 1, json!({"nodes": [], "edges": []}))
            .expect("valid version");
        assert_eq!(version.version_number, 1);
        // No mutators exist: the type only exposes construction and
        // reads. Rejects zero version numbers and null snapshots.
        assert!(WorkflowVersion::new(WorkflowId::new(), 0, json!({})).is_err());
        assert!(WorkflowVersion::new(WorkflowId::new(), 1, Value::Null).is_err());
    }

    #[test]
    fn simulator_traces_reachable_nodes_in_order() {
        let workflow = draft_workflow();
        let workflow_id = workflow.workflow_id;
        let trigger = WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::TriggerNode,
            json!({"trigger": "manual"}),
            (0, 0),
        )
        .expect("trigger");
        let action = action_node(workflow_id);
        let delay = WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::DelayNode,
            json!({"duration_secs": 60}),
            (0, 0),
        )
        .expect("delay");
        let orphan = action_node(workflow_id);

        let e1 =
            WorkflowEdge::new(workflow_id, trigger.node_id, action.node_id, None).expect("edge");
        let e2 = WorkflowEdge::new(workflow_id, action.node_id, delay.node_id, None).expect("edge");

        let nodes = vec![trigger.clone(), action.clone(), delay.clone(), orphan];
        let edges = vec![e1, e2];
        let trace = Simulator::simulate(&workflow, &nodes, &edges, &json!({}));

        // Trigger → action → delay, in traversal order; the orphan is
        // unreachable and absent.
        assert_eq!(trace.len(), 3);
        assert_eq!(trace[0].node_id, trigger.node_id);
        assert_eq!(trace[0].status, SimulationStepStatus::WouldRun);
        assert_eq!(trace[1].node_id, action.node_id);
        assert_eq!(trace[2].node_id, delay.node_id);
        assert!(trace[2].detail.contains("60"));
    }

    #[test]
    fn simulator_marks_conditions_skipped_and_never_executes() {
        let workflow = draft_workflow();
        let workflow_id = workflow.workflow_id;
        let trigger = WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::TriggerNode,
            json!({"trigger": "manual"}),
            (0, 0),
        )
        .expect("trigger");
        let condition = WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::ConditionNode,
            json!({"predicate": "contact.vip"}),
            (0, 0),
        )
        .expect("condition");
        let edge =
            WorkflowEdge::new(workflow_id, trigger.node_id, condition.node_id, None).expect("edge");

        let trace = Simulator::simulate(
            &workflow,
            &[trigger, condition.clone()],
            &[edge],
            &json!({}),
        );
        assert_eq!(trace.len(), 2);
        assert_eq!(trace[1].node_id, condition.node_id);
        assert_eq!(trace[1].status, SimulationStepStatus::Skipped);
    }

    #[test]
    fn simulator_without_trigger_yields_empty_trace() {
        let workflow = draft_workflow();
        let nodes = vec![action_node(workflow.workflow_id)];
        let trace = Simulator::simulate(&workflow, &nodes, &[], &json!({}));
        assert!(trace.is_empty());
    }

    #[test]
    fn simulator_scopes_graph_to_the_workflow() {
        let workflow = draft_workflow();
        let other_id = WorkflowId::new();
        // A trigger belonging to a *different* workflow is ignored.
        let foreign_trigger = WorkflowNode::new(
            other_id,
            WorkflowNodeType::TriggerNode,
            json!({"trigger": "manual"}),
            (0, 0),
        )
        .expect("trigger");
        let trace = Simulator::simulate(&workflow, &[foreign_trigger], &[], &json!({}));
        assert!(trace.is_empty());
    }

    #[test]
    fn simulator_terminates_on_cycles() {
        let workflow = draft_workflow();
        let workflow_id = workflow.workflow_id;
        let trigger = WorkflowNode::new(
            workflow_id,
            WorkflowNodeType::TriggerNode,
            json!({"trigger": "manual"}),
            (0, 0),
        )
        .expect("trigger");
        let action = action_node(workflow_id);
        // trigger → action → trigger (cycle).
        let e1 =
            WorkflowEdge::new(workflow_id, trigger.node_id, action.node_id, None).expect("edge");
        let e2 =
            WorkflowEdge::new(workflow_id, action.node_id, trigger.node_id, None).expect("edge");

        let trace = Simulator::simulate(&workflow, &[trigger, action], &[e1, e2], &json!({}));
        // Each node visited at most once: the cycle terminates.
        assert_eq!(trace.len(), 2);
    }
}
