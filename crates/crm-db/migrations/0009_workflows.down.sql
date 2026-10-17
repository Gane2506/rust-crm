-- SPDX-License-Identifier: GPL-3.0-only
-- 0009_workflows.down: roll back the Workflow builder vertical slice.
--
-- Drops in reverse dependency order: nodes, edges, triggers and
-- versions reference workflows.

DROP TABLE IF EXISTS workflow_versions;
DROP TABLE IF EXISTS workflow_triggers;
DROP TABLE IF EXISTS workflow_edges;
DROP TABLE IF EXISTS workflow_nodes;
DROP TABLE IF EXISTS workflows;
