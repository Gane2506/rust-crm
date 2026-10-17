# Database operations — PostgreSQL

The Rust CRM stores everything in PostgreSQL 16. This page is the operator
runbook: configuration, migrations, seeds, backup/restore, and health
checks.

## Configuration reference

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `DATABASE_URL` | yes (server, tests) | — | Postgres connection URL, e.g. `postgres://postgres:postgres@localhost:5432/crm_test` |
| `CRM_SEED_DEV` | no | unset | Set to `1` to let the dev seed write (see Seeds) |
| `CRM_API_ADDR` | no | `127.0.0.1:3000` | API bind address |
| `RUST_LOG` | no | `crm_api=info,tower_http=info` | Log filter |

Copy `.env.example` to `.env` for local work. `DATABASE_URL` is the only
database setting: pool sizing and timeouts are code-level (see below), not
env-driven, until task 16-ADMIN wires centralized configuration.

### Connection pool

`crm_db::create_pool()` builds the SQLx pool with `max_connections(5)`.
Guidance:

- **Local dev / tests:** 5 is plenty; each integration test opens its own
  database on the same server.
- **Single-instance production:** raise to roughly `2 × CPU cores` of the
  API host, never above Postgres's `max_connections` (default 100) minus
  headroom for `psql`/maintenance.
- The pool is created once at startup and shared by every repository; there
  is one pool per `crm-api` process.

## Migrations

Migration files live in `crates/crm-db/migrations/` as versioned pairs:
`NNNN_name.sql` (up) plus `NNNN_name.down.sql` (down). They are embedded
into the `crm-db` binary at compile time via `sqlx::migrate!`, so the
running server always carries the exact schema it needs — no external
migration files to deploy.

Rules (enforced by review, not tooling):

- Migrations are **additive**: new tables/columns/indexes only. Destructive
  changes need a reviewed ADR and a multi-release expand-migrate-contract
  plan.
- Every up migration ships a **down migration** that reverses it.
- Never edit an applied migration; add a new version instead.

### Apply (forward)

The API server applies pending migrations automatically at startup
(`crm_db::run_migrations`), tracking state in `_sqlx_migrations`.
Re-running is a no-op. To apply manually:

```sh
DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example migrate -- up
```

### Inspect

```sh
DATABASE_URL=... cargo run -p crm-db --example migrate -- status
```

Lists applied versions oldest-first from the `_sqlx_migrations` journal.

### Roll back

```sh
DATABASE_URL=... cargo run -p crm-db --example migrate -- down 1
```

Runs the down migrations back to (excluding) the target version;
`down 0` rolls back everything. The explicit target version is the guard
against accidental use — down migrations are destructive. Rollback is
verified by the automated `migrations_apply_up_and_roll_back_down` test
in `crates/crm-db/tests/migrations_verify.rs` (up → journal check →
full rollback → table check → re-apply).

## Seeds (development only)

The dev seed inserts the demo client profile plus five sample contacts
through the real service layer (normalization, dedupe and audit events
behave exactly as in production). It is **idempotent**: re-running skips
contacts whose normalized email already exists.

```sh
docker compose up -d
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

- Without `CRM_SEED_DEV=1` the seed prints its plan and exits **without
  writing anything** — it can never seed a database by accident.
- Migrations run first, so a fresh database works.
- The demo profile id matches the `crm-web` vertical slice's demo tenant
  (`123e4567-e89b-42d3-a456-426614174000`) until real auth/tenancy lands
  in 03-SEC.
- Seeds never run automatically at server startup.

## Backup and restore

Backups use `pg_dump` against the compose database (adjust host/user/db
for managed Postgres):

```sh
# Full logical backup (schema + data, including _sqlx_migrations)
docker compose exec -T postgres \
    pg_dump -U postgres -F c -f /tmp/crm_backup.dump crm_test
docker compose cp postgres:/tmp/crm_backup.dump ./crm_backup.dump

# Restore into a fresh database
docker compose exec -T postgres psql -U postgres -c 'CREATE DATABASE crm_restore;'
docker compose cp ./crm_backup.dump postgres:/tmp/crm_backup.dump
docker compose exec -T postgres \
    pg_restore -U postgres -d crm_restore /tmp/crm_backup.dump
```

Recovery notes:

- The backup includes the `_sqlx_migrations` journal, so a restored
  database resumes migrations exactly where the source left off.
- `audit_events` is append-only by domain contract; restores must preserve
  the full chain or `verify_chain` will fail on the gap.
- Retention, encryption at rest, and offsite copies are deployment
  concerns (task 16-ADMIN); this procedure covers the mechanism.

## Health checks

- `GET /healthz` — process liveness. Always `200 {"status":"ok"}` while
  the server runs. Use for "is the process up".
- `GET /readyz` — readiness. Runs `SELECT 1` against the backing store:
  `200 {"status":"ok","database":"ok"}` when reachable,
  `503` `application/problem+json` when not. **Gate traffic and
  orchestrator health checks on `/readyz`**, not `/healthz` — the API
  cannot serve without PostgreSQL.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `DATABASE_URL must be set` at startup | env not exported / `.env` not loaded | Set it; see `.env.example` |
| `database setup failed` at startup | Postgres down, wrong credentials, migration error | `docker compose up -d`; check the logged cause |
| `/readyz` returns 503 | DB unreachable or overloaded | Check Postgres, pool sizing, network |
| Integration tests panic `DATABASE_URL must be set` | tests need a live DB | `docker compose up -d` with `DATABASE_URL` set |
| `DROP DATABASE` fails with 55006 in tests | stray backend (shouldn't happen) | fixed via `pg_terminate_backend` in test teardown |
