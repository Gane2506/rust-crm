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
