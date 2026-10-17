-- SPDX-License-Identifier: GPL-3.0-only
-- 0010_forms.down: roll back the Form builder vertical slice.
--
-- Drops in reverse dependency order: submissions and fields
-- reference forms; submissions also reference client profiles and
-- contacts.

DROP TABLE IF EXISTS form_submissions;
DROP TABLE IF EXISTS form_fields;
DROP TABLE IF EXISTS forms;
