-- 0003_identity: staff identity, sessions, RBAC grants and delegations.
--
-- Backs the `crm-domain::security` model from task 03-SEC-01:
-- * `staff_users` — agency staff accounts; the password credential lives
--   in a `phc_hash TEXT` column (never a plaintext column; NULL until
--   enrolled). `email` is unique and stored normalized (lowercase) by the
--   service layer.
-- * `delegations` — bounded delegations of grant subsets to agent
--   principals. The validated grant subset is stored as JSONB; there is no
--   repository trait for delegations yet (03-SEC-02 fails closed on
--   delegation-linked sessions), so this table is forward schema for the
--   agent-delegation workstream.
-- * `sessions` — login sessions keyed by `session_id`; only the SHA-256
--   `token_hash` is stored, never the raw token. Rows are never deleted:
--   revocation is a timestamp, because session history is
--   security-relevant.
-- * `grants` — additive grant assignments; revocation is a `revoked_at`
--   tombstone, not a delete, because grant history is audit-relevant.
--   `list_for_staff` returns only non-revoked rows.
--
-- Additive only; re-runnable via the _sqlx_migrations tracking table.

CREATE TABLE IF NOT EXISTS staff_users (
    staff_id UUID PRIMARY KEY,
    agency_id UUID NOT NULL,
    -- Normalized (lowercase) by the service layer before write and lookup.
    email TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    -- 'active' | 'suspended'.
    status TEXT NOT NULL,
    -- PHC-format hash string (`$argon2id$...`); NULL until enrolled.
    -- Never a plaintext column.
    phc_hash TEXT,
    credential_updated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS delegations (
    delegation_id UUID PRIMARY KEY,
    delegator_staff_id UUID NOT NULL REFERENCES staff_users(staff_id),
    agent_principal_id UUID NOT NULL,
    -- The validated grant subset, serialized as JSON (`Vec<Grant>`).
    grants JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS sessions (
    session_id UUID PRIMARY KEY,
    staff_id UUID NOT NULL REFERENCES staff_users(staff_id),
    -- SHA-256 over the bearer token; the raw token is never persisted.
    token_hash BYTEA NOT NULL UNIQUE,
    -- When set, an agent session acting under the delegation. NULL for
    -- ordinary staff sessions.
    delegation_id UUID REFERENCES delegations(delegation_id),
    issued_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    last_activity_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS sessions_staff_id_idx ON sessions (staff_id);

CREATE TABLE IF NOT EXISTS grants (
    grant_id UUID PRIMARY KEY,
    staff_id UUID NOT NULL REFERENCES staff_users(staff_id),
    -- Resource/operation strings, e.g. 'contact' / 'read'. See
    -- `resource_as_str` / `operation_as_str` in `crates/crm-db/src/postgres.rs`.
    resource TEXT NOT NULL,
    operation TEXT NOT NULL,
    -- 'agency' | 'client_profile'.
    scope_kind TEXT NOT NULL,
    agency_id UUID NOT NULL,
    -- NULL for agency scope; set for client-profile scope.
    client_profile_id UUID,
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Revocation tombstone; NULL means the grant is live.
    revoked_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS grants_staff_id_idx ON grants (staff_id);
