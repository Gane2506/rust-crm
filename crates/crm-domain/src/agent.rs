// SPDX-License-Identifier: GPL-3.0-only
//! Agent action records: delegated authority, authorization and outcome.
//!
//! Every tool call an AI agent makes is recorded here: who the principal
//! was, under which delegated grant it acted (and that grant's ceiling),
//! what it called with (as a hash — never the raw input, which may carry
//! PII or secrets), the authorization decision, and the outcome. Human
//! approval is a durable [`ActionOutcome::AwaitingApproval`] state, not a
//! transient prompt.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::PermissionDecision;
use crate::ids::{AgentActionId, ClientProfileId};

/// Outcome of an agent action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActionOutcome {
    /// The tool call started.
    Started,
    /// The tool call completed successfully.
    Succeeded,
    /// The tool call failed.
    Failed,
    /// The call was denied authorization and never ran.
    Denied,
    /// Durably paused waiting for human approval.
    AwaitingApproval,
}

/// Record of one agent tool-call action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAction {
    /// The workflow/agent run this action belongs to.
    pub run_id: Uuid,
    /// Stable identifier of this action record.
    pub action_id: AgentActionId,
    /// The agent principal that initiated the action.
    pub principal_agent_id: String,
    /// The delegated grant the agent acted under, if any. The grant's
    /// ceiling bounds what the agent may do; absence means no delegated
    /// authority (deny by default).
    pub delegated_grant_id: Option<String>,
    /// Client-profile scope of the action, if any.
    pub client_profile_id: Option<ClientProfileId>,
    /// Name of the tool that was (or would be) called.
    pub tool_name: String,
    /// SHA-256 of the canonical tool input. The raw input is never stored
    /// here — it may contain PII or secrets.
    pub input_hash: [u8; 32],
    /// Whether the action was authorized.
    pub authorization_decision: PermissionDecision,
    /// The human approval record, when approval was required.
    pub approval_id: Option<String>,
    /// Idempotency key: retries with the same key must not double-apply
    /// side effects.
    pub idempotency_key: String,
    /// Current outcome of the action.
    pub outcome: ActionOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_action() -> AgentAction {
        AgentAction {
            run_id: Uuid::from_u128(100),
            action_id: AgentActionId::from(Uuid::from_u128(101)),
            principal_agent_id: "agent-orchestrator".to_string(),
            delegated_grant_id: Some("grant-7".to_string()),
            client_profile_id: Some(ClientProfileId::from(Uuid::from_u128(102))),
            tool_name: "send_email".to_string(),
            input_hash: [3u8; 32],
            authorization_decision: PermissionDecision::Allow,
            approval_id: None,
            idempotency_key: "idem-1".to_string(),
            outcome: ActionOutcome::Started,
        }
    }

    #[test]
    fn action_serializes_without_raw_input() {
        // The raw tool input is never part of the record — only its hash.
        let json = serde_json::to_string(&test_action()).expect("serialize");
        assert!(json.contains("send_email"));
        assert!(!json.contains("input\""));
    }

    #[test]
    fn denied_action_carries_decision_and_outcome() {
        let mut action = test_action();
        action.authorization_decision = PermissionDecision::Deny;
        action.outcome = ActionOutcome::Denied;
        assert_eq!(action.authorization_decision, PermissionDecision::Deny);
        assert_eq!(action.outcome, ActionOutcome::Denied);
    }
}
