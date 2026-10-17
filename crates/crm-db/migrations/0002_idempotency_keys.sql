-- 0002_idempotency_keys: persistent idempotency-key store for the API.
--
-- The vertical slice kept idempotent responses in process memory, which
-- loses them on restart and breaks the "all mutations require idempotency"
-- handoff rule for a multi-process deployment. This table persists the
-- (request fingerprint, response body) pair per key so replays survive
-- restarts. A repeated key with a *different* fingerprint is still a
-- conflict, enforced by comparing the stored fingerprint in the API layer.
--
-- Additive only; re-runnable via the _sqlx_migrations tracking table.

CREATE TABLE IF NOT EXISTS idempotency_keys (
    -- `key` is a reserved word in PostgreSQL; the column is named
    -- `idempotency_key` instead.
    idempotency_key UUID PRIMARY KEY,
    -- Bit-preserving `u64` request fingerprint stored as BIGINT; the API
    -- layer casts back with `as u64`.
    fingerprint BIGINT NOT NULL,
    -- The exact response body to replay.
    response JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
