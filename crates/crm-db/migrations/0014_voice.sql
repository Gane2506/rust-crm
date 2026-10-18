-- SPDX-License-Identifier: GPL-3.0-only
-- 0014_voice: Calling vertical slice (task 14-VOICE-02).
--
-- * `calls` holds one row per call per client profile: direction
--   ('outbound'/'inbound'), the validated from/to numbers, the
--   forward-only lifecycle status ('initiated'/'ringing'/
--   'in_progress'/'completed'/'failed'/'no_answer'/'busy'), the
--   opaque provider-side call reference, timing, and the recording
--   consent ('granted'/'denied'/'unknown' — 'unknown' is the
--   default; silence is not consent).
-- * `call_recordings` holds the recording lifecycle per call: the
--   consent-pinned start/stop times and the audio asset id (the
--   bytes live in the asset store, never inline).
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS calls (
    call_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    contact_id UUID REFERENCES contacts (contact_id),
    direction TEXT NOT NULL,
    from_number TEXT NOT NULL,
    to_number TEXT NOT NULL,
    status TEXT NOT NULL,
    provider_call_id TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ,
    duration_secs BIGINT,
    failure_reason TEXT,
    recording_consent TEXT NOT NULL DEFAULT 'unknown',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS call_recordings (
    recording_id UUID PRIMARY KEY,
    call_id UUID NOT NULL REFERENCES calls (call_id) ON DELETE CASCADE,
    asset_id UUID,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    stopped_at TIMESTAMPTZ,
    CONSTRAINT call_recordings_call_unique UNIQUE (call_id)
);

-- Tenant-scoped call reads, newest first.
CREATE INDEX IF NOT EXISTS calls_client_profile_id_idx
    ON calls (client_profile_id, created_at DESC, call_id DESC);

-- Contact-scoped call reads, newest first.
CREATE INDEX IF NOT EXISTS calls_contact_id_idx
    ON calls (contact_id, created_at DESC, call_id DESC);

-- Recording reads for one call, oldest first.
CREATE INDEX IF NOT EXISTS call_recordings_call_id_idx
    ON call_recordings (call_id, started_at ASC);
