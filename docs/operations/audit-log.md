# Audit log operations runbook

How to operate the tamper-evident security audit log: querying events,
verifying the chain, responding to verification failures, tuning
thresholds, and the retention policy. For the recording points and the
fail-loud tradeoff, see the "Security audit log" section in
`security.md`; for the threat model, `threat-model.md`.

The audit log lives in the `audit_events` table. Every row carries a
`chain_hash` linking it to its predecessor (from a zero genesis hash),
a JSONB `payload` with the forensic detail, and an `idempotency_key`
that makes retried recordings return the original event instead of a
duplicate. Contact-lifecycle events share the same chain with a NULL
payload.

Privacy rule: the log never contains raw emails or IP addresses — only
SHA-256 fingerprints and IDs. If you ever see a raw address in a
payload, treat it as a bug and file it.

## Querying the log

`GET /api/v1/audit/events` — authenticated, and authorized as
`AuditLog:Read` **at agency scope**. A profile-scoped caller (even one
holding an `AuditLog:Read` grant for their own profile) gets 403: the
audit log is an agency-level resource, and a 403 (rather than an empty
page) keeps profile readers from learning whether agency-level events
exist.

Query parameters (all optional):

| Parameter | Meaning |
|---|---|
| `actor_id` | UUID of the actor (staff, agent, or system) |
| `action` | Pinned action name, e.g. `auth.login`, `auth.login_failed`, `auth.rate_limit_tripped` |
| `client_profile_id` | UUID of the client profile |
| `from` / `to` | RFC 3339 time range (`to` is exclusive) |
| `after` | Event UUID cursor — the last event of the previous page |
| `limit` | Page size (default 50, capped at 100) |
| `newest_first` | `true` returns the newest events first instead of chain order |

Events come back in chain order (oldest first) by default, each with
its forensic `payload`; `?newest_first=true` returns the newest events
first instead (the `after` cursor then pages backwards from the
previous page's last event). Newest-first is a presentation order
only — storage, hashing and verification never change. Example:

```bash
TOKEN="$(curl -s -X POST localhost:3000/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"auditor@example.com","password":"..."}' | jq -r .token)"

# Failed logins in the last hour, first page:
curl -s "localhost:3000/api/v1/audit/events?action=auth.login_failed&from=$(date -u -d '1 hour ago' +%FT%TZ)&limit=50" \
  -H "Authorization: Bearer $TOKEN" | jq '.events[] | {action, occurred_at, payload}'

# Next page (cursor = last event's id):
curl -s "localhost:3000/api/v1/audit/events?action=auth.login_failed&after=<event-id>&limit=50" \
  -H "Authorization: Bearer $TOKEN" | jq .
```

## Verifying the chain

`GET /api/v1/audit/verify` — same authorization. Returns:

```json
{
  "valid": true,
  "events_checked": 1234,
  "head": "9f2c…",
  "first_bad_event": null
}
```

Run it on a schedule (hourly is plenty at current scale) and after any
database maintenance. `valid: false` with a `first_bad_event` UUID is a
**security incident** — see below. Note that verification reads the
whole table; at current scale this is trivial, but revisit if the log
grows past a few million rows.

## Incident response: verification FAILED

A broken chain means one of two things: the database was tampered with,
or there is a bug in the chaining code. Either way, treat it as an
incident:

1. **Freeze writes.** Stop the API (`systemctl stop crm-api` or
   equivalent) so no new events are appended on top of a suspect chain.
2. **Preserve the database.** Take a filesystem-level snapshot / `pg_dump`
   before touching anything. Do not run `VACUUM FULL` or restore over it.
3. **Scope the damage.** `first_bad_event` names the first row whose
   `chain_hash` does not match its predecessor. Everything before it is
   still trustworthy; everything from it onward is suspect. Query the
   events around it (`GET /api/v1/audit/events`) to see what happened
   just before the break.
4. **Compare against backups.** Restore the latest pre-break backup to a
   scratch database and diff the `audit_events` rows up to the break
   point. Rows that differ (or are missing) past the break point tell you
   what was altered or deleted.
5. **Decide.** If the break is explained (e.g. a DBA hand-edited a row —
   find out why), document it, re-anchor, and resume. If it is not
   explained, assume compromise: rotate credentials, revoke sessions
   (see `security.md`), and rebuild from a known-good backup.
6. **Re-anchoring (only after the cause is understood).** There is no
   automatic repair — by design. An operator recomputes the chain from
   the break point forward with the documented procedure, records the
   old head hash in the incident ticket, and the new head becomes the
   trusted tip.

## Anomaly signals

Signals are emitted to the logs as `tracing` warnings (`anomaly.name`,
`anomaly.signal`). The process takes no automated action — a human (or
a log-based alerter) triages them.

| Signal | Meaning | Triage |
|---|---|---|
| `anomaly.login_velocity` | The per-email login rate limiter tripped: many attempts against one address in a short window. | Likely credential stuffing or a brute-force attempt. Check `auth.login_failed` events for the fingerprinted address, confirm no `auth.login` succeeded for it, consider notifying the account owner. The fingerprint (not the address) is in the `auth.rate_limit_tripped` payload. |
| `anomaly.repeated_authorization_denials` | One actor hit 403s repeatedly (default: 10 in 5 minutes). | Likely permission probing — someone (or something) mapping what they can access. Query `authz.denied` trace events and the actor's recent grants. If the actor is a service account, check for a misconfigured client before assuming malice. |

## Thresholds

All tuning knobs are environment variables read at startup; unset or
unparseable values fall back to the defaults (never a startup failure).

| Variable | Default | Meaning |
|---|---|---|
| `CRM_RATE_LIMIT_IP_MAX` | `30` | Max login attempts per client IP per window |
| `CRM_RATE_LIMIT_IP_WINDOW_SECS` | `60` | Per-IP sliding window (seconds) |
| `CRM_RATE_LIMIT_EMAIL_MAX` | `10` | Max login attempts per normalized email per window |
| `CRM_RATE_LIMIT_EMAIL_WINDOW_SECS` | `300` | Per-email sliding window (seconds) |
| `CRM_DENIAL_THRESHOLD` | `10` | Denials per actor per window that emit `anomaly.repeated_authorization_denials` |
| `CRM_DENIAL_WINDOW_SECS` | `300` | Denial counting window (seconds) |

Tightening the email bucket is the cheapest brute-force mitigation;
loosening it reduces false positives for shared office IPs (the IP
bucket is the one that bites NAT'd offices — watch 429s after
deploying behind a new egress IP).

## Audit-write failures (the fail-loud tradeoff)

Recording a security event never fails the request it describes: a
failed audit write logs `security audit write failed` at `error!` and
the request continues. This is deliberate — a login must not 500
because the audit table hiccuped, and an attacker must not be able to
deny logins by filling the disk.

The cost is a possible gap in the log under storage failure. **Alert
on it:** a single occurrence is noise (a transient DB blip); a burst
is an operational incident — investigate the database before the gap
grows. Correlate with `GET /api/v1/audit/verify` afterwards: a gap
does not break the chain (the next event links to the last written
one), but you will want to know what is missing.

## Retention policy

**Security audit events are retained indefinitely.** The log is
append-only and tamper-evident; deleting rows destroys the chain's
value as evidence.

If the table must be pruned (disk pressure, data-retention law):

- Pruning is a **deliberate operator action**, never automatic. There
  is no background job that deletes audit rows.
- You cannot simply `DELETE` old rows: every later row's `chain_hash`
  commits to the deleted prefix, so verification would fail from the
  cut point onward. The honest procedure is:
  1. Export the rows to be pruned (immutable archive — WORM storage
     or signed dump).
  2. Record the archive's hash and the pruned head `chain_hash` in
     the change ticket.
  3. Delete the rows, then insert a single **re-anchor event**
     (`action = "audit.reanchored"`, payload carrying the old head
     hash and the archive reference) whose `prev_hash` is the pruned
     head — the new genesis for verification purposes.
  4. Verify the chain from the re-anchor forward.
- Automated pruning with re-anchoring is a follow-on task, not this
  one: getting the re-anchor wrong silently forks the chain, so it
  ships only with its own review and tests.

## Health reporting

The domain defines a `HealthReporter` port (`Healthy` / `Degraded` /
`Unhealthy`) for components to report their own health. The API does
**not** wire it into `/healthz` or `/readyz` today, deliberately:

- `/healthz` is process liveness (always 200 while the server runs) —
  a reporter adds nothing.
- `/readyz` already performs a bounded (3s) database health check via
  `backend_healthy()` and returns 200/503. Wrapping that bool in a
  `HealthReporter` would add indirection without new signal: we have
  no `Degraded` criteria (e.g. latency budgets) defined yet.

When a second component needs health reporting (cache, queue, upstream
dependency), implement the port for it and aggregate in the probes.
Until then, the port stays available and unused — no dead code shipped
to satisfy a checklist.

## Smoke test procedure

After deploying (or after any change to the auth/audit path), run this
against the live server to prove the slice end to end:

```bash
BASE=http://localhost:3000
# 1. Log in as the auditor (needs AuditLog:Read at agency scope).
TOKEN="$(curl -s -X POST $BASE/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"auditor@example.com","password":"<redacted>"}' | jq -r .token)"

# 2. A failed login must appear in the log with a fingerprint payload.
curl -s -X POST $BASE/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"auditor@example.com","password":"wrong"}' -o /dev/null -w "%{http_code}\n"
# expect 401
curl -s "$BASE/api/v1/audit/events?action=auth.login_failed&limit=1" \
  -H "Authorization: Bearer $TOKEN" | jq '.events[0] | {action, payload}'
# expect action "auth.login_failed" and a payload with email_fingerprint (no raw email)

# 3. The chain must verify.
curl -s "$BASE/api/v1/audit/verify" -H "Authorization: Bearer $TOKEN" | jq '{valid, events_checked}'
# expect {"valid": true, ...}

# 4. A caller without the audit grant must get 403, not data.
OTHER="$(curl -s -X POST $BASE/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"staff@example.com","password":"<redacted>"}' | jq -r .token)"
curl -s -o /dev/null -w "%{http_code}\n" "$BASE/api/v1/audit/events" \
  -H "Authorization: Bearer $OTHER"
# expect 403

# 5. Probes still answer.
curl -s $BASE/healthz; echo
curl -s -o /dev/null -w "%{http_code}\n" $BASE/readyz
# expect {"status":"ok"} and 200
```
