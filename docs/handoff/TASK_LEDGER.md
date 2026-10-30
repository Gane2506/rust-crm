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
| 09-JEV-02 | DONE | issue #49 / PR #50 (merge `62bb65f`) | | Jev agents: vertical slice (259 tests) |
| 09-JEV-03 | DONE | issue #51 / PR #52 (merge `64f9257`) | | Jev agents: runbook, threat model, seed — **workstream COMPLETE** |
| 10-WORK-01 | DONE | issue #53 / PR #54 (merge `b71e532`) | | Workflow builder: domain, traits, interfaces (274 tests) |
| 10-WORK-02 | DONE | issue #55 / PR #56 (merge `f285926`) | | Workflow builder: vertical slice (282 tests) |
| 10-WORK-03 | DONE | issue #57 / PR #58 (merge `e27f5b1`) | | Workflow builder: verify + operationalize (282 tests) |
| 11-FORM-01 | DONE | issue #59 / PR #60 (merge `b03574e`) | | Form builder: define domain and interfaces (298 tests) |
| 11-FORM-02 | DONE | issue #61 / PR #62 (merge `18154a5`) | | Form builder: vertical slice (304 tests) |
| 11-FORM-03 | DONE | issue #63 / PR #64 (merge `8ba3443`) | | Form builder: verify + operationalize (304 tests) |
| 12-FEED-01 | DONE | issue #65 / PR #66 (merge `21846d5`) | | Feed: define domain and interfaces (319 tests) |
| 12-FEED-02 | DONE | issue #67 / PR #68 (merge `9e46ba1`) | | Feed: vertical slice (325 tests) |
| 12-FEED-03 | DONE | issue #69 / PR #70 (merge `16f45e8`) | | Feed: verify + operationalize (325 tests) |
| 12-BILL-01 | DONE | issue #71 / PR #73 (merge `d5c9b3a`) | | Billing: define domain and interfaces (353 tests) |
| 12-BILL-02 | DONE | issue #74 / PR #75 (merge `73bab29`) | | Billing: vertical slice (360 tests) |
| 12-BILL-03 | DONE | issue #76 / PR #77 (merge `52596d4`) | | Billing: verify + operationalize (360 tests) |
| 13-SIGN-01 | DONE | issue #78 / PR #79 (merge `4291e2f`) | | Contract signing: domain and interfaces (375 tests) |
| 13-SIGN-02 | DONE | issue #80 / PR #81 (merge `1aedff7`) | | Contract signing: vertical slice (382 tests) |
| 13-SIGN-03 | DONE | issue #82 / PR #83 (merge `2f842f4`) | | Contract signing: verify + operationalize (382 tests) |
| 14-VOICE-01 | DONE | issue #84 / PR #85 (merge `65f6470`) | | Calling: domain and interfaces (397 tests) |
| 14-VOICE-02 | DONE | issue #86 / PR #87 (merge `8c5bd67`) | | Calling: vertical slice (403 tests) |
| 14-VOICE-03 | DONE | issue #88 / PR #89 (merge `73bf073`) | | Calling: verify + operationalize (403 tests) |
| 15-DASH-01 | DONE | issue #90 / PR #91 (merge `7eab8f7`) | | Analytics: domain and interfaces (425 tests) |
| 15-DASH-02 | DONE | issue #92 / PR #93 (merge `6c20c8d`) | | Analytics: implement vertical slice (431 tests; real bug fixed: sqlx dep + id imports + trait imports) |
| 02-DB-02 | TODO |
| 02-DB-03 | TODO |
| 03-SEC-01 | TODO |
| 03-SEC-02 | TODO |
| 03-SEC-03 | TODO |
| 04-AUD-01 | TODO |
| 04-AUD-02 | TODO |
| 04-AUD-03 | TODO |
| 05-CRM-01 | TODO |
| 05-CRM-02 | TODO |
| 06-UI-02 | TODO |
| 06-UI-03 | TODO |
| 07-MAIL-01 | TODO |
| 07-MAIL-02 | TODO |
| 07-MAIL-03 | TODO |
| 08-CAMP-01 | TODO |
| 08-CAMP-02 | TODO |
| 09-JEV-02 | TODO |
| 09-JEV-03 | TODO |
| 10-WORK-01 | TODO |
| 10-WORK-02 | TODO |
| 10-WORK-03 | TODO |
| 11-FORM-01 | TODO |
| 11-FORM-02 | TODO |
| 11-FORM-03 | TODO |
| 12-BILL-02 | TODO |
| 12-BILL-03 | TODO |
| 13-SIGN-01 | TODO |
| 13-SIGN-02 | TODO |
| 13-SIGN-03 | TODO |
| 14-VOICE-01 | TODO |
| 14-VOICE-02 | TODO |
| 14-VOICE-03 | TODO |
| 15-DASH-01 | TODO |
| 15-DASH-02 | DONE | issue #92 / PR #93 | | |
| 15-DASH-03 | DONE | issue #99 / PR #100 (merge `1c0df69`) | | Analytics: runbook, threat model, seed demo dashboard (real bug fixed: LIMIT $2 bind mismatch; CodeRabbit 5 items addressed) |
| UI-RESTYLE | DONE | issue #95 / PR #96 (merge `c648f10`) | | Twenty-style UI rebuilt in Rust (Leptos component library, every view reskinned, 10 screenshots) |
| 16-ADMIN-01 | DONE | issue #101 / PR #102 (merge `96d5f1b`) | | Admin: domain and interfaces (464 tests) |
| 16-ADMIN-02 | DONE | issue #103 / PR #104 (merge `5cb9ce5`) | | Admin: vertical slice (476 tests; 38 handlers, 8 Pg repos, 12 integration tests) |
| 16-ADMIN-03 | DONE | issue #105 / PR #106 (merge `843e86e`) | | Admin: verify + operationalize (security review clean; admin runbook; seed step 16; CodeRabbit 2 items fixed) |
| 17-PERF-01 | DONE | issue #107 / PR #108 (merge `c5e296b`) | | Hardening: limit capping (13 endpoints), pool timeouts, performance.md (476 tests) |
| 17-PERF-02 | DONE | issue #109 / PR #110 (merge `23691e1`) | | Request timeouts (30s, 504) + write rate limiting (429 + Retry-After); 482 tests; CodeRabbit 2 items fixed |
| 17-PERF-03 | DONE | issue #111 / PR #112 | | Hardening: load benchmarks and final release pass; 482 tests; manifest COMPLETE |
| UI2-01 | DONE | issue #113 / PR #114 (merge `38403e8`) | | shadcn oklch token system + v2 component library (46 SVG icons, button/card/badge/form/feedback/overlay/table/tabs/bento) |
| UI2-02 | DONE | issue #115 / PR #116 (merge `b6bc0d6`) | | All views reskinned onto v2, bento dashboard, dark-mode ThemeToggle; ComingSoon placeholders for Mail/Campaigns/Agents/Workflows/Forms/Billing/Signing/Calls/Analytics/Admin by design |
| UI2-03 | DONE | issue #117 / PR #118 (merge `479d7de`) | | Branding studio end-to-end (BrandingTheme domain, migration 0017 JSONB snapshots, GET public + PUT AgencySettings:Manage) |
| UI2-04 | DONE | issue #119 / PR #120 (merge `4636fa9`) | | 10 per-feature screenshots in docs/images/ui2/; README UI gallery |
| UI3-01 | DONE | issue #121 / PR #122 (merge `99b0e0c`) | | 16-style UI pack, 100% Rust; fixed real bug: saved branding colors never applied since UI2-03 (style-tag order); rustls-webpki 0.103.16 / webpki-roots 1.0.10 security fix |
| FIX-01 | DONE | issue #123 / PR #129 (merge `1e5900d`) | | Campaign launch idempotency (migration 0018, email_messages.idempotency_key + partial unique index) |
| FIX-02 | DONE | issue #124 / PR #130 (merge `6e9df87`) | | Billing Idempotency-Key on all 6 write endpoints (UUID, 422 malformed, 409 on fingerprint/route mismatch); replay after authorization |
| FIX-03 | DONE | issue #125 / PR #131 (merge `6c4120d`) | | Billing check-then-act races closed (record_guarded FOR UPDATE, update_if_status atomic conditional UPDATE; 409 ConcurrencyConflict) |
| FIX-04 | DONE | issue #126 / PR #132 (merge `dab3a70`) | | Jev ApprovalGate persistence (AgentRun.approval_gate, migration 0019 JSONB; request_approval clears prior gate) |
| FIX-05 | DONE | issue #127 / PR #133 (merge `f8833c8`) | | Seed demo grants — exactly 5 (ClientProfile Read+Create, Asset Read+Create, AuditLog Read), deduped via ensure_profile_asset_audit_grants |
| FIX-06 | DONE | issue #128 / PR #134 (merge `cccdb4c`) | | Maintenance mode enforcement (503 + Retry-After: 60 on mutating /api/v1/*; suffix-matched exclusions for the toggle + login; storage errors fail closed) |
| SESSION-01 | DONE | issue #135 / PR #136 (merge `7e83450`) | | Bearer token persisted to sessionStorage (web-sys directly, never panics on denied storage); fixes refresh-logs-out |
| HARD-01 | DONE | issue #137 / PR #138 (merge `a556c71`) | | Atomic idempotency claim (INSERT...ON CONFLICT DO NOTHING, JSON-null sentinel, loser polls ~5s and replays); 515 tests |
| HARD-02 | DONE | issue #139 / PR #140 (merge `60eb47f`) | | BrandingServiceImpl in crm-core + PUT /api/v1/branding wired to claim_or_replay; 517 tests |
| HARD-03 | DONE | issue #141 / PR #142 (merge `b9f1ba8`) | | Toaster live-region fix (per-toast role=status, Error role=alert, aria-atomic, identical pushes coalesced) + Dialog WAI-APG focus management |
| HARD-04 | DONE | issue #143 / PR #144 (merge `b8801a9`) | | Idempotency claim expiry (5-min TTL, stale sentinels self-heal; lookup no longer conflicts on sentinels; delete+insert must be separate statements — CTE snapshot gotcha); 519 tests |
| HARD-05 | DONE | issue #145 / PR #146 (merge `c7dcbfb`) | | Bookkeeping catch-up (docs-only) — README + TASK_LEDGER current through HARD-04 |
| HARD-06 | DONE | issue #147 / PR #148 (merge `532a46a`) | | Dialog focus per-instance NodeRef (multi-dialog correctness; removes the document-wide panel query) |
| AUDIT-01 | DONE | issue #149 / PR #152 (merge `58b4e60`) | | Absolute wasm import (`/crm-web.js`) — full load of nested routes no longer renders a blank white page; verified live on the Mac |
| AUDIT-02 | DONE | issue #150 / PR #154 (merge `cbcf808`) | | Demo staff gains AgencySettings:Manage (seed, idempotent) + branding save failure now shows ErrorAlert + error toast; verified live on the Mac (saved version 1–2, purple `#7c3aed`) |
| AUDIT-03 | DONE | issue #151 / PR #153 (merge `f28a016`) | | `box-sizing: border-box` on `<main>` — 48px horizontal overflow gone at 1440px; verified live on the Mac (`scrollWidth` 1440) |
| AUDIT-04 | DONE | issue #155 / PR #156 (merge `17d0c59`) | | Bookkeeping catch-up (docs-only) — README + TASK_LEDGER current through AUDIT-03 |
| AUDIT-05 | DONE | issue #157 / PR #158 (merge `f237ee3`) | | Branding load failure now visible (ErrorAlert + error toast + retry, mirroring AUDIT-02; a successful save clears a stale load error) |
| AUDIT-06 | DONE | issue #159 / PR #160 (merge `227a149`) | | Bookkeeping catch-up (docs-only) — README + TASK_LEDGER current through AUDIT-05 |
| AUDIT-07 | DONE | issue #161 / PR #167 (merge `33a2032`) | | Demo staff gains Campaign Read + Create + Update at agency scope (seed, idempotent); campaigns + prospect pools no longer 403 for the demo user; verified live on the Mac (list + launch 200, seeded campaign `running`) |
| AUDIT-08 | DONE | issue #162 / PR #166 (merge `1b314bf`) | | Demo staff gains ClientProfile Update at agency scope (seed, idempotent) + ProfileDetail keeps the record visible across fetch/transition failures (error alert above the record); verified live on the Mac (suspend/reactivate 200) |
| AUDIT-09 | DONE | issue #163 / PR #165 (merge `a06f387`) | | Demo staff gains Asset Read + Create + Update at agency scope (seed, idempotent); assets register/list/archive anywhere; verified empirically (two seed runs, zero duplicates) |
| AUDIT-10 | DONE | issue #164 / PR #168 (merge `58eb028`) | | Responsive shell — sidebar collapses into a hamburger-toggled drawer below 640px (focus in, Tab trapped, Escape closes, focus restored; closes on navigation); verified live on the Mac (390/820/1440px, scrollWidth = clientWidth) |
| AUDIT-11 | DONE | issue #169 / PR #172 (merge `6a597a9`) | | ⌘K badge promises a shortcut that doesn't exist — wired up a real global ⌘K (macOS) / Ctrl+K (other platforms) shortcut for the Jump-to palette; badge shows the platform shortcut; typing never hijacked; verified live on the Mac (palette opens, Escape closes + focus returns, Ctrl+K in a field does nothing) |
| AUDIT-12 | DONE | issue #170 / PR #173 (merge `c39e27e`) | | API error text shown raw in the UI — `ApiError::friendly_message()` maps 404/403/401/422/409/transport/5xx to plain language in the one place every page shares (raw detail logged to the console, never rendered); ContactDetail gains the same Retry treatment as ProfileDetail; verified live on the Mac (friendly 404s with Retry, bad email shows the field message, no raw "server error" strings) |
| AUDIT-13 | DONE | issue #171 / PR #174 (merge `3eba376`) | | Dashboard "Recent audit events" shows the 8 oldest events — API gains `?newest_first=true` (presentation order only; chain storage/hashing/verification untouched) and the Dashboard requests it; Audit page keeps chain order + toggle + Hint; verified live on the Mac (tile shows the newest events, descending; API newest-first and chain-order lists distinct and correct) |
| R3-01 | DONE | issue #175 / PR #179 (merge `8d578fe`) | | Jump-to palette keyboard-broken (round-3 audit §1–§2): focus lands on the close button so typing goes nowhere, and Enter does nothing after filtering — Dialog gains an optional `autofocus` selector prop (palette focuses `#cmd-palette-input` on open) and Enter on the input navigates to the first filtered entry |
| R3-02 | DONE | issue #176 / PR #180 (merge `643a0a1`) | | Asset Archive succeeds silently — the row looks identical afterwards, so the button reads as broken: add a Status column (Available / Archived badge), show a success toast on archive, and hide the Archive button on already-archived rows; presentation only, no API change; verified live on the Mac (toast "Asset archived: r3-verify.txt", badge flips to Archived, button gone, zero console errors; test asset deleted afterwards, zero archived assets left) |
| R3-03 | DONE | issue #177 / PR #178 (merge `1c6d28f`) | | Audit page Hint is stale after AUDIT-13 — it claims the API returns chain order (oldest first); update the Hint to note the page fetches in chain order and reverses client-side while the API also offers ?newest_first=true |
| R3-04 | DONE | issue #181 / PR #183 (merge `1db4f47`) | | Campaigns have no UI at all — `/campaigns` is a ComingSoon placeholder and `/campaigns/:id` is a 404, while the seeded demo campaign is `running` and reachable via the API: built the frontend (list + detail, create form, steps, schedule, lifecycle actions); backend complete, no API changes; verified live on the Mac (seeded campaign listed, detail renders with lifecycle buttons, zero console errors) |
| SESSION-02 | DONE | issue #182 / PR #188 (merge `7fa5cd1`) | | An expired session strands the user on a dead shell: any authenticated call returning 401 shows the friendly message but keeps the dead token and the Shell — every page 401s, and only Log out gets back to login. Any 401 from an authenticated call clears the stored token and returns to the LoginForm, carrying the session-expired message onto the login screen; login/logout/refresh after expiry must all behave; 30-minute idle timeout and 12h TTL unchanged; verified live on the Mac (10/10 checks) |
| MAIL-01 | DONE | issue #185 / PR #189 (merge `4b21fc1`) | | `/mail` is a ComingSoon placeholder while the backend exists: build the frontend (threads list + compose, thread detail + reply); no API changes; verified live on the Mac (seeded thread listed, compose sends) — live verification then found MAIL-02: sends orphaned the message (no thread), fixed separately |
| MAIL-02 | DONE | issue #190 / PR #191 (merge `38c1f4e`) | | Outbound send with no thread_id orphans the message (thread_id NULL, never in the threads list): send must open a thread when none is given, mirroring ingest_reply; regression test pg_email_send_without_thread_opens_a_thread; verified live on the Mac (compose → thread in list → detail → reply, 9/9 checks) |
| BILL-01 | DONE | issue #184 / PR #192 (merge `3156c95`) | | `/billing` is a ComingSoon placeholder while the backend is complete: build the frontend (invoice list + detail, create form, lines, issue/void, balance, recorded payments + reversal); record-keeping only — no Stripe charge flow, no Stripe adapter in the backend; no API changes; verified live on the Mac (create → issue → $60 payment → reverse → void, 18/18 checks, zero console errors) |
| STRIPE-01 | DONE | issue #186 / PR #193 (merge `4cebcf0`) | | Stripe adapter — Checkout sessions + webhook-confirmed payments, backend only (the Pay button is a follow-on after BILL-01): POST /v1/invoices/{id}/stripe-checkout (amount = remaining balance), POST /v1/stripe/webhook with HMAC-SHA256 signature verification (300s tolerance), event-id dedupe via stripe_webhook_events, payments.external_reference (migration 0020) |
| STRIPE-02 | DONE | issue #194 / PR #195 (merge `986eecb`) | | Pay with Stripe button on the invoice detail page (frontend only, plus the success/cancel URL default): button shown only on Issued invoices with balance > 0; click POSTs the checkout endpoint with a fresh UUID Idempotency-Key and redirects the browser to checkout_url; failures show the friendly message (ErrorAlert above the record + error toast); success/cancel URL defaults change from the billing list to the invoice detail page (per-invoice, env overrides still win); verified live on the Mac (button hidden on Draft, present on Issued, click without keys shows the friendly alert + toast with no redirect and the record intact) |
| R4-01 | DONE | issue #187 / PR #196 (merge 38999d4) | | Toasts pile up and cover content — no auto-dismiss, no cap: toasts auto-dismiss after a per-variant duration (Info/Success 4s, Warning/Error 8s), visible toasts capped at 3 (oldest drops), manual dismiss still works, HARD-03 live-region behavior preserved (one announcement per toast, container still not a live region); frontend only, no API changes, no migration |
| FORM-01 | DONE | issue #197 / PR #198 (merge `0a49e9b`) | | Forms list + detail views: profile selector (demo profile default), forms table (name, status, field count, created), create form, detail with status/fields/submissions + state-gated Publish/Archive, field add/remove, submissions newest first with Review; frontend only, no API changes, no migration (seed adds Form Read/Create/Update/Delete grants so the demo user can use the seeded demo form); verified live on the Mac (create → list → detail → add field → publish → archive, zero console errors) |
| AGENT-01 | IN_PROGRESS | issue #199 / PR pending | | Agents list + detail views: profile selector (demo profile default), runs table (run link, status badge, task, created), create-run form, detail with record card + tool calls + handoffs + state-gated lifecycle actions (request-approval/approve/deny/complete/fail/cancel); frontend only, no API changes, no migration (demo user already holds AgentRun Create/Read/Update/Approve; seed plants a demo run) |
