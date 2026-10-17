# Email operations runbook

How the email vertical slice (tasks 07-MAIL-01 through 07-MAIL-03)
works in production: sending, reply ingestion, thread management,
provider adapters, and troubleshooting. For the auth model, see
`security.md`; for the audit events email emits, `audit-log.md`; for
the webhook's security posture, `threat-model.md`.

## What it is

The email slice lets agency staff send email on a client's behalf
(outbound) and ingest replies (inbound), grouped into threads per
client profile. Four layers:

- **Domain** (`crates/crm-domain/src/email.rs`): `EmailMessage`
  (forward-only status state machine), `EmailThread`
  (open/close/reopen/archive), `EmailAttachment` (link to the
  content-addressed asset store — no byte duplication), the pure
  `match_thread_by_subject` heuristic, and the `EmailService` /
  `EmailProvider` traits.
- **Persistence** (`crates/crm-db`, migration `0006_email`):
  `email_threads`, `email_messages`, `email_attachments`. The
  provider's message id is unique per profile (at-most-once
  ingestion); the provider's thread id is unique per profile
  (exact-match reply correlation).
- **Service** (`crates/crm-core/src/email.rs`): `EmailServiceImpl`
  over the repository + provider + audit ports. `StubEmailProvider`
  (in-memory, no network) is the dev/test adapter; real Gmail/Zoho
  adapters are a later task.
- **HTTP** (`crates/crm-api/src/emails.rs`): `POST
  /api/v1/emails/send`, `POST /api/v1/emails/ingest` (provider
  webhook), `GET /api/v1/threads`, `GET /api/v1/threads/{id}`.

Every message belongs to one client profile — the tenant boundary.
Authorization uses the `Email` and `EmailThread` resources with
profile-scoped grants (`email/create` to send, `email_thread/read`
to read threads).

## Sending flow

`POST /api/v1/emails/send` (bearer auth required, `email/create`
grant on the message's profile):

```
Draft → Queued → Sending → Sent → Delivered
                     │         └→ Bounced (provider-reported)
                     └→ Failed (any non-terminal state)
```

1. The handler validates the body, resolves the caller's
   `ActionContext`, and honors the `Idempotency-Key` request header:
   a replayed key returns the stored response with `200` instead of
   sending again (`201` on a fresh send).
2. The service persists the message as `Queued`, hands it to the
   provider adapter as `Sending`, and records the provider's message
   id on `Sent`. A provider error moves the message to terminal
   `Failed` — the row is kept for the audit trail; re-sending
   creates a new idempotency-keyed message.
3. Audit: `email.sent` / `email.failed` events (see `audit-log.md`).

`Delivered`, `Failed`, and `Bounced` are terminal in the domain:
there are no transitions out of them.

## Ingest webhook setup

`POST /api/v1/emails/ingest` receives provider callbacks for
inbound replies. Providers cannot hold staff sessions, so this
endpoint is **not** bearer-authenticated — it lives outside the auth
middleware and is gated by a shared secret instead:

```bash
# Required. When unset, the endpoint refuses ALL traffic with 503
# (fail-closed) — ingestion silently stops rather than running open.
CRM_EMAIL_WEBHOOK_SECRET=<high-entropy-secret> ./target/release/crm-api
```

- The provider sends the secret in the `x-webhook-secret` request
  header.
- `202` — accepted (returns the ingested message; duplicate
  `provider_message_id`s return the existing message, at-most-once).
- `401` — wrong secret.
- `503` — `CRM_EMAIL_WEBHOOK_SECRET` is not configured.
- `422` — malformed body.

The webhook acts as the system principal scoped to the body's
`client_profile_id`; the profile id comes from the request body, so
**the secret is the only authentication** — treat it like a
password (see `threat-model.md` for rotation).

### Thread-matching cascade

Each ingested reply is correlated with a thread, strongest signal
first:

1. **Provider thread id** — the body's `provider_thread_id`
   matches a thread's (exact, per-profile unique).
2. **Parent message lookup** — `in_reply_to` / `references` resolve
   to a known message via its `provider_message_id`; its thread
   wins.
3. **Subject heuristic** — normalized subject (`Re:`/`Fw:`/`Fwd:`
   stripped, case-insensitive) matches the most recently active
   thread's subject. Last resort.
4. **New thread** — nothing matches; a thread is opened for the
   reply.

Recording the message bumps the thread's count, refreshes
`last_message_at`, and adds new participants. An inbound message on
a `Closed` thread reopens it (`Archived` stays archived). Audit:
`email.received`.

## Thread management

- `GET /api/v1/threads?client_profile_id=<uuid>&limit=<n>` —
  threads for one profile, most-recently-active first (default 20,
  max 100). Requires `email_thread/read` on that profile.
- `GET /api/v1/threads/{id}` — the thread plus up to 100 of its
  messages, oldest first. Fetches first, then authorizes against the
  thread's own profile (a missing thread is `404` before any grant
  check — the `get_asset` pattern).
- Lifecycle (`close` / `reopen` / `archive`) is domain-level today;
  there is no HTTP endpoint for it yet — it is SQL or a follow-on
  task. `Archived` is terminal.

## Provider adapters

`EmailProvider` is the seam between the service and the outside
world. The implementor contract (rustdoc on the trait): idempotent
sends keyed by `provider_message_id`, provider errors mapped to
`DomainError`, no scope escape (the adapter never touches another
profile's data).

- **`StubEmailProvider`** (current): records sent messages
  in-memory, mints `stub-<uuid>` provider ids, `fetch_message`
  always errors. No network I/O. This is what production runs
  today — outbound mail goes nowhere.
- **Real Gmail/Zoho adapters** (later task): need OAuth2 service
  credentials (Gmail) or API keys (Zoho) in the deployment's secret
  management, a verified sending domain / `from` address, and
  webhook registration pointing at `/api/v1/emails/ingest` with the
  shared secret. Nothing in the domain or service layers changes
  when they land — only the wiring in `emails.rs::email_service`.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated by
`CRM_SEED_DEV=1`) creates one demo thread on the demo profile with
an outbound `Sent` message and an inbound reply joined via the
`in_reply_to` cascade, plus `email/create` and `email_thread/read`
grants for the demo staff user — so the whole slice is tryable
locally:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on `POST /api/v1/emails/send` | Missing bearer token, or no `email/create` grant on the profile | Re-login; have an operator check the `grants` table for `email` / `Create` on the profile scope. |
| `403` on send or thread reads | Grant missing for that profile | Same as above — grants are per-profile; cross-profile access is denied by design. |
| `503` on `POST /api/v1/emails/ingest` | `CRM_EMAIL_WEBHOOK_SECRET` unset | Set the env var and restart. Fail-closed is intentional. |
| `401` on ingest | Wrong `x-webhook-secret` header | Rotate/check the provider's configured secret. |
| Ingest `202` but reply lands in a new thread | No provider thread id and no parent match; subject heuristic missed | Check the body's `provider_thread_id` / `in_reply_to` — providers must send them for reliable threading. Subject-only matching is best-effort. |
| Ingest `202` but no new message row | Duplicate `provider_message_id` — at-most-once dedupe returned the existing message | Expected behavior, not a bug. |
| Ingest `422` | Blank subject on a reply that matches no thread | Thread creation requires a subject; a subject-less orphan reply is rejected. Give the provider payload a subject or thread hint. |
| Message stuck in `Sending` | Provider adapter hung or crashed mid-send | With the stub this cannot happen; with real adapters, check provider status. The row stays `Sending` — re-send creates a new message; the stuck row is audit trail. |
| `email.failed` audit events spike | Provider rejecting mail (bad credentials, unverified domain) | Check the adapter's error detail in the audit payload, then provider credentials / domain verification. |

## Known limitations

- **Stub provider only.** Outbound mail is recorded in-memory and
  goes nowhere until the real Gmail/Zoho adapters land.
- **No attachment HTTP surface.** The `EmailAttachment` domain and
  repository exist, but no service method or endpoint creates
  attachments yet — attachments on sent/ingested mail are a
  follow-on.
- **`NewEmailMessage.idempotency_key` is HTTP-layer only.** The
  field documents service-level dedupe, but replay protection is
  implemented by the handler's `IdempotencyStore`, not the service.
  Concurrent sends with the same key but no `Idempotency-Key`
  header can double-send.
- **Sending into a thread does not bump its counters.** `send`
  with a `thread_id` links the message, but `message_count` /
  participants are only updated by `ingest_reply`.
- **`GET /api/v1/threads` `total` is the page size**, not the
  profile's thread count.
- **No thread lifecycle endpoints.** Close/reopen/archive are
  domain operations without HTTP exposure.
- **`body_html` is stored as-is.** No sanitization; any future
  renderer must escape it (see `threat-model.md`).
- **No email UI.** The Leptos frontend has no compose, inbox, or
  thread views — email is API-only until a frontend task picks it
  up.
- **Single-instance webhook secret.** The secret is read from the
  process environment per request; rotation requires a restart
  (no reload endpoint).
