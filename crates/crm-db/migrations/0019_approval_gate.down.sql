-- SPDX-License-Identifier: GPL-3.0-only
-- 0019_approval_gate down: remove the approval_gate column.

ALTER TABLE agent_runs
    DROP COLUMN IF EXISTS approval_gate;
