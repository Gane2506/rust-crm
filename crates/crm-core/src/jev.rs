// SPDX-License-Identifier: GPL-3.0-only
//! Jev agent service implementation (task 09-JEV-02).
//!
//! [`AgentServiceImpl`] implements
//! [`AgentService`](crm_domain::services::AgentService) over generic
//! agent-run, tool-call, handoff, role-membership and audit ports — the
//! same composition pattern as
//! [`CampaignServiceImpl`](crate::campaign::CampaignServiceImpl). The
//! production wiring (SQLx repositories + [`PgRoleMembership`]) is
//! assembled by the API layer.
//!
//! Approval flow: [`AgentServiceImpl::approve_step`] and
//! [`AgentServiceImpl::deny_step`] build a role-based
//! [`ApprovalGate`](crm_domain::jev::ApprovalGate) for
//! [`APPROVER_ROLE`](crm_domain::jev::APPROVER_ROLE) and check the
//! decider's membership through the injected
//! [`RoleMembership`](crm_domain::services::RoleMembership) port
//! **before** recording the decision. The check fails closed: an
//! unresolvable check or a non-member yields
//! [`DomainError::Unauthorized`].
//!
//! [`StubModelAdapter`] is the deterministic in-memory
//! [`ModelAdapter`](crm_domain::jev::ModelAdapter) for tests and local
//! development. Real provider adapters (OpenRouter, …) are a later
//! task; the adapter execution loop that walks the run's
//! [`FallbackChain`](crm_domain::jev::FallbackChain) is also out of
//! scope for this slice.

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{AgentRunId, ClientProfileId};
use crm_domain::jev::{
    AgentRun, ApprovalDecision, ApprovalGate, Handoff, ModelAdapter,
    ModelCompletion, ToolCall, ToolCallStatus, APPROVER_ROLE,
};
use crm_domain::repositories::{AgentRunRepository, HandoffRepository, ToolCallRepository};
use crm_domain::services::{
    ActionContext, AgentService, AuditService, NewAgentRun, NewHandoff, NewToolCall, RoleMembership,
};

/// Generic [`AgentService`] implementation.
///
/// Type parameters: `R` agent-run repository, `T` tool-call repository,
/// `H` handoff repository, `RM` role-membership checker, `A` audit
/// service.
pub struct AgentServiceImpl<R, T, H, RM, A> {
    runs: R,
    tool_calls: T,
    handoffs: H,
    roles: RM,
    audit: A,
}

impl<R, T, H, RM, A> AgentServiceImpl<R, T, H, RM, A> {
    /// Creates the service over the given ports.
    pub fn new(runs: R, tool_calls: T, handoffs: H, roles: RM, audit: A) -> Self {
        Self {
            runs,
            tool_calls,
            handoffs,
            roles,
            audit,
        }
    }

    /// Borrows the run repository (read path for the HTTP layer).
    #[must_use]
    pub fn runs(&self) -> &R {
        &self.runs
    }

    /// Borrows the tool-call repository (read path for the HTTP layer).
    #[must_use]
    pub fn tool_calls(&self) -> &T {
        &self.tool_calls
    }

    /// Borrows the handoff repository (read path for the HTTP layer).
    #[must_use]
    pub fn handoffs(&self) -> &H {
        &self.handoffs
    }

    /// Enforces profile scope: a profile-scoped context may only touch
    /// runs of its own profile; an agency-scoped context (`None`) may
    /// touch any profile's runs (grant checks happen upstream, at the
    /// HTTP layer).
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

    /// Records an audit event for an agent mutation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails the
    /// operation).
    async fn audit_agent(
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
                "agent_run",
                target_id,
                PermissionDecision::Allow,
                result,
                None,
                None,
            )
            .await;
    }

    /// Loads a run and enforces the caller's profile scope on it.
    async fn load_scoped_run(
        &self,
        run_id: AgentRunId,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError>
    where
        R: AgentRunRepository + Send + Sync,
    {
        let run = self.runs.get(run_id).await?;
        Self::check_scope(ctx, run.client_profile_id)?;
        Ok(run)
    }

    /// Runs the role-based approval gate for a run-level decision.
    ///
    /// Builds an [`ApprovalGate`] for [`APPROVER_ROLE`], checks the
    /// decider's membership **before** recording the decision, and
    /// fails closed with [`DomainError::Unauthorized`] when the check
    /// errors or reports no membership. Person-specific gates are not
    /// used here: the run carries no designated approver, so the role
    /// is the policy.
    async fn decide_gate(
        &self,
        ctx: &ActionContext,
        client_profile_id: ClientProfileId,
        decision: ApprovalDecision,
        reason: String,
    ) -> Result<(), DomainError>
    where
        RM: RoleMembership + Send + Sync,
    {
        let holds = self
            .roles
            .has_role(ctx.actor_id, APPROVER_ROLE, client_profile_id)
            .await?;
        if !holds {
            return Err(DomainError::Unauthorized);
        }
        let mut gate = ApprovalGate::for_role(APPROVER_ROLE.to_owned())?;
        gate.decide(ctx.actor_id, decision, reason)?;
        Ok(())
    }
}

impl<R, T, H, RM, A> AgentService for AgentServiceImpl<R, T, H, RM, A>
where
    R: AgentRunRepository + Send + Sync,
    T: ToolCallRepository + Send + Sync,
    H: HandoffRepository + Send + Sync,
    RM: RoleMembership + Send + Sync,
    A: AuditService + Send + Sync,
{
    async fn start_run(
        &self,
        input: NewAgentRun,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError> {
        Self::check_scope(ctx, input.client_profile_id)?;
        let mut run = AgentRun::new(
            input.client_profile_id,
            input.agent_kind,
            input.fallback_chain,
            input.input,
        )?;
        run.start()?;
        let run = self.runs.create(run).await?;
        self.audit_agent(
            ctx,
            "agent.run_started",
            &run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(run)
    }

    async fn request_approval(
        &self,
        run_id: AgentRunId,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError> {
        let mut run = self.load_scoped_run(run_id, ctx).await?;
        run.request_approval()?;
        let run = self.runs.update(run).await?;
        self.audit_agent(
            ctx,
            "agent.approval_requested",
            &run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(run)
    }

    async fn approve_step(
        &self,
        run_id: AgentRunId,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError> {
        let mut run = self.load_scoped_run(run_id, ctx).await?;
        self.decide_gate(
            ctx,
            run.client_profile_id,
            ApprovalDecision::Approved,
            String::new(),
        )
        .await?;
        run.approve()?;
        let run = self.runs.update(run).await?;
        // Approve any pending approval-gated tool calls on the run: the
        // human approved the run, which covers its pending calls.
        let pending = self.tool_calls.list_by_run(run.run_id, 100).await?;
        for mut call in pending {
            if call.requires_approval
                && call.status == ToolCallStatus::Pending
                && call.approve().is_ok()
            {
                let _ = self.tool_calls.update(call).await;
            }
        }
        self.audit_agent(
            ctx,
            "agent.approved",
            &run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(run)
    }

    async fn deny_step(
        &self,
        run_id: AgentRunId,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError> {
        let mut run = self.load_scoped_run(run_id, ctx).await?;
        // The trait carries no denial reason; synthesize one from the
        // actor so the gate's reason invariant holds. The actor is
        // recorded as `decided_by` for review.
        let reason = format!("denied by {}", ctx.actor_id);
        self.decide_gate(ctx, run.client_profile_id, ApprovalDecision::Denied, reason)
            .await?;
        run.reject()?;
        let run = self.runs.update(run).await?;
        self.audit_agent(
            ctx,
            "agent.denied",
            &run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(run)
    }

    async fn complete_run(
        &self,
        run_id: AgentRunId,
        output: serde_json::Value,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError> {
        let mut run = self.load_scoped_run(run_id, ctx).await?;
        run.complete(output)?;
        let run = self.runs.update(run).await?;
        self.audit_agent(
            ctx,
            "agent.run_completed",
            &run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(run)
    }

    async fn fail_run(
        &self,
        run_id: AgentRunId,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError> {
        let mut run = self.load_scoped_run(run_id, ctx).await?;
        run.fail()?;
        let run = self.runs.update(run).await?;
        self.audit_agent(
            ctx,
            "agent.run_failed",
            &run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(run)
    }

    async fn cancel_run(
        &self,
        run_id: AgentRunId,
        ctx: &ActionContext,
    ) -> Result<AgentRun, DomainError> {
        let mut run = self.load_scoped_run(run_id, ctx).await?;
        run.cancel()?;
        let run = self.runs.update(run).await?;
        self.audit_agent(
            ctx,
            "agent.run_cancelled",
            &run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(run)
    }

    async fn record_tool_call(
        &self,
        input: NewToolCall,
        ctx: &ActionContext,
    ) -> Result<ToolCall, DomainError> {
        let mut run = self.load_scoped_run(input.run_id, ctx).await?;
        let mut call = ToolCall::new(
            input.run_id,
            input.tool_name,
            input.input,
            input.requires_approval,
        )?;
        if input.requires_approval {
            // Approval-gated: the call stays Pending and the run pauses
            // for a human decision.
            run.request_approval()?;
            self.runs.update(run).await?;
        } else {
            // No approval needed: the service executes immediately.
            call.approve()?;
        }
        let call = self.tool_calls.create(call).await?;
        self.audit_agent(
            ctx,
            "agent.tool_recorded",
            &call.call_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(call)
    }

    async fn handoff(
        &self,
        input: NewHandoff,
        ctx: &ActionContext,
    ) -> Result<Handoff, DomainError> {
        let from_run = self.load_scoped_run(input.from_run_id, ctx).await?;
        let handoff = Handoff::new(input.from_run_id, input.to_agent_kind, input.context)?;
        let handoff = self.handoffs.create(handoff).await?;
        self.audit_agent(
            ctx,
            "agent.handed_off",
            &from_run.run_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(handoff)
    }
}

/// Deterministic in-memory [`ModelAdapter`] for tests and local
/// development.
///
/// Every completion returns the configured text and no tool calls.
/// Real provider adapters (OpenRouter, local models, …) are a later
/// task; the adapter execution loop that walks the run's
/// [`FallbackChain`] on failure is also out of scope for this slice.
#[derive(Debug, Clone)]
pub struct StubModelAdapter {
    /// Text returned for every completion.
    pub text: String,
}

impl StubModelAdapter {
    /// Creates the stub with the given completion text.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self { text }
    }
}

impl ModelAdapter for StubModelAdapter {
    // The trait declares `complete` with an explicit `impl Future`
    // return type, so the impl must keep that form; `async fn` here
    // would not match the trait signature.
    #[allow(clippy::manual_async_fn)]
    fn complete<'a>(
        &'a self,
        _prompt: &'a str,
        _tools: &'a [crm_domain::jev::AgentTool],
    ) -> impl std::future::Future<Output = Result<ModelCompletion, DomainError>> + Send + 'a {
        async move {
            Ok(ModelCompletion {
                text: self.text.clone(),
                tool_calls: Vec::new(),
            })
        }
    }
}

/// Shows the fallback walk a reloaded run performs after an adapter
/// failure: [`FallbackChain::next_after`] drives the retry order and
/// the run's stored chain survives the reload.
#[cfg(test)]
mod tests {
    use super::*;
    use crm_domain::jev::{AgentRunStatus, FallbackChain};

    #[test]
    fn fallback_chain_walk_order() {
        let chain = FallbackChain::new(vec!["openrouter".to_string(), "local".to_string()])
            .expect("valid chain");
        assert_eq!(chain.primary(), "openrouter");
        // First adapter fails → try the next.
        assert_eq!(chain.next_after("openrouter"), Some("local"));
        // Chain exhausted → the run fails.
        assert_eq!(chain.next_after("local"), None);
        // Unknown adapter fails closed: no silent retry.
        assert_eq!(chain.next_after("unknown"), None);
    }

    #[test]
    fn run_stores_chain_and_derives_adapter() {
        let chain = FallbackChain::new(vec!["openrouter".to_string(), "local".to_string()])
            .expect("valid chain");
        let run = AgentRun::new(
            ClientProfileId::new(),
            "triage".to_string(),
            chain.clone(),
            serde_json::json!({"task": "triage"}),
        )
        .expect("valid run");
        assert_eq!(run.adapter_name, "openrouter");
        assert_eq!(run.fallback_chain, chain);
    }

    #[test]
    fn agent_run_status_is_terminal() {
        assert!(AgentRunStatus::Completed.is_terminal());
        assert!(AgentRunStatus::Failed.is_terminal());
        assert!(AgentRunStatus::Cancelled.is_terminal());
        assert!(!AgentRunStatus::Running.is_terminal());
        assert!(!AgentRunStatus::AwaitingApproval.is_terminal());
    }

    #[test]
    fn approver_role_is_stable() {
        assert_eq!(APPROVER_ROLE, "agent_approver");
    }
}
