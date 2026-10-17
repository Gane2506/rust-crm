-- 0004_audit_payload: forensic payload and idempotency for the audit log.
--
-- Task 04-AUD-02 persists the `SecurityEvent` taxonomy alongside the
-- `AuditEvent` envelope:
-- * `payload` JSONB holds the serialized `SecurityEvent` (typed forensic
--   detail: fingerprints, ids, reasons). NULL for pre-existing
--   contact-lifecycle events, which have no security payload.
-- * `idempotency_key` UUID (UNIQUE) makes security-event recording
--   idempotent: re-recording with the same key returns the original
--   event instead of appending a duplicate. NULL for older rows; the
--   unique index only constrains non-NULL keys.
-- * The three composite indexes serve the `AuditQuery` filters
--   (client_profile_id, actor_id, action) ordered by occurred_at.
--
-- Additive only; re-runnable.

ALTER TABLE audit_events ADD COLUMN IF NOT EXISTS payload JSONB;
ALTER TABLE audit_events ADD COLUMN IF NOT EXISTS idempotency_key UUID;

CREATE UNIQUE INDEX IF NOT EXISTS audit_events_idempotency_key_uniq
    ON audit_events (idempotency_key);

CREATE INDEX IF NOT EXISTS audit_events_profile_occurred_idx
    ON audit_events (client_profile_id, occurred_at);
CREATE INDEX IF NOT EXISTS audit_events_actor_occurred_idx
    ON audit_events (actor_id, occurred_at);
CREATE INDEX IF NOT EXISTS audit_events_action_occurred_idx
    ON audit_events (action, occurred_at);
