-- SPDX-License-Identifier: GPL-3.0-only
-- 0011_feed.down: roll back the Feed vertical slice.
--
-- Drops the feed_items table. Feed items reference client profiles
-- only; nothing else references feed items.

DROP TABLE IF EXISTS feed_items;
