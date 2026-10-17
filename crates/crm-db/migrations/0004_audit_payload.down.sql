-- Down migration for 0004_audit_payload: drops the payload column, the
-- idempotency key column, and the query indexes. The audit envelope
-- columns (and the chain) are untouched.
DROP INDEX IF EXISTS audit_events_action_occurred_idx;
DROP INDEX IF EXISTS audit_events_actor_occurred_idx;
DROP INDEX IF EXISTS audit_events_profile_occurred_idx;
DROP INDEX IF EXISTS audit_events_idempotency_key_uniq;
ALTER TABLE audit_events DROP COLUMN IF EXISTS idempotency_key;
ALTER TABLE audit_events DROP COLUMN IF EXISTS payload;
