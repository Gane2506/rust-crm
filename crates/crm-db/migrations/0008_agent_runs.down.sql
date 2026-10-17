-- SPDX-License-Identifier: GPL-3.0-only
-- 0008_agent_runs.down: roll back the Jev agents vertical slice.
--
-- Drops in reverse dependency order: handoffs and tool_calls reference
-- agent_runs.

DROP TABLE IF EXISTS handoffs;
DROP TABLE IF EXISTS tool_calls;
DROP TABLE IF EXISTS agent_runs;
