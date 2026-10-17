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
- Client-side attacks (XSS in the SPA) — the UI workstream's scope.
- Denial of service beyond login rate limiting.
- Insider threat from the trusted operator or database superuser.
