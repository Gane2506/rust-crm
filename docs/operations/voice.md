# Calling operations runbook

How the calling vertical slice (tasks
14-VOICE-01 through 14-VOICE-03) works in
production: the forward-only call lifecycle,
consent-gated recordings, the provider
abstraction, and the 7 HTTP endpoints. For the
auth model, see `security.md`; for the audit
events calling emits, `audit-log.md`; for the
calling security posture, `threat-model.md`.

## What it is

Staff place outbound calls to contacts (and
receive inbound ones); each call moves through a
forward-only status machine, and recordings are
gated on explicit called-party consent — a legal
requirement the domain enforces, not a UX
nicety. Four layers:

- **Domain** (`crates/crm-domain/src/voice.rs`):
  `PhoneNumber` (E.164-ish: optional leading `+`,
  7–15 ASCII digits; manual `Deserialize`
  through `new()` so JSON bodies cannot bypass
  validation), `Call` + `CallStatus`
  (forward-only transitions; terminal statuses
  cannot be left), `CallDirection`
  (outbound/inbound), `CallRecording`
  (consent-gated; the audio is an `AssetId`
  reference — bytes live in the content-addressed
  asset store, never inline), `RecordingConsent`
  (`granted` / `denied` / `unknown`; `unknown`
  is the default — silence is not consent),
  `CallLog` (the immutable write-once record of
  a finished call, built only via
  `Call::to_log()`). Also the `VoiceProvider`
  port and the `VoiceService` / `CallRepository`
  ports.
- **Persistence** (`crates/crm-db`, migration
  `0014_voice`): `calls` (profile + nullable
  contact FK, direction, from/to numbers,
  status, provider ref, started/ended, duration,
  recording consent, timestamps) and
  `call_recordings` (`UNIQUE(call_id)` — one
  recording row per call, upserted on update).
  `PgCallRepository` hydrates the full `Call`
  aggregate (including the latest recording row)
  on read and rejects cross-profile row moves.
- **Service** (`crates/crm-core/src/voice.rs`):
  `VoiceServiceImpl` over the call repository,
  a `VoiceProvider`, and the audit port.
  `place_call` validates, dials through the
  provider, and stores; `end_call` drives the
  terminal transition the domain allows from
  the current state; `start_recording` routes
  through the domain consent gate; scope checks
  run on `ctx.client_profile_id`. Every mutation
  records an audit event; audit failures are
  swallowed (fail-loud at the HTTP layer, never
  fail the operation).
- **HTTP** (`crates/crm-api/src/voice.rs`): 7
  endpoints under `/api/v1` — calls CRUD plus
  answer / end and recording start / stop.

Every call belongs to one client profile — the
tenant boundary. Authorization uses the `Call`
resource with profile-scoped grants (`call/read`,
`call/create`, `call/update`). A missing call is
`404` before any grant check (fetch-then-authorize).

## Call lifecycle

```
Initiated → Ringing → InProgress → Completed
               ↓           ↓
            NoAnswer     Failed
               ↓           ↓
             Busy      (terminal)
Initiated → Failed ("call ended before connecting")
```

- `place_call()` → `Initiated`: the call is
  dialed through the provider, which returns an
  opaque provider-side reference
  (`stub-call-N` under the stub provider).
- `Ringing → InProgress` (`answer_call()`): the
  far end answered.
- `end_call()`: moves the call to the terminal
  status the domain allows from its current
  state — `InProgress → Completed` (sets
  `ended_at`, computes `duration_secs` clamped
  at zero), `Ringing → NoAnswer`,
  `Initiated → Failed` ("call ended before
  connecting"). Ending an already-finished call
  is a `Validation` error. The provider hangup
  is best-effort and idempotent.
- `Failed` needs a non-blank reason; `NoAnswer`
  and `Busy` are terminal. A recording left
  running is stopped implicitly on completion.

A finished call is summarized as an immutable
`CallLog` (`to_log()` rejects non-terminal
calls) — there is no HTTP surface for logs yet.

## Consent model

Recording a call is **consent-gated** — the
domain enforces three rules:

1. `start_recording()` rejects unless
   `RecordingConsent::Granted` was recorded
   first. `Unknown` and `Denied` both reject —
   silence is not consent.
2. Consent is captured per call via
   `set_recording_consent()`, allowed while the
   call is active. `Unknown → Granted` mid-call
   is legal (the party was asked again); once a
   recording has started, consent is **pinned** —
   changing it would rewrite the legal basis.
3. Recording requires a connected call
   (`InProgress`) and attaches an `AssetId`:
   the audio bytes live in the asset store, so
   retention and deletion follow the asset
   lifecycle.

The `/recordings` endpoint captures consent and
starts recording in one shot (`{consent:
"granted"}`); anything else is a `422`. The
`/recordings/stop` endpoint takes the audio
asset id in the body — the caller supplies the
asset (uploaded through the asset endpoints);
the API does not create it.

## Provider abstraction

`VoiceProvider` (`initiate_call`, `hangup`,
`call_status`) is the port the service dials
through — provider-agnostic by design, the
provider reference an opaque string the domain
never parses. Today it is
`StubVoiceProvider`: deterministic
`stub-call-N` ids from a process-wide counter,
`hangup` a no-op, `call_status` always
`InProgress`. The Telnyx adapter is a later
workstream's concern — until it ships, calls
never leave the process.

Endpoints (`{id}` is the call UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/calls` `{client_profile_id, contact_id?, direction, from, to}` | `call/create` | Place a call (`201`; `422` on invalid phone or blank fields) |
| `GET /api/v1/calls?client_profile_id=<uuid>&limit=<n>` | `call/read` | List calls for one profile, newest first (`200`; response `{calls, total}`; `422` without the query param) |
| `GET /api/v1/calls/{id}` | `call/read` | Fetch a call (`200`) |
| `POST /api/v1/calls/{id}/answer` | `call/update` | Answer a ringing call (`Ringing → InProgress`; `200`) |
| `POST /api/v1/calls/{id}/end` | `call/update` | End the call at its terminal status (`200`; `422` on an already-finished call) |
| `POST /api/v1/calls/{id}/recordings` `{consent}` | `call/update` | Capture consent and start recording (`200`; `422` on anything but `granted`, or a non-connected call) |
| `POST /api/v1/calls/{id}/recordings/stop` `{asset_id}` | `call/update` | Stop recording, attaching the audio asset (`200`; `422` with no active recording) |

## Audit trail

The service records audit events with these pinned
action names (see `audit-log.md`):

`voice.call_placed`, `voice.call_answered`,
`voice.call_ended`, `voice.recording_started`,
`voice.recording_stopped`.

Reads (list/get) are not audited, matching the
domain contract. Audit is fail-loud at the HTTP
layer and never fails the operation. Audit
payloads carry the call id, not the phone
numbers — but the numbers are stored plaintext
on the `calls` row (see the threat model).

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated
by `CRM_SEED_DEV=1`) places a demo outbound call
on the demo profile through the real
`VoiceServiceImpl` (so dialing, consent, and
audit behavior match production), then ends it —
plus `call` read/create/update grants for the
demo staff user:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed is idempotent (skipped when any calls
already exist on the demo profile). Seeding uses
the service layer — never raw SQL — so storage
behavior matches production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on call endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on call endpoints | No `call/*` grant on the profile | Have an operator check the `grants` table. |
| `422` on place-call | Invalid phone number | Numbers must be 7–15 digits with an optional leading `+` — no dashes, spaces, or letters. |
| `422` on answer | Call is not ringing | Only `Ringing` calls can be answered; a fresh call is `Initiated`. |
| `422` on end | Call already finished | Terminal statuses cannot be left — check the call's `status`. |
| `422` on recordings start | Consent not granted, or call not connected | Send `{"consent": "granted"}` and only on an `InProgress` call. `unknown`/`denied` never authorize recording. |
| `422` on recordings stop | No active recording | Start recording first; stopping an already-stopped recording is an error. |
| `422` on list | Missing `client_profile_id` | Listing is profile-scoped by design — pass the query param. |
| `404` on call endpoints | Wrong id, or the call belongs to another profile | Fetch-then-authorize: a missing row is `404` before any grant check. |

## Known limitations

- **No call UI.** The Leptos frontend has no
  call views — calling is API-only until a
  frontend task picks it up.
- **The stub provider dials nothing.**
  `StubVoiceProvider` returns fake ids and never
  touches a network — there is no real
  telephony until the Telnyx adapter ships.
- **`answer_call` is unreachable through the
  service today.** Nothing advances
  `Initiated → Ringing` over HTTP — the service
  never calls `ring()`, and the provider's
  status is never polled. `Ringing`,
  `InProgress`, `Completed`, `NoAnswer`, and
  `Busy` are only reachable through domain-level
  manipulation; the HTTP surface can place
  (`Initiated`) and end (`Failed` on an early
  end). The provider-status polling (or a
  webhook) that drives the live lifecycle is a
  later task.
- **`total` is the page length.** The list
  response's `total` counts the returned items,
  not the profile's full call corpus — there is
  no unfiltered count query yet.
- **No idempotency keys on call writes.** Like
  billing and signing, the call endpoints do not
  accept `Idempotency-Key` — a retried `POST`
  dials twice. Clients should reconcile via the
  list endpoints before retrying.
- **`list_by_contact` has no HTTP surface.**
  The repository supports it, but there is no
  endpoint — the only listing is
  profile-scoped.
- **Phone numbers are plaintext at rest.**
  `from_number`/`to_number` are stored and
  served in the clear; there is no field-level
  encryption (same posture as contacts and
  email).
