# Contract signing operations runbook

How the contract-signing vertical slice (tasks
13-SIGN-01 through 13-SIGN-03) works in production:
Rust-native signing templates, immutable rendered
document snapshots, forensic signing evidence, and
the signed ≠ effective payment gate. For the auth
model, see `security.md`; for the audit events
signing emits, `audit-log.md`; for the signing
security posture, `threat-model.md`.

## What it is

Staff create contract templates with
`{{placeholder}}` variables, render contracts from
them for a client profile, collect signatures with
forensic evidence, and satisfy the payment gate to
make a contract effective. Four layers:

- **Domain** (`crates/crm-domain/src/signing.rs`,
  extending `crates/crm-domain/src/contract.rs`):
  `SigningTemplate` (immutable versions; `{{placeholder}}`
  variables validated at construction; `render()`
  substitutes variables into a `ContractDocument`),
  `ContractDocument` (the immutable snapshot a
  contract references — template id + version +
  rendered body + SHA-256 content hash),
  `SigningEvidence` (who signed, when, how; the
  raw IP and user agent never persist, only their
  SHA-256 fingerprints), `SignatureMethod` (typed:
  typed / drawn / click-to-sign, never free text).
  The `Contract` aggregate itself lives in
  `contract.rs` (task 01-FND-01) and owns the state
  machine; the signing module extends it rather
  than duplicating it. Also: the `SigningService`
  and repository traits.
- **Persistence** (`crates/crm-db`, migration
  `0013_signing`): `signing_templates`
  (`UNIQUE(name, version)`), `contracts` (status,
  signers JSONB, payment requirement JSONB,
  document snapshot JSONB, evidence bundle ref),
  `signing_evidence` (`UNIQUE(contract_id,
  signer_email)`). The repository hydrates the
  full `Contract` aggregate on read.
- **Service** (`crates/crm-core/src/signing.rs`):
  `SigningServiceImpl` over the contract, template,
  and audit ports. `create_contract` renders the
  template into a `ContractDocument` snapshot and
  stores it alongside the contract; `sign_contract`
  fingerprints the network identifiers, records
  the evidence, and advances the state machine as
  signers sign; `satisfy_payment_gate` is the
  reconciliation hook that asserts the required
  payment was verified. Every mutation records an
  audit event; audit failures are swallowed
  (fail-loud at the HTTP layer, never fail the
  operation).
- **HTTP** (`crates/crm-api/src/signing.rs`): 11
  endpoints under `/api/v1` — signing templates
  CRUD, contracts CRUD plus request / sign /
  satisfy-payment-gate / void, and document +
  evidence reads.

Every contract belongs to one client profile — the
tenant boundary. Authorization uses the `Contract`
resource with profile-scoped grants
(`contract/read`, `contract/create`,
`contract/update`). Templates are agency-level (no
profile). A missing contract is `404` before any
grant check (fetch-then-authorize).

## Signing lifecycle

```
Draft → Sent → PartiallySigned → SignedAwaitingPayment → Effective
  ↓        ↓          ↓                    ↓
  Cancelled Cancelled Cancelled          Cancelled
```

- `Draft → Sent` (`request_signature()`): the
  contract is sent for signature. The rendered
  document snapshot travels with it — the signer
  sees the exact bytes pinned by the content hash.
- `Sent → PartiallySigned → SignedAwaitingPayment`
  (`sign_contract()`): each signature records
  `SigningEvidence` and marks the signer's slot.
  When some but not all signers have signed, the
  contract is `PartiallySigned`; when all have
  signed, it becomes `SignedAwaitingPayment`. The
  domain rejects the direct `Sent →
  SignedAwaitingPayment` jump — the service walks
  both edges (this is the two-step the
  implementation added during 13-SIGN-02
  verification).
- `SignedAwaitingPayment → Effective`
  (`satisfy_payment_gate()`): **signed ≠ effective.**
  The transition requires `payment_verified =
  true`; `transition_to` rejects it otherwise. The
  method is a reconciliation hook: invoking it
  asserts the required payment was verified. A
  future task wires it to billing; until then it is
  manual.
- Any pre-effective state `→ Cancelled`
  (`void_contract()`): idempotent on `Cancelled`.
  An `Effective` contract cannot be cancelled —
  effectiveness is terminal (disputes are a separate,
  later flow).

## Template rendering

Templates are Rust-native: a name, a body with
`{{placeholder}}` variables, and an immutable
version. Placeholder names must be non-empty
alphanumeric + underscore; unclosed placeholders
are rejected at construction.

```text
"Dear {{client_name}}, your {{plan}} plan starts {{start_date}}."
```

- **Versions are append-only.** Editing a template
  creates a new row with a bumped version
  (`UNIQUE(name, version)`); there is no update or
  delete. `create_template` computes the next
  version from the existing rows for that name.
- **`render()` is strict on coverage.** Every
  placeholder must have a variable; a missing
  variable is a `Validation` error. Extra variables
  are ignored (callers may pass a superset).
- **Contracts reference the snapshot, never the
  template.** `create_contract` renders the body and
  stores a `ContractDocument` (template id +
  version + rendered body + SHA-256 content hash)
  alongside the contract. Later template edits
  cannot rewrite what the signer saw.

## Evidence model

Each signature produces one `SigningEvidence`:

- **Who**: signer name + email (must match a signer
  slot on the contract — an unknown signer is a
  `Validation` error).
- **When**: `signed_at` (UTC, set at record time).
- **How**: `SignatureMethod` — `typed`, `drawn`, or
  `click_to_sign`. Typed, never free text: the
  method is part of the evidence and must be one of
  the flows the product implements.
- **Where**: the signer's IP and user agent are
  fingerprinted with SHA-256 (`audit::fingerprint`)
  **before** persistence. The raw values never
  reach the database — the evidence proves *which*
  network/client signed without storing PII in the
  clear. The fingerprints are deterministic: the
  same IP always hashes the same way, so repeat
  signers are correlatable by the operator.

Evidence rows are append-only (`UNIQUE(contract_id,
signer_email)`); the `/evidence` endpoint lists
them oldest first.

## The payment gate

The 01-FND-01 invariant stands: **a signed contract
is not an effective contract.** `PaymentRequirement`
(`None` / `Deposit { amount_cents, currency }` /
full amount) travels on the contract from creation.
`satisfy_payment_gate()` calls
`transition_to(Effective, true)` — the domain
rejects the move when the contract is not
`SignedAwaitingPayment`, or when `payment_verified`
is false.

Today the gate is **manual**: there is no automatic
link between a billing payment and the gate.
Calling the endpoint asserts the operator verified
the required payment. Wiring it to billing (e.g.
auto-satisfying when an invoice for the contract is
paid) is a later task — the runbook for that task
will define the reconciliation semantics.

Endpoints (`{id}` is the contract UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/signing/templates` `{name, body}` | `contract/create` (agency scope) | Create a template version (`201`; version auto-bumped) |
| `GET /api/v1/signing/templates?limit=<n>` | `contract/read` (agency scope) | List template versions, newest first (`200`; response `{templates, total}`) |
| `POST /api/v1/contracts` `{client_profile_id, template_id, variables, payment_requirement, signers}` | `contract/create` | Create a draft contract + render the document snapshot (`201`; `422` on missing variable, blank fields, no signers) |
| `GET /api/v1/contracts?client_profile_id=<uuid>&limit=<n>` | `contract/read` | List contracts for one profile, newest first (`200`; response `{contracts, total}`) |
| `GET /api/v1/contracts/{id}` | `contract/read` | Fetch a contract (`200`) |
| `GET /api/v1/contracts/{id}/document` | `contract/read` | Fetch the rendered document snapshot (`200`) |
| `GET /api/v1/contracts/{id}/evidence` | `contract/read` | List signing evidence, oldest first (`200`) |
| `POST /api/v1/contracts/{id}/request` | `contract/update` | Send a draft for signature (`Draft → Sent`; `200`) |
| `POST /api/v1/contracts/{id}/sign` `{signer_name, signer_email, ip_address, user_agent, method}` | `contract/update` | Record one signature with evidence (`200`; `422` on unknown signer, blank fields, illegal transition) |
| `POST /api/v1/contracts/{id}/satisfy-payment-gate` | `contract/update` | Assert payment verified (`SignedAwaitingPayment → Effective`; `200`) |
| `POST /api/v1/contracts/{id}/void` | `contract/update` | Cancel a pre-effective contract (`200`; idempotent; `422` on effective) |

## Audit trail

The service records audit events with these pinned
action names (see `audit-log.md`):

`signing.contract_created`,
`signing.signature_requested`,
`signing.contract_signed`,
`signing.contract_effective`,
`signing.payment_gate_satisfied`,
`signing.template_created`,
`signing.contract_voided`.

Note that `satisfy_payment_gate` records **two**
events — `signing.payment_gate_satisfied` and
`signing.contract_effective` — so the gate
assertion and the state change are independently
auditable. Reads (list/get contract, document,
evidence, templates) are not audited, matching the
domain contract. Audit is fail-loud at the HTTP
layer and never fails the operation.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated
by `CRM_SEED_DEV=1`) creates a demo signing template
("MSA", one `{{client_name}}` placeholder) and a
draft demo contract on the demo profile through the
real `SigningServiceImpl` (so rendering, evidence,
and audit behavior match production) — plus
`contract` read/create/update grants for the demo
staff user:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed is idempotent (skipped when any contracts
already exist on the demo profile). Seeding uses
the service layer — never raw SQL — so storage
behavior matches production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on signing endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on signing endpoints | No `contract/*` grant on the profile (or at agency scope for templates) | Have an operator check the `grants` table. Templates need agency-scoped grants; contracts need profile-scoped grants. |
| `422` on create-template | Blank name/body, or bad placeholder syntax | Placeholder names must be non-empty alphanumeric + underscore; `{{` must be closed. |
| `422` on create-contract | Missing variable, blank signer fields, or no signers | Supply a value for every `{{placeholder}}`; each signer needs a name and email. |
| `422` on sign | Unknown signer, or illegal transition | The signer email must match a slot on the contract; the contract must be `Sent` or `PartiallySigned`. |
| `422` on satisfy-payment-gate | Contract is not awaiting payment | Only `SignedAwaitingPayment` contracts can become effective — all signers must have signed first. |
| `422` on void | Contract is effective | Effective contracts cannot be cancelled; disputes are a separate flow. |
| `404` on contract endpoints | Wrong id, or the contract belongs to another profile | Fetch-then-authorize: a missing row is `404` before any grant check. |

## Known limitations

- **No signing UI.** The Leptos frontend has no
  template, contract, or signing views — signing is
  API-only until a frontend task picks it up.
- **The payment gate is manual.** There is no
  automatic link between billing payments and
  `satisfy_payment_gate`; calling it asserts the
  operator verified payment. Auto-wiring to billing
  is a later task.
- **`total` is the page length.** The list
  responses' `total` counts the returned items, not
  the profile's full contract/template corpus —
  there is no unfiltered count query yet.
- **No idempotency keys on signing writes.** Like
  billing, the signing endpoints do not accept
  `Idempotency-Key` — a retried `POST` creates a
  second contract/template/signature. Clients should
  reconcile via the list endpoints before retrying.
- **Evidence is keyed by signer email.** Re-signing
  by the same email overwrites nothing (the row is
  unique per contract+email); a signer who signs
  twice produces one evidence row and one slot
  update.
