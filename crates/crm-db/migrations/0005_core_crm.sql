-- 0005_core_crm: Core CRM vertical slice (task 05-CRM-02).
--
-- * `client_profiles` grows from the FK placeholder into a real entity
--   table: `agency_id`, `name`, `status`, `updated_at`. The new columns
--   are nullable except `status`/`updated_at` so the
--   `ensure_client_profile` upsert (which inserts only the id) keeps
--   working; repository `create` fills the full row via
--   INSERT ... ON CONFLICT DO UPDATE.
-- * `assets` holds file/media attachment metadata, content-addressed by
--   SHA-256 (`sha256` BYTEA, unique per profile for upload dedupe).
-- * `contacts` already carries source columns and `merged_into` from
--   the foundation migration; nothing to add there.
--
-- Additive only; re-runnable.

ALTER TABLE client_profiles
    ADD COLUMN IF NOT EXISTS agency_id UUID,
    ADD COLUMN IF NOT EXISTS name TEXT,
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'active',
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE INDEX IF NOT EXISTS client_profiles_agency_id_idx
    ON client_profiles (agency_id);

CREATE TABLE IF NOT EXISTS assets (
    asset_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    uploaded_by UUID NOT NULL,
    file_name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes > 0),
    sha256 BYTEA NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped reads: every asset query is filtered by client_profile_id.
CREATE INDEX IF NOT EXISTS assets_client_profile_id_idx
    ON assets (client_profile_id);

-- Upload dedupe: the same bytes are never stored twice under one profile.
CREATE UNIQUE INDEX IF NOT EXISTS assets_profile_sha256_uniq
    ON assets (client_profile_id, sha256);
