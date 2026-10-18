-- SPDX-License-Identifier: GPL-3.0-only
-- 0013_signing: Contract signing vertical slice (task 13-SIGN-02).
--
-- * `signing_templates` holds immutable Rust-native contract templates:
--   name, body with `{{placeholder}}` variables, and a monotonic
--   version. (name, version) is unique; a new version is a new row —
--   templates are never updated or deleted.
-- * `contracts` holds the contract envelope per client profile:
--   lifecycle status ('draft'/'sent'/'partially_signed'/
--   'signed_awaiting_payment'/'effective'/'cancelled'/'expired'/
--   'disputed'), the template revision it was generated from, the
--   rendered document snapshot (body + content hash — the exact bytes
--   the signer saw), the payment requirement and signers as JSONB, and
--   the evidence bundle reference.
-- * `signing_evidence` holds the forensic record of each signature:
--   who signed, when, how, and the SHA-256 fingerprints of the IP
--   address and user agent (raw values never persist).
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS signing_templates (
    template_id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    body TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT signing_templates_name_version_unique UNIQUE (name, version)
);

CREATE TABLE IF NOT EXISTS contracts (
    contract_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    status TEXT NOT NULL,
    template_revision TEXT NOT NULL,
    document JSONB,
    payment_requirement JSONB NOT NULL,
    signers JSONB NOT NULL,
    evidence_bundle_ref TEXT,
    effective_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS signing_evidence (
    evidence_id UUID PRIMARY KEY,
    contract_id UUID NOT NULL REFERENCES contracts (contract_id) ON DELETE CASCADE,
    signer_name TEXT NOT NULL,
    signer_email TEXT NOT NULL,
    signed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ip_hash TEXT NOT NULL,
    user_agent_hash TEXT NOT NULL,
    method TEXT NOT NULL,
    CONSTRAINT signing_evidence_contract_signer_unique UNIQUE (contract_id, signer_email)
);

-- Tenant-scoped contract reads, newest first.
CREATE INDEX IF NOT EXISTS contracts_client_profile_id_idx
    ON contracts (client_profile_id, created_at DESC, contract_id DESC);

-- Template version lookups by name, newest version first.
CREATE INDEX IF NOT EXISTS signing_templates_name_idx
    ON signing_templates (name, version DESC);

-- Evidence reads for one contract, oldest first.
CREATE INDEX IF NOT EXISTS signing_evidence_contract_id_idx
    ON signing_evidence (contract_id, signed_at ASC);
