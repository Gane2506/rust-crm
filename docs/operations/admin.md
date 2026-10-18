# Admin and operations runbook

How the admin/operations vertical slice (tasks 16-ADMIN-01
through 16-ADMIN-03) works in production: staff
administration (invitations, role assignments,
disable/reactivate), agency settings versioning,
maintenance mode, system health aggregation, backups,
data exports, system upgrades, and installation state.
Admin is **agency-wide by construction** — every method
in `AdminService` rejects client-profile-scoped contexts,
and every HTTP endpoint authorizes at agency scope
before touching the service. For the auth model, see
`security.md`; for the audit events admin emits,
`audit-log.md`; for the admin security posture,
`threat-model.md`.

## What it is

Four layers:

- **Domain** (`crates/crm-domain/src/admin.rs`):
  `StaffInvitation` (`Pending` → `Accepted` | `Expired` |
  `Revoked`, forward-only; expiry enforced on accept),
  `RoleAssignment` (self-assignment rejected at the
  domain level; revocation is idempotent so retried
  commands stay safe), `AgencySettings` (immutable
  versioned snapshots — `update()` returns version+1,
  never mutates; display name non-blank, support email
  validated, session TTL bounded 900s–7d and can only
  tighten the platform max), `MaintenanceMode` (toggle
  with a non-blank reason), `BackupRecord`
  (`Scheduled` → `InProgress` → `Completed` | `Failed`),
  `DataExport` (`Requested` → `InProgress` → `Ready` |
  `Failed`, `Ready` → `Expired`), `SystemUpgrade`
  (`Scheduled` → `InProgress` → `Completed` | `Failed`,
  `Failed` → `RolledBack`), `Installation`
  (`NotStarted` → `InProgress` → `Completed`, runs
  exactly once), and `SystemHealthSummary` — a pure
  worst-status-wins aggregation over
  `observability::HealthReport` (never duplicated;
  computed by `summarize_health()`). All types hydrate
  through validating constructors — a corrupt row
  fails loudly instead of producing a half-valid
  entity.
- **Persistence** (`crates/crm-db`, migration
  `0016_admin`): `staff_invitations`,
  `role_assignments`, `agency_settings` (version
  `BIGINT UNIQUE`, per-agency), `maintenance_state`
  and `installation_state` (singletons, `id = 1`
  `CHECK`), `backups`, `exports`, `upgrades`.
  Payloads are JSONB hydrated through the domain's
  validating `Deserialize`; key columns exist for
  indexing. Singleton rows are created lazily — a
  missing row reads as the domain default, never an
  error.
- **Service** (`crates/crm-core/src/admin.rs`):
  `AdminServiceImpl` implements all 35 `AdminService`
  trait methods. Every method calls
  `require_agency_scope(ctx)?` first (defense in
  depth behind the HTTP layer's agency-scope
  authorization), drives the domain lifecycles, and
  records the pinned `ADMIN_*` audit events. An
  audit-recording failure is discarded (`let _ =`)
  and never fails the operation — the HTTP handler
  returns success regardless. Accepting an invitation
  creates the `StaffUser` account in the inviter's
  agency (with no credential — it cannot log in until
  a credential is set) and marks the invitation
  accepted. A staff member cannot disable their own
  account. The first settings update creates version
  1 (the agency comes from the acting staff member's
  record). `get_system_health` folds the configured
  `HealthReporter`s (currently the bounded
  `SELECT 1` database probe) through
  `summarize_health()`.
- **HTTP** (`crates/crm-api/src/admin.rs`): 38
  endpoints under `/api/v1/admin` — invitations
  (create/list/get/accept/revoke), staff
  disable/reactivate, role assignments
  (assign/list/revoke), agency settings (get/update),
  maintenance (get/set), system health (get), backups
  / exports / upgrades (lifecycle steps), and
  installation (get/begin/complete). Authorization
  uses `authorize_admin`, which resolves the context
  with no client profile — admin never accepts a
  profile scope. Staff admin ops use
  `Resource::StaffUser` + `Manage`/`Read`; settings
  use `Resource::AgencySettings`; everything else
  uses `Resource::System`. Fetch-then-authorize: a
  missing row is `404` before any grant check.

## Staff administration

Invitations are **staff-mediated**: there is no
self-service accept link and no invitation token in
this slice — an authorized operator accepts the
invitation on the invitee's behalf, and the domain
explicitly defers token verification to a future
service layer (the 03-SEC-02 password-verification
precedent). Accepting creates the account in the
**inviter's** agency.

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/admin/invitations` `{email, role_ids, expires_at}` | `staff_user/manage` | Create a pending invitation (`201`; email validated, at least one role required) |
| `GET /api/v1/admin/invitations?limit=<n>` | `staff_user/read` | List invitations, newest first (`200`) |
| `GET /api/v1/admin/invitations/{id}` | `staff_user/read` | Fetch one invitation (`200`) |
| `POST /api/v1/admin/invitations/{id}/accept` | `staff_user/manage` | Accept: creates the `StaffUser` (no credential) and marks the invitation accepted (`200`; rejected when expired or non-pending) |
| `POST /api/v1/admin/invitations/{id}/revoke` | `staff_user/manage` | Revoke a pending invitation (`200`) |
| `POST /api/v1/admin/staff/{id}/disable` | `staff_user/manage` | Suspend a staff account (`204`; self-disable rejected with `422`) |
| `POST /api/v1/admin/staff/{id}/reactivate` | `staff_user/manage` | Reactivate a suspended account (`204`) |
| `POST /api/v1/admin/role-assignments` `{staff_id, role_id, scope}` | `staff_user/manage` | Assign a role (`201`; self-assignment rejected) |
| `GET /api/v1/admin/role-assignments?staff_id=<uuid>&limit=<n>` | `staff_user/read` | List a staff member's assignments (`200`) |
| `POST /api/v1/admin/role-assignments/{id}/revoke` | `staff_user/manage` | Revoke an assignment (`200`; idempotent) |

## Agency settings

Settings are immutable snapshots: every update
creates version+1, and the full history is retained.
The first version requires `display_name` and
`session_ttl_secs` (there is nothing to inherit);
later versions inherit unchanged fields. The agency
is taken from the acting staff member's record, so
settings can never be written for another agency.

| Method & path | Grant | Effect |
|---|---|---|
| `GET /api/v1/admin/settings` | `agency_settings/read` | Current (highest-version) settings (`200`) |
| `PUT /api/v1/admin/settings` `{display_name?, support_email?, session_ttl_secs?}` | `agency_settings/manage` | Create a new version (`200`) |

## Maintenance mode and health

Maintenance mode is an **operational signal, not an
enforcement gate** in this slice: toggling it records
the state with a reason and an audit event, but no
middleware rejects traffic while it is enabled.
Operators use it to coordinate with external
maintenance (deployments, database work).

System health folds the configured health reporters
through the pure `summarize_health()` — the worst
component status wins (`Unhealthy` > `Degraded` >
`Healthy`). The endpoint is staff-only (requires the
`system/read` grant); it is not a public probe —
`/healthz` and `/readyz` keep serving that role.

| Method & path | Grant | Effect |
|---|---|---|
| `GET /api/v1/admin/maintenance` | `system/read` | Current maintenance state (`200`; missing row reads as disabled) |
| `PUT /api/v1/admin/maintenance` `{enabled, reason}` | `system/manage` | Toggle maintenance (`200`; reason required) |
| `GET /api/v1/admin/health` | `system/read` | Aggregated health summary (`200`; read-only, no audit) |

## Backups, exports, upgrades, installation

These are **lifecycle trackers**, not executors: the
service records state transitions (scheduled →
running → completed/failed) for work performed by
external operators or future automation. Retrying a
failed backup or upgrade creates a **new record** so
every attempt is individually auditable; an upgrade
rolls back only from `Failed`, and the recovery path
is explicit in the type.

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/admin/backups` | `system/manage` | Schedule a backup (`201`) |
| `POST /api/v1/admin/backups/{id}/start` | `system/manage` | Mark running (`200`) |
| `POST /api/v1/admin/backups/{id}/complete` `{size_bytes}` | `system/manage` | Mark completed (`200`) |
| `POST /api/v1/admin/backups/{id}/fail` `{reason}` | `system/manage` | Mark failed (`200`) |
| `GET /api/v1/admin/backups?limit=<n>` | `system/read` | List backups (`200`) |
| `POST /api/v1/admin/exports` `{client_profile_id?, format}` | `system/manage` | Request a data export (`201`) |
| `POST /api/v1/admin/exports/{id}/{start\|complete\|fail\|expire}` | `system/manage` | Drive the export lifecycle (`200`) |
| `GET /api/v1/admin/exports?limit=<n>` | `system/read` | List exports (`200`) |
| `POST /api/v1/admin/upgrades` `{from_version, to_version}` | `system/manage` | Schedule an upgrade (`201`; versions must differ) |
| `POST /api/v1/admin/upgrades/{id}/{start\|complete\|fail\|rollback}` | `system/manage` | Drive the upgrade lifecycle (`200`; rollback only from `Failed`) |
| `GET /api/v1/admin/upgrades?limit=<n>` | `system/read` | List upgrades (`200`) |
| `GET /api/v1/admin/installation` | `system/read` | Installation state (`200`) |
| `POST /api/v1/admin/installation/begin` | `system/manage` | Begin installation (`200`; only from `NotStarted`) |
| `POST /api/v1/admin/installation/complete` `{version}` | `system/manage` | Complete installation (`200`) |

## Audit trail

The service records audit events with these pinned
action names (see `audit-log.md`):

`admin.staff_invited`, `admin.invitation_accepted`,
`admin.invitation_revoked`, `admin.staff_disabled`,
`admin.staff_reactivated`, `admin.role_assigned`,
`admin.role_revoked`, `admin.settings_updated`,
`admin.maintenance_enabled`,
`admin.maintenance_disabled`, `admin.backup_started`,
`admin.backup_completed`, `admin.backup_failed`,
`admin.export_requested`, `admin.export_ready`,
`admin.export_failed`, `admin.upgrade_started`,
`admin.upgrade_completed`, `admin.upgrade_failed`,
`admin.upgrade_rolled_back`,
`admin.installation_completed`.

Reads (`list_*`, `get_*`, `get_system_health`) are
not audited, matching the domain contract. The
service deliberately emits **no** audit events for
`schedule_backup`, `schedule_upgrade`,
`start_export`, `expire_export`, and
`begin_installation` — the trait contract carries no
"Records" lines and no pinned consts exist for them.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated
by `CRM_SEED_DEV=1`) writes agency settings v1 for
the demo staff's agency (display name "Demo Agency",
support email `support@example.com`, session TTL 1
hour) and ensures the maintenance singleton exists in
the disabled state. The seed is idempotent — it
skips when a settings v1 already exists.

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

## Security review findings (16-ADMIN-03)

The admin surface was reviewed line-by-line in this
task; no exploitable bugs were found. The findings
below are documented behavior, not defects:

- **Every one of the 38 handlers authorizes.**
  Lifecycle-step handlers (`start_backup`,
  `complete_export`, `fail_upgrade`, …) delegate to
  shared `*_step` helpers that call `authorize_admin`
  — there is no unauthenticated step endpoint.
- **Agency scope is enforced twice.** The HTTP
  layer resolves the context with no client profile
  (a profile scope can never authorize admin), and
  the service independently rejects any
  profile-scoped context (`require_agency_scope`).
- **Invitation accept is staff-mediated.** There is
  no invitation token and no public accept endpoint;
  the domain explicitly records the invitation and
  its transitions and leaves token verification to
  a future service layer. The created account joins
  the inviter's agency and starts with **no
  credential** — it cannot log in until a credential
  is set, which also bounds the blast radius of a
  partial failure (staff row created, invitation
  update lost).
- **Self-disable and self-assignment are rejected**
  at the domain level (`422`), so an operator cannot
  lock themselves out or escalate their own roles
  through these endpoints.
- **Settings are agency-pinned.** The agency is
  taken from the acting staff member's record, never
  from the request body — settings cannot be written
  for another agency.
- **Maintenance mode is advisory.** Toggling it
  records state and an audit event, but no
  middleware rejects traffic while enabled. Do not
  rely on it as an access control.
- **Health is staff-only.** `GET /admin/health`
  requires the `system/read` grant; the public
  liveness/readiness probes stay on `/healthz` and
  `/readyz`.
- **No idempotency keys on admin writes** (same
  known limitation as 12-BILL-03): retried requests
  can create duplicate invitations, backups,
  exports, or upgrades. Revocation stays safe
  because it is idempotent in the domain.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on admin endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on admin endpoints | No `staff_user/*`, `agency_settings/*`, or `system/*` grant at agency scope | Have an operator check the `grants` table. Admin endpoints never accept a profile-scoped session — use an agency-scoped one. |
| `403` with profile-scoped grants | Profile contexts are rejected by design | Switch to an agency-scoped session. |
| `422` on accept invitation | Invitation expired or already accepted/revoked | Expired invitations must be expired (not accepted) so the terminal state is recorded accurately; create a new invitation. |
| `422` on disable staff | Attempting to disable your own account | Have another operator disable it. |
| `422` on first settings update | `display_name` or `session_ttl_secs` missing | The first version requires both; later versions inherit. |
| `422` on rollback | Upgrade is not in `Failed` state | Only failed upgrades can roll back; retrying schedules a new record. |
| Health shows `Degraded`/`Unhealthy` | A reporter flagged a component | The per-component reports in the response say which component and why; reasons are secret-free by domain rule. |
| Duplicate invitations after retry | No idempotency keys on admin writes | Revoke the duplicates; revocation is safe. |

## Known limitations

- **No invitation tokens or self-service accept.**
  Acceptance is staff-mediated; token verification is
  a future service-layer concern.
- **Maintenance mode is advisory only** — no
  request enforcement while enabled.
- **Lifecycle trackers, not executors.** Backups,
  exports, and upgrades record state transitions;
  the actual work happens outside this slice.
- **No idempotency keys on admin writes** — retried
  requests can create duplicates (same limitation as
  12-BILL-03).
- **No admin UI.** The Leptos frontend has no admin
  views — the admin surface is API-only until a
  frontend task picks it up.
