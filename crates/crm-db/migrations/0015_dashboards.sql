-- SPDX-License-Identifier: GPL-3.0-only
-- 0015_dashboards: Analytics vertical slice (task 15-DASH-02).
--
-- * `dashboards` holds one row per saved dashboard: display name,
--   owner scope (owner_client_profile_id NULL = the agency-level
--   master dashboard), and the Draft/Published lifecycle status
--   ('draft'/'published').
-- * `dashboard_widgets` holds the widgets of one dashboard: title,
--   widget type ('count'/'timeseries'/'breakdown'), the metric
--   definition as JSONB (hydrated through the domain's validating
--   constructors, so a stored metric can never violate a domain
--   invariant), and the 0-based grid position; the unique
--   constraint on (dashboard_id, position_x, position_y) is
--   defense in depth behind the domain's position-uniqueness
--   invariant.
-- * `reports` holds saved, parameterized metric queries: display
--   name, the owning staff id (recorded by id; no FK — staff rows
--   are managed by the identity workstream), the metric as JSONB,
--   and the parameter values as JSONB.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS dashboards (
    dashboard_id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    owner_client_profile_id UUID REFERENCES client_profiles (client_profile_id),
    status TEXT NOT NULL DEFAULT 'draft',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS dashboard_widgets (
    widget_id UUID PRIMARY KEY,
    dashboard_id UUID NOT NULL REFERENCES dashboards (dashboard_id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    widget_type TEXT NOT NULL,
    metric JSONB NOT NULL,
    position_x INTEGER NOT NULL,
    position_y INTEGER NOT NULL,
    CONSTRAINT dashboard_widgets_position_unique UNIQUE (dashboard_id, position_x, position_y)
);

CREATE TABLE IF NOT EXISTS reports (
    report_id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    owner_staff_id UUID NOT NULL,
    metric JSONB NOT NULL,
    parameters JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Owner-scoped dashboard reads, newest first.
CREATE INDEX IF NOT EXISTS dashboards_owner_profile_idx
    ON dashboards (owner_client_profile_id, created_at DESC, dashboard_id DESC);

-- Widget reads for one dashboard.
CREATE INDEX IF NOT EXISTS dashboard_widgets_dashboard_id_idx
    ON dashboard_widgets (dashboard_id);

-- Report reads, newest first.
CREATE INDEX IF NOT EXISTS reports_created_at_idx
    ON reports (created_at DESC, report_id DESC);
