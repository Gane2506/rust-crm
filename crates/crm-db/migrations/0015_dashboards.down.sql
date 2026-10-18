-- SPDX-License-Identifier: GPL-3.0-only
-- 0015_dashboards.down: roll back the Analytics vertical slice.
--
-- Drops the reports, dashboard_widgets and dashboards tables (in
-- dependency order). Widgets reference dashboards with
-- ON DELETE CASCADE; reports reference no other table.

DROP TABLE IF EXISTS reports;
DROP TABLE IF EXISTS dashboard_widgets;
DROP TABLE IF EXISTS dashboards;
