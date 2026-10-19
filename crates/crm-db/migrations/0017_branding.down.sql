-- 0017_branding down: removes the branding table and its index.

DROP INDEX IF EXISTS branding_agency_version_idx;
DROP TABLE IF EXISTS branding;
