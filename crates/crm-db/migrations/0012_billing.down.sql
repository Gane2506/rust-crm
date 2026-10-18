-- SPDX-License-Identifier: GPL-3.0-only
-- 0012_billing.down: roll back the Billing vertical slice.
--
-- Drops the payments, invoice_lines and invoices tables (in
-- dependency order). Invoices reference client profiles only;
-- nothing else references invoices.

DROP TABLE IF EXISTS payments;
DROP TABLE IF EXISTS invoice_lines;
DROP TABLE IF EXISTS invoices;
