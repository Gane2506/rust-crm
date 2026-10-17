-- Down migration for 0002_idempotency_keys: drops the table.
DROP TABLE IF EXISTS idempotency_keys;
