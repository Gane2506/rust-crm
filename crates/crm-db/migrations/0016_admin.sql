-- SPDX-License-Identifier: GPL-3.0-only
-- 0016_admin: Admin and operations vertical slice (task 16-ADMIN-02).
--
-- * `staff_invitations` holds staff invitations: invitee email, the
--   roles granted on acceptance as JSONB (array of role UUIDs), the
--   inviting staff id, the lifecycle status
--   ('pending'/'accepted'/'expired'/'revoked') and the expiry. The
--   `payload` column carries the whole invitation as JSONB; rows are
--   hydrated through the domain's validating manual `Deserialize`
--   impl, so a corrupt row fails loudly instead of bypassing the
--   invitation invariants.
-- * `role_assignments` holds role grants to staff members: staff id,
--   role id, the scope as JSONB (the domain `Scope` enum), the
--   assigning staff id, the status ('active'/'revoked') and the
--   revocation timestamp (NULL while held). Revocation is terminal:
--   re-granting creates a new row.
-- * `agency_settings` holds versioned agency settings snapshots:
--   `version` is UNIQUE and monotonically increasing per agency;
--   `get_current` reads the highest version. The `payload` column
--   carries the full validated snapshot; it is hydrated through the
--   domain's manual `Deserialize` (validations re-run on read).
-- * `maintenance_state` is a singleton row (`id = 1`): whether
--   maintenance mode is enabled and the operator-facing reason.
-- * `installation_state` is a singleton row (`id = 1`): the
--   first-run installer state ('not_started'/'in_progress'/
--   'completed'). The singleton rows are created lazily by the
--   service on first use (there is no staff id to attribute at
--   migration time), so readers treat a missing row as the
--   domain default.
-- * `backups` holds database backup attempts: lifecycle status
--   ('scheduled'/'in_progress'/'completed'/'failed'), the initiating
--   staff id, timestamps and, on completion, the byte size; on
--   failure, the reason.
-- * `exports` holds data-portability export requests: the covered
--   client profile (NULL = agency-wide extract), the file format
--   ('csv'/'json'), the lifecycle status
--   ('requested'/'in_progress'/'ready'/'failed'/'expired'), the
--   requesting staff id, readiness and failure bookkeeping.
-- * `upgrades` holds system upgrade attempts: the from/to versions
--   (both must be non-blank and different), the lifecycle status
--   ('scheduled'/'in_progress'/'completed'/'failed'/'rolled_back'),
--   the scheduling staff id and failure bookkeeping.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS staff_invitations (
    invitation_id UUID PRIMARY KEY,
    email TEXT NOT NULL,
    roles JSONB NOT NULL,
    invited_by UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS role_assignments (
    assignment_id UUID PRIMARY KEY,
    staff_id UUID NOT NULL,
    role_id UUID NOT NULL,
    scope JSONB NOT NULL,
    assigned_by UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    payload JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS agency_settings (
    settings_id UUID PRIMARY KEY,
    agency_id UUID NOT NULL,
    version BIGINT NOT NULL UNIQUE,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS maintenance_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    reason TEXT,
    set_by UUID NOT NULL,
    set_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS installation_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    state TEXT NOT NULL DEFAULT 'not_started',
    payload JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS backups (
    backup_id UUID PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'scheduled',
    initiated_by UUID NOT NULL,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS exports (
    export_id UUID PRIMARY KEY,
    client_profile_id UUID REFERENCES client_profiles (client_profile_id),
    format TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'requested',
    requested_by UUID NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS upgrades (
    upgrade_id UUID PRIMARY KEY,
    from_version TEXT NOT NULL,
    to_version TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'scheduled',
    scheduled_by UUID NOT NULL,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL
);

-- Pending-invitation sweeps and lookups by status.
CREATE INDEX IF NOT EXISTS staff_invitations_status_idx
    ON staff_invitations (status, expires_at, invitation_id);

-- A staff member's role assignments, newest first.
CREATE INDEX IF NOT EXISTS role_assignments_staff_id_idx
    ON role_assignments (staff_id, assigned_at DESC, assignment_id DESC);

-- Current-settings lookup: highest version per agency.
CREATE INDEX IF NOT EXISTS agency_settings_agency_version_idx
    ON agency_settings (agency_id, version DESC);

-- Operational listings, newest first.
CREATE INDEX IF NOT EXISTS backups_scheduled_at_idx
    ON backups (scheduled_at DESC, backup_id DESC);

CREATE INDEX IF NOT EXISTS exports_requested_at_idx
    ON exports (requested_at DESC, export_id DESC);

CREATE INDEX IF NOT EXISTS upgrades_scheduled_at_idx
    ON upgrades (scheduled_at DESC, upgrade_id DESC);
