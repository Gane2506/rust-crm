-- SPDX-License-Identifier: GPL-3.0-only
-- 0012_billing: Billing vertical slice (task 12-BILL-02).
--
-- * `invoices` holds the invoice header per client profile: lifecycle
--   status ('draft'/'issued'/'paid'/'void') and the ISO 4217 currency
--   shared by every line and the total.
-- * `invoice_lines` holds the billed lines: description, quantity and
--   the unit price as (amount, currency). Lines are ordered by
--   `position`; they are only ever added while the invoice is a
--   draft — once issued the total is a financial fact.
-- * `payments` holds append-only payment records: amount, currency
--   and status ('recorded'/'reversed'). Reversals are explicit status
--   flips; there is no deletion.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS invoices (
    invoice_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    status TEXT NOT NULL,
    currency CHAR(3) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS invoice_lines (
    line_id UUID PRIMARY KEY,
    invoice_id UUID NOT NULL REFERENCES invoices (invoice_id) ON DELETE CASCADE,
    description TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price_amount BIGINT NOT NULL CHECK (unit_price_amount >= 0),
    unit_price_currency CHAR(3) NOT NULL,
    position INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS payments (
    payment_id UUID PRIMARY KEY,
    invoice_id UUID NOT NULL REFERENCES invoices (invoice_id) ON DELETE CASCADE,
    amount BIGINT NOT NULL CHECK (amount > 0),
    currency CHAR(3) NOT NULL,
    status TEXT NOT NULL DEFAULT 'recorded',
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped invoice reads, newest first.
CREATE INDEX IF NOT EXISTS invoices_client_profile_id_idx
    ON invoices (client_profile_id, created_at DESC, invoice_id DESC);

-- Line hydration for one invoice, in the order added.
CREATE INDEX IF NOT EXISTS invoice_lines_invoice_id_idx
    ON invoice_lines (invoice_id, position ASC);

-- Payment coverage reads for one invoice, oldest first.
CREATE INDEX IF NOT EXISTS payments_invoice_id_idx
    ON payments (invoice_id, recorded_at ASC);
