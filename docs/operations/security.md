# Security operations runbook

How to operate the identity and RBAC slice of the Rust CRM: staff
provisioning, credential handling, grant assignment, suspension, session
revocation, and incident response. For the design rationale and the
honest limitations, read `threat-model.md` alongside this file.

> **Demo credentials warning.** The dev seed (`cargo run -p crm-db
> --example seed` with `CRM_SEED_DEV=1`) creates `demo@example.com` /
> `crm-demo-password`. That password is printed on every seed run and is
> **dev-only**: it must never exist in a shared, staging, or production
> database. The seed refuses to run without `CRM_SEED_DEV=1`, so it
> cannot populate a production database by accident.

## Provisioning a staff user

There is no admin UI or provisioning endpoint yet (a deliberate
03-SEC-03 limitation — see `threat-model.md`); provisioning is done
with SQL. Passwords are **never** stored or transmitted in plaintext:
only the Argon2id PHC string goes into `staff_users.phc_hash`.

1. Generate the hash. Pipe the password on stdin (never as a command
   argument, so it stays out of shell history):

   ```sh
   printf '%s' 'the-new-password' \
     | cargo run -q -p crm-db --example hash-password
   # $argon2id$v=19$m=65536,t=3,p=4$...
   ```

2. Insert the user (emails are normalized to lowercase by the service
   layer; store them lowercase):

   ```sql
   INSERT INTO staff_users
     (staff_id, agency_id, email, display_name, status,
      phc_hash, credential_updated_at)
   VALUES
     (gen_random_uuid(), '<agency-uuid>', 'op@example.com', 'Op',
      'active', '<phc-hash>', now());
   ```

3. Assign grants (see below). A user with no grants can log in but every
   API action is `403`.

Password policy is an operator decision today (no built-in complexity
rules); choose a strong generated password and deliver it out of band.

## Rotating a password

Generate a new hash as above, then:

```sql
UPDATE staff_users
   SET phc_hash = '<new-phc-hash>',
       credential_updated_at = now(),
       updated_at = now()
 WHERE email = 'op@example.com';
```

Rotation does not invalidate existing sessions by itself — revoke them
(see below) if the rotation is incident-driven.

## Assigning and revoking grants

Grants are additive rows; revocation is a tombstone (`revoked_at`), not
a delete, so the history survives. Resource/operation strings and scope
kinds:

| Column              | Values                                                                          |
| ------------------- | ------------------------------------------------------------------------------- |
| `resource`          | `contact`, `contract`, `campaign`, `agent_action`, `audit_log`, `client_profile`, `staff_user`, `session` |
| `operation`         | `read`, `create`, `update`, `delete`, `approve`, `manage`                        |
| `scope_kind`        | `agency`, `client_profile`                                                      |
| `client_profile_id` | NULL for `agency` scope, set for `client_profile` scope                         |

Assign:

```sql
INSERT INTO grants
  (grant_id, staff_id, resource, operation, scope_kind,
   agency_id, client_profile_id)
VALUES
  (gen_random_uuid(), '<staff-uuid>', 'contact', 'read', 'client_profile',
   '<agency-uuid>', '<profile-uuid>');
```

Revoke (tombstone — the delegation cascade in `Delegation::effective_grants`
picks this up immediately; delegated agents lose the capability without
any further cleanup):

```sql
UPDATE grants
   SET revoked_at = now()
 WHERE grant_id = '<grant-uuid>' AND revoked_at IS NULL;
```

List live grants for a user:

```sql
SELECT grant_id, resource, operation, scope_kind, client_profile_id
  FROM grants
 WHERE staff_id = '<staff-uuid>' AND revoked_at IS NULL;
```

## Suspending and reactivating staff

```sql
-- Suspend: login and every token validation fail closed as 401.
UPDATE staff_users SET status = 'suspended', updated_at = now()
 WHERE email = 'op@example.com';

-- Reactivate.
UPDATE staff_users SET status = 'active', updated_at = now()
 WHERE email = 'op@example.com';
```

Suspension is **indistinguishable from a wrong password** at the login
endpoint (same `401`, same Argon2 timing), so suspending an account
reveals nothing to a prober. Existing sessions of a suspended user stop
validating on their next use — but revoke them explicitly anyway (below)
so nothing lingers.

## Session revocation

- **Self-service:** `POST /api/v1/auth/logout` with the bearer token →
  `204`. Idempotent; an unknown token still yields `204`.
- **Admin (one session):**

  ```sql
  UPDATE sessions SET revoked_at = now()
   WHERE session_id = '<session-uuid>' AND revoked_at IS NULL;
  ```

- **Admin (all sessions for a user — the incident path):**

  ```sql
  UPDATE sessions SET revoked_at = now()
   WHERE staff_id = '<staff-uuid>' AND revoked_at IS NULL;
  ```

- **Inspect live sessions:**

  ```sql
  SELECT session_id, issued_at, expires_at, last_activity_at,
         revoked_at, delegation_id
    FROM sessions
   WHERE staff_id = '<staff-uuid>' AND revoked_at IS NULL
   ORDER BY last_activity_at DESC;
  ```

Sessions also expire absolutely (12 h default, 7 d hard cap) and go idle
after 30 minutes without an authorized request. The idle window slides:
every authorized request touches `last_activity_at`.

## Login rate limits

`POST /api/v1/auth/login` is rate-limited per client IP (30 attempts /
60 s) and per normalized email (10 attempts / 5 min); every attempt
counts, successful or not. An exhausted bucket is `429`
`application/problem+json` with a `Retry-After` response header (seconds
until the oldest attempt in the bucket expires).

The limiter is in-memory and per-process: exact for the single-instance
deployment, approximate behind multiple instances (see
`threat-model.md`). Limits are code-level constants in
`crates/crm-api/src/rate_limit.rs` (`LoginRateLimitConfig::default`);
changing them is a rebuild. If legitimate users hit `429`, check for a
shared egress IP (NAT) before raising the IP bucket.

## Security headers

Every `/api/*` response carries:

| Header                 | Value                                              |
| ---------------------- | -------------------------------------------------- |
| `X-Content-Type-Options` | `nosniff`                                        |
| `X-Frame-Options`      | `DENY`                                             |
| `Referrer-Policy`      | `no-referrer`                                      |
| `Permissions-Policy`   | `camera=(), microphone=(), geolocation=(), payment=()` |

`Strict-Transport-Security` is intentionally **not** set by the app
server: it binds loopback and never terminates TLS — HSTS is the reverse
proxy's job. No `Content-Security-Policy` on API responses (JSON gains
nothing from one); the SPA's document policy belongs to the UI
workstream.

## Security audit log (04-AUD-02)

Security-relevant occurrences are appended to the shared `audit_events`
table as hash-chained [`AuditEvent`]s whose `payload` JSONB column carries
the serialized [`SecurityEvent`]. Recorded on the request path: login
succeeded, login failed (email fingerprint only — never the address),
rate-limit tripped (fingerprinted bucket key), logout, and
authorization denials (counted toward the probing anomaly signal).
Contact-lifecycle events share the same chain with a NULL payload.

- `GET /api/v1/audit/events` — filtered query (actor, action, profile,
  time range, cursor, limit; chain order). Authorized as
  `AuditLog:Read` **at agency scope**: a profile-scoped caller gets 403,
  so profile readers can never learn whether agency-level events exist.
- `GET /api/v1/audit/verify` — verifies the whole chain from genesis
  (same authorization). `valid: false` with a `first_bad_event` means
  the audit store itself is suspect — treat as an incident.

### Audit-write failure tradeoff

Recording a security event never fails the request it describes: a
failed audit write logs at `error!` and the request continues. This is
deliberate — turning a login into a 500 because the audit table hiccuped
would make an observability dependency an availability dependency, and
would let an attacker deny logins by filling the disk. The cost is a
possible gap in the log under storage failure; watch for the
`security audit write failed` error in logs and treat a burst as an
operational incident.

## Incident response: suspected credential compromise

Per `docs/handoff/24-security-incident-response.md`:

1. **Contain:** revoke all sessions for the affected user (SQL above) and
   suspend the account. Both are immediate; in-flight bearer tokens stop
   validating on next use.
2. **Rotate:** generate a new password hash and update `phc_hash`. Do
   not reuse the old password.
3. **Scope:** query the audit trail for the affected actor and time
   window (audit events carry the real staff actor since 03-SEC-02):

   ```sql
   SELECT * FROM audit_events
    WHERE actor_id = '<staff-uuid>'
      AND occurred_at BETWEEN '<start>' AND '<end>'
    ORDER BY seq;
   ```

   Verify the chain with `GET /api/v1/audit/verify` (authorized as
   `AuditLog:Read` at agency scope) — a broken chain (`valid: false`)
   means the audit store itself is suspect.
4. **Recover:** reactivate the account only after the new credential is
   delivered out of band, and confirm the user can log in.
5. **Review:** check for grants assigned to the account during the
   window (`grants` history has no delete — tombstones preserve it) and
   revoke anything unexpected.

If the compromise may extend beyond one account (e.g. database access),
treat it as a full incident per doc 24: the threat model documents what
the auth boundary does and does not cover.
