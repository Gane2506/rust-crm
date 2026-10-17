-- 0001_foundation: persistence foundation for rust-crm.
--
-- Tables: client_profiles (tenant boundary), contacts (person records),
-- audit_events (tamper-evident log), outbox (transactional outbox for
-- reliable domain-event publishing).
--
-- Conventions:
-- * UUID primary keys everywhere; no business meaning in ids.
-- * TIMESTAMPTZ for every timestamp.
-- * TEXT for small enums (lifecycle, actor kind, decisions) so values stay
--   readable in psql; the Rust mappers in crm-db translate to/from the
--   domain enums explicitly (no format drift via serializers).
-- * JSONB for structured payloads (consent records, outbox payloads).
-- * BYTEA for the 32-byte SHA-256 chain hashes.
-- * audit_events.seq (BIGSERIAL) gives a total append order so "latest
--   head" reads are deterministic even when two events share a timestamp.
-- Additive only; re-runnable via the _sqlx_migrations tracking table.

CREATE TABLE client_profiles (
    client_profile_id UUID PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE contacts (
    contact_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    normalized_emails TEXT[] NOT NULL DEFAULT '{}',
    normalized_phones TEXT[] NOT NULL DEFAULT '{}',
    lifecycle TEXT NOT NULL,
    first_source TEXT NOT NULL,
    latest_source TEXT NOT NULL,
    source_touches TEXT[] NOT NULL DEFAULT '{}',
    consent_records JSONB NOT NULL DEFAULT '[]',
    merged_into UUID NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped reads: every contact query is filtered by client_profile_id.
CREATE INDEX contacts_client_profile_id_idx ON contacts (client_profile_id);

CREATE TABLE audit_events (
    seq BIGSERIAL UNIQUE,
    event_id UUID PRIMARY KEY,
    occurred_at TIMESTAMPTZ NOT NULL,
    actor_kind TEXT NOT NULL,
    actor_id UUID NOT NULL,
    client_profile_id UUID NULL REFERENCES client_profiles (client_profile_id),
    action TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    correlation_id UUID NOT NULL,
    permission_decision TEXT NOT NULL,
    result TEXT NOT NULL,
    before_ref TEXT NULL,
    after_ref TEXT NULL,
    prev_hash BYTEA NOT NULL,
    chain_hash BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Audit timeline reads: one tenant's events, newest first.
CREATE INDEX audit_events_client_occurred_idx
    ON audit_events (client_profile_id, occurred_at DESC);

CREATE TABLE outbox (
    id UUID PRIMARY KEY,
    aggregate_type TEXT NOT NULL,
    aggregate_id UUID NOT NULL,
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ NULL
);

-- The outbox relay polls unpublished rows; the partial index keeps that
-- scan cheap no matter how large the published history grows.
CREATE INDEX outbox_unpublished_idx ON outbox (published_at)
    WHERE published_at IS NULL;
