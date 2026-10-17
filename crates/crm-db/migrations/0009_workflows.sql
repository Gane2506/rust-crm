-- SPDX-License-Identifier: GPL-3.0-only
-- 0009_workflows: Workflow builder vertical slice (task 10-WORK-02).
--
-- * `workflows` holds the workflow aggregate: owning client profile,
--   name, lifecycle status (draft/published/archived) and timestamps.
-- * `workflow_nodes` holds the typed canvas nodes: the owning workflow,
--   node type, opaque config JSONB and canvas position.
-- * `workflow_edges` holds typed connections between nodes: the owning
--   workflow, from/to node references and an optional label. Edges are
--   cheap to delete and recreate; there is no UPDATE path.
-- * `workflow_triggers` holds the trigger that starts a workflow's
--   runs. Exactly one trigger per workflow, enforced by the UNIQUE
--   constraint on `workflow_id`; setting a trigger replaces the
--   existing one.
-- * `workflow_versions` is append-only: immutable snapshots taken at
--   publish time, keyed by (workflow_id, version_number). No UPDATE or
--   DELETE path exists.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS workflows (
    workflow_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped workflow reads, newest first.
CREATE INDEX IF NOT EXISTS workflows_client_profile_id_idx
    ON workflows (client_profile_id, created_at DESC);

CREATE TABLE IF NOT EXISTS workflow_nodes (
    node_id UUID PRIMARY KEY,
    workflow_id UUID NOT NULL REFERENCES workflows (workflow_id) ON DELETE CASCADE,
    node_type TEXT NOT NULL,
    config JSONB NOT NULL,
    pos_x INTEGER NOT NULL,
    pos_y INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Nodes of one workflow (canvas order is client-side; creation order
-- is a stable default).
CREATE INDEX IF NOT EXISTS workflow_nodes_workflow_id_idx
    ON workflow_nodes (workflow_id, created_at ASC);

CREATE TABLE IF NOT EXISTS workflow_edges (
    edge_id UUID PRIMARY KEY,
    workflow_id UUID NOT NULL REFERENCES workflows (workflow_id) ON DELETE CASCADE,
    from_node UUID NOT NULL,
    to_node UUID NOT NULL,
    label TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Edges of one workflow, oldest first (stable traversal order).
CREATE INDEX IF NOT EXISTS workflow_edges_workflow_id_idx
    ON workflow_edges (workflow_id, created_at ASC);

CREATE TABLE IF NOT EXISTS workflow_triggers (
    trigger_id UUID PRIMARY KEY,
    workflow_id UUID NOT NULL UNIQUE REFERENCES workflows (workflow_id) ON DELETE CASCADE,
    trigger_type TEXT NOT NULL,
    config JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS workflow_versions (
    version_id UUID PRIMARY KEY,
    workflow_id UUID NOT NULL REFERENCES workflows (workflow_id) ON DELETE CASCADE,
    version_number INTEGER NOT NULL CHECK (version_number > 0),
    snapshot JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workflow_id, version_number)
);

-- Versions of one workflow, newest first.
CREATE INDEX IF NOT EXISTS workflow_versions_workflow_id_idx
    ON workflow_versions (workflow_id, version_number DESC);
