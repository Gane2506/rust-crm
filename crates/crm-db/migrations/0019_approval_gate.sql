-- SPDX-License-Identifier: GPL-3.0-only
-- 0019_approval_gate: persist Jev approval gate decisions (task FIX-04).
--
-- The `ApprovalGate` decision (who decided, when, approve/deny, reason)
-- was built and decided in memory but never persisted — `decide_gate`
-- dropped the gate after recording the decision. The gate now lives on
-- the `AgentRun` as JSONB, so `decided_by`/`decided_at` survive and are
-- visible on run reads.
--
-- Additive only; re-runnable.

ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS approval_gate JSONB;
