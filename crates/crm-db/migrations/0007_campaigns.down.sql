-- 0007_campaigns.down: roll back the Campaigns vertical slice.
--
-- Drops in reverse dependency order. The campaigns table is last
-- because campaign_steps, conversions reference it.

DROP TABLE IF EXISTS conversions;
DROP TABLE IF EXISTS opt_outs;
DROP TABLE IF EXISTS pool_members;
DROP TABLE IF EXISTS prospect_pools;
DROP TABLE IF EXISTS campaign_steps;
DROP TABLE IF EXISTS campaigns;
