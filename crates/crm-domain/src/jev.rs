// SPDX-License-Identifier: GPL-3.0-only
//! Jev agent domain: runs, model adapters, tools, approval gates and handoffs.
//!
//! Jev is the agent-native core of the CRM: staff (and other agents)
//! delegate work to agent runs that call typed tools through a
//! provider-agnostic model adapter. Every run is scoped to one client
//! profile and every mutation is authorized and audited by the service
//! layer (task 09-JEV-02).
//!
//! ## Authorization
//!
//! Agent operations authorize against
//! [`Resource::AgentRun`](crate::security::Resource::AgentRun):
//! profile-scoped contexts may only touch their own profile's runs;
//! agency-scoped contexts require an agency grant. The service-trait
//! docs reference the authorization requirement.
//!
//! Note: [`Resource::AgentAction`](crate::security::Resource::AgentAction)
//! (task 01-FND-01) covers audit records of delegated tool calls; the
//! `AgentRun` resource covers the run lifecycle itself (start, approve,
//! cancel, handoff).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::DomainError;
use crate::ids::{ActorId, AgentRunId, AgentToolId, ClientProfileId, HandoffId, ToolCallId};

/// Lifecycle of an agent run.
///
/// The happy path is `Pending → Running → Completed`. A run that needs
/// a human decision moves to `AwaitingApproval`; `approve()` resumes it
/// to `Running`, `reject()` fails it. `Cancelled` is reachable from any
/// non-terminal state; `Failed` is reachable from `Running` and
/// `AwaitingApproval`. Both are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentRunStatus {
    /// Created but not yet started.
    Pending,
    /// Actively executing (model calls and tool calls in flight).
    Running,
    /// Paused waiting for a human approval decision.
    AwaitingApproval,
    /// Finished successfully; `output` is set.
    Completed,
    /// Finished with an error; terminal.
    Failed,
    /// Stopped by an authorized actor; terminal.
    Cancelled,
}

impl AgentRunStatus {
    /// Whether the status is terminal (no further transitions allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentRunStatus::Completed | AgentRunStatus::Failed | AgentRunStatus::Cancelled
        )
    }
}

/// One delegated unit of agent work inside a client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    /// Stable identifier of this run.
    pub run_id: AgentRunId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Which kind of agent executes this run (e.g. `"triage"`, `"drafter"`).
    pub agent_kind: String,
    /// Current lifecycle status.
    pub status: AgentRunStatus,
    /// Model-adapter name currently selected for this run. Starts as the
    /// chain's primary; the service advances it along
    /// [`FallbackChain`] when an adapter fails (task 09-JEV-02).
    pub adapter_name: String,
    /// Ordered fallback chain of adapter names, stored on the run so a
    /// reloaded run can keep walking the chain after a failure.
    pub fallback_chain: FallbackChain,
    /// Input payload for the run (task description, parameters).
    pub input: Value,
    /// Output payload, set on completion.
    pub output: Option<Value>,
    /// The human approval gate decision, once made (FIX-04). Set when
    /// a run-level approval is decided via [`ApprovalGate::decide`];
    /// `None` while no decision has been recorded.
    pub approval_gate: Option<ApprovalGate>,
    /// When the run was created.
    pub created_at: DateTime<Utc>,
    /// When the run was last updated.
    pub updated_at: DateTime<Utc>,
}

impl AgentRun {
    /// Creates a run in `Pending` status.
    ///
    /// The initial adapter is the chain's primary; `adapter_name` stays
    /// consistent with the stored chain by construction.
    ///
    /// Rejects: blank `agent_kind`, a null input.
    ///
    /// ```
    /// use crm_domain::ids::ClientProfileId;
    /// use crm_domain::jev::{AgentRun, AgentRunStatus, FallbackChain};
    /// use serde_json::json;
    ///
    /// let chain = FallbackChain::new(vec!["openrouter".to_string()])
    ///     .expect("valid chain");
    /// let run = AgentRun::new(
    ///     ClientProfileId::new(),
    ///     "triage".to_string(),
    ///     chain,
    ///     json!({"task": "triage inbox"}),
    /// )
    /// .expect("valid run");
    /// assert_eq!(run.status, AgentRunStatus::Pending);
    /// assert_eq!(run.adapter_name, "openrouter");
    /// ```
    pub fn new(
        client_profile_id: ClientProfileId,
        agent_kind: String,
        fallback_chain: FallbackChain,
        input: Value,
    ) -> Result<Self, DomainError> {
        let agent_kind = agent_kind.trim().to_owned();
        if agent_kind.is_empty() {
            return Err(DomainError::Validation(
                "agent kind must not be blank".to_string(),
            ));
        }
        if input.is_null() {
            return Err(DomainError::Validation(
                "run input must not be null".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            run_id: AgentRunId::new(),
            client_profile_id,
            agent_kind,
            status: AgentRunStatus::Pending,
            adapter_name: fallback_chain.primary().to_owned(),
            fallback_chain,
            input,
            output: None,
            approval_gate: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Starts a pending run.
    pub fn start(&mut self) -> Result<(), DomainError> {
        if self.status != AgentRunStatus::Pending {
            return Err(DomainError::Validation(format!(
                "cannot start a run in status {:?}",
                self.status
            )));
        }
        self.transition(AgentRunStatus::Running, "start")
    }

    /// Pauses a running run to await a human approval decision.
    pub fn request_approval(&mut self) -> Result<(), DomainError> {
        self.transition(AgentRunStatus::AwaitingApproval, "request approval")?;
        // Awaiting a fresh decision: the previous gate (if any) is
        // stale and must not be exposed as the current decision.
        self.approval_gate = None;
        Ok(())
    }

    /// Resumes an approval-awaiting run after a human approved.
    pub fn approve(&mut self) -> Result<(), DomainError> {
        self.transition(AgentRunStatus::Running, "approve")
    }

    /// Fails an approval-awaiting run after a human rejected it.
    pub fn reject(&mut self) -> Result<(), DomainError> {
        self.transition(AgentRunStatus::Failed, "reject")
    }

    /// Completes a running run, recording its output.
    pub fn complete(&mut self, output: Value) -> Result<(), DomainError> {
        if self.status != AgentRunStatus::Running {
            return Err(DomainError::Validation(format!(
                "only a running run can complete (status: {:?})",
                self.status
            )));
        }
        self.status = AgentRunStatus::Completed;
        self.output = Some(output);
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Fails a running (or approval-awaiting) run.
    pub fn fail(&mut self) -> Result<(), DomainError> {
        match self.status {
            AgentRunStatus::Running | AgentRunStatus::AwaitingApproval => {
                self.status = AgentRunStatus::Failed;
                self.updated_at = Utc::now();
                Ok(())
            }
            other => Err(DomainError::Validation(format!(
                "cannot fail a run in status {other:?}"
            ))),
        }
    }

    /// Cancels a run from any non-terminal status. Idempotent on
    /// already-cancelled runs.
    pub fn cancel(&mut self) -> Result<(), DomainError> {
        if self.status == AgentRunStatus::Cancelled {
            return Ok(());
        }
        if self.status.is_terminal() {
            return Err(DomainError::Validation(format!(
                "run status {:?} is terminal",
                self.status
            )));
        }
        self.status = AgentRunStatus::Cancelled;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Validates and applies a single forward transition.
    fn transition(&mut self, next: AgentRunStatus, verb: &str) -> Result<(), DomainError> {
        let allowed = matches!(
            (self.status, next),
            (AgentRunStatus::Pending, AgentRunStatus::Running)
                | (AgentRunStatus::Running, AgentRunStatus::AwaitingApproval)
                | (AgentRunStatus::AwaitingApproval, AgentRunStatus::Running)
                | (AgentRunStatus::AwaitingApproval, AgentRunStatus::Failed)
        );
        if !allowed {
            return Err(DomainError::Validation(format!(
                "cannot {verb} a run in status {:?}",
                self.status
            )));
        }
        self.status = next;
        self.updated_at = Utc::now();
        Ok(())
    }
}

/// One tool-call request produced by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// Name of the tool to call.
    pub tool_name: String,
    /// Arguments for the tool (JSON).
    pub input: Value,
}

/// The model's completion: free text plus any tool calls it requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCompletion {
    /// Assistant text (may be empty when the model only calls tools).
    pub text: String,
    /// Tool calls requested by the model, in order.
    pub tool_calls: Vec<ToolCallRequest>,
}

/// Provider-agnostic model adapter (RPITIT).
///
/// Adapters wrap one LLM provider (OpenRouter, a local model, …) behind
/// a single `complete` call. The service layer picks the adapter from
/// the run's [`FallbackChain`] and retries the next adapter on failure.
///
/// ## Implementor contract
///
/// * **No PII logging.** Prompts and completions may contain contact
///   PII; adapters must never log them.
/// * **Timeouts.** Implementations must bound the call (provider
///   timeouts, retries) and surface a timeout as
///   [`DomainError::Validation`](crate::error::DomainError::Validation),
///   never by hanging the caller.
/// * **Error mapping.** Provider errors map to [`DomainError`]:
///   authentication/quota problems to `DomainError::Unauthorized` or
///   `DomainError::Validation`; never leak provider-internal details.
/// * **Scope.** An adapter must not call tools itself; it only *requests*
///   tool calls, which the service executes under the run's grants.
pub trait ModelAdapter {
    /// Runs one model completion against the prompt, offering the given
    /// tools. Returns the assistant text plus any requested tool calls.
    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        tools: &'a [AgentTool],
    ) -> impl std::future::Future<Output = Result<ModelCompletion, DomainError>> + Send + 'a;
}

/// Ordered fallback chain of model-adapter names.
///
/// When the selected adapter fails, the service walks the chain with
/// [`FallbackChain::next_after`] and retries the run on the next
/// adapter. The chain is a pure value type so the ordering logic stays
/// unit-testable without providers.
///
/// Invariants (non-empty, trimmed, deduplicated names) are enforced
/// both at construction and on deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FallbackChain {
    /// Adapter names in preference order; the first is the default.
    adapters: Vec<String>,
}

impl<'de> Deserialize<'de> for FallbackChain {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct FallbackChainShape {
            adapters: Vec<String>,
        }
        let shape = FallbackChainShape::deserialize(deserializer)?;
        FallbackChain::new(shape.adapters).map_err(serde::de::Error::custom)
    }
}

impl FallbackChain {
    /// Builds a chain; rejects an empty list or blank names.
    pub fn new(adapters: Vec<String>) -> Result<Self, DomainError> {
        if adapters.is_empty() {
            return Err(DomainError::Validation(
                "fallback chain requires at least one adapter".to_string(),
            ));
        }
        let mut cleaned = Vec::with_capacity(adapters.len());
        for name in adapters {
            let name = name.trim().to_owned();
            if name.is_empty() {
                return Err(DomainError::Validation(
                    "adapter names must not be blank".to_string(),
                ));
            }
            if !cleaned.contains(&name) {
                cleaned.push(name);
            }
        }
        Ok(Self { adapters: cleaned })
    }

    /// Adapter names in preference order; the first is the default.
    #[must_use]
    pub fn adapters(&self) -> &[String] {
        &self.adapters
    }

    /// The preferred (first) adapter.
    #[must_use]
    pub fn primary(&self) -> &str {
        // `new` guarantees at least one adapter.
        self.adapters.first().map_or("", String::as_str)
    }

    /// The adapter to try after `failed` one, or `None` when the chain
    /// is exhausted (the run should then fail).
    #[must_use]
    pub fn next_after(&self, failed: &str) -> Option<&str> {
        let position = self.adapters.iter().position(|name| name == failed)?;
        self.adapters.get(position + 1).map(String::as_str)
    }
}

/// A typed tool an agent may call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTool {
    /// Stable identifier of this tool definition.
    pub tool_id: AgentToolId,
    /// Tool name as the model sees it (e.g. `"crm.create_contact"`).
    pub name: String,
    /// Human-readable description for the model and auditors.
    pub description: String,
    /// JSON Schema describing the tool's input (structural check only).
    pub input_schema: Value,
    /// Whether calling this tool pauses the run for human approval.
    pub requires_approval: bool,
}

impl AgentTool {
    /// Defines a tool.
    ///
    /// Rejects: blank name/description, a non-object or null schema.
    /// The schema is a structural check only — full JSON Schema
    /// validation happens at the tool-execution boundary.
    ///
    /// ```
    /// use crm_domain::jev::AgentTool;
    /// use serde_json::json;
    ///
    /// let tool = AgentTool::new(
    ///     "crm.create_contact".to_string(),
    ///     "Creates a contact".to_string(),
    ///     json!({"type": "object", "properties": {}}),
    ///     true,
    /// )
    /// .expect("valid tool");
    /// assert!(tool.requires_approval);
    /// ```
    pub fn new(
        name: String,
        description: String,
        input_schema: Value,
        requires_approval: bool,
    ) -> Result<Self, DomainError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "tool name must not be blank".to_string(),
            ));
        }
        let description = description.trim().to_owned();
        if description.is_empty() {
            return Err(DomainError::Validation(
                "tool description must not be blank".to_string(),
            ));
        }
        if !input_schema.is_object() {
            return Err(DomainError::Validation(
                "tool input schema must be a JSON object".to_string(),
            ));
        }
        Ok(Self {
            tool_id: AgentToolId::new(),
            name,
            description,
            input_schema,
            requires_approval,
        })
    }
}

/// Lifecycle of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCallStatus {
    /// Requested by the model; not yet executed.
    Pending,
    /// Human approved; ready to execute.
    Approved,
    /// Human denied; terminal.
    Denied,
    /// Executed successfully; `output` is set.
    Executed,
    /// Execution failed; terminal.
    Failed,
}

impl ToolCallStatus {
    /// Whether the status is terminal (no further transitions allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ToolCallStatus::Denied | ToolCallStatus::Executed | ToolCallStatus::Failed
        )
    }
}

/// One tool invocation inside an agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Stable identifier of this call.
    pub call_id: ToolCallId,
    /// The run this call belongs to.
    pub run_id: AgentRunId,
    /// Tool name (matches [`AgentTool::name`]).
    pub tool_name: String,
    /// Arguments supplied by the model.
    pub input: Value,
    /// Current lifecycle status.
    pub status: ToolCallStatus,
    /// Whether the tool required human approval, captured at record
    /// time (task 09-JEV-02).
    pub requires_approval: bool,
    /// Tool output, set on execution.
    pub output: Option<Value>,
    /// When the call was recorded.
    pub created_at: DateTime<Utc>,
}

impl ToolCall {
    /// Records a model-requested tool call in `Pending` status.
    ///
    /// Rejects: blank tool name, null input.
    pub fn new(
        run_id: AgentRunId,
        tool_name: String,
        input: Value,
        requires_approval: bool,
    ) -> Result<Self, DomainError> {
        let tool_name = tool_name.trim().to_owned();
        if tool_name.is_empty() {
            return Err(DomainError::Validation(
                "tool name must not be blank".to_string(),
            ));
        }
        if input.is_null() {
            return Err(DomainError::Validation(
                "tool input must not be null".to_string(),
            ));
        }
        Ok(Self {
            call_id: ToolCallId::new(),
            run_id,
            tool_name,
            input,
            status: ToolCallStatus::Pending,
            requires_approval,
            output: None,
            created_at: Utc::now(),
        })
    }

    /// Approves a pending call (human approval gate).
    pub fn approve(&mut self) -> Result<(), DomainError> {
        if self.status != ToolCallStatus::Pending {
            return Err(DomainError::Validation(format!(
                "only a pending tool call can be approved (status: {:?})",
                self.status
            )));
        }
        self.status = ToolCallStatus::Approved;
        Ok(())
    }

    /// Denies a pending call (human approval gate). Terminal.
    pub fn deny(&mut self, reason: &str) -> Result<(), DomainError> {
        if self.status != ToolCallStatus::Pending {
            return Err(DomainError::Validation(format!(
                "only a pending tool call can be denied (status: {:?})",
                self.status
            )));
        }
        if reason.trim().is_empty() {
            return Err(DomainError::Validation(
                "denial reason must not be blank".to_string(),
            ));
        }
        self.status = ToolCallStatus::Denied;
        Ok(())
    }

    /// Marks an approved call executed, recording its output.
    pub fn mark_executed(&mut self, output: Value) -> Result<(), DomainError> {
        if self.status != ToolCallStatus::Approved {
            return Err(DomainError::Validation(format!(
                "only an approved tool call can execute (status: {:?})",
                self.status
            )));
        }
        self.status = ToolCallStatus::Executed;
        self.output = Some(output);
        Ok(())
    }

    /// Marks an approved call failed. Terminal.
    pub fn mark_failed(&mut self) -> Result<(), DomainError> {
        if self.status != ToolCallStatus::Approved {
            return Err(DomainError::Validation(format!(
                "only an approved tool call can fail (status: {:?})",
                self.status
            )));
        }
        self.status = ToolCallStatus::Failed;
        Ok(())
    }
}

/// Decision of a human approval gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApprovalDecision {
    /// The human approved the pending action.
    Approved,
    /// The human denied the pending action.
    Denied,
}

/// Well-known role whose holders may approve agent runs.
///
/// The role is resolved to the `agent_run`/`approve` (or `manage`)
/// grant covering the run's client profile: either a profile-scoped
/// grant on that exact profile or an agency-scoped grant for the
/// profile's agency. The service layer checks membership through
/// [`crate::services::RoleMembership`] before recording a decision.
pub const APPROVER_ROLE: &str = "agent_approver";

/// A human-in-the-loop approval gate for one pending action.
///
/// The gate names who must decide (a staff actor or a role); the
/// decision, once recorded, is immutable — there is no "undecide".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalGate {
    /// Staff actor that must decide, if the gate is person-specific.
    pub approver: Option<ActorId>,
    /// Role that may decide, when the gate is role-based.
    pub approver_role: Option<String>,
    /// The decision, once made. Immutable afterwards.
    pub decision: Option<ApprovalDecision>,
    /// Reason recorded with the decision (required on deny).
    pub reason: Option<String>,
    /// Who recorded the decision, for later review.
    pub decided_by: Option<ActorId>,
    /// When the decision was recorded.
    pub decided_at: Option<DateTime<Utc>>,
    /// When the gate was created.
    pub created_at: DateTime<Utc>,
}

impl ApprovalGate {
    /// Opens a gate for a specific staff approver.
    pub fn for_actor(approver: ActorId) -> Self {
        Self {
            approver: Some(approver),
            approver_role: None,
            decision: None,
            reason: None,
            decided_by: None,
            decided_at: None,
            created_at: Utc::now(),
        }
    }

    /// Opens a gate for anyone holding a role.
    pub fn for_role(role: String) -> Result<Self, DomainError> {
        let role = role.trim().to_owned();
        if role.is_empty() {
            return Err(DomainError::Validation(
                "approver role must not be blank".to_string(),
            ));
        }
        Ok(Self {
            approver: None,
            approver_role: Some(role),
            decision: None,
            reason: None,
            decided_by: None,
            decided_at: None,
            created_at: Utc::now(),
        })
    }

    /// Records the human's decision. Immutable: a second call is
    /// rejected, so an approval can never be silently reversed.
    ///
    /// The decider must be the gate's named approver when the gate is
    /// person-specific; role-based gates record whoever decided (role
    /// membership is checked by the service layer). The decider and the
    /// decision time are recorded for later review.
    pub fn decide(
        &mut self,
        decider: ActorId,
        decision: ApprovalDecision,
        reason: String,
    ) -> Result<(), DomainError> {
        if self.decision.is_some() {
            return Err(DomainError::Validation(
                "approval decision is immutable once made".to_string(),
            ));
        }
        if let Some(required) = &self.approver {
            if required != &decider {
                return Err(DomainError::Unauthorized);
            }
        }
        let reason = reason.trim().to_owned();
        if decision == ApprovalDecision::Denied && reason.is_empty() {
            return Err(DomainError::Validation(
                "denial requires a reason".to_string(),
            ));
        }
        self.decision = Some(decision);
        self.reason = if reason.is_empty() {
            None
        } else {
            Some(reason)
        };
        self.decided_by = Some(decider);
        self.decided_at = Some(Utc::now());
        Ok(())
    }
}

/// Agent-to-agent context transfer.
///
/// When a run hands work to another agent kind, the handoff carries the
/// context the next agent needs. The context is opaque JSON; the
/// receiving agent validates it against its own input contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    /// Stable identifier of this handoff.
    pub handoff_id: HandoffId,
    /// The run handing work off.
    pub from_run_id: AgentRunId,
    /// Which agent kind receives the work.
    pub to_agent_kind: String,
    /// Context for the receiving agent (opaque JSON, never null).
    pub context: Value,
    /// When the handoff was recorded.
    pub created_at: DateTime<Utc>,
}

impl Handoff {
    /// Records a handoff.
    ///
    /// Rejects: blank `to_agent_kind`, null context.
    pub fn new(
        from_run_id: AgentRunId,
        to_agent_kind: String,
        context: Value,
    ) -> Result<Self, DomainError> {
        let to_agent_kind = to_agent_kind.trim().to_owned();
        if to_agent_kind.is_empty() {
            return Err(DomainError::Validation(
                "handoff target agent kind must not be blank".to_string(),
            ));
        }
        if context.is_null() {
            return Err(DomainError::Validation(
                "handoff context must not be null".to_string(),
            ));
        }
        Ok(Self {
            handoff_id: HandoffId::new(),
            from_run_id,
            to_agent_kind,
            context,
            created_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pending_run() -> AgentRun {
        AgentRun::new(
            ClientProfileId::new(),
            "triage".to_string(),
            FallbackChain::new(vec!["openrouter".to_string()]).expect("valid chain"),
            json!({"task": "triage"}),
        )
        .expect("valid run")
    }

    #[test]
    fn run_validates_inputs() {
        let profile = ClientProfileId::new();
        let chain = FallbackChain::new(vec!["x".to_string()]).expect("valid chain");
        assert!(AgentRun::new(profile, "  ".to_string(), chain.clone(), json!({})).is_err());
        assert!(AgentRun::new(profile, "x".to_string(), chain, Value::Null).is_err());
    }

    #[test]
    fn run_happy_path() {
        let mut run = pending_run();
        run.start().expect("start");
        assert_eq!(run.status, AgentRunStatus::Running);
        run.complete(json!({"done": true})).expect("complete");
        assert_eq!(run.status, AgentRunStatus::Completed);
        assert!(run.output.is_some());
        assert!(run.status.is_terminal());
    }

    #[test]
    fn run_approval_flow() {
        let mut run = pending_run();
        run.start().expect("start");
        run.request_approval().expect("request approval");
        assert_eq!(run.status, AgentRunStatus::AwaitingApproval);
        // Illegal moves from AwaitingApproval.
        assert!(run.start().is_err());
        assert!(run.complete(json!({})).is_err());
        run.approve().expect("approve");
        assert_eq!(run.status, AgentRunStatus::Running);
        run.request_approval().expect("request again");
        run.reject().expect("reject");
        assert_eq!(run.status, AgentRunStatus::Failed);
        assert!(run.status.is_terminal());
    }

    #[test]
    fn run_forward_only() {
        let mut run = pending_run();
        // Cannot skip Pending.
        assert!(run.complete(json!({})).is_err());
        assert!(run.request_approval().is_err());
        run.start().expect("start");
        // Cannot go back to Pending.
        assert!(run.start().is_err());
        run.fail().expect("fail from running");
        assert_eq!(run.status, AgentRunStatus::Failed);
        // Terminal: nothing moves.
        assert!(run.cancel().is_err());
        assert!(run.start().is_err());
    }

    #[test]
    fn run_cancel_from_any_non_terminal() {
        // From Pending.
        let mut run = pending_run();
        run.cancel().expect("cancel");
        assert_eq!(run.status, AgentRunStatus::Cancelled);
        // Idempotent.
        run.cancel().expect("cancel idempotent");

        // From Running.
        let mut run = pending_run();
        run.start().expect("start");
        run.cancel().expect("cancel");
        assert_eq!(run.status, AgentRunStatus::Cancelled);

        // From AwaitingApproval.
        let mut run = pending_run();
        run.start().expect("start");
        run.request_approval().expect("request approval");
        run.cancel().expect("cancel");
        assert_eq!(run.status, AgentRunStatus::Cancelled);
    }

    #[test]
    fn fallback_chain_orders_and_exhausts() {
        let chain = FallbackChain::new(vec![
            "openrouter".to_string(),
            "local".to_string(),
            "openrouter".to_string(),
        ])
        .expect("valid chain");
        assert_eq!(chain.adapters, vec!["openrouter", "local"]);
        assert_eq!(chain.primary(), "openrouter");
        assert_eq!(chain.next_after("openrouter"), Some("local"));
        assert_eq!(chain.next_after("local"), None);
        assert_eq!(chain.next_after("unknown"), None);
        assert!(FallbackChain::new(vec![]).is_err());
        assert!(FallbackChain::new(vec!["  ".to_string()]).is_err());
    }

    #[test]
    fn tool_call_approval_lifecycle() {
        let mut call = ToolCall::new(
            AgentRunId::new(),
            "crm.create_contact".to_string(),
            json!({"email": "a@example.com"}),
            true,
        )
        .expect("valid call");
        assert_eq!(call.status, ToolCallStatus::Pending);
        // Cannot execute without approval.
        assert!(call.mark_executed(json!({})).is_err());
        call.approve().expect("approve");
        assert_eq!(call.status, ToolCallStatus::Approved);
        // Approval is one-way.
        assert!(call.approve().is_err());
        assert!(call.deny("no").is_err());
        call.mark_executed(json!({"id": 1})).expect("execute");
        assert_eq!(call.status, ToolCallStatus::Executed);
        assert!(call.status.is_terminal());
    }

    #[test]
    fn tool_call_deny_is_terminal() {
        let mut call = ToolCall::new(
            AgentRunId::new(),
            "crm.delete_contact".to_string(),
            json!({}),
            true,
        )
        .expect("valid call");
        assert!(call.deny("  ").is_err());
        call.deny("too risky").expect("deny");
        assert_eq!(call.status, ToolCallStatus::Denied);
        assert!(call.status.is_terminal());
        assert!(call.approve().is_err());
    }

    #[test]
    fn tool_validates_schema_shape() {
        assert!(AgentTool::new("t".to_string(), "d".to_string(), Value::Null, false).is_err());
        assert!(AgentTool::new("t".to_string(), "d".to_string(), json!([1, 2]), false).is_err());
    }

    #[test]
    fn approval_gate_decision_is_immutable() {
        let approver = ActorId::new();
        let mut gate = ApprovalGate::for_actor(approver);
        assert!(gate.decision.is_none());
        // Wrong actor cannot decide a person-specific gate.
        assert!(gate
            .decide(ActorId::new(), ApprovalDecision::Approved, String::new())
            .is_err());
        gate.decide(approver, ApprovalDecision::Approved, String::new())
            .expect("approve");
        assert_eq!(gate.decision, Some(ApprovalDecision::Approved));
        assert_eq!(gate.decided_by, Some(approver));
        assert!(gate.decided_at.is_some());
        // Second decision rejected — approvals cannot be reversed.
        assert!(gate
            .decide(
                approver,
                ApprovalDecision::Denied,
                "changed mind".to_string()
            )
            .is_err());

        let mut role_gate = ApprovalGate::for_role("manager".to_string()).expect("role gate");
        assert!(role_gate
            .decide(ActorId::new(), ApprovalDecision::Denied, String::new())
            .is_err());
        let decider = ActorId::new();
        role_gate
            .decide(decider, ApprovalDecision::Denied, "not now".to_string())
            .expect("deny with reason");
        assert_eq!(role_gate.decided_by, Some(decider));
        assert!(ApprovalGate::for_role("  ".to_string()).is_err());
    }

    #[test]
    fn handoff_validates() {
        let run_id = AgentRunId::new();
        assert!(Handoff::new(run_id, "  ".to_string(), json!({"a": 1})).is_err());
        assert!(Handoff::new(run_id, "drafter".to_string(), Value::Null).is_err());
        let handoff =
            Handoff::new(run_id, "drafter".to_string(), json!({"a": 1})).expect("valid handoff");
        assert_eq!(handoff.to_agent_kind, "drafter");
    }
}
