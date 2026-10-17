-- 0006_email down: drops the email vertical-slice tables.
--
-- Reverses 0006_email.sql. Tables drop in dependency order
-- (attachments -> messages -> threads).

DROP TABLE IF EXISTS email_attachments;
DROP TABLE IF EXISTS email_messages;
DROP TABLE IF EXISTS email_threads;
