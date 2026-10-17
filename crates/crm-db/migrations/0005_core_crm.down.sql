-- Down migration for 0005_core_crm: drops the assets table and the
-- client_profiles entity columns, restoring the FK-placeholder shape.
-- Contact source columns and merged_into are foundation columns and
-- stay untouched.
DROP TABLE IF EXISTS assets;
DROP INDEX IF EXISTS client_profiles_agency_id_idx;
ALTER TABLE client_profiles DROP COLUMN IF EXISTS updated_at;
ALTER TABLE client_profiles DROP COLUMN IF EXISTS status;
ALTER TABLE client_profiles DROP COLUMN IF EXISTS name;
ALTER TABLE client_profiles DROP COLUMN IF EXISTS agency_id;
