# Campaigns operations runbook

How the campaigns vertical slice (tasks 08-CAMP-01 through
08-CAMP-03) works in production: campaign lifecycle, prospect
pools, bulk sending with opt-out enforcement, attribution, and
troubleshooting. For the auth model, see `security.md`; for the
audit events campaigns emit, `audit-log.md`; for the campaigns
security posture, `threat-model.md`.

## What it is

The campaigns slice lets agency staff run bulk outreach (email
today, calls later) against prospect pools per client profile,
with mandatory opt-out enforcement and conversion attribution.
Four layers:

- **Domain** (`crates/crm-domain/src/campaign.rs`): `Campaign`
  (forward-only `DeliveryState` machine), `CampaignStep`
  (`Email`/`Call`/`Wait`), `ProspectPool` (dedupe-on-add member
  list), `OptOut` (immutable — no update or delete API exists),
  `AttributionModel` + the pure `attribute_conversion()`
  function, `Conversion`, and the `CampaignService` /
  repository traits.
- **Persistence** (`crates/crm-db`, migration `0007_campaigns`):
  `campaigns`, `campaign_steps` (unique position per campaign),
  `prospect_pools`, `pool_members` (composite PK enforces
  dedupe), `opt_outs` (unique per profile/contact/channel —
  idempotent re-recording), `conversions`.
- **Service** (`crates/crm-core/src/campaign.rs`):
  `CampaignServiceImpl` over the repository + contact + email +
  audit ports. Bulk email steps execute through the injected
  `EmailService` (the stub provider in dev/test).
- **HTTP** (`crates/crm-api/src/campaigns.rs`): campaign CRUD +
  lifecycle, prospect pools + members, opt-outs, conversions
  (14 endpoints).

Every campaign is owned by an agency or one client profile
(`OwnerContext`) — the tenant boundary. Authorization uses the
`Campaign` resource (`campaign/create`, `campaign/read`,
`campaign/update`) in the campaign's owner scope. Agency-owned
campaigns are listed when no `client_profile_id` is given;
profile-owned campaigns require it.

## Campaign lifecycle

```
Draft → Scheduled → Running ⇄ Paused → Completed
  │         │           │                  │
  └─────────┴───────────┴──────────────────┴→ Cancelled (any non-terminal)
```

- `POST /api/v1/campaigns` (201) — creates a `Draft` campaign.
  Body: `name`, `owner_kind` (`"agency"` or `"client_profile"`
  + `client_profile_id`), `channel`, `prospect_pool_id`,
  `schedule` (cron), `suppression_policy`, `conversion_rules`,
  `attribution_window_days`. Requires `campaign/create` in the
  owner scope.
- `POST /api/v1/campaigns/{id}/steps` (201) — appends a step.
  `Email`/`Call` steps require a `template_id`; `Wait` steps
  require `wait_duration_secs`. Positions are unique per
  campaign (DB-enforced).
- `POST /api/v1/campaigns/{id}/schedule` — `Draft → Scheduled`.
  Requires a future `schedule_for`; the one-shot launch time is
  distinct from the recurring cron `schedule`.
- `POST /api/v1/campaigns/{id}/launch` — `Draft`/`Scheduled →
  Running`. Requires ≥1 step. **This sends mail** — see "Bulk
  send flow" below.
- `POST /api/v1/campaigns/{id}/pause|resume` — `Running ⇄ Paused`.
- `POST /api/v1/campaigns/{id}/complete` — terminal.
- `POST /api/v1/campaigns/{id}/cancel` — terminal from any
  non-terminal state; idempotent.
- `GET /api/v1/campaigns?client_profile_id=<uuid>` — lists
  campaigns in the scope (`campaign/read`). `GET
  /api/v1/campaigns/{id}` — one campaign with its steps
  (fetch-then-authorize, the `get_asset` pattern).

`Completed` and `Cancelled` are terminal: no transitions out.
Audit: `campaign.created`, `campaign.launched`,
`campaign.paused`, `campaign.completed`, `campaign.cancelled`,
plus `campaign.bulk_send_completed` with the send summary.

## Prospect pools

`POST /api/v1/prospect-pools` (201) creates a pool on a profile;
`GET /api/v1/prospect-pools?client_profile_id=` lists them.
Members are managed with `POST
/api/v1/prospect-pools/{id}/members` (`{contact_id}`) and
`DELETE .../members/{contact_id}`.

- Dedupe is enforced twice: the domain's `add_member` ignores
  duplicates, and the `pool_members` composite primary key
  rejects them at the database.
- A campaign's `prospect_pool_id` is a string parsed as a UUID at
  launch; an unparseable id fails launch with a validation error
  (configuration error, not a 500).
- A campaign with no pool (or an empty pool) launches to zero
  recipients — the launch succeeds and the audit summary reads
  `sent=0 skipped=0`.

## Bulk send flow

`POST /api/v1/campaigns/{id}/launch` executes `Email` steps
immediately, inline in the request:

1. The campaign is loaded and scope-checked; steps are loaded
   (≥1 required by the domain).
2. The prospect pool is resolved to its member contact ids.
3. For each `Email` step, for each member contact:
   - **Opt-out check (mandatory, per-send, no caching).** If the
     contact is opted out on the `Email` channel, the send is
     skipped and counted.
   - The contact's first normalized email is used; contacts with
     no email address are skipped (not an error).
   - The email is sent through the injected `EmailService` with
     a per-contact `ActionContext` scoped to the contact's own
     profile. Subject is `"Campaign: {name}"`, body is the
     step's template. The idempotency key is deterministic:
     `campaign-{id}-step-{id}-contact-{id}`.
   - **Failure isolation.** A single send failure does not abort
     the campaign — the failure is counted and the launch
     continues.
4. The campaign row is updated to `Running`; two audit events are
   recorded: `campaign.launched` and `campaign.bulk_send_completed`
   with `sent={n} skipped={m}` in the detail.

`Call` and `Wait` steps are **not executed**: call steps are
recorded but skipped (no telephony in this slice), wait steps are
scheduler concerns (there is no scheduling daemon — see
limitations).

## Opt-out compliance

Opt-outs are the compliance backbone of bulk outreach:

- `POST /api/v1/opt-outs` (201) records an opt-out for a
  contact on a channel (`email`/`sms`/`call`/`all`). A duplicate
  recording returns the existing row with `200` (idempotent).
  Requires `campaign/update` on the contact's profile.
- **Immutable by construction.** `OptOut` has no update or delete
  methods in the domain or the repository trait — the type system
  enforces append-only. Re-consent is a separate event, not an
  edit (future work).
- **Channel matching.** An `all`-channel opt-out covers every
  channel (`covers()`); the launch check asks specifically about
  `Email`.
- **Enforcement point.** The per-send `is_opted_out` check in
  `launch()` is currently the only enforcement point. Any future
  send path (scheduling daemon, call execution) must perform the
  same check — grep for `is_opted_out` when adding one.

## Attribution and conversions

`POST /api/v1/conversions` (201) records a conversion:
`client_profile_id`, `contact_id`, optional `campaign_id`,
optional `value_cents`, `occurred_at`. Requires
`campaign/create` on the profile.

- When `campaign_id` is supplied, the service attributes via
  `attribute_conversion` (first-touch or last-touch per the
  campaign's `conversion_rules`); without touchpoints the
  conversion is recorded unattributed.
- The attribution window (`attribution_window_days`) bounds how
  far back a touch can claim credit.
- Audit: `campaign.conversion`.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated by
`CRM_SEED_DEV=1`) creates one draft campaign on the demo profile
with an email step, one prospect pool holding the demo contacts,
and one opt-out (grace@example.com, email channel) — so the
launch flow and opt-out enforcement are tryable locally. The
campaign is left in `Draft`: launching it is the operator's
explicit action.

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on campaign endpoints | Missing bearer token | Re-login. |
| `403` on campaign endpoints | No `campaign/*` grant in the campaign's owner scope | Grants are per-scope; agency-owned vs profile-owned campaigns need grants in the matching scope. |
| `422` on create | Blank name, bad `owner_kind`, unknown channel | The detail names the field. |
| `422` on launch | No steps, or `prospect_pool_id` is not a UUID | Add a step first; check the pool id. |
| `422` on schedule | `schedule_for` not in the future | Pick a future time. |
| Launch `200` but `sent=0` | Empty pool, all members opted out, or members have no email addresses | Check the pool membership and the `opt_outs` table. |
| Launch `200` but some contacts skipped | Opted-out contacts or send failures | `skipped` counts both; the audit detail only has totals — query `email_messages` for per-recipient status. |
| `409` on add step | Duplicate position | Positions are unique per campaign; use the next free one. |
| `200` on opt-out instead of `201` | Already opted out on that channel | Idempotent re-record; not a bug. |
| Campaign stuck `Running` | No scheduler advances campaigns; `Running` persists until pause/complete/cancel | Operate the lifecycle explicitly via the endpoints. |

## Known limitations

- **Launch is not idempotent.** The deterministic idempotency keys
  (`campaign-{id}-step-{id}-contact-{id}`) are generated but never
  read — `EmailServiceImpl::send` ignores `NewEmailMessage.idempotency_key`
  (replay protection lives in the HTTP layer's `IdempotencyStore`,
  which the campaign launch path does not use). A crash mid-launch
  leaves the campaign in its pre-launch state (the row is only
  updated to `Running` after all sends), so re-launching re-sends
  to contacts already mailed. Do not retry a launch that may have
  partially sent; reconcile via `email_messages` first.
- **Call steps are not executed.** No telephony integration
  exists; `Call` steps are recorded and skipped at launch.
- **Wait steps are no-ops.** There is no scheduling daemon;
  `Wait` durations are stored but never slept on.
- **No scheduling daemon.** `Scheduled` campaigns do not
  auto-launch at `schedule_for` — an operator (or a future
  daemon) must call `launch`.
- **No unsubscribe link.** Opt-outs are recorded via the API
  only; sent mail carries no List-Unsubscribe header or link
  (see `threat-model.md`).
- **Bulk send is synchronous.** Launch sends inline in the HTTP
  request; large pools will hold the connection open. A future
  task should move execution to a background worker.
- **Per-send opt-out check, no caching.** Every recipient costs
  a DB round-trip at launch; acceptable at current scale,
  revisit for large pools.
- **No campaign UI.** The Leptos frontend has no campaign,
  pool, or opt-out views — campaigns are API-only until a
  frontend task picks them up.
- **Single-instance everything.** Like the rest of the app, no
  multi-instance coordination for launches (see `database.md`).
