# Jev agents operations runbook

How the Jev agents vertical slice (tasks 09-JEV-01 through 09-JEV-03)
works in production: agent runs, model adapters with fallback,
approval-gated tool calls, handoffs, and the audit trail. For the
auth model, see `security.md`; for the audit events agents emit,
`audit-log.md`; for the agents' security posture, `threat-model.md`.

## What it is

Jev is the agent-native core of the CRM: staff (and other agents)
delegate work to agent runs that call typed tools through a
provider-agnostic model adapter. Four layers:

- **Domain** (`crates/crm-domain/src/jev.rs`): `AgentRun`
  (forward-only status state machine), `FallbackChain` (ordered
  adapter names, invariants enforced at construction *and*
  deserialization), `AgentTool` (typed tool definition with JSON
  Schema), `ToolCall` (approval-gated lifecycle), `ApprovalGate`
  (human-in-the-loop decision, immutable once made, decider
  recorded), `Handoff` (agent-to-agent context transfer), the
  `ModelAdapter` trait (provider seam), and the `AgentService` /
  repository traits.
- **Persistence** (`crates/crm-db`, migration `0008_agent_runs`):
  `agent_runs` (the fallback chain is stored as JSONB *on the run*,
  so a reloaded run keeps walking the chain after an adapter
  failure), `tool_calls` (`requires_approval` captured at record
  time), `handoffs` (append-only — no update/delete).
- **Service** (`crates/crm-core/src/jev.rs`):
  `AgentServiceImpl` over the run / tool-call / handoff /
  role-membership / audit ports. `StubModelAdapter` (deterministic,
  in-memory) is the dev/test adapter; real provider adapters are a
  later task. The adapter execution loop that walks the fallback
  chain on model failure is also out of scope for this slice.
- **HTTP** (`crates/crm-api/src/jev.rs`): 13 endpoints under
  `/api/v1` — agent-run CRUD plus the six lifecycle transitions,
  tool-call record/list, and handoff create/list.

Every run belongs to one client profile — the tenant boundary.
Authorization uses the `AgentRun` resource with profile-scoped
grants (`agent_run/create` to start, `agent_run/read` to inspect,
`agent_run/update` to drive lifecycle and record calls,
`agent_run/approve` to approve or deny).

## Run lifecycle

`POST /api/v1/agent-runs` (bearer auth, `agent_run/create` grant on
the profile). The body carries `client_profile_id`, `agent_kind`
(e.g. `"triage"`), a `fallback_chain` (adapter names, primary first),
and the run `input`:

```
Pending → Running → Completed
            ↕
     AwaitingApproval → (approve) Running
                      → (reject)  Failed
Running / AwaitingApproval → Failed | Cancelled (any non-terminal)
```

- `start()` only works from `Pending` — resuming an
  approval-awaiting run is `approve()`, never `start()`.
- `complete()` sets the `output` and is the only way to leave
  `Running` successfully.
- `cancel()` is idempotent on already-cancelled runs and rejected
  on terminal states.
- Terminal states (`Completed`, `Failed`, `Cancelled`) have no
  outgoing transitions.

Endpoints (`{id}` is the run UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/agent-runs` | `agent_run/create` | Create a `Pending` run; adapter = chain primary |
| `GET /api/v1/agent-runs?client_profile_id=<uuid>&status=<s>` | `agent_run/read` | List runs for one profile, optional status filter |
| `GET /api/v1/agent-runs/{id}` | `agent_run/read` | Fetch-then-authorize against the run's own profile |
| `POST /api/v1/agent-runs/{id}/request-approval` | `agent_run/update` | `Running → AwaitingApproval` |
| `POST /api/v1/agent-runs/{id}/approve` | `agent_run/approve` | `AwaitingApproval → Running`; also approves the run's pending gated tool calls |
| `POST /api/v1/agent-runs/{id}/deny` | `agent_run/approve` | `AwaitingApproval → Failed`; reason synthesized as `denied by {actor}` |
| `POST /api/v1/agent-runs/{id}/complete` | `agent_run/update` | `Running → Completed` with output |
| `POST /api/v1/agent-runs/{id}/fail` | `agent_run/update` | `Running`/`AwaitingApproval → Failed` |
| `POST /api/v1/agent-runs/{id}/cancel` | `agent_run/update` | Any non-terminal → `Cancelled` (idempotent) |
| `POST /api/v1/agent-runs/{id}/tool-calls` | `agent_run/update` | Record a tool call (see below) |
| `GET /api/v1/agent-runs/{id}/tool-calls` | `agent_run/read` | List the run's calls, oldest first |
| `POST /api/v1/agent-runs/{id}/handoff` | `agent_run/update` | Record a handoff to another agent kind |
| `GET /api/v1/agent-runs/{id}/handoffs` | `agent_run/read` | List the run's handoffs |

## Approval flow

A run that needs a human decision moves to `AwaitingApproval`.
Two gate shapes:

- **Person-specific** (`ApprovalGate::for_actor`): only the named
  approver may decide. `decide()` rejects any other actor with
  `Unauthorized` — this is enforced by the domain type itself.
- **Role-based** (`ApprovalGate::for_role("agent_approver")`):
  anyone holding the role may decide. The service checks the
  decider's membership through the `RoleMembership` port **before**
  calling `decide()`; the check fails closed (`PgRoleMembership`
  resolves the role to the `agent_run`/`approve` — or `manage` —
  grant covering the run's profile, profile-scoped or
  agency-scoped). An unresolvable check or a non-member yields
  `Unauthorized` and the gate stays open.

Decision accountability: `decide()` records `decided_by` and
`decided_at` on the gate, and a second call is rejected — an
approval can never be silently reversed. Denials require a reason.
The decided gate is persisted on the run (`agent_runs.approval_gate`
JSONB, FIX-04) and returned on run reads, so the decision, decider,
and decision time are part of the durable audit trail.

**Approving a run approves its pending gated tool calls.** When a
human approves the run (`approve_step`), the service also approves
that run's `Pending` gated calls — the human approved the run,
which covers its calls. This is deliberate and audited
(`agent.approved`).

## Fallback

The run stores its `FallbackChain` as JSONB on the `agent_runs`
row. `adapter_name` is the *currently selected* adapter: it starts
as the chain's primary and advances along `next_after()` when an
adapter fails. Chain exhaustion (no next adapter, or an unknown
name) fails closed — the run fails rather than retrying silently.

Today only `StubModelAdapter` exists (deterministic, in-memory —
it returns a fixed completion text and never fails). The execution
loop that actually walks the chain on model failure is a later
task; the chain is stored and exposed so that loop can pick up
reloaded runs exactly where they left off.

## Tool calls

`POST /api/v1/agent-runs/{id}/tool-calls` records a
model-requested call (`tool_name`, `input`, `requires_approval`
declared by the caller — the tool registry is a later task):

```
Pending → Approved → Executed
        → Denied (terminal)
        Approved → Failed (terminal)
```

- **Gated** (`requires_approval: true`): stays `Pending` until a
  human approves it (run approval covers it, see above) or denies
  it with a reason. Recording a gated call also pauses the run in
  `AwaitingApproval`.
- **Ungated**: the service approves the call itself
  (`ToolCall::approve`, no human involved) — the call lands in
  `Approved` status as a stand-in for execution, because no tool
  executor exists yet. Note: `Approved` therefore does **not**
  exclusively mean "human-approved" today; the two paths are
  distinguishable only by the `requires_approval` flag captured on
  the row and the `agent.tool_recorded` audit event. The
  tool-execution task should drive real `Executed`/`Failed`
  transitions and reserve `Approved` for humans.

`requires_approval` is captured on the row at record time for
audit/display. Tool outputs are stored on the call once an
executor exists; today no output is produced.

## Handoffs

`POST /api/v1/agent-runs/{id}/handoff` records an agent-to-agent
transfer (`to_agent_kind`, opaque JSON `context`, never null).
Handoffs are append-only — no update/delete exists. The receiving
agent starts as a *separate* `AgentRun`; the handoff is the
provenance link between them. The receiver validates the context
against its own input contract.

## Audit trail

The service records audit events with these pinned action names
(see `audit-log.md`):

`agent.run_started`, `agent.approval_requested`, `agent.approved`,
`agent.denied`, `agent.run_completed`, `agent.run_failed`,
`agent.run_cancelled`, `agent.tool_recorded`, `agent.handed_off`.

Audit is fail-loud at the HTTP layer and never fails the
operation: a recording failure is swallowed in the service (loud
warn) so agent work is never blocked by the audit pipeline.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated by
`CRM_SEED_DEV=1`) creates one completed demo run on the demo
profile (`agent_kind: "demo-triage"`, chain
`["stub", "local"]`), driven through the real service layer
(start → complete), plus `agent_run` create/read/update/approve
grants for the demo staff user:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed run is idempotent (skipped when a `demo-triage` run
already exists on the demo profile). Seeding uses the service —
never raw SQL — so scope checks and audit events behave exactly
as in production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on agent endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on run create/read | No `agent_run/create` or `agent_run/read` grant on the profile | Have an operator check the `grants` table for `agent_run` on that profile scope. Grants are per-profile; cross-profile access is denied by design. |
| `403` on approve/deny | No `agent_run/approve` grant | Same as above, for the `Approve` operation. |
| `403` on approve for a role gate | Decider is not a member of `agent_approver` | Grant `agent_run/approve` (or `manage`) on the profile or its agency. The role check fails closed — an unresolvable check also denies. |
| `422` on create | Blank `agent_kind`, null `input`, or an invalid chain (empty, blank name) | Fix the body; the chain's invariants are enforced at construction. |
| `422` on lifecycle transition | Illegal transition (e.g. `complete` on a `Pending` run, `start` on a `Running` run) | Check the state machine above; terminal states never transition. |
| `404` on run/tool-call/handoff reads | Wrong id, or the run belongs to another profile | Fetch-then-authorize: a missing row is `404` before any grant check. |
| Run stuck in `Running` | No execution daemon exists — a run stays `Running` until a human or a future loop drives it | Cancel it (`POST .../cancel`) or drive it to `Completed`/`Failed` manually. |
| Run stuck in `AwaitingApproval` | Nobody has decided the gate | Someone with `agent_run/approve` must approve or deny. |
| Approval rejected as `Unauthorized` | Wrong actor on a person-specific gate | Only the named approver may decide; check `ApprovalGate.approver`. |

## Known limitations

- **Stub adapter only.** `StubModelAdapter` returns fixed text and
  never fails — the fallback walk has never executed against a
  real failure. Real provider adapters (OpenRouter, …) are a
  later task.
- **No execution loop.** Nothing drives a `Running` run on its
  own: no model invocations, no timeout, no cancellation daemon.
  Runs are driven entirely through the lifecycle endpoints today.
- **No run UI.** The Leptos frontend has no agent-run, approval,
  or handoff views — agents are API-only until a frontend task
  picks them up.
- **`NewToolCall.requires_approval` is caller-declared.** There is
  no tool registry yet; the caller asserts whether the tool needs
  approval. A malicious or buggy caller can mark a dangerous tool
  ungated — the registry task must make this authoritative.
- **`Approved` is not human-only (yet).** Ungated tool calls are
  system-approved by the service, so the status alone doesn't
  prove a human was involved. Downstream consumers (and the
  future tool executor) must check `requires_approval` on the
  row, not just the status.
- **`deny_step` synthesizes the reason.** The service-trait
  signature carries no reason, so the service records
  `denied by {actor}`; the actor is preserved as `decided_by`.
- **No run-level timeout.** A run can sit in `Running` or
  `AwaitingApproval` indefinitely; there is no SLA or escalation.
- **Prompts and outputs are plaintext.** `input`, `output`, tool
  `input`/`output`, and handoff `context` are stored as JSONB,
  unencrypted — anyone with database read access sees them (see
  `threat-model.md`).
