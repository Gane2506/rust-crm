# Stripe operations runbook

How the Stripe adapter (STRIPE-01) works in production: Checkout
sessions that let a client pay an issued invoice on Stripe's hosted
page, and the webhook that records the payment when Stripe confirms
it. This server never sees card details. For the billing record
keeping underneath, see `billing.md`; for the auth model,
`security.md`; for the webhook's security posture, `threat-model.md`.

## What it is

- **Checkout** (`POST /v1/invoices/{id}/stripe-checkout`,
  authenticated, `invoice/update` grant): creates a Stripe Checkout
  Session for an issued invoice. Amount = remaining balance,
  currency = invoice currency, one line item priced from the
  balance. Returns `201` with `{ "checkout_url" }`. A draft, void,
  or already-paid invoice (or a zero balance) is a `422`. Supports
  `Idempotency-Key` like the other billing writes — and the key is
  also passed to Stripe, so a retried creation returns the same
  session instead of a duplicate.
- **Webhook** (`POST /v1/stripe/webhook`, NOT bearer-authenticated):
  handles `checkout.session.completed` only. The signature is
  verified first (see below); a missing or bad signature is a `400`
  and nothing is processed. A paid session records the payment
  against the invoice named in the session's `metadata[invoice_id]`
  through the existing guarded payment path, so a webhook payment
  can never over-apply. The Stripe session id is stored on the
  payment as `external_reference`.
- **Idempotency**: one row per Stripe event id in
  `stripe_webhook_events` (migration `0020`), claimed atomically
  before recording. A redelivered event reports the payment the
  first delivery recorded instead of recording again. A claim is
  held only while the winner is processing; a claim older than five
  minutes with no payment is reclaimed by the next delivery.

## Configuration

Environment only. No Stripe keys are ever stored in the database,
in config files, or in the repo.

| Variable | Required | Purpose |
| --- | --- | --- |
| `STRIPE_SECRET_KEY` | for checkout | Stripe secret key (`sk_test_…` / `sk_live_…`). Without it the server still starts and everything else works; the checkout endpoint answers `503 "Stripe not configured"`. |
| `STRIPE_WEBHOOK_SECRET` | for the webhook | The endpoint's signing secret (`whsec_…`). Without it the webhook answers `503`. Each Stripe endpoint has its own secret — copy the one for *this* endpoint. |
| `STRIPE_SUCCESS_URL` | no | Where Stripe redirects after a successful Checkout. Default: the invoice detail page (`http://127.0.0.1:3000/billing/{invoice_id}`), so the payer lands back on the invoice they just paid. |
| `STRIPE_CANCEL_URL` | no | Where Stripe redirects when the customer cancels Checkout. Default: the invoice detail page (`http://127.0.0.1:3000/billing/{invoice_id}`). |

`STRIPE_API_BASE_URL` is a test-only override (the test suite
points it at a mock server). Never set it in production.

## Registering the webhook in the Stripe dashboard

1. In the Stripe dashboard, go to Developers → Webhooks → Add
   endpoint.
2. Endpoint URL: `https://<your-public-host>/api/v1/stripe/webhook`
   (the webhook path is `/api/v1/stripe/webhook`; the server listens
   on `127.0.0.1:3000` locally, so a public deployment needs its
   reverse proxy in front of it, as for the rest of the API).
3. Events to send: `checkout.session.completed` only. Other event
   types are acknowledged with `200` and ignored.
4. Copy the endpoint's **Signing secret** (`whsec_…`) into
   `STRIPE_WEBHOOK_SECRET` and restart the server.

## Test-mode flow

1. Set `STRIPE_SECRET_KEY` to a **test-mode** secret key
   (`sk_test_…`) and `STRIPE_WEBHOOK_SECRET` to the test-mode
   endpoint's signing secret. Never use a live key for testing.
2. Create and issue an invoice, then
   `POST /v1/invoices/{id}/stripe-checkout` with a staff token.
   The response's `checkout_url` opens Stripe's test Checkout page.
   In the UI, the same flow is the "Pay with Stripe" button on an
   issued invoice's detail page (shown only while a balance is owed).
3. Pay with Stripe's test card `4242 4242 4242 4242` (any future
   expiry, any CVC). Stripe redirects to the invoice detail page
   (`/billing/{invoice_id}`); the payment appears there once the
   webhook lands — the page does not poll for it, so a reload may be
   needed if the webhook is still in flight.
4. In the dashboard's webhook deliveries for the endpoint, the
   `checkout.session.completed` delivery should show `200`, and
   the invoice should be `Paid` with one payment carrying the
   session id as its external reference. Redelivering the same
   event from the dashboard must still show `200` and must not
   create a second payment.
5. For local webhook testing without a public URL, use the Stripe
   CLI: `stripe listen --forward-to http://127.0.0.1:3000/api/v1/stripe/webhook`
   prints a `whsec_…` secret for the forwarded endpoint — use that
   as `STRIPE_WEBHOOK_SECRET`. Note the CLI secret belongs to the
   listen session, not to a dashboard endpoint; do not mix them.

## Troubleshooting

- **Checkout answers `503 "Stripe not configured"`** — the server
  has no `STRIPE_SECRET_KEY`. Set it and restart.
- **Webhook answers `503 "Webhook not configured"`** — no
  `STRIPE_WEBHOOK_SECRET` is set.
- **Webhook answers `400 "Invalid signature"`** — the secret does
  not match the endpoint that sent the event (each endpoint has its
  own secret; the Stripe CLI's listen secret is different from a
  dashboard endpoint's), or the request body was modified in
  transit (a proxy must not re-serialize the JSON — verification is
  over the exact bytes Stripe sent). The server logs the failure
  reason without the secret or the payload.
- **Webhook answers `200` but no payment appears** — the event was
  acknowledged as permanently unrecordable; check the server log
  for the warn line. Common causes: the session's
  `metadata[invoice_id]` does not name an invoice (sessions created
  outside this API have no metadata), the invoice was voided or
  already paid, or a manual payment covered the balance after the
  Checkout Session was created (the guarded path refuses to
  over-apply — reconcile in the Stripe dashboard, refunding if the
  customer was charged).
- **Webhook answers `409`** — another delivery of the same event is
  still being processed. Stripe retries; the retry records or
  replays the payment.
- **Webhook answers `500`** — a storage error. Stripe retries;
  the event claim makes the retry safe.
