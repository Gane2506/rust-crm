-- SPDX-License-Identifier: GPL-3.0-only
-- 0010_forms: Form builder vertical slice (task 11-FORM-02).
--
-- * `forms` holds the form aggregate: owning client profile, name,
--   lifecycle status (draft/published/archived) and timestamps.
-- * `form_fields` holds the typed fields on a form: the owning form,
--   field type, label, required flag, display position and opaque
--   config JSONB. Fields are cheap to delete and recreate; there is
--   no UPDATE path.
-- * `form_submissions` is append-mostly: submitted answers are stored
--   as an answers JSONB array, never updated; only the status is
--   mutable (new → reviewed → archived).
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS forms (
    form_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tenant-scoped form reads, newest first.
CREATE INDEX IF NOT EXISTS forms_client_profile_id_idx
    ON forms (client_profile_id, created_at DESC);

CREATE TABLE IF NOT EXISTS form_fields (
    field_id UUID PRIMARY KEY,
    form_id UUID NOT NULL REFERENCES forms (form_id) ON DELETE CASCADE,
    field_type TEXT NOT NULL,
    label TEXT NOT NULL,
    required BOOLEAN NOT NULL DEFAULT FALSE,
    position INTEGER NOT NULL DEFAULT 0,
    config JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fields of one form in display order.
CREATE INDEX IF NOT EXISTS form_fields_form_id_idx
    ON form_fields (form_id, position ASC, field_id ASC);

CREATE TABLE IF NOT EXISTS form_submissions (
    submission_id UUID PRIMARY KEY,
    form_id UUID NOT NULL REFERENCES forms (form_id) ON DELETE CASCADE,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    contact_id UUID REFERENCES contacts (contact_id),
    submitted_by_label TEXT NOT NULL,
    status TEXT NOT NULL,
    answers JSONB NOT NULL,
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Submissions of one form, newest first.
CREATE INDEX IF NOT EXISTS form_submissions_form_id_idx
    ON form_submissions (form_id, submitted_at DESC);
