# Threat model: authentication and authorization boundary

Concise statement of what the auth boundary (tasks 03-SEC-01 through
03-SEC-03) enforces, what it assumes, and what it does not yet cover.
The operations companion is `security.md`.

## Trust boundaries

```
untrusted network ──▶ reverse proxy (TLS) ──▶ crm-api (127.0.0.1:3000)
                                                  │ SQLx
                                                  ▼
                                              PostgreSQL 16
```

- **Clients** (browsers, API consumers) are untrusted: every request is
  authenticated and authorized server-side; the client is never trusted
  with scope decisions.
- **The reverse proxy** is trusted: it terminates TLS and is the only
  path to `crm-api`. `X-Forwarded-For` is honored for rate limiting on
  that basis.
- **The operator** (whoever holds database and shell access) is trusted:
  provisioning, grant assignment, and incident response are SQL-driven
  today.
- **PostgreSQL** is trusted storage: the audit chain, sessions, and
  grants are only as safe as database access control.

## What the boundary enforces

- **Authentication:** Argon2id password hashing (default params, blocking
  pool, constant-time verify); opaque 122-bit session tokens, of which
  only the SHA-256 hash is persisted; the token is rendered once, in the
  login response, and never logged.
- **No oracles:** unknown email, wrong password, suspended account, and
  credential-less account all cost one Argon2 verification and return the
  same `401`. Missing/malformed/unknown/expired/revoked/idle tokens are
  one indistinguishable `401`. Logout is `204` even for unknown tokens.
- **Authorization:** every `/api/v1/*` route except login passes the auth
  middleware; handlers authorize each action against the caller's grants
  within an agency-anchored scope. Deny by default; cross-agency access
  is impossible by construction (`Scope::covers`).
- **Delegation safety:** the delegation ceiling is enforced at creation;
  revocation cascades through `effective_grants`; delegation-linked
  sessions fail closed until the delegation repository exists.
- **Brute-force mitigation:** per-IP (30/60 s) and per-email (10/5 min)
  sliding-window limits on login; `429` + `Retry-After` on exhaustion;
  fail-closed on limiter failure.
- **Response hygiene:** defensive security headers on all API responses
  (`nosniff`, `DENY` framing, `no-referrer`, locked-down
  `Permissions-Policy`); RFC 7807 errors never echo secrets.
- **Session hygiene:** 12 h default TTL, 7 d hard cap, 30 min sliding
  idle timeout, revocation timestamps (no deletes), suspension fails
  closed.

## Assumptions (out of scope for the app server)

- **TLS termination upstream.** `crm-api` binds loopback and speaks
  plaintext; confidentiality in transit is the reverse proxy's job. The
  server intentionally does not emit HSTS.
- **Single instance.** The login rate limiter is in-memory and
  per-process. Behind N instances an attacker gets ~N× the budget.
- **Database access control.** Anyone with write access to PostgreSQL
  can mint sessions, alter grants, and rewrite history — the audit hash
  chain detects tampering after the fact (`verify_chain`), it does not
  prevent it.
- **Secret handling.** `DATABASE_URL` and operator credentials live in
  the deployment's secret management, never in the repo or chat.
- **Clock.** Session expiry and idle timeout assume a sane system clock.

## Known limitations (honest gaps)

1. **No `DelegationRepository` trait yet.** Agent sessions (delegation-
   linked) authorize nothing — they fail closed with `401`. The
   `delegations` table is forward schema for the agent workstream.
2. **Session refresh is service-level only.** `AuthenticationService::
   refresh` exists but is not exposed over HTTP; clients re-login after
   expiry. (The idle touch keeps active sessions alive.)
3. **No MFA.** Password-only authentication; phishing a password phishes
   the account. MFA is future work.
4. **No admin provisioning endpoint.** Staff, grants, and revocations
   outside self-service logout are SQL operations by a trusted operator
   (see `security.md`).
5. **In-memory rate limiter.** See single-instance assumption above.
6. **No login anomaly alerting.** Repeated 401/429 bursts are visible in
   logs/traces but nobody is paged; alerting belongs to the observability
   workstream (04-AUD).
7. **Argon2 parameters are defaults.** Tuned for general use, not for
   this hardware; revisit under load testing.

## What is explicitly not defended here

- Application-layer attacks on future endpoints (injection, etc.) —
  each workstream owns its input validation.
- Client-side attacks (XSS in the SPA) — the UI workstream's scope
  (see "Frontend (SPA) security posture" below).
- Denial of service beyond login rate limiting.
- Insider threat from the trusted operator or database superuser.

## Email surface

The email vertical slice (tasks 07-MAIL-01 through 07-MAIL-03)
adds an inbound webhook and PII-bearing storage to the boundary:

- **Webhook secret is the only gate on ingest.** `POST
  /api/v1/emails/ingest` is not bearer-authenticated (providers
  cannot hold staff sessions); possession of
  `CRM_EMAIL_WEBHOOK_SECRET` (presented in the `x-webhook-secret`
  header) is the entire authentication. Anyone holding the secret
  can inject inbound mail into any client profile named in the
  request body — the profile id is caller-supplied, so the secret's
  confidentiality *is* the tenant boundary for ingestion. Rotation:
  generate a new high-entropy value, update the provider's webhook
  configuration, then restart `crm-api` (the secret is read from
  the process environment per request; there is no reload
  endpoint). A leaked secret is an incident: rotate immediately and
  review `email.received` audit events for forged ingestion.
- **Fail-closed when unconfigured.** With no secret set, ingest
  returns `503` on every request — ingestion stops rather than
  running open. Do not "fix" a 503 ingest by bypassing the check.
- **Spoofing / inbound forgery.** The server does not verify SPF,
  DKIM, or DMARC — it trusts whatever the provider's webhook
  delivers. Forgery *before* the provider (spoofed mail accepted
  by Gmail/Zoho) is the provider's problem; forgery *of* the
  webhook (direct POSTs) is stopped only by the secret. There is no
  second factor on this path by design (providers can't do
  better); the mitigation is secret hygiene plus audit review.
- **PII in subjects and bodies.** Email content is PII by nature
  and is stored plaintext in `email_messages` (subject,
  `body_text`, `body_html`). Access is gated by profile-scoped
  `Email` / `EmailThread` grants and every send/receive/failure is
  audited (`email.sent`, `email.received`, `email.failed`) — but
  anyone with database read access sees everything, same as
  contacts. No field-level encryption is planned.
- **No HTML sanitization.** `body_html` is stored exactly as
  received and served back verbatim. Any future renderer (the
  Leptos frontend has no email UI yet) must treat it as untrusted
  and escape or sandbox it — rendering stored HTML unsanitized
  would be stored XSS.
- **Provider trust boundary.** Today the provider is
  `StubEmailProvider` (in-memory, no network) — outbound mail goes
  nowhere, which is the safe default. Real Gmail/Zoho adapters
  (later task) move the trust boundary outward: provider
  credentials become high-value secrets, a compromised provider
  account can send mail as the agency, and provider-returned
  content (subjects, bodies, thread ids) becomes untrusted input
  to the thread-matching cascade. The cascade's exact-match steps
  trust provider-supplied ids; a malicious provider could thread
  mail arbitrarily — contained per profile by the tenant scope.
- **Timing side-channel on the secret check.** The comparison is a
  length check plus short-circuiting byte equality, not
  constant-time. Accepted: the secret is high-entropy, so the
  leak is negligible compared to the operational risk of secret
  mishandling. Do not weaken this further (e.g. prefix compares).
- **Denial of service.** Ingest is unauthenticated-by-bearer and
  does one DB round-trip per request plus audit; there is no rate
  limiter on it (the login limiter does not apply). A flood of
  webhook calls with a valid secret is bounded only by the
  database. Rate limiting the webhook is future work.

## Campaigns surface

The campaigns vertical slice (tasks 08-CAMP-01 through 08-CAMP-03)
adds bulk sending and PII-bearing campaign content to the
boundary:

- **Bulk-send abuse.** `POST /api/v1/campaigns/{id}/launch`
  sends one email per pool member, inline in the request. A
  compromised staff account holding `campaign/update` on a
  profile can spam that profile's entire prospect pool — the
  per-launch cost is bounded only by pool size. Mitigations in
  place: profile-scoped grants (a compromised account can only
  reach profiles it holds grants on), the `campaign.launched` /
  `campaign.bulk_send_completed` audit trail with sent/skipped
  counts, and opt-out enforcement (opted-out contacts are never
  mailed, even by an attacker). Missing: per-campaign or
  per-profile send rate limits, pool-size caps, and anomaly
  detection on send volume (a `campaign.bulk_send_completed`
  with an unusual `sent=` count is the signal to watch today).
  Do not expose launch to broader roles without these.
- **Opt-out compliance.** Sending to an opted-out contact is a
  legal risk (CAN-SPAM / CASL / GDPR territory depending on the
  client's jurisdiction). Enforcement is a per-send
  `is_opted_out` check inside `launch()` — no caching, no
  bypass path. The check is only as good as its coverage: any
  future send path (scheduling daemon, call execution, a new
  bulk endpoint) must perform the same check; grep for
  `is_opted_out` when adding one. Opt-out rows are immutable
  (no update/delete API exists), so the suppression list can
  only grow — an attacker cannot silently un-suppress a
  contact through the API.
- **PII in campaign content.** Campaign names, templates, pool
  membership, and conversion values are stored plaintext and
  are PII-adjacent. Access is gated by profile-scoped
  `Campaign` grants and lifecycle changes are audited — but
  anyone with database read access sees everything, same as
  contacts and email. No field-level encryption is planned.
- **No unsubscribe link.** Outbound campaign mail carries no
  `List-Unsubscribe` header or one-click unsubscribe link;
  opt-outs are recorded through the API only. Until this lands,
  every campaign's template should include a manual
  reply-to-unsubscribe instruction, and operators must record
  those replies as opt-outs promptly. This is a compliance gap,
  not just a UX one.
- **Synchronous launch.** Bulk send executes inline in the HTTP
  request, holding the connection open for the whole pool. A
  very large pool is a self-inflicted slowloris on the
  operator's own connection — and, with the stub provider, on
  nothing else. With real providers, add timeouts and move
  execution to a background worker before raising pool sizes.

## Jev agents surface

The Jev agents vertical slice (tasks 09-JEV-01 through 09-JEV-03)
adds autonomous, model-driven execution to the boundary. The
novel risks are all about the model acting on untrusted content
and staff delegating authority to it:

- **Prompt injection via tool inputs/outputs.** The agent
  processes untrusted content: tool outputs, handoff contexts,
  and ingested replies can all carry attacker-crafted text that
  reaches the model's context. The domain treats tool *inputs*
  as opaque JSON — there is no sanitization boundary between
  "data" and "instruction" today. Mitigations in place: typed
  tools (the model can only request named tools with
  schema-shaped inputs), profile scoping (injected content can
  only reach the run's own profile), and the audit trail on
  every tool call. Missing: an explicit injection policy for
  tool outputs, output redaction before display, and any
  restriction on which tools a run may call (there is no tool
  registry yet — see below). Treat every tool output as
  untrusted input to the next model call.
- **Approval bypass.** The approval gate is the human brake on
  dangerous tools. What the checks cover: person-specific gates
  reject the wrong actor *in the domain type* (`decide()` returns
  `Unauthorized`); role-based gates check `agent_approver`
  membership through the `RoleMembership` port *before*
  `decide()`, failing closed. What they don't cover: the
  `requires_approval` flag is caller-declared (a malicious or
  buggy caller can mark a dangerous tool ungated — the tool
  registry task must make this authoritative); approving a run
  approves *all* its pending gated calls at once (no
  per-call review); and `decide()` is only as strong as the
  service that calls it — a future service path that skips the
  role check would silently open role-based gates. Grep for
  `decide(` when adding service methods.
- **Model provider trust boundary.** Prompts and contact PII
  leave the agency to the model provider (OpenRouter, …) the
  moment a real adapter is wired in. Mitigations in place: the
  `ModelAdapter` implementor contract forbids PII logging and
  requires bounded timeouts; adapters never execute tools
  themselves. Missing: data-processing agreements with
  providers, prompt PII minimization (send the minimum contact
  fields the task needs), and any provider-side retention
  controls. Today only `StubModelAdapter` exists — no data
  leaves the process — so this boundary is dormant but must be
  addressed *before* the first real adapter ships, not after.
- **PII at rest in agent tables.** `agent_runs.input` /
  `output`, `tool_calls.input` / `output`, and
  `handoffs.context` are stored as plaintext JSONB. Access is
  gated by profile-scoped `AgentRun` grants and lifecycle
  changes are audited — but anyone with database read access
  sees every prompt and tool result, same as contacts and
  email. No field-level encryption is planned.
- **Runaway runs.** There is no execution timeout, no step
  budget, and no cancellation daemon: a run can sit in
  `Running` or `AwaitingApproval` indefinitely, and nothing
  escalates a stalled approval. With the stub adapter this is
  inert; with real adapters and a future execution loop, add
  per-run timeouts and step caps before enabling unattended
  runs.
- **Tool-call blast radius.** Tools execute with the run's
  authority — a tool the model requests runs as the agency, not
  as the model. The blast radius is bounded by the run's
  profile scope and the grants the *service* holds, but there
  is no per-tool allowlist and no per-run tool budget today.
  The tool registry task must add both.
- **Audit coverage.** The run lifecycle, approvals, tool
  recordings, and handoffs all emit audit events with pinned
  action names (`agent.*`, see `jev-agents.md`). Audit is
  fail-loud, never fail-blocking — a recording failure cannot
  stop agent work, which is the right tradeoff, but it means a
  degraded audit pipeline is silent from the operator's side.
  Watch for gaps in `agent.*` event volume the way login
  anomalies are watched today.

## Workflow builder surface

The workflow builder (tasks 10-WORK-01 through 10-WORK-03)
adds operator-authored automation *definitions* — canvas
workflows with typed nodes, triggers, immutable versions, and a
dry-run simulator. There is **no execution engine yet**: no
runner, no scheduler, no webhook ingestion endpoint. Most of
this surface is therefore dormant-but-designed, and the
threat model for execution must be addressed *before* that
task ships, not after:

- **Trigger abuse (dormant).** `WebhookTrigger` looks like an
  unauthenticated entry point — but no endpoint exists to call
  it, so today it cannot fire. When the execution task adds an
  ingestion endpoint, that endpoint needs its own auth
  posture: per-workflow secrets (not bearer staff tokens),
  signature verification, replay protection, and rate limits.
  `EventTrigger` and `ScheduleTrigger` are equally inert
  until a runner exists. Do not let the presence of trigger
  definitions imply a live attack surface — but do not ship
  the runner without the endpoint hardening either.
- **Version tampering.** Published versions are append-only:
  the type has no mutators, the repository has no
  update/delete, and `version_number` is monotonic. What that
  protects: a published definition can always be reconstructed
  exactly, and a compromised operator cannot rewrite history
  to hide what a workflow did. What it doesn't protect:
  *publishing itself* is just `workflow/update` — anyone with
  that grant can publish a malicious workflow as a new
  version. Publishing is the privileged act; treat the grant
  accordingly.
- **Simulator vs execution confusion.** The simulator's trace
  says what *would* happen; nothing ran. The danger is
  operational, not technical: an operator (or a future agent)
  reading a trace as a guarantee that actions fired. The API
  never marks anything executed, and the runbook calls this
  out — but any UI or automation built on top of simulate
  must keep the dry-run framing visible.
- **Node config injection.** Node and trigger configs are
  opaque operator-controlled JSONB. The domain validates
  shape (non-null, positive delay durations) but the payload
  is otherwise trusted. Today that's safe because nothing
  executes configs. The execution task must validate every
  config at the execution boundary — allowlisted action
  names, typed parameters, no template evaluation of
  untrusted fields — before the first real run.
- **Workflow privilege.** When execution ships, runs will act
  with the authority of whoever *published* the workflow, not
  whoever triggered it. That makes `workflow/update` (which
  covers publish) a privilege-escalation-adjacent grant: a
  staffer with `update` but no direct grant on the target
  resource could publish a workflow that touches it. The
  execution design must define the run's authority model
  (publisher authority vs triggerer authority vs per-action
  grants) explicitly.
- **PII at rest in workflow tables.** Node configs, trigger
  configs, version snapshots, and simulate inputs are stored
  as plaintext JSONB. Access is gated by profile-scoped
  `Workflow` grants — but anyone with database read access
  sees every workflow definition, same as contacts and
  email. No field-level encryption is planned.

## Form builder surface

The form builder (tasks 11-FORM-01 through 11-FORM-03)
collects end-client data through staff-composed typed forms.
The distinguishing trait: unlike workflows (definitions),
forms ingest *untrusted input* — answers authored by
outsiders — so the submission path is the surface that
matters:

- **Submission intake abuse / spam.** Today's intake is
  staff-authenticated (bearer <redacted> + `form/create` grant),
  so a spammer must first be an authenticated staffer —
  abuse collapses to insider threat, which grants and the
  audit log cover. If a public intake endpoint ships later
  (anonymous link the public can fill), that endpoint needs
  its own posture *before* it ships: per-form secrets or
  CAPTCHA, rate limiting on the login pattern (per-IP +
  per-form buckets), and abuse alerting. Do not let
  staff-authenticated intake imply the public surface is
  free.
- **Answer injection.** Validate-then-persist means invalid
  answers never reach storage: unknown fields, cross-form
  fields, missing required answers, and wrongly-typed
  values are all `422` before any write. But per-type
  validation is a *structural* check, not sanitization:
  `TextField` accepts arbitrary text, so answers can carry
  HTML, script payloads, or control characters into
  storage. Anything that renders answers (a future form
  UI, exports, email templates) must escape them at
  render time — the API returns raw values. Treat stored
  answers as untrusted input forever, not as validated-safe
  just because they passed `validate_value`.
- **Select-option bypass.** Option membership is checked
  against the form's *current* field set at submit time, in
  the service — not trusted from the client's copy of the
  options. Editing a published form's options changes what
  future submissions accept; old submissions keep their
  historical answers. The ordering matters: the service
  loads fields fresh per submission, so a tampered
  client-side option list cannot widen what is accepted.
- **Field config injection.** Field `config` JSONB is
  operator-controlled (options, labels, future per-type
  settings). The domain validates shape (non-null; select
  requires a non-empty `options` string array) but the
  payload is otherwise opaque. Today the only consumers
  are the field constructor and submit-time validation —
  neither evaluates config as code. Any future execution
  consumer (conditional logic, webhooks on submit, dynamic
  rendering) must treat config as untrusted at the
  consumption boundary.
- **PII in answers at rest.** Answers JSONB holds
  end-client PII (names, emails, phones) in plaintext,
  keyed by field id. Access is gated by profile-scoped
  `Form` grants — but anyone with database read access
  sees every answer, same as contacts and email. There
  is no retention policy yet: submissions persist
  indefinitely. Operators handling GDPR-style erasure
  requests must delete at the submission row level
  (`form_submissions`), since answers live inside the
  row, not in a separate table. Field removal does not
  rewrite old submissions — historical answers survive
  the field, which is correct for auditability but must
  be accounted for in any erasure flow.
- **Cross-tenant answer leakage.** A submission's
  `client_profile_id` is inherited from its form, never
  from the request body; the service enforces the
  caller's scope on the form before validating. The
  remaining leakage vector is *operator error*: a staffer
  with grants on two profiles could read both profiles'
  submissions. Grants are per-profile by design; keep
  them least-privilege.

## Feed surface

The feed (tasks 12-FEED-01 through 12-FEED-03) is a
read-mostly activity timeline: write-once items,
ordered pinned → newest, annotated with read / pin /
hide. The surface is narrow (list + three annotation
endpoints), but it aggregates PII-adjacent facts from
*every* workstream into one place, so the leakage
vectors matter:

- **Cross-profile information leakage.** Every item
  carries a `client_profile_id`, and every endpoint is
  fetch-then-authorize against `Resource::FeedItem` with
  profile-scoped grants: a missing item is `404` before
  any grant check, and the service enforces the
  caller's scope on each item. `list_feed` goes further —
  it fails closed (`Forbidden`) on an agency-scoped
  context that names no profile, so a caller cannot
  enumerate the whole feed corpus. The remaining
  vector is *operator error*: a staffer with grants on
  two profiles sees both profiles' timelines. Grants
  are per-profile by design; keep them least-privilege,
  and remember the feed concentrates what would
  otherwise require several separate reads.
- **Hidden-item bypass.** Hiding is sticky at the
  domain level (no `unhide()`), and hidden items are
  excluded from reads unless the query passes
  `include_hidden: true`. Reading hidden items goes
  through the same `feed_item/read` grant as normal
  reads — there is no separate "see hidden" grant, so
  anyone who can read the feed can opt hidden items
  back in. If hidden must mean *invisible to some
  staff* (e.g. moderation), that needs a distinct
  grant and is a later task. Treat `include_hidden`
  as an operator visibility toggle, not a security
  boundary.
- **Feed poisoning by producers.** Items are
  write-once: once written, content cannot be edited
  through the feed API — only visibility and the read
  flag change. That protects history, but the *content*
  is only as trustworthy as its producer. Today the
  only writers are the repository (seed) and future
  producer code; when workstreams start emitting
  items, each producer must be a trusted service —
  titles and bodies flow from workstream data into
  the timeline unescaped, so a producer that lets
  attacker-controlled text into a title (e.g. a
  contact name rendered as a feed headline) poisons
  every reader's view. Producers must sanitize at the
  emission boundary; renderers must escape at display
  time.
- **Read state as a side channel.** The `read` flag is
  shared per item, not per staffer: anyone with
  `feed_item/update` can mark anything read, and the
  change is visible to everyone on the profile. This
  is a coordination feature, not a privacy mechanism —
  do not infer who read what from it. Per-user read
  records (a later task) would introduce per-staff
  rows and their own privacy questions; the shared
  flag deliberately avoids storing them.

## Billing surface

The billing slice (tasks 12-BILL-01 through 12-BILL-03)
is financial record-keeping: invoices with a strict
status machine, line items frozen at issue, and
append-only payment records with coverage tracking. It
does not execute charges. The money-relevant attack
surface:

- **Invoice tampering.** The dangerous mutation is a
  moving total: an attacker (or a buggy client) that
  can add lines to an issued invoice rewrites a
  financial fact — inflating what the client "owes"
  or shrinking it to hide theft. The domain rejects
  `add_line()` on anything but a draft invoice, and
  `total()` is infallible because overflow is rejected
  at line-add time. Status transitions go only
  through domain methods (`issue()`, `void()`,
  `mark_paid()`, `reopen()`), each with explicit
  preconditions; there is no "set status" endpoint.
  The residual vector is the `Invoice` struct's `pub`
  fields (a 12-BILL-01-era design choice, matching
  the rest of the codebase): the domain methods are
  the enforcement point only if callers use them. The
  service layer is the sole mutator today and goes
  through the methods; any future direct writer must
  be audited against the same rule.
- **Payment fraud vectors.** The classic attacks are
  overpayment (record a payment larger than the
  balance to manufacture credit), zero/negative
  payments (move the balance without moving money),
  and double-processing (retry a payment POST). The
  slice defends in depth: `Payment::record()` rejects
  zero amounts; `Money` rejects negatives at
  construction and on deserialization (manual
  `Deserialize` through `Money::new` — a JSON body
  with `"amount": -5000` is a `422`, not a negative
  payment); `record_payment` checks coverage
  **before persisting** — an overpayment is a
  `Validation` error with zero DB writes, so there
  is no window where a payment exists but the
  invoice does not reflect it. Double-processing is
  **not** defended: billing endpoints take no
  `Idempotency-Key` (see the runbook), so a retried
  `POST /payments` records twice. Operators must
  reconcile via the list endpoints; idempotent
  payment intake is a later task.
- **Currency confusion.** Amounts are currency-tagged
  (`Money` = amount + ISO 4217 code); mixed-currency
  arithmetic is a `Validation` error at every
  boundary — `Money::add`/`sub`, `add_line`, and
  `remaining_due()` all reject mismatches, and the
  currency code itself is validated (exactly three
  uppercase ASCII letters) at construction and on
  deserialization. A USD payment against a EUR
  invoice is a `422`, never a silent conversion.
  There is no FX anywhere in the slice: cross-currency
  billing must be handled by the operator as separate
  invoices.
- **The `Paid → Issued` reopen as an audit-visible
  event.** Reversing a payment that completed coverage
  reopens the invoice to `Issued` — the state change
  is real (the invoice is no longer fully paid) and
  it is recorded as `billing.payment_reversed`.
  Because reversals are explicit and append-only
  (no deletion of payment records), the full history
  survives: an auditor can replay every payment and
  reversal to reconstruct any past balance. The
  vector to watch is *reversal without cause*: anyone
  with `invoice/update` can reverse a payment and
  reopen a paid invoice, which is legitimate for
  bounced payments but is also how an insider
  manufactures an "unpaid" invoice. The audit trail
  is the control — review `billing.payment_reversed`
  events, not just the current status.
- **Cross-profile leakage.** Every endpoint is
  fetch-then-authorize against `Resource::Invoice`
  with profile-scoped grants: a missing invoice is
  `404` before any grant check, and the service
  enforces the caller's profile scope on each
  operation (`Forbidden` on mismatch). `list_invoices`
  fails closed on an agency-scoped context that
  names no profile. The remaining vector is operator
  error, as with the feed: a staffer with grants on
  two profiles sees both profiles' invoices. Keep
  invoice grants least-privilege — they gate both
  financial visibility and the ability to mutate
  financial facts.

## Frontend (SPA) security posture

The Leptos frontend (`crates/crm-web`, task 06-UI) is a thin client:
it holds no authority — every scope decision is re-checked
server-side per request. Its own attack surface:

- **XSS.** The app never uses `inner_html` / `dangerously_set_inner_html`
  or equivalent; all rendering goes through Leptos's typed view macro,
  which escapes text by construction. The remaining XSS vector is a
  compromised dependency (supply chain), mitigated by `cargo audit` /
  `cargo deny` in CI and the lockfile.
- **Token storage.** The bearer token lives in a Leptos signal (memory
  only) — never `localStorage`, `sessionStorage`, or cookies. A stolen
  disk image can't replay it; closing the tab ends the client session.
  The tradeoff: no persistence across tab closes (accepted).
- **No CSRF.** There are no cookies, so there is nothing for a
  cross-site request to ride on. The `Authorization: Bearer` header is
  not sent cross-origin by browsers without CORS opt-in, which the API
  does not grant.
- **Route params → API URLs.** Contact/profile ids from the URL are
  interpolated into API paths client-side. They are untrusted input,
  but the backend parses them as UUIDs and returns 404 on garbage —
  the worst case is a failed fetch, not an injection (no SQL, no
  filesystem, no shell anywhere near this path).
- **Error hygiene.** The UI renders `ApiError` display strings, which
  the client keeps secret-free; 409 dedupe messages never echo the
  conflicting email/phone.
- **Framing.** The API sends `X-Frame-Options: DENY` on all responses,
  so the SPA cannot be clickjacked via iframe embed even if an
  attacker could serve it.
- **WASM supply chain.** The served WASM is built from this repo's
  lockfile via the pinned toolchain; `wasm-bindgen` CLI version must
  match the lockfile (0.2.129) or the module fails closed at load.
  Verify the served bytes match a local build when investigating a
  suspected compromise (see `frontend.md`).
