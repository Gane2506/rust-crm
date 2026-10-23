-- SPDX-License-Identifier: GPL-3.0-only
-- 0018_email_idempotency_key: idempotency key on email_messages (FIX-01).
--
-- Campaign launch generates deterministic idempotency keys
-- (`campaign-{id}-step-{id}-contact-{id}`) but never persisted them, so
-- `EmailServiceImpl::send` could not honor them and a crash mid-launch
-- followed by a re-launch re-sent to already-mailed contacts. Storing the
-- key per message lets `send` return the original message on replay.
--
-- The partial unique index is the backstop against concurrent duplicate
-- sends: two writers racing the check-then-create window collide here
-- instead of double-sending. NULL keys (ad-hoc sends without a key) are
-- excluded so they never collide.

ALTER TABLE email_messages
    ADD COLUMN IF NOT EXISTS idempotency_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS email_messages_profile_idempotency_key_uniq
    ON email_messages (client_profile_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
