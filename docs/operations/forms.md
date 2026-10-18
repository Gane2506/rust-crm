# Form builder operations runbook

How the form builder vertical slice (tasks 11-FORM-01 through
11-FORM-03) works in production: typed intake/lead forms, per-type
field validation, validate-then-persist submissions, and a
review-gated submission lifecycle. For the auth model, see
`security.md`; for the audit events forms emit, `audit-log.md`;
for the form security posture, `threat-model.md`.

## What it is

Staff compose intake forms (lead capture, client intake,
feedback) from typed fields; end-client answers flow through
published forms into reviewable submissions. Four layers:

- **Domain** (`crates/crm-domain/src/form.rs`):
  `Form` (`Draft → Published → Archived`, terminal, publish
  only from draft, archive idempotent), `FormField` (7 field
  types, per-type `validate_value`, required/optional,
  `position` display order, opaque non-null `config` JSON;
  `SelectField` must carry a non-empty `options` string
  array), `FormSubmission` (`New → Reviewed → Archived`,
  forward-only, review idempotent, review-before-archive),
  `FormAnswer`, and the `FormService` / repository traits.
- **Persistence** (`crates/crm-db`, migration `0010_forms`):
  `forms`, `form_fields` (no update path — delete/recreate),
  `form_submissions` (append-mostly; only `status` changes).
- **Service** (`crates/crm-core/src/form.rs`):
  `FormServiceImpl` over the form / field / submission /
  audit ports. `submit` runs
  `FormSubmission::validate_against(&fields)` **before**
  persisting (validate-then-persist): invalid answers never
  reach storage.
- **HTTP** (`crates/crm-api/src/forms.rs`): 11 endpoints on 8
  route paths under `/api/v1` — form CRUD plus
  publish/archive, field add/list/remove, submission
  submit/list/review.

Every form belongs to one client profile — the tenant
boundary. Authorization uses the `Form` resource with
profile-scoped grants (`form/create`, `form/read`,
`form/update`, `form/delete`). A submission's
`client_profile_id` comes from its form, not the request:
the tenant boundary is inherited, so callers cannot cross it
by lying about scope.

## Form lifecycle

`POST /api/v1/forms` (bearer auth, `form/create` grant on the
profile). The body carries `client_profile_id` and `name`
(validated non-blank):

```
Draft → Published → Archived (terminal)
```

- `publish()` works only from `Draft`. Publishing marks
  the form ready for intake — only `Published` forms accept
  submissions (the service rejects submissions to draft or
  archived forms).
- `archive()` is idempotent: archiving an already-archived
  form succeeds without changing it.
- `Archived` is terminal — nothing moves out of it.
- Field definitions are not versioned: editing a published
  form's fields changes what future submissions are validated
  against.

Endpoints (`{id}` is the form UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/forms` | `form/create` | Create a `Draft` form |
| `GET /api/v1/forms?client_profile_id=<uuid>` | `form/read` | List forms for one profile |
| `GET /api/v1/forms/{id}` | `form/read` | Fetch-then-authorize; returns the form plus its fields |
| `POST /api/v1/forms/{id}/publish` | `form/update` | `Draft → Published`; starts accepting submissions |
| `POST /api/v1/forms/{id}/archive` | `form/update` | `Draft`/`Published → Archived` (idempotent) |
| `POST /api/v1/forms/{id}/fields` | `form/update` | Add a typed field (`201`) |
| `GET /api/v1/forms/{id}/fields` | `form/read` | List the form's fields in display order |
| `DELETE /api/v1/forms/{id}/fields/{field_id}` | `form/delete` | Remove a field (path ownership verified) |
| `POST /api/v1/forms/{id}/submissions` | `form/create` | Submit answers (`201`; validate-then-persist, `422` on invalid) |
| `GET /api/v1/forms/{id}/submissions` | `form/read` | List submissions, newest first |
| `POST /api/v1/forms/{id}/submissions/{submission_id}/review` | `form/update` | `New → Reviewed` (idempotent) |

## Field types and validation

Seven field types; validation is structural, per value, and
runs both at field construction (config shape) and at submit
time (answer values):

| Type | API name | Validates |
|---|---|---|
| `TextField` | `text` | Free text (required ⇒ non-blank) |
| `EmailField` | `email` | Structural email check (same `is_valid_email` as the rest of the domain) |
| `PhoneField` | `phone` | ≥ 7 ASCII digits; dashes kept (phones are stored trim-deduped, never E.164) |
| `NumberField` | `number` | Parses as `f64` |
| `SelectField` | `select` | Value is a member of config `options` (string array, non-empty, required at construction) |
| `CheckboxField` | `checkbox` | `"true"` / `"false"`, case-insensitive |
| `DateField` | `date` | ISO `YYYY-MM-DD` (real calendar date) |

- Blank values pass for optional fields and fail for required
  ones. All values are trimmed before checking.
- A submission is validated against the form's **current**
  fields: every field must belong to the submission's form,
  every answer must reference a known field, every answer
  must validate against its field's type, and every required
  field must have a non-blank answer.
- Removing a field does not rewrite old submissions — their
  answers are historical records; only *new* submissions are
  validated against the current field set.

## Submission flow

`POST /api/v1/forms/{id}/submissions` (bearer auth,
`form/create` grant). The body carries an optional
`contact_id`, a non-blank `submitted_by_label`, and the
answers. The service:

1. loads the form and enforces the caller's profile scope;
2. loads the form's current fields;
3. builds the `FormSubmission` (`New` status, profile
   inherited from the form);
4. runs `validate_against(&fields)` — **before anything is
   stored**; a validation failure is a `422` and no row is
   written;
5. persists the submission and records `form.submitted`.

**Submission intake is staff-authenticated in this slice.**
The endpoint requires a staff bearer <redacted> and a
`form/create` grant, and every submission is written to the
audit log. Anonymous public intake (a link the public can
fill) is deliberately out of scope — see the threat model.

## Review lifecycle

Submissions are triaged, not just collected:

```
New → Reviewed → Archived (terminal)
```

- `review()` works only from `New`; reviewing an already-
  reviewed submission is idempotent.
- `archive()` works only from `Reviewed` (review-before-
  archive is a domain invariant, not a service
  convenience); archiving an archived submission is
  idempotent. `Archived` is terminal.
- The HTTP surface exposes review; archival runs through the
  domain + repository (a future task can pin a
  `form.submission_archived` audit name if the HTTP surface
  needs it).

## Audit trail

The service records audit events with these pinned action
names (see `audit-log.md`):

`form.created`, `form.published`, `form.archived`,
`form.field_added`, `form.field_removed`,
`form.submitted`, `form.submission_reviewed`.

Audit is fail-loud at the HTTP layer and never fails the
operation: a recording failure is swallowed in the service
(loud warn) so form work is never blocked by the audit
pipeline.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated by
`CRM_SEED_DEV=1`) creates one **published** demo form
(`"demo-seed-form"`) on the demo profile — a required text
field ("Full name"), a required email field ("Work email"),
and an optional select field ("Plan", options
`["basic", "pro"]`) — plus one reviewed demo submission
(`"Demo User"` answers), all driven through the real service
layer, plus `form` create/read/update/delete grants for the
demo staff user:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed form is idempotent (skipped when a
`demo-seed-form` already exists on the demo profile). It is
published at seed time — unlike workflows, which stay draft —
so the demo submission can be validated against a live form.
Seeding uses the service — never raw SQL — so scope checks,
validation, and audit events behave exactly as in
production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on form endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on form create/submit | No `form/create` grant on the profile | Have an operator check the `grants` table for `form` on that profile scope. Grants are per-profile; cross-profile access is denied by design. |
| `403` on publish/archive/field add/review | No `form/update` grant | Same as above, for the `Update` operation. |
| `403` on field removal | No `form/delete` grant | Removals use the `Delete` operation, not `Update`. |
| `422` on create | Blank form name | Names are validated non-blank. |
| `422` on field add | Blank label, null config, unknown type, or a select without non-empty string `options` | Fix the field body. |
| `422` on publish | Form is not in `Draft` | Only `Draft → Published` is allowed. |
| `422` on submit | Missing required answer, unknown field, answer of the wrong type, or an answer whose field belongs to another form | Fix the answers; check the form's current fields with `GET /api/v1/forms/{id}/fields`. |
| `422` on review | Submission is `Archived` | Archived is terminal. |
| `404` on form/field/submission reads | Wrong id, or the row belongs to another profile | Fetch-then-authorize: a missing row is `404` before any grant check. |
| `404` on field/submission under a form | The field/submission does not belong to the form in the path | Path ownership is verified separately from existence. |

## Known limitations

- **No form UI.** The Leptos frontend has no form builder,
  field editor, or submission inbox — forms are API-only
  until a frontend task picks them up.
- **Submission intake is staff-authenticated.** There is no
  anonymous public intake endpoint; the public-facing form
  link (rate-limited, CAPTCHA-guarded) is a later task, and
  its threat model must be addressed *before* it ships, not
  after.
- **Fields are not versioned.** Publishing snapshots
  nothing; editing a published form's fields changes what
  future submissions validate against. If definitions must
  be frozen per submission, that is a later task.
- **Per-type validation is structural, not sanitization.**
  `validate_value` checks shape (email structure, digits,
  option membership); it does not strip HTML, scripts, or
  control characters from answers. Consumers that render
  answers must escape them.
- **No submission-level timeout or retention policy.**
  Submissions persist indefinitely; retention is an
  operator decision (see the audit retention posture in
  `audit-log.md`).
