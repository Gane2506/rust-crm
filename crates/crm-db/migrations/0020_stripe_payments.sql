-- SPDX-License-Identifier: GPL-3.0-only
-- 0020_stripe_payments: Stripe Checkout/webhook support (STRIPE-01).
--
-- - `payments.external_reference`: the payment provider's reference for
--   the payment (the Stripe Checkout Session id, e.g. `cs_test_…`),
--   NULL for manually recorded payments. The webhook looks a payment
--   up by this reference to tell "already recorded" from "not yet
--   recorded", which is what makes webhook redelivery idempotent.
-- - `stripe_webhook_events`: one row per Stripe webhook event id,
--   claimed atomically (`INSERT ... ON CONFLICT DO NOTHING`) before
--   the payment is recorded, so a redelivered event can never
--   double-record. `payment_id` is filled in once the payment exists
--   (NULL while the claim is in flight); `invoice_id` is kept for
--   operator debugging.
--
-- Additive only; re-runnable.

ALTER TABLE payments
    ADD COLUMN IF NOT EXISTS external_reference TEXT;

CREATE TABLE IF NOT EXISTS stripe_webhook_events (
    event_id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    invoice_id UUID,
    payment_id UUID REFERENCES payments (payment_id),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
