-- 0007_campaigns: Campaigns vertical slice (task 08-CAMP-02).
--
-- * `campaigns` holds the campaign aggregate: owner context (agency or
--   one client profile), channel, schedule (cron), one-shot
--   `schedule_for`, delivery state, and attribution configuration.
-- * `campaign_steps` holds the ordered outreach sequence (email / call
--   / wait). Position is unique per campaign (enforced by the service;
--   the domain documents this).
-- * `prospect_pools` groups prospect contacts inside a client profile.
-- * `pool_members` is the many-to-many join; the unique constraint
--   enforces the domain's dedupe-on-add invariant at the storage
--   layer.
-- * `opt_outs` is append-only (compliance): no UPDATE or DELETE path
--   exists. The unique constraint makes re-recording idempotent.
-- * `conversions` records attributed (or unattributed) conversions.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS campaigns (
    campaign_id UUID PRIMARY KEY,
    client_profile_id UUID REFERENCES client_profiles (client_profile_id),
    owner_kind TEXT NOT NULL,
    name TEXT NOT NULL,
    channel TEXT NOT NULL,
    prospect_pool_id TEXT NOT NULL,
    schedule TEXT NOT NULL,
    schedule_for TIMESTAMPTZ,
    suppression_policy TEXT NOT NULL DEFAULT '',
    delivery_state TEXT NOT NULL DEFAULT 'draft',
    conversion_rules TEXT NOT NULL DEFAULT '',
    attribution_window_days INTEGER NOT NULL DEFAULT 30 CHECK (attribution_window_days >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped campaign reads, newest first. Agency-owned campaigns
-- have a NULL client_profile_id and are read via the owner_kind index.
CREATE INDEX IF NOT EXISTS campaigns_client_profile_id_idx
    ON campaigns (client_profile_id, created_at DESC);

CREATE INDEX IF NOT EXISTS campaigns_owner_kind_idx
    ON campaigns (owner_kind);

CREATE TABLE IF NOT EXISTS campaign_steps (
    step_id UUID PRIMARY KEY,
    campaign_id UUID NOT NULL REFERENCES campaigns (campaign_id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    template_id TEXT,
    wait_seconds BIGINT CHECK (wait_seconds IS NULL OR wait_seconds > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (campaign_id, position)
);

-- Ordered step reads per campaign.
CREATE INDEX IF NOT EXISTS campaign_steps_campaign_id_idx
    ON campaign_steps (campaign_id, position ASC);

CREATE TABLE IF NOT EXISTS prospect_pools (
    pool_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped pool reads.
CREATE INDEX IF NOT EXISTS prospect_pools_client_profile_id_idx
    ON prospect_pools (client_profile_id);

CREATE TABLE IF NOT EXISTS pool_members (
    pool_id UUID NOT NULL REFERENCES prospect_pools (pool_id) ON DELETE CASCADE,
    contact_id UUID NOT NULL REFERENCES contacts (contact_id) ON DELETE CASCADE,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (pool_id, contact_id)
);

-- Member listing per pool.
CREATE INDEX IF NOT EXISTS pool_members_pool_id_idx
    ON pool_members (pool_id);

CREATE TABLE IF NOT EXISTS opt_outs (
    opt_out_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    contact_id UUID NOT NULL REFERENCES contacts (contact_id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (client_profile_id, contact_id, channel)
);

-- Opt-out checks per contact (the hot path for bulk-send suppression).
CREATE INDEX IF NOT EXISTS opt_outs_contact_id_idx
    ON opt_outs (contact_id);

CREATE TABLE IF NOT EXISTS conversions (
    conversion_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    contact_id UUID NOT NULL REFERENCES contacts (contact_id) ON DELETE CASCADE,
    campaign_id UUID REFERENCES campaigns (campaign_id) ON DELETE SET NULL,
    value_cents BIGINT CHECK (value_cents IS NULL OR value_cents >= 0),
    occurred_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Conversion reads per campaign and per contact.
CREATE INDEX IF NOT EXISTS conversions_campaign_id_idx
    ON conversions (campaign_id, occurred_at DESC);

CREATE INDEX IF NOT EXISTS conversions_contact_id_idx
    ON conversions (contact_id, occurred_at DESC);
