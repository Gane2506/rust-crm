-- Down migration for 0003_identity: drops the tables in reverse
-- dependency order.
DROP TABLE IF EXISTS grants;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS delegations;
DROP TABLE IF EXISTS staff_users;
