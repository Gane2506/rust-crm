-- Down migration for 0001_foundation.
--
-- Drops the foundation tables in dependency order. Run explicitly, e.g.
-- via `Migrator::undo`; `Migrator::run` never applies down migrations.

DROP TABLE IF EXISTS outbox;
DROP TABLE IF EXISTS audit_events;
DROP TABLE IF EXISTS contacts;
DROP TABLE IF EXISTS client_profiles;
