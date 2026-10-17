-- SPDX-License-Identifier: GPL-3.0-only
-- 0008_agent_runs: Jev agents vertical slice (task 09-JEV-02).
--
-- * `agent_runs` holds the agent-run aggregate: owning client profile,
--   agent kind, lifecycle status, the selected model-adapter name, the
--   stored fallback chain (JSONB, so a reloaded run can walk the chain
--   after an adapter failure), input/output payloads and timestamps.
-- * `tool_calls` holds model-requested tool invocations: the owning
--   run, tool name, input/output JSONB, approval-gated status and the
--   `requires_approval` flag captured at record time.
-- * `handoffs` is append-only: agent-to-agent context transfers. No
--   UPDATE or DELETE path exists.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS agent_runs (
    run_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    agent_kind TEXT NOT NULL,
    status TEXT NOT NULL,
    adapter_name TEXT NOT NULL,
    fallback_chain JSONB NOT NULL,
    input JSONB NOT NULL,
    output JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped run reads, newest first; plus status-filtered reads.
CREATE INDEX IF NOT EXISTS agent_runs_client_profile_id_idx
    ON agent_runs (client_profile_id, created_at DESC);

CREATE INDEX IF NOT EXISTS agent_runs_status_idx
    ON agent_runs (client_profile_id, status, created_at DESC);

CREATE TABLE IF NOT EXISTS tool_calls (
    call_id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES agent_runs (run_id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    input JSONB NOT NULL,
    status TEXT NOT NULL,
    output JSONB,
    requires_approval BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tool calls for one run, oldest first (execution order).
CREATE INDEX IF NOT EXISTS tool_calls_run_id_idx
    ON tool_calls (run_id, created_at ASC);

CREATE TABLE IF NOT EXISTS handoffs (
    handoff_id UUID PRIMARY KEY,
    from_run_id UUID NOT NULL REFERENCES agent_runs (run_id) ON DELETE CASCADE,
    to_agent_kind TEXT NOT NULL,
    context JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Handoffs out of one run, newest first.
CREATE INDEX IF NOT EXISTS handoffs_from_run_id_idx
    ON handoffs (from_run_id, created_at DESC);
