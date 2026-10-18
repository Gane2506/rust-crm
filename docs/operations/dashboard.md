# Analytics operations runbook

How the analytics vertical slice (tasks 15-DASH-01 through
15-DASH-03) works in production: saved dashboards with
metric widgets, and immutable parameterized reports.
Analytics is **read-mostly** — dashboards and reports are
configuration; `run_widget` executes one widget's metric
against the entity tables and returns the computed result.
For the auth model, see `security.md`; for the audit events
analytics emits, `audit-log.md`; for the analytics security
posture, `threat-model.md`.

## What it is

Staff save dashboards (a named set of widgets, each with a
metric definition) and reports (a named metric plus
parameter values). Four layers:

- **Domain** (`crates/crm-domain/src/dashboard.rs`):
  `MetricEntity` (closed set of 8: contact, campaign,
  invoice, payment, call, email, form_submission,
  contract; `Deserialize` implemented manually so
  unknown entities are rejected), `MetricAggregation`
  (`Count` / `Sum{field}` — blank sum field rejected),
  `DateRange` (`Last7Days` / `Last30Days` / `Last90Days` /
  `Custom` — `Custom` validates `from ≤ to`),
  `MetricDefinition` (pure query definition; private
  fields; manual `Deserialize` through the validating
  constructor), `DashboardWidget` (`Count` / `Timeseries` /
  `Breakdown`; **type/metric compatibility is enforced**:
  `Breakdown` requires `group_by`, `Count` rejects it;
  grid positions are unique per dashboard),
  `Dashboard` (owner scope `Option<ClientProfileId>` —
  `None` is the agency-level master dashboard;
  `Draft` / `Published`; publishing requires `Draft`
  plus at least one widget; mutations are rejected on
  published dashboards), `Report` (immutable; staff
  owner; duplicate parameter names rejected),
  `WidgetResult` / `WidgetData`, and the
  `DashboardService` / repository traits.
- **Persistence** (`crates/crm-db`, migration
  `0015_dashboards`): `dashboards`, `dashboard_widgets`
  (`UNIQUE(dashboard_id, position_x, position_y)` as
  defense-in-depth behind the domain invariant),
  `reports`. Metrics and parameters are stored as
  JSONB and hydrated through the domain's validating
  constructors — a corrupt row fails loudly instead
  of producing a half-valid metric.
- **Service** (`crates/crm-core/src/dashboard.rs`):
  `DashboardServiceImpl` over the dashboard, report,
  and audit ports plus a `PgPool` for read-only metric
  queries. `run_widget` executes metrics against the
  entity tables with SQL built **only** from a
  hardcoded per-entity allowlist (`entity_plan`: table,
  timestamp column, groupable text columns, summable
  numeric columns); `group_by` and sum fields are
  validated against those allowlists first, so no
  caller-controlled string ever reaches query text.
  Dates and the profile filter travel as bound
  parameters. Every mutation records an audit event; an
  audit-recording failure is discarded in the service
  (`let _ =`) and never fails the operation — the HTTP
  handler returns success regardless.
- **HTTP** (`crates/crm-api/src/dashboard.rs`): 9
  endpoints under `/api/v1` — dashboards CRUD plus
  widget run, and reports create / list / get.

Authorization uses `Resource::Dashboard`
(profile-scoped to the dashboard's owner scope;
agency-level dashboards authorize at agency scope)
and `Resource::Report` (agency scope — reports are
staff-owned and carry no client profile).
Fetch-then-authorize: a missing dashboard or report
is `404` before any grant check.

## Metric definitions

A metric is a pure query definition:

- **Entity** — one of the 8 closed entities. Each maps
  to one table and one timestamp column:

  | Entity | Table | Date-range column |
  |---|---|---|
  | contact | `contacts` | `created_at` |
  | campaign | `campaigns` | `created_at` |
  | invoice | `invoices` | `created_at` |
  | payment | `payments` | `recorded_at` |
  | call | `calls` | `created_at` |
  | email | `email_messages` | `created_at` |
  | form_submission | `form_submissions` | `submitted_at` |
  | contract | `contracts` | `created_at` |

- **Aggregation** — `Count` (rows), or `Sum{field}`.
  `Sum` is supported **only** for `payment`/`amount` —
  every other entity rejects `Sum` with a validation
  error rather than guessing at a numeric column.
- **Date range** — `Last7Days` / `Last30Days` /
  `Last90Days` / `Custom{from, to}`. `Custom`
  rejects `from > to`.
- **Group by** — optional field name, validated
  against the entity's allowlist (e.g. contact
  `lifecycle`, campaign `delivery_state` / `channel`,
  payment `status`). Blank is rejected.

## Widget types and `run_widget` semantics

- **`Count`** — a single headline number. Rejects
  `group_by` (a grouped count is a breakdown).
- **`Timeseries`** — one value per day over the date
  range. Days with no rows are **zero-filled**
  (ungrouped); a grouped timeseries emits one point
  per group per day with the label
  `"<key> · <date>"`.
- **`Breakdown`** — one value per group. Requires the
  metric's `group_by`.

`POST /api/v1/dashboards/{id}/widgets/{widget_id}/run`
(`200`) executes the widget's metric and returns the
`WidgetResult`. It is **read-only**: no audit event,
no mutation. The metric executes in the dashboard's
own scope: a profile-scoped dashboard counts only its
profile's rows; the agency-level master dashboard
counts across all profiles.

## Scope rules

- A profile-scoped context may only touch dashboards
  owned by its own profile.
- Agency-level dashboards (`client_profile_id: None`)
  require an agency-scoped context — a profile must
  never see agency-wide aggregates.
- An agency-scoped context may run any dashboard; the
  metric executes in the dashboard's own scope.
- Reports are staff-owned and carry no profile; all
  report endpoints authorize at agency scope.

Endpoints (`{id}` is the dashboard UUID, `{wid}` the
widget UUID, `{rid}` the report UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/dashboards` `{name, client_profile_id?}` | `dashboard/create` | Create a draft dashboard (`201`) |
| `GET /api/v1/dashboards?client_profile_id=<uuid>&limit=<n>` | `dashboard/read` | List dashboards for one owner scope, newest first (`200`; `client_profile_id` omitted lists the agency-level dashboards) |
| `GET /api/v1/dashboards/{id}` | `dashboard/read` | Fetch a dashboard with its widgets (`200`) |
| `PATCH /api/v1/dashboards/{id}` `{name?, widgets_to_add?, widget_ids_to_remove?}` | `dashboard/update` | Draft-only mutations: rename, add/remove widgets (`200`; widget positions must be free; rejected on published dashboards) |
| `DELETE /api/v1/dashboards/{id}` | `dashboard/delete` | Delete a dashboard and its widgets (`204`) |
| `POST /api/v1/dashboards/{id}/widgets/{wid}/run` | `dashboard/read` | Execute the widget's metric (`200`; read-only, no audit) |
| `POST /api/v1/reports` `{name, owner, metric, parameters?}` | `report/create` | Save a parameterized report (`201`; duplicate parameter names rejected) |
| `GET /api/v1/reports?limit=<n>` | `report/read` | List reports, newest first (`200`) |
| `GET /api/v1/reports/{rid}` | `report/read` | Fetch a report (`200`) |

Example — create an agency-level dashboard with a
contacts-count widget:

```bash
curl -s -X POST localhost:3000/api/v1/dashboards \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{
    "name": "Agency overview"
  }'
# → 201 { "dashboard_id": "…", "name": "Agency overview",
#        "client_profile_id": null, "status": "Draft", "widgets": [], … }

curl -s -X PATCH localhost:3000/api/v1/dashboards/<id> \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{
    "widgets_to_add": [{
      "title": "Contacts (30d)",
      "widget_type": "count",
      "metric": {
        "entity": "contact",
        "aggregation": "count",
        "date_range": "last_30_days"
      },
      "position": { "x": 0, "y": 0 }
    }]
  }'

curl -s -X POST \
  localhost:3000/api/v1/dashboards/<id>/widgets/<wid>/run \
  -H "Authorization: Bearer $TOKEN"
# → 200 { "widget_id": "…", "data": { "count": 42 } }
```

## Audit trail

The service records audit events with these pinned
action names (see `audit-log.md`):

`dashboard.dashboard_created`,
`dashboard.dashboard_updated`,
`dashboard.dashboard_deleted`,
`dashboard.report_created`.

Reads (`list_dashboards`, `get_dashboard`,
`list_reports`, `get_report`) and `run_widget` are
not audited, matching the domain contract — a metric
execution is a read. An audit-recording failure is
discarded in the service (`let _ =`) and never fails
the operation: the HTTP handler returns success
regardless.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated
by `CRM_SEED_DEV=1`) creates an agency-level demo
dashboard through the real `DashboardServiceImpl` (so
scope, widget validation, and audit behavior match
production): "Agency overview" with two widgets — a
`Count` of contacts over the last 30 days at (0, 0),
and a `Breakdown` of contacts by `lifecycle` over the
last 30 days at (1, 0) — plus a demo report ("New
contacts by lifecycle", same metric, no parameters):

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed is idempotent (skipped when any
agency-level dashboards already exist). Seeding uses
the service layer — never raw SQL — so storage
behavior matches production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on analytics endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on dashboard endpoints | No `dashboard/*` grant in the dashboard's owner scope | Have an operator check the `grants` table. Profile-scoped dashboards need grants on that profile; the agency-level dashboard needs agency-scope grants. Grants are per-scope; cross-scope access is denied by design. |
| `403` on an agency-level dashboard with profile grants | Profile contexts can't see agency aggregates | Use an agency-scoped session. By design, a profile must never see agency-wide numbers. |
| `422` on create/update dashboard | Blank name, duplicate widget position, or mutating a published dashboard | Names must be non-blank; grid positions are unique per dashboard; published dashboards are immutable in this slice. |
| `422` on run_widget | Unsupported `Sum` entity, unknown `group_by` field, or invalid date range | `Sum` is only for `payment`/`amount`; `group_by` must be one of the entity's allowlisted fields; `Custom` ranges require `from ≤ to`. |
| `404` on dashboard/report endpoints | Wrong id, or the dashboard belongs to another scope | Fetch-then-authorize: a missing row is `404` before any grant check. |
| Empty widget results (`count: 0`, empty buckets) | No rows in the date range, or scope filtering | Check the date range against the data; remember a profile-scoped dashboard only counts its own profile's rows, and agency-owned campaigns with NULL `client_profile_id` are excluded from profile-scoped campaign metrics. |
| Metric hydrate failure in logs | Corrupt metric JSONB in `dashboard_widgets`/`reports` | The repository hydrates through the domain's validating constructors and fails loudly on purpose — a half-valid metric must never execute. Restore the row from backup or delete and recreate the widget/report. |

## Known limitations

- **Dashboards stay drafts.** The domain defines
  `publish()` (Draft → Published, requires at least
  one widget), but the service slice has no
  publish/unpublish operations — dashboards remain
  drafts and all mutations stay available. Publish is
  a later task.
- **No dashboard or report UI.** The Leptos frontend
  has no analytics views — dashboards and reports
  are API-only until a frontend task picks them up.
- **`Sum` only for payment/amount.** Every other
  entity rejects `Sum` with a validation error; if a
  new entity gains a natural numeric column, extend
  `entity_plan` in `crates/crm-core/src/dashboard.rs`.
- **Metric SQL lives in crm-core.** `run_widget`'s
  query building is in `DashboardServiceImpl` rather
  than behind a repository port — a deliberate,
  documented layering tradeoff: the SQL is built
  exclusively from compile-time-constant allowlists,
  so the repository boundary buys no additional
  safety here.
- **Agency-owned campaigns excluded from
  profile-scoped campaign metrics.** Campaigns with
  NULL `client_profile_id` (agency-owned) are not
  counted when a profile-scoped dashboard runs a
  campaign metric — a profile must never see rows
  outside its scope, even by aggregation.
- **`total` is not returned.** List responses return
  the items; there is no unfiltered count query yet.
