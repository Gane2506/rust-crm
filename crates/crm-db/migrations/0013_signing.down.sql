-- SPDX-License-Identifier: GPL-3.0-only
-- 0013_signing.down: roll back the Contract signing vertical slice.
--
-- Drops the signing_evidence, contracts and signing_templates tables
-- (in dependency order). Contracts reference client profiles only;
-- nothing else references contracts or templates.

DROP TABLE IF EXISTS signing_evidence;
DROP TABLE IF EXISTS contracts;
DROP TABLE IF EXISTS signing_templates;
