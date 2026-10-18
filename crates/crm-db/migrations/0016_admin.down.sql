-- SPDX-License-Identifier: GPL-3.0-only
-- 0016_admin.down: roll back the Admin and operations vertical slice.
--
-- Drops the upgrades, exports, backups, installation_state,
-- maintenance_state, agency_settings, role_assignments and
-- staff_invitations tables. Exports reference client_profiles with
-- no cascade, so exports drop before any table they depend on.

DROP TABLE IF EXISTS upgrades;
DROP TABLE IF EXISTS exports;
DROP TABLE IF EXISTS backups;
DROP TABLE IF EXISTS installation_state;
DROP TABLE IF EXISTS maintenance_state;
DROP TABLE IF EXISTS agency_settings;
DROP TABLE IF EXISTS role_assignments;
DROP TABLE IF EXISTS staff_invitations;
