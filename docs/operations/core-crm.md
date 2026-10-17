# Core CRM operations runbook

How to operate the Core CRM vertical slice: client profiles, contacts
(sources, dedupe, merge), and assets. For the security model (auth,
RBAC, audit), see `security.md`; for the audit log itself,
`audit-log.md`.

## Client profiles

Profiles are agency-managed: the agency creates and operates them;
clients never log in. All profile endpoints require the
`ClientProfile` grant at **agency scope** — the agency comes from the
caller's session, never from request parameters. A caller from another
agency gets 404 (not 403), so agencies can't probe each other's
profiles.

| Endpoint | Effect |
|---|---|
| `POST /api/v1/profiles` `{"name": "..."}` | Create (201). Idempotent via `Idempotency-Key`. |
| `GET /api/v1/profiles` | List this agency's profiles. |
| `GET /api/v1/profiles/{id}` | Fetch one. Cross-agency → 404. |
| `POST /api/v1/profiles/{id}/suspend` | Suspend. Idempotent. |
| `POST /api/v1/profiles/{id}/reactivate` | Reactivate a suspended profile. Idempotent. |
| `POST /api/v1/profiles/{id}/archive` | Archive. **Terminal** — a suspended profile can be reactivated, an archived one cannot. |

When to use each:

- **Suspend** — the client pauses the engagement, or you need to freeze
  activity while investigating something. Contacts and assets stay
  readable (subject to grants); creates should stop at the UI layer.
- **Reactivate** — the engagement resumes.
- **Archive** — the engagement is over. The data stays for audit and
  history; the profile just leaves the active set. There is no delete:
  profile rows are never removed, matching the append-only posture of
  the audit log.

## Contacts: sources

Every contact carries `first_source` (immutable — where the contact
first came from), `latest_source`, and the full `source_touches`
history. The typed `ContactSource` vocabulary (`manual`, `import`,
`web-form`, `api`, `agent-action`, `campaign`, `other`) is stored as
strings, so unknown future sources read back as `other` without
breaking.

Operational notes:

- The create form (and the API) should always send a source. An empty
  source is accepted but makes attribution and dedupe triage harder.
- `source_touches` is append-only: re-importing the same CSV adds a
  touch, it doesn't rewrite history.

## Contacts: dedupe triage

Creating a contact whose normalized email already exists in the same
client profile returns **409 Conflict**. The error never echoes the
address (no PII in errors). Dedupe is strictly profile-scoped: the
same email in two different profiles is two different contacts, by
design — cross-tenant dedupe is impossible by construction.

What a 409 means in practice:

1. **Same person, re-submitted.** The usual case: a web form
   re-submitted, a CSV re-imported, an agent re-adding someone. Check
   the existing contact; update it instead of creating a duplicate.
2. **Same email, different person.** Shared inboxes (`info@…`,
   `sales@…`) legitimately belong to multiple humans. If the 409 is
   blocking a real second person, the current escape hatch is to add a
   distinguishing phone number and let the operator decide — there is
   no automatic merge-suggest yet (see below).
3. **Spike in 409s.** A sudden rise usually means a broken import job
   re-sending its file, or a form double-submit bug. Check
   `source_touches` on recent contacts and the `auth.rate_limit_tripped`
   audit events for the same window.

When to merge vs keep separate:

- **Merge** when the two records are the same human (same email, same
  phone, operator confirms). Use `POST /api/v1/contacts/merge`.
- **Keep separate** when they're distinct humans sharing an address, or
  when you're unsure. A false merge is harder to undo than a duplicate
  is to clean up later.

## Contacts: merge procedure and audit trail

`POST /api/v1/contacts/merge` takes
`{"survivor_id": "…", "duplicate_id": "…"}`. Requirements:

- Both contacts must be in the **same client profile** (else 403).
- The caller needs `contact:update` on that profile's scope.
- Neither party may already be merged away (else 409).

Semantics (from the domain):

- The survivor absorbs the union of emails, phones, sources, and
  consent records (deduped, sorted for a deterministic record).
- Lifecycle: the further-along stage wins; **Archived is sticky**.
- The duplicate is archived and tombstoned via `merged_into` pointing
  at the survivor. It is **never deleted**.
- A `contact.merge` audit event is recorded with the survivor's id, so
  every merge is traceable in `GET /api/v1/audit/events`.

To review a merge afterwards: fetch the survivor (union visible),
fetch the duplicate (shows `lifecycle: Archived` and
`merged_into: <survivor id>`), and query the audit log for
`action=contact.merge`.

## Assets

Assets are file/media **metadata** — the CRM tracks the file's identity
(name, MIME, size, SHA-256), not its bytes. Byte storage is out of scope
(documented limitation); when it lands, the SHA-256 already on the row
is the integrity anchor.

| Endpoint | Effect |
|---|---|
| `POST /api/v1/assets` | Register metadata (201). Idempotent via `Idempotency-Key`. |
| `GET /api/v1/assets?client_profile_id=…` | List a profile's assets (profile-scoped). |
| `GET /api/v1/assets/{id}` | Fetch one. Cross-profile → 404. |
| `POST /api/v1/assets/{id}/archive` | Archive. Idempotent; never deleted. |

Upload dedupe: registering the same SHA-256 under the same profile
returns **200 with the existing asset** instead of a duplicate row —
even if the file name differs. This is per-profile: the same file
uploaded for two clients is two assets.

Archive when the file is superseded or the client asks for removal.
The row stays (auditability); serve 410/Gone at the byte-store layer
when it exists.

## Seed procedure

`crates/crm-db/examples/seed.rs` populates the demo tenant. It is
**dev-only** and gated by `CRM_SEED_DEV=1` (dry-run otherwise), and
idempotent (re-runs skip what's already there):

```bash
CRM_SEED_DEV=1 cargo run -p crm-db --example seed
```

It creates:

- The demo profile ("Demo Client", id `123e4567-e89b-42d3-a456-426614174000`)
  claimed for the demo agency.
- Five demo contacts with varied sources (`web-form`, `csv-import`,
  `referral`, `phone-call`). Re-runs meet the dedupe 409 and count
  those as "skipped", not failures.
- One demo asset (`demo-welcome.pdf`, `application/pdf`), idempotent
  via the content-hash unique index.
- The demo staff user `demo@example.com` / `crm-demo-password`
  (printed on every run; dev-only) with contact read/create/update
  grants on the demo profile.

## UI boundary with 06-UI

The current frontend (`crates/crm-web`) is the 01-FND vertical slice
plus auth: the contacts page lists contacts (showing each contact's
`latest_source`) and hosts the create form. Profiles and assets have
**no UI yet** — they are API-only in this workstream.

06-UI owns the full frontend: profile switcher/management, asset
lists, dedupe triage UI (showing candidate duplicates on 409),
and the merge workflow. Until then, profiles and assets are operated
via the API as documented above.
