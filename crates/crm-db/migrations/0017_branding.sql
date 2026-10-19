-- 0017_branding: per-agency visual customization (task UI2-03).
--
-- `branding` holds versioned branding-theme snapshots, following the
-- `agency_settings` pattern: one row per version, `version` globally
-- unique and monotonically increasing, `payload` carrying the theme as
-- JSONB. The latest row per `agency_id` is the current theme.

CREATE TABLE IF NOT EXISTS branding (
    branding_id UUID PRIMARY KEY,
    agency_id UUID NOT NULL,
    version BIGINT NOT NULL UNIQUE,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS branding_agency_version_idx
    ON branding (agency_id, version DESC);
