-- SPDX-License-Identifier: GPL-3.0-only
-- 0011_feed: Feed vertical slice (task 12-FEED-02).
--
-- * `feed_items` holds the per-client-profile activity/notification
--   stream: the originating workstream event (source), a human-readable
--   title, optional body, the actor kind, and a reference (entity type
--   + id) back to the source record. Items are write-once facts
--   produced by the other workstreams; the feed only annotates them
--   (visibility, read flag). Visibility is sticky: hidden items are
--   never deleted here.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS feed_items (
    feed_item_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    source TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    actor_kind TEXT NOT NULL,
    ref_type TEXT NOT NULL,
    ref_id UUID NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'normal',
    read BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped feed reads, pinned-first then newest-first.
CREATE INDEX IF NOT EXISTS feed_items_client_profile_id_idx
    ON feed_items (client_profile_id, created_at DESC, feed_item_id DESC);
