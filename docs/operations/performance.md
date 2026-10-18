# Performance baseline and hardening

How rust-crm performs under its 1/2-core, 2/4 GiB envelope, and the
hardening that keeps it there. Covers the HTTP layer, the database
pool, and the per-endpoint-family cost model.

For the auth model, see `security.md`; for database operations,
`database.md`; for the analytics cost model, `dashboard.md`.

## Resource envelope

The server is designed to run on 1–2 CPU cores with 2–4 GiB of RAM:

- **Tokio multi-thread runtime** (default worker count = cores).
- **Postgres pool: 5 connections** (`crates/crm-db/src/lib.rs`,
  `connect()`). Five is deliberately small: every repository query is
  a short indexed lookup or a bounded `LIMIT` scan, so five
  connections saturate neither the pool nor a small Postgres.
- **Pool acquire timeout: 5s.** When all five connections are busy,
  new requests fail fast with a pool-timeout error (surfaced as a
  `500 problem+json`) instead of queueing indefinitely behind a
  saturated pool.
- **Idle timeout: 300s; max lifetime: 1800s.** Long-lived
  connections are recycled so a stale backend (failover, config
  reload) cannot pin the pool forever.

## Request lifecycle bounds

Applied on the API router (`crates/crm-api/src/main.rs`, `app()`),
outermost-first:

| Layer | Bound | Effect |
|---|---|---|
| Request id + tracing | — | observability, no cost bound |
| Write rate limit | 300 writes/min/IP | `429` + `Retry-After` on floods |
| Security headers | — | response headers only |
| Auth middleware | — | 401 before any handler runs |
| Request timeout | 30 s | `504 problem+json` on expiry |
| Handlers | per-handler | see cost model below |

- **Global request timeout (task 17-PERF-02):** `tower::timeout::TimeoutLayer`
  wrapped in a `HandleErrorLayer` (Axum's `Router::layer` requires the
  error type to convert to `Infallible`; bare `TimeoutLayer` produces
  `Elapsed`) maps expiry to `504 problem+json`. Default 30 s,
  env-tunable via `CRM_REQUEST_TIMEOUT_SECS` (minimum 1 s). The timer
  bounds handler latency, not body streaming.

- **Write rate limiting (task 17-PERF-02):** every `POST`/`PUT`/`PATCH`/
  `DELETE` on `/api/v1/*` consumes from a per-client-IP sliding-window
  bucket (300/min default; env-tunable via `CRM_RATE_LIMIT_WRITE_MAX`
  / `CRM_RATE_LIMIT_WRITE_WINDOW_SECS`); reads are unlimited. The
  middleware sits outside the auth middleware so floods are rejected
  before session lookups. Trips are `429` with `Retry-After` and an
  `auth.rate_limit_tripped` security event (fingerprinted bucket key).
  The login endpoint keeps its own dual-bucket limiter (30/min/IP,
  10/5min/email).

- **Body size:** Axum's default `Json` extractor limit (**2 MiB**)
  applies to every JSON body; larger payloads are rejected with
  `413` before deserialization. No override is configured — the
  default is the bound.
- **List pagination:** every list endpoint caps `limit` at **100**
  rows at the HTTP layer (`query.limit.unwrap_or(20).min(100)`),
  except contacts which validates through `Pagination` (max 100)
  and feed which the domain bounds at 10,000 (HTTP layer caps at
  100 too). An unbounded `limit` can no longer `fetch_all` a whole
  table into memory.
- **Contacts offset:** bounded at 10,000 (`MAX_OFFSET`); the handler
  over-fetches `offset + limit` rows and slices in memory because
  the domain port is limit-only.

## Per-endpoint-family cost model

All repository reads are single indexed queries; there are no N+1
patterns in the service layer (list endpoints issue one `SELECT …
LIMIT`, then hydrate in memory).

| Family | Typical cost | Notes |
|---|---|---|
| `/healthz`, `/readyz` | one `SELECT 1` (readyz) | readyz bounded by a 3s probe timeout |
| Auth (login/logout) | 1–2 indexed lookups + Argon2id verify | Argon2id is the dominant cost by design; login is rate-limited (30/min/IP, 10/5min/email) |
| Contacts / profiles / assets | 1 indexed query per read; lists 1 query + `LIMIT` | merge is the heaviest write (dedupe signal scan) |
| Email ingest webhook | 1 JSON parse + thread lookup + insert | gated by `CRM_EMAIL_WEBHOOK_SECRET`, outside bearer auth |
| Campaigns | 1 query per op; launch is the heaviest (member fan-out) | deterministic idempotency keys; known launch idempotency gap (see `campaigns.md`) |
| Billing | 1–2 queries per op | coverage check before persist; known check-then-act race on `record_payment` |
| Signing / voice | 1–2 queries per op | voice `answer_call` currently unreachable over HTTP |
| Dashboards `run_widget` | 1 aggregate query per widget | SQL built only from hardcoded per-entity allowlists; timeseries zero-filled in memory |
| Admin | 1–2 queries per op | all agency-scoped; health folds `DbHealthReporter` (`SELECT 1`) |
| Audit reads | 1 query + `LIMIT` | chain verification is O(n) over the returned page only |

## Hardening checklist (17-PERF-01)

- [x] List `limit` capped at 100 on every HTTP list endpoint
      (admin, billing, dashboard, feed, signing, voice).
- [x] Pool acquire/idle/lifetime timeouts set; 5-connection cap
      documented.
- [x] Global 30s request timeout on the API router (task
      17-PERF-02): `TimeoutLayer` + `HandleErrorLayer` → `504`
      `problem+json`, env-tunable via `CRM_REQUEST_TIMEOUT_SECS`.
- [x] Per-endpoint write rate limiting (task 17-PERF-02): every
      `POST`/`PUT`/`PATCH`/`DELETE` on `/api/v1/*` consumes from a
      per-client-IP bucket (300/min default, env-tunable via
      `CRM_RATE_LIMIT_WRITE_MAX` / `CRM_RATE_LIMIT_WRITE_WINDOW_SECS`);
      reads are unlimited. Trips are `429` with `Retry-After` and an
      `auth.rate_limit_tripped` audit event.
- [x] No N+1 query patterns found in repository/service layers.
- [x] Admin endpoints verified behind the auth middleware (they
      merge into the authenticated `api` router; the email ingest
      webhook is the only deliberate exception).
- [ ] No load benchmarks have been run; the numbers above are
      reasoned from the query shapes, not measured. A `k6` or
      `oha` pass against a seeded database is the natural
      17-PERF-03 follow-up.

## Known limitations

- `cargo` benchmarks (`criterion`) are not wired up; performance
  claims are architectural, not measured.
- The in-memory vertical slice (tests) does not apply the timeout
  or pool settings — production-only hardening.
- Rate limiters are per-process and in-memory (exact for the
  documented single-instance deployment; a multi-instance deployment
  needs a shared store).
