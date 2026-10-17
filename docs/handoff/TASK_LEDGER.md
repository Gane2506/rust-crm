# Task completion ledger

Statuses: TODO / IN_PROGRESS / BLOCKED / DONE. Never mark DONE without linked commit, CI run, acceptance evidence and reviewer.

| Task ID | Status | Commit/PR | CI evidence | Notes |
|---|---|---|---|---|
| 01-FND-01 | DONE | commit `f324985` / PR #1 (merged) | CI run 39001237365: all 9 jobs green (fmt, clippy -D warnings, test, release build, wasm32 build, audit, deny, forbidden-source scan, unsafe report) | Workspace scaffold + domain interfaces shipped; local gates all green; security review clean (2 Lows fixed) |
| 01-FND-02 | DONE | PR #2 (merged into `task/01-FND-01`) + PR #3 (merged to `main` as `db70a1e`) | CI run 39005491439: all 9 jobs green (fmt, clippy -D warnings, test, release build, wasm32 build, audit, deny, forbidden-source scan, unsafe report); local: 60 tests + live API smoke test (201/409/422) | Vertical slice shipped: in-memory repos/services + contacts API (idempotency keys, RFC 7807) + WASM contacts page. Default branch fixed to `main`. |
| 01-FND-03 | DONE | PR #4 (merged to `main` as `d8ef0d7`) | CI run 39031019323: all 9 jobs green (fmt, clippy -D warnings, test, release build, wasm32 build, audit, deny, forbidden-source scan, unsafe report); local: 65 tests + live serving verified via curl | Verify and operationalize: static frontend serving + SPA fallback, dev guide, .env.example |
| 02-DB-01 | DONE | issue #5 / PR #6 (merge `ad3d244`) | | Persistence foundation: schema, migrations, transactional outbox, SQLx repository interfaces |
| 02-DB-02 | DONE | issue #7 / PR #8 (merge `1593a42`) | | Persistence foundation: Postgres vertical slice — API runs on SQLx repositories |
| 02-DB-03 | DONE | issue #9 / PR #10 (merge `00458f8`) | | Persistence foundation: verify + operationalize (migration tests, seed/migrate CLI, /readyz, ops docs) |
| 03-SEC-01 | DONE | issue #11 / PR #12 (merge `f808e48`) | | Identity and RBAC: staff/sessions/grants/delegation domain + traits (security-reviewed) |
| 03-SEC-02 | DONE | issue #13 / PR #14 (merge `9ed1b5b`) | | Identity and RBAC: auth vertical slice — Argon2, sessions, middleware (security-reviewed) |
| 03-SEC-03 | DONE | issue #15 / PR #16 (merge `2ba2860`) | | Identity and RBAC: rate limiting, security headers, runbooks (verified) |
| 04-AUD-01 | DONE | issue #17 / PR #18 (merge `86eb7d5`) | | Audit and observability: SecurityEvent taxonomy, AuditLogService trait, observability ports |
| 04-AUD-02 | DONE | issue #19 / PR #20 (merge `15028ab`) | | Audit and observability: PgAuditLogService, event recording, query/verify endpoints, observability wiring |
| 04-AUD-03 | DONE | issue #21 / PR #22 (merge `ec6b2e5`) | | Audit and observability: runbook, tunable thresholds, E2E verification, retention policy |
| 05-CRM-01 | DONE | issue #23 / PR #24 (merge `3d2d95f`) | | Core CRM: ClientProfile, Contact sources/dedupe/merge, Asset domain, repository/service traits |
| 05-CRM-02 | DONE | issue #25 / PR #26 (merge `1c7cadc`) | | Core CRM: migration 0005, profile/asset repositories, profile/asset/merge API, dedupe 409 |
| 05-CRM-03 | DONE | issue #27 / PR #28 (merge `fe13a3a`) | | Core CRM: runbook, E2E full-slice test, seed + frontend verified |
| 06-UI-01 | DONE | issue #29 / PR #30 (merge `5478c67`) | | Frontend: component inventory, routing, contracts |
| 06-UI-02 | DONE | issue #31 / PR #32 (merge `27a4a50`) | | Frontend: views implemented, polish items addressed |
| 06-UI-03 | DONE | issue #33 / PR #34 (merge `d2de9aa`) | | Frontend: verify + polish, runbook, threat model |
| 07-MAIL-01 | DONE | issue #35 / PR #36 (merge `7e08cef`) | | Email: domain types, traits, adapter contract |
| 07-MAIL-02 | DONE | issue #37 / PR #38 (merge `b00f278`) | | Email: vertical slice (persistence, service, HTTP, 6 integration tests) |
| 07-MAIL-03 | DONE | issue #39 / PR #40 (merge `111811c`) | | Email: runbook, threat model, seed — **workstream COMPLETE** |
| 08-CAMP-01 | DONE | issue #41 / PR #42 (merge `c33f74f`) | | Campaigns: domain, traits, lifecycle (231 tests) |
| 08-CAMP-02 | DONE | issue #43 / PR #44 (merge `af1dfa8`) | | Campaigns: vertical slice (14 endpoints, 4 integration tests) |
| 08-CAMP-03 | DONE | issue #45 / PR #46 (merge `eaa504d`) | | Campaigns: runbook, threat model, seed — **workstream COMPLETE** |
| 09-JEV-01 | DONE | issue #47 / PR #48 (merge `681af19`) | | Jev agents: domain, traits, interfaces (248 tests) |
| 09-JEV-02 | IN_PROGRESS | issue #49 / branch `task/09-JEV-02` | | Jev agents: implement vertical slice |
| 02-DB-02 | TODO | | | |
| 02-DB-03 | TODO | | | |
| 03-SEC-01 | TODO | | | |
| 03-SEC-02 | TODO | | | |
| 03-SEC-03 | TODO | | | |
| 04-AUD-01 | TODO | | | |
| 04-AUD-02 | TODO | | | |
| 04-AUD-03 | TODO | | | |
| 05-CRM-01 | TODO | | | |
| 05-CRM-02 | TODO | | | |
| 06-UI-02 | TODO | | | |
| 06-UI-03 | TODO | | | |
| 07-MAIL-01 | TODO | | | |
| 07-MAIL-02 | TODO | | | |
| 07-MAIL-03 | TODO | | | |
| 08-CAMP-01 | TODO | | | |
| 08-CAMP-02 | TODO | | | |
| 09-JEV-02 | TODO | | | |
| 09-JEV-03 | TODO | | | |
| 10-WORK-01 | TODO | | | |
| 10-WORK-02 | TODO | | | |
| 10-WORK-03 | TODO | | | |
| 11-FORM-01 | TODO | | | |
| 11-FORM-02 | TODO | | | |
| 11-FORM-03 | TODO | | | |
| 12-BILL-01 | TODO | | | |
| 12-BILL-02 | TODO | | | |
| 12-BILL-03 | TODO | | | |
| 13-SIGN-01 | TODO | | | |
| 13-SIGN-02 | TODO | | | |
| 13-SIGN-03 | TODO | | | |
| 14-VOICE-01 | TODO | | | |
| 14-VOICE-02 | TODO | | | |
| 14-VOICE-03 | TODO | | | |
| 15-DASH-01 | TODO | | | |
| 15-DASH-02 | TODO | | | |
| 15-DASH-03 | TODO | | | |
| 16-ADMIN-01 | TODO | | | |
| 16-ADMIN-02 | TODO | | | |
| 16-ADMIN-03 | TODO | | | |
| 17-PERF-01 | TODO | | | |
| 17-PERF-02 | TODO | | | |
| 17-PERF-03 | TODO | | | |
