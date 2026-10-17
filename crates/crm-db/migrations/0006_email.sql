-- 0006_email: Email vertical slice (task 07-MAIL-02).
--
-- * `email_threads` groups a conversation inside a client profile.
--   `provider_thread_id` is the provider's thread id (Gmail `threadId`),
--   unique per profile, for exact-match reply correlation.
-- * `email_messages` holds one email per row. `provider_message_id`
--   is unique per profile: it keys idempotent retries and reply
--   correlation (at-most-once ingestion).
-- * `email_attachments` links a message to content-addressed bytes in
--   the asset store (see migration 0005); no byte duplication.
--
-- Additive only; re-runnable.

CREATE TABLE IF NOT EXISTS email_threads (
    thread_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    provider_thread_id TEXT,
    subject TEXT NOT NULL,
    status TEXT NOT NULL,
    participants TEXT[] NOT NULL DEFAULT '{}',
    message_count BIGINT NOT NULL DEFAULT 0 CHECK (message_count >= 0),
    last_message_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Exact-match reply correlation: one provider thread id maps to one
-- thread per profile. NULLs are not indexed as duplicates.
CREATE UNIQUE INDEX IF NOT EXISTS email_threads_profile_provider_thread_uniq
    ON email_threads (client_profile_id, provider_thread_id);

-- Tenant-scoped thread reads, most-recently-active first.
CREATE INDEX IF NOT EXISTS email_threads_client_profile_id_idx
    ON email_threads (client_profile_id, last_message_at DESC);

CREATE TABLE IF NOT EXISTS email_messages (
    message_id UUID PRIMARY KEY,
    client_profile_id UUID NOT NULL REFERENCES client_profiles (client_profile_id),
    thread_id UUID REFERENCES email_threads (thread_id),
    direction TEXT NOT NULL,
    status TEXT NOT NULL,
    from_address TEXT NOT NULL,
    to_addresses TEXT[] NOT NULL DEFAULT '{}',
    cc_addresses TEXT[] NOT NULL DEFAULT '{}',
    bcc_addresses TEXT[] NOT NULL DEFAULT '{}',
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL DEFAULT '',
    body_html TEXT,
    provider_message_id TEXT,
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Idempotent retries and reply correlation: one provider message id
-- maps to one message per profile.
CREATE UNIQUE INDEX IF NOT EXISTS email_messages_profile_provider_message_uniq
    ON email_messages (client_profile_id, provider_message_id);

-- Tenant-scoped message reads, newest first.
CREATE INDEX IF NOT EXISTS email_messages_client_profile_id_idx
    ON email_messages (client_profile_id, created_at DESC);

-- Thread message listing, oldest first.
CREATE INDEX IF NOT EXISTS email_messages_thread_id_idx
    ON email_messages (thread_id, created_at ASC);

CREATE TABLE IF NOT EXISTS email_attachments (
    attachment_id UUID PRIMARY KEY,
    email_message_id UUID NOT NULL REFERENCES email_messages (message_id),
    asset_id UUID NOT NULL REFERENCES assets (asset_id),
    file_name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes > 0),
    attached_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Attachment listing per message.
CREATE INDEX IF NOT EXISTS email_attachments_email_message_id_idx
    ON email_attachments (email_message_id);
