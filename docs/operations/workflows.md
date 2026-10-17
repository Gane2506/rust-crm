# Workflow builder operations runbook

How the workflow builder vertical slice (tasks 10-WORK-01 through
10-WORK-03) works in production: typed visual workflows, triggers,
immutable versioning, and a pure dry-run simulator. For the auth
model, see `security.md`; for the audit events workflows emit,
`audit-log.md`; for the workflow security posture,
`threat-model.md`.

## What it is

Staff compose automation workflows on a visual canvas: typed nodes
(trigger / action / condition / delay) connected by typed edges,
started by one trigger (event, webhook, schedule or manual). Four
layers:

- **Domain** (`crates/crm-domain/src/workflow.rs`):
  `Workflow` (forward-only lifecycle), `WorkflowNode` (typed,
  canvas-positioned), `WorkflowEdge` (no self-loops),
  `Trigger` (exactly one per workflow), `WorkflowVersion`
  (immutable snapshot — no mutators at all), `Simulator` (pure
  dry-run graph walk that never executes actions), and the
  `WorkflowService` / repository traits.
- **Persistence** (`crates/crm-db`, migration `0009_workflows`):
  `workflows`, `workflow_nodes`, `workflow_edges`,
  `workflow_triggers` (a `UNIQUE(workflow_id)` constraint
  enforces one trigger per workflow at the database level),
  `workflow_versions` (append-only — no update/delete).
- **Service** (`crates/crm-core/src/workflow.rs`):
  `WorkflowServiceImpl` over the workflow / node / edge /
  trigger / version / audit ports. `publish_workflow` freezes an
  immutable version snapshot (`version_number = max + 1`);
  `remove_node` deletes dangling edges first;
  `set_trigger` upserts (replaces) the workflow's single
  trigger; `simulate` runs the pure `Simulator`.
- **HTTP** (`crates/crm-api/src/workflows.rs`): 15 endpoints on
  11 route paths under `/api/v1` — workflow CRUD plus
  publish/archive, node add/list/remove, edge add/list/remove,
  trigger set/get, simulate, and version list.

Every workflow belongs to one client profile — the tenant
boundary. Authorization uses the `Workflow` resource with
profile-scoped grants (`workflow/create`, `workflow/read`,
`workflow/update`, `workflow/delete`).

## Workflow lifecycle

`POST /api/v1/workflows` (bearer auth, `workflow/create` grant on
the profile). The body carries `client_profile_id` and `name`
(validated non-blank):

```
Draft → Published → Archived (terminal)
```

- `publish()` works only from `Draft` — it also freezes a
  `WorkflowVersion` snapshot of the current nodes, edges and
  trigger (see below).
- `archive()` is idempotent: archiving an already-archived
  workflow succeeds without changing it.
- `Archived` is terminal — nothing moves out of it.

Endpoints (`{id}` is the workflow UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/workflows` | `workflow/create` | Create a `Draft` workflow |
| `GET /api/v1/workflows?client_profile_id=<uuid>` | `workflow/read` | List workflows for one profile |
| `GET /api/v1/workflows/{id}` | `workflow/read` | Fetch-then-authorize against the workflow's own profile |
| `POST /api/v1/workflows/{id}/publish` | `workflow/update` | `Draft → Published`; freezes a version snapshot |
| `POST /api/v1/workflows/{id}/archive` | `workflow/update` | `Draft`/`Published → Archived` (idempotent) |
| `POST /api/v1/workflows/{id}/nodes` | `workflow/update` | Add a typed node (`node_type`, `config`, `position`) |
| `GET /api/v1/workflows/{id}/nodes` | `workflow/read` | List the workflow's nodes |
| `DELETE /api/v1/workflows/{id}/nodes/{node_id}` | `workflow/delete` | Remove a node and its dangling edges (path ownership verified) |
| `POST /api/v1/workflows/{id}/edges` | `workflow/update` | Add an edge (no self-loops; both endpoints must exist) |
| `GET /api/v1/workflows/{id}/edges` | `workflow/read` | List the workflow's edges |
| `DELETE /api/v1/workflows/{id}/edges/{edge_id}` | `workflow/delete` | Remove an edge |
| `PUT /api/v1/workflows/{id}/trigger` | `workflow/update` | Set (or replace) the workflow's trigger |
| `GET /api/v1/workflows/{id}/trigger` | `workflow/read` | Get the workflow's trigger |
| `POST /api/v1/workflows/{id}/simulate` | `workflow/read` | Dry-run trace (read-only; records an audit event) |
| `GET /api/v1/workflows/{id}/versions` | `workflow/read` | List published versions, newest first |

## Nodes, edges, triggers

**Nodes** are typed: `TriggerNode`, `ActionNode`,
`ConditionNode`, `DelayNode`. Construction rejects a null
`config`, and a `DelayNode` must carry a positive
`duration_secs` number. `position` is display-only canvas
coordinates — the domain places no bounds on them, and
`move_to()` exists for canvas dragging.

**Edges** connect two nodes. The domain rejects self-loops
(`from_node == to_node`); the service additionally validates
that *both endpoints exist* in the workflow before creating the
edge. Edge repositories have no update — edges are cheap to
delete and recreate. Removing a node deletes its dangling edges
first, so no edge ever references a deleted node.

**Triggers** come in four flavors: `EventTrigger` (fires on a
domain event like `contact.created`), `WebhookTrigger` (fires on
an inbound HTTP call), `ScheduleTrigger` (cron-style), and
`ManualTrigger` (staff-started). Exactly one trigger per
workflow: `set_trigger` replaces the existing one (upsert), and
the `UNIQUE(workflow_id)` constraint on `workflow_triggers`
enforces the invariant at the database level. Note: defining a
webhook trigger does **not** create a public ingestion endpoint
today — there is no workflow execution engine yet, so no trigger
can actually fire. See `threat-model.md`.

## Versioning

Publishing freezes an immutable `WorkflowVersion` snapshot: the
workflow's name, nodes, edges and trigger as one JSON document,
with `version_number = max + 1` (starting at 1). Versions are
append-only — the type has no mutators and the repository has no
update/delete, so a published definition can always be
reconstructed exactly. Republishing after edits creates version
2, 3, …; each snapshot is a complete, self-contained definition.

## Simulator

`POST /api/v1/workflows/{id}/simulate` runs the pure
`Simulator::simulate` — a dry-run graph walk that returns an
ordered `SimulationTrace` (one step per reachable node, in
traversal order). It **never executes actions**, never calls
providers, never touches the network: it is a pure function of
the workflow, nodes, edges and input.

Traversal rules:

- starts at the `TriggerNode`; a workflow with no trigger node
  yields an empty trace;
- only nodes and edges belonging to the workflow are
  considered;
- breadth-first; each node is visited at most once (cycles
  terminate);
- nodes unreachable from the trigger are absent;
- `ConditionNode` branches are *followed but not evaluated*
  (marked `Skipped`);
- a `DelayNode` with an unreadable duration is marked
  `Failed` (defensive — construction rejects such configs).

Step statuses: `WouldRun`, `Skipped`, `Failed`. Simulate
authorizes with the `Read` operation — it is read-only — and
records a `workflow.simulated` audit event.

**Simulator output must never be mistaken for execution.** The
trace says what *would* happen; nothing ran. This distinction
is load-bearing for operators and is called out again in
`threat-model.md`.

## Audit trail

The service records audit events with these pinned action names
(see `audit-log.md`):

`workflow.created`, `workflow.published`, `workflow.archived`,
`workflow.node_added`, `workflow.node_removed`,
`workflow.edge_added`, `workflow.edge_removed`,
`workflow.trigger_set`, `workflow.simulated`.

Audit is fail-loud at the HTTP layer and never fails the
operation: a recording failure is swallowed in the service (loud
warn) so workflow work is never blocked by the audit pipeline.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated by
`CRM_SEED_DEV=1`) creates one draft demo workflow
(`"demo-seed-workflow"`) on the demo profile with a manual
trigger node, an action node, a delay node (`duration_secs:
300`), and edges connecting trigger → action → delay — driven
through the real service layer, plus `workflow`
create/read/update/delete grants for the demo staff user:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed workflow is idempotent (skipped when a
`demo-seed-workflow` already exists on the demo profile). It
stays in `Draft` — publishing is the operator's explicit
action, and it snapshots a version. Seeding uses the service —
never raw SQL — so scope checks and audit events behave exactly
as in production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on workflow endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on workflow create/read | No `workflow/create` or `workflow/read` grant on the profile | Have an operator check the `grants` table for `workflow` on that profile scope. Grants are per-profile; cross-profile access is denied by design. |
| `403` on node/edge/trigger writes | No `workflow/update` grant | Same as above, for the `Update` operation. |
| `403` on node/edge removal | No `workflow/delete` grant | Removals use the `Delete` operation, not `Update`. |
| `422` on create | Blank workflow name | Names are validated non-blank. |
| `422` on node add | Null config, or a delay node without a positive `duration_secs` | Fix the config. |
| `422` on edge add | Self-loop, or an endpoint that doesn't exist in the workflow | Pick two distinct existing nodes. |
| `422` on publish | Workflow is not in `Draft` | Only `Draft → Published` is allowed. |
| `422` on trigger set | Null config | Fix the config. |
| `404` on workflow/node/edge/trigger reads | Wrong id, or the row belongs to another profile | Fetch-then-authorize: a missing row is `404` before any grant check. |
| Empty simulate trace | No trigger node in the workflow, or no reachable nodes | Add a trigger node and connect it. |
| Webhook trigger never fires | There is no execution engine or ingestion endpoint yet | Expected — triggers are definitions only. See known limitations. |

## Known limitations

- **No execution engine.** Defining a workflow (even with a
  webhook trigger) does not make anything run: there is no
  runner, no scheduler, and no webhook ingestion endpoint. The
  simulator is dry-run only. Execution is a later task, and the
  threat model for it must be addressed *before* it ships, not
  after.
- **No workflow UI.** The Leptos frontend has no workflow
  canvas, node editor, or version views — workflows are
  API-only until a frontend task picks them up.
- **Node configs are operator-controlled JSON.** The domain
  validates shape (non-null, delay durations) but the config
  payload is otherwise opaque. The future execution engine
  must validate every config at the execution boundary.
- **One trigger per workflow.** By design (upsert + unique
  constraint); multi-trigger workflows would need a later
  task.
- **No workflow-level timeout or step budget.** The simulator
  terminates on cycles by visiting each node once, but a
  future execution engine will need real timeouts and step
  caps — see `threat-model.md`.
- **Workflow privilege is the publisher's.** When execution
  ships, runs will act with the authority of whoever
  published the workflow; publishing is therefore a
  privileged operation guarded by `workflow/update`.
