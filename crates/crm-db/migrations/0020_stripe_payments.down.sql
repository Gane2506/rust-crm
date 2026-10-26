-- SPDX-License-Identifier: GPL-3.0-only
-- 0020_stripe_payments down: remove the Stripe webhook events table
-- and the payments external-reference column.

DROP TABLE IF EXISTS stripe_webhook_events;

ALTER TABLE payments
    DROP COLUMN IF EXISTS external_reference;
