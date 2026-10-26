# Billing operations runbook

How the billing vertical slice (tasks 12-BILL-01 through
12-BILL-03) works in production: invoices raised against
client profiles, line items, and payment records with
coverage tracking. This is **record-keeping** — it tracks
invoice lifecycles and payment coverage; it does not
execute charges itself. Charges go through the Stripe
adapter (STRIPE-01): see `stripe.md`. For the auth model,
see `security.md`; for the audit events billing emits,
`audit-log.md`; for the billing security posture,
`threat-model.md`.

## What it is

Staff raise invoices against a client profile for
services rendered, and record payments against them.
Four layers:

- **Domain** (`crates/crm-domain/src/billing.rs`):
  `Money` (minor-unit `i64`, never negative; validated
  ISO 4217 code; `Deserialize` implemented manually
  through `Money::new` so the invariants hold for
  deserialized values too), `Invoice` (`Draft` /
  `Issued` / `Paid` / `Void`; lines frozen at issue;
  `mark_paid()` is transition-only, coverage is the
  service's job), `InvoiceLine` (non-blank description,
  positive quantity, overflow-checked totals),
  `Payment` (`Recorded` / `Reversed`, append-only),
  `Invoice::remaining_due(&[Payment])` (validates
  invoice/currency match per payment, excludes
  reversed, rejects overpayment), and the
  `BillingService` / repository traits.
- **Persistence** (`crates/crm-db`, migration
  `0012_billing`): `invoices`, `invoice_lines`,
  `payments`. The repository hydrates the full
  `Invoice` (with lines) on read.
- **Service** (`crates/crm-core/src/billing.rs`):
  `BillingServiceImpl` over the invoice, payment, and
  audit ports. Coverage is checked **before** anything
  is persisted (an overpayment is a `Validation` error
  with zero DB writes); a fully-covering payment
  transitions the invoice `Issued → Paid` via
  `mark_paid()`; a reversal that leaves a `Paid`
  invoice with a balance reopens it to `Issued` via
  `reopen()`. Every mutation records an audit event;
  audit failures are swallowed (fail-loud at the HTTP
  layer, never fail the operation).
- **HTTP** (`crates/crm-api/src/billing.rs`): 10
  endpoints under `/api/v1` — invoices CRUD plus
  lines / issue / void / balance, and payments
  record / list / reverse.

Every invoice belongs to one client profile — the
tenant boundary. Authorization uses the `Invoice`
resource with profile-scoped grants
(`invoice/read`, `invoice/create`, `invoice/update`).
A missing invoice is `404` before any grant check
(fetch-then-authorize).

## Invoice lifecycle

```
Draft → Issued → Paid
  ↓        ↓
  Void     Void        (void is idempotent on Void;
Paid → Issued          rejected on Paid — refunds are
  (reopen,             a separate, later flow)
  on payment reversal)
```

- `Draft → Issued` (`issue()`): requires at least one
  line — a zero-line invoice bills nothing and must
  not be issued. Once issued, the total is a
  **financial fact**: `add_line()` rejects non-draft
  invoices.
- `Issued → Paid` (`mark_paid()`): only the
  transition; the service performs the coverage check
  via `remaining_due()` first and calls `mark_paid()`
  only when the remaining balance is zero.
- `Paid → Issued` (`reopen()`): the reversal path.
  When a payment that completed the coverage is
  reversed, the invoice is no longer fully paid, so it
  returns to `Issued` rather than sitting in `Paid`
  with a positive balance. Audited as
  `billing.payment_reversed`.
- `Draft/Issued → Void` (`void()`): idempotent on
  `Void`; rejected on `Paid` ("a paid invoice cannot
  be voided; issue a refund instead").

## Payment coverage semantics

`record_payment` runs, in order:

1. Fetch the invoice; enforce profile scope.
2. Reject currency mismatch against the invoice.
3. `Payment::record()` rejects zero amounts — nothing
   is persisted yet.
4. **Coverage check before persist**: list the
   invoice's payments, compute `remaining_due()`; a
   payment exceeding the remaining balance is a
   `Validation` error with **zero DB writes**.
5. Persist the payment; re-read; if the balance is now
   zero, `mark_paid()` and persist the transition.

`reverse_payment` flips the payment to `Reversed`
(idempotent), persists, then reopens the invoice to
`Issued` if it was `Paid` and a balance remains.

Endpoints (`{id}` is the invoice UUID, `{pid}` the
payment UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `POST /api/v1/invoices` `{client_profile_id, currency}` | `invoice/create` | Create a draft invoice (`201`) |
| `GET /api/v1/invoices?client_profile_id=<uuid>&limit=<n>` | `invoice/read` | List invoices for one profile, newest first (`200`; response `{invoices, total}`) |
| `GET /api/v1/invoices/{id}` | `invoice/read` | Fetch an invoice with its lines (`200`) |
| `POST /api/v1/invoices/{id}/lines` `{description, quantity, unit_price}` | `invoice/update` | Add a line to a draft invoice (`201`; `422` on issued/paid/void invoices, blank description, zero quantity, currency mismatch) |
| `POST /api/v1/invoices/{id}/issue` | `invoice/update` | Issue a draft invoice (`200`; `422` on empty or non-draft) |
| `POST /api/v1/invoices/{id}/void` | `invoice/update` | Void a draft or issued invoice (`200`; idempotent; `422` on paid) |
| `GET /api/v1/invoices/{id}/balance` | `invoice/read` | Amount still owed, as `Money` (`200`) |
| `POST /api/v1/invoices/{id}/payments` `{amount}` | `invoice/update` | Record a payment (`201`; fully-covering payment transitions the invoice to `Paid`; `422` on overpayment, zero amount, currency mismatch) |
| `GET /api/v1/invoices/{id}/payments` | `invoice/read` | List payments, oldest first (`200`; response `{payments, total}`) |
| `POST /api/v1/payments/{id}/reverse` | `invoice/update` | Reverse a payment (`200`; idempotent; reopens a `Paid` invoice left with a balance) |

## Audit trail

The service records audit events with these pinned
action names (see `audit-log.md`):

`billing.invoice_created`, `billing.line_added`,
`billing.invoice_issued`, `billing.invoice_voided`,
`billing.payment_recorded`, `billing.payment_reversed`.

Reads (`list_invoices`, `invoice_balance`,
`list_payments`) are not audited, matching the
domain contract. Audit is fail-loud at the HTTP layer
and never fails the operation: a recording failure is
swallowed in the service so billing work is never
blocked by the audit pipeline.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated
by `CRM_SEED_DEV=1`) creates a demo invoice on the demo
profile through the real `BillingServiceImpl` (so
lifecycle, coverage, and audit behavior match
production): a USD draft invoice with two lines
("Monthly retainer" × 1 @ $250.00, "Onboarding" × 1 @
$150.00), issued, with a partial $200.00 payment
recorded (balance $200.00, still `Issued`) — plus
`invoice` read/create/update grants for the demo staff
user:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed is idempotent (skipped when any invoices
already exist on the demo profile). Seeding uses the
service layer — never raw SQL — so storage behavior
matches production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on billing endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on billing endpoints | No `invoice/*` grant on the profile | Have an operator check the `grants` table for `invoice` on that profile scope. Grants are per-profile; cross-profile access is denied by design. |
| `422` on add-line | Invoice is not a draft | Lines are frozen at issue — a financial fact. Create a new invoice or void and re-raise. |
| `422` on issue | No lines, or already issued/void/paid | An invoice with no lines cannot be issued; transitions are one-way. |
| `422` on void | Invoice is paid | A paid invoice cannot be voided; refunds are a separate, later flow. |
| `422` on record-payment | Overpayment, zero amount, or currency mismatch | Payments may never exceed the remaining balance; amounts must be positive and match the invoice currency. |
| `404` on invoice endpoints | Wrong id, or the invoice belongs to another profile | Fetch-then-authorize: a missing row is `404` before any grant check. |
| `422` on create-invoice | Bad currency code | Currency must be a 3-letter uppercase ISO 4217 code (`USD`, `EUR`, …). |

## Known limitations

- **Record-keeping only, no payment processing.**
  Nothing here executes a charge, talks to Stripe, or
  moves money — it records invoices and payments the
  agency raises by hand (or a later adapter feeds in).
  Processor adapters are a later workstream.
- **No billing UI.** The Leptos frontend has no
  invoice or payment views — billing is API-only until
  a frontend task picks it up.
- **No refunds.** A paid invoice cannot be voided and
  there is no refund operation; reversing a payment
  reopens the invoice to `Issued` but does not model
  money flowing back to the client. Refunds are a
  later task.
- **`total` is the page length.** The list responses'
  `total` counts the returned items, not the profile's
  full invoice/payment corpus — there is no
  unfiltered count query yet.
- **Idempotency keys on billing writes.** All `POST`
  billing endpoints accept `Idempotency-Key` (a UUID):
  a repeated request with the same key and payload
  replays the original response; the same key with a
  different payload is a `409`. A key identifies one
  operation — reusing it on a different route is also
  a `409`. The replay check runs after authorization.
  (FIX-02, 2026-10-22.)
- **Concurrency guards.** `record_payment` is atomic:
  it locks the invoice row (`SELECT ... FOR UPDATE`),
  validates coverage against the latest payments, and
  marks the invoice paid when covered — all in one
  transaction. Concurrent payments serialize on the
  lock; the loser fails validation instead of
  over-applying. `add_line`, `issue`, and `void` use an
  atomic conditional update (`WHERE status = ANY(...)`)
  so a concurrent transition surfaces a `409` instead
  of a lost update. (FIX-03, 2026-10-22.)
