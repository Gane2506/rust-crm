-- SPDX-License-Identifier: GPL-3.0-only
-- 0018_email_idempotency_key down migration (FIX-01).

DROP INDEX IF EXISTS email_messages_profile_idempotency_key_uniq;
ALTER TABLE email_messages DROP COLUMN IF EXISTS idempotency_key;
