// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Email vertical slice (task 07-MAIL-02).
//!
//! [`PgEmailMessageRepository`], [`PgEmailThreadRepository`] and
//! [`PgEmailAttachmentRepository`] implement the
//! [`crm_domain::repositories`] email traits over the `email_messages`,
//! `email_threads` and `email_attachments` tables (migration 0006).
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::email::{
    EmailAttachment, EmailDirection, EmailMessage, EmailStatus, EmailThread, ThreadStatus,
};
use crm_domain::error::DomainError;
use crm_domain::ids::{
    ActorId, AssetId, ClientProfileId, EmailAttachmentId, EmailMessageId, EmailThreadId,
};
use crm_domain::repositories::{
    EmailAttachmentRepository, EmailMessageRepository, EmailThreadRepository,
};

use crate::postgres::map_storage_error;

/// Renders an [`EmailDirection`] as its stable database string.
fn email_direction_as_str(direction: EmailDirection) -> &'static str {
    match direction {
        EmailDirection::Outbound => "outbound",
        EmailDirection::Inbound => "inbound",
    }
}

/// Parses an [`EmailDirection`] from its database string.
fn email_direction_from_str(raw: &str) -> Result<EmailDirection, DomainError> {
    match raw {
        "outbound" => Ok(EmailDirection::Outbound),
        "inbound" => Ok(EmailDirection::Inbound),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown email direction '{other}'"
        ))),
    }
}

/// Renders an [`EmailStatus`] as its stable database string.
fn email_status_as_str(status: EmailStatus) -> &'static str {
    match status {
        EmailStatus::Draft => "draft",
        EmailStatus::Queued => "queued",
        EmailStatus::Sending => "sending",
        EmailStatus::Sent => "sent",
        EmailStatus::Delivered => "delivered",
        EmailStatus::Failed => "failed",
        EmailStatus::Bounced => "bounced",
    }
}

/// Parses an [`EmailStatus`] from its database string.
fn email_status_from_str(raw: &str) -> Result<EmailStatus, DomainError> {
    match raw {
        "draft" => Ok(EmailStatus::Draft),
        "queued" => Ok(EmailStatus::Queued),
        "sending" => Ok(EmailStatus::Sending),
        "sent" => Ok(EmailStatus::Sent),
        "delivered" => Ok(EmailStatus::Delivered),
        "failed" => Ok(EmailStatus::Failed),
        "bounced" => Ok(EmailStatus::Bounced),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown email status '{other}'"
        ))),
    }
}

/// Renders a [`ThreadStatus`] as its stable database string.
fn thread_status_as_str(status: ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Open => "open",
        ThreadStatus::Closed => "closed",
        ThreadStatus::Archived => "archived",
    }
}

/// Parses a [`ThreadStatus`] from its database string.
fn thread_status_from_str(raw: &str) -> Result<ThreadStatus, DomainError> {
    match raw {
        "open" => Ok(ThreadStatus::Open),
        "closed" => Ok(ThreadStatus::Closed),
        "archived" => Ok(ThreadStatus::Archived),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown thread status '{other}'"
        ))),
    }
}

/// Columns selected for an [`EmailMessage`] row.
const MESSAGE_COLUMNS: &str = "message_id, client_profile_id, thread_id, direction, status, \
     from_address, to_addresses, cc_addresses, bcc_addresses, subject, body_text, body_html, \
     provider_message_id, idempotency_key, sent_at, created_at";

/// Columns selected for an [`EmailThread`] row.
const THREAD_COLUMNS: &str = "thread_id, client_profile_id, provider_thread_id, subject, status, \
     participants, message_count, last_message_at, created_at, updated_at";

/// Columns selected for an [`EmailAttachment`] row.
const ATTACHMENT_COLUMNS: &str = "attachment_id, email_message_id, asset_id, file_name, \
     mime_type, size_bytes, attached_by, created_at";

/// Hydrates an `email_messages` row into an [`EmailMessage`].
fn row_to_message(row: &sqlx::postgres::PgRow) -> Result<EmailMessage, DomainError> {
    let thread_id: Option<Uuid> = row.try_get("thread_id").map_err(map_storage_error)?;
    let sent_at: Option<DateTime<Utc>> = row.try_get("sent_at").map_err(map_storage_error)?;
    let provider_message_id: Option<String> = row
        .try_get("provider_message_id")
        .map_err(map_storage_error)?;
    let idempotency_key: Option<String> =
        row.try_get("idempotency_key").map_err(map_storage_error)?;
    Ok(EmailMessage {
        message_id: EmailMessageId::from(
            row.try_get::<Uuid, _>("message_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        thread_id: thread_id.map(EmailThreadId::from),
        direction: email_direction_from_str(
            row.try_get::<String, _>("direction")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        status: email_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        from: row.try_get("from_address").map_err(map_storage_error)?,
        to: row.try_get("to_addresses").map_err(map_storage_error)?,
        cc: row.try_get("cc_addresses").map_err(map_storage_error)?,
        bcc: row.try_get("bcc_addresses").map_err(map_storage_error)?,
        subject: row.try_get("subject").map_err(map_storage_error)?,
        body_text: row.try_get("body_text").map_err(map_storage_error)?,
        body_html: row.try_get("body_html").map_err(map_storage_error)?,
        provider_message_id,
        idempotency_key,
        sent_at,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates an `email_threads` row into an [`EmailThread`].
fn row_to_thread(row: &sqlx::postgres::PgRow) -> Result<EmailThread, DomainError> {
    let message_count: i64 = row.try_get("message_count").map_err(map_storage_error)?;
    Ok(EmailThread {
        thread_id: EmailThreadId::from(
            row.try_get::<Uuid, _>("thread_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        provider_thread_id: row
            .try_get("provider_thread_id")
            .map_err(map_storage_error)?,
        subject: row.try_get("subject").map_err(map_storage_error)?,
        status: thread_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        participants: row.try_get("participants").map_err(map_storage_error)?,
        message_count: u64::try_from(message_count).map_err(|_| {
            DomainError::Validation("storage error: negative thread message_count".to_owned())
        })?,
        last_message_at: row
            .try_get::<DateTime<Utc>, _>("last_message_at")
            .map_err(map_storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates an `email_attachments` row into an [`EmailAttachment`].
fn row_to_attachment(row: &sqlx::postgres::PgRow) -> Result<EmailAttachment, DomainError> {
    let size_bytes: i64 = row.try_get("size_bytes").map_err(map_storage_error)?;
    Ok(EmailAttachment {
        attachment_id: EmailAttachmentId::from(
            row.try_get::<Uuid, _>("attachment_id")
                .map_err(map_storage_error)?,
        ),
        email_message_id: EmailMessageId::from(
            row.try_get::<Uuid, _>("email_message_id")
                .map_err(map_storage_error)?,
        ),
        asset_id: AssetId::from(
            row.try_get::<Uuid, _>("asset_id")
                .map_err(map_storage_error)?,
        ),
        file_name: row.try_get("file_name").map_err(map_storage_error)?,
        mime_type: row.try_get("mime_type").map_err(map_storage_error)?,
        size_bytes: u64::try_from(size_bytes).map_err(|_| {
            DomainError::Validation("storage error: negative attachment size".to_owned())
        })?,
        attached_by: ActorId::from(
            row.try_get::<Uuid, _>("attached_by")
                .map_err(map_storage_error)?,
        ),
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// SQLx implementation of [`EmailMessageRepository`].
#[derive(Debug, Clone)]
pub struct PgEmailMessageRepository {
    pool: PgPool,
}

impl PgEmailMessageRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl EmailMessageRepository for PgEmailMessageRepository {
    async fn create(&self, message: EmailMessage) -> Result<EmailMessage, DomainError> {
        crate::postgres::ensure_client_profile(&self.pool, message.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO email_messages (message_id, client_profile_id, thread_id, direction, status, \
             from_address, to_addresses, cc_addresses, bcc_addresses, subject, body_text, body_html, \
             provider_message_id, idempotency_key, sent_at, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        )
        .bind(message.message_id.as_uuid())
        .bind(message.client_profile_id.as_uuid())
        .bind(message.thread_id.map(|id| id.as_uuid()))
        .bind(email_direction_as_str(message.direction))
        .bind(email_status_as_str(message.status))
        .bind(&message.from)
        .bind(&message.to)
        .bind(&message.cc)
        .bind(&message.bcc)
        .bind(&message.subject)
        .bind(&message.body_text)
        .bind(&message.body_html)
        .bind(&message.provider_message_id)
        .bind(&message.idempotency_key)
        .bind(message.sent_at)
        .bind(message.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(message)
    }

    async fn get(&self, message_id: EmailMessageId) -> Result<EmailMessage, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM email_messages WHERE message_id = $1"
        ))
        .bind(message_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_message(&row),
            None => Err(DomainError::NotFound {
                kind: "email_message".to_owned(),
                id: message_id.to_string(),
            }),
        }
    }

    async fn update(&self, message: EmailMessage) -> Result<EmailMessage, DomainError> {
        let result = sqlx::query(
            "UPDATE email_messages
             SET thread_id = $2, status = $3, provider_message_id = $4, sent_at = $5
             WHERE message_id = $1",
        )
        .bind(message.message_id.as_uuid())
        .bind(message.thread_id.map(|id| id.as_uuid()))
        .bind(email_status_as_str(message.status))
        .bind(&message.provider_message_id)
        .bind(message.sent_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "email_message".to_owned(),
                id: message.message_id.to_string(),
            });
        }
        Ok(message)
    }

    async fn list_for_thread(
        &self,
        thread_id: EmailThreadId,
        limit: u64,
    ) -> Result<Vec<EmailMessage>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM email_messages
             WHERE thread_id = $1
             ORDER BY created_at ASC, message_id ASC
             LIMIT $2"
        ))
        .bind(thread_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_message).collect()
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<EmailMessage>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM email_messages
             WHERE client_profile_id = $1
             ORDER BY created_at DESC, message_id DESC
             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_message).collect()
    }

    async fn find_by_provider_message_id(
        &self,
        client_profile_id: ClientProfileId,
        provider_message_id: &str,
    ) -> Result<Option<EmailMessage>, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM email_messages
             WHERE client_profile_id = $1 AND provider_message_id = $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(provider_message_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(|row| row_to_message(&row)).transpose()
    }

    async fn find_by_idempotency_key(
        &self,
        client_profile_id: ClientProfileId,
        idempotency_key: &str,
    ) -> Result<Option<EmailMessage>, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM email_messages
             WHERE client_profile_id = $1 AND idempotency_key = $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(|row| row_to_message(&row)).transpose()
    }
}

/// SQLx implementation of [`EmailThreadRepository`].
#[derive(Debug, Clone)]
pub struct PgEmailThreadRepository {
    pool: PgPool,
}

impl PgEmailThreadRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl EmailThreadRepository for PgEmailThreadRepository {
    async fn create(&self, thread: EmailThread) -> Result<EmailThread, DomainError> {
        crate::postgres::ensure_client_profile(&self.pool, thread.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO email_threads (thread_id, client_profile_id, provider_thread_id, subject, \
             status, participants, message_count, last_message_at, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT DO NOTHING",
        )
        .bind(thread.thread_id.as_uuid())
        .bind(thread.client_profile_id.as_uuid())
        .bind(&thread.provider_thread_id)
        .bind(&thread.subject)
        .bind(thread_status_as_str(thread.status))
        .bind(&thread.participants)
        .bind(thread.message_count as i64)
        .bind(thread.last_message_at)
        .bind(thread.created_at)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(thread)
    }

    async fn get(&self, thread_id: EmailThreadId) -> Result<EmailThread, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {THREAD_COLUMNS} FROM email_threads WHERE thread_id = $1"
        ))
        .bind(thread_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_thread(&row),
            None => Err(DomainError::NotFound {
                kind: "email_thread".to_owned(),
                id: thread_id.to_string(),
            }),
        }
    }

    async fn update(&self, thread: EmailThread) -> Result<EmailThread, DomainError> {
        let result = sqlx::query(
            "UPDATE email_threads
             SET provider_thread_id = $2, subject = $3, status = $4, participants = $5,
                 message_count = $6, last_message_at = $7, updated_at = $8
             WHERE thread_id = $1",
        )
        .bind(thread.thread_id.as_uuid())
        .bind(&thread.provider_thread_id)
        .bind(&thread.subject)
        .bind(thread_status_as_str(thread.status))
        .bind(&thread.participants)
        .bind(thread.message_count as i64)
        .bind(thread.last_message_at)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "email_thread".to_owned(),
                id: thread.thread_id.to_string(),
            });
        }
        Ok(thread)
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<EmailThread>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {THREAD_COLUMNS} FROM email_threads
             WHERE client_profile_id = $1
             ORDER BY last_message_at DESC, thread_id DESC
             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_thread).collect()
    }

    async fn find_by_provider_thread_id(
        &self,
        client_profile_id: ClientProfileId,
        provider_thread_id: &str,
    ) -> Result<Option<EmailThread>, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {THREAD_COLUMNS} FROM email_threads
             WHERE client_profile_id = $1 AND provider_thread_id = $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(provider_thread_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(|row| row_to_thread(&row)).transpose()
    }
}

/// SQLx implementation of [`EmailAttachmentRepository`].
#[derive(Debug, Clone)]
pub struct PgEmailAttachmentRepository {
    pool: PgPool,
}

impl PgEmailAttachmentRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl EmailAttachmentRepository for PgEmailAttachmentRepository {
    async fn create(&self, attachment: EmailAttachment) -> Result<EmailAttachment, DomainError> {
        sqlx::query(
            "INSERT INTO email_attachments (attachment_id, email_message_id, asset_id, file_name, \
             mime_type, size_bytes, attached_by, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT DO NOTHING",
        )
        .bind(attachment.attachment_id.as_uuid())
        .bind(attachment.email_message_id.as_uuid())
        .bind(attachment.asset_id.as_uuid())
        .bind(&attachment.file_name)
        .bind(&attachment.mime_type)
        .bind(attachment.size_bytes as i64)
        .bind(attachment.attached_by.as_uuid())
        .bind(attachment.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(attachment)
    }

    async fn get(&self, attachment_id: EmailAttachmentId) -> Result<EmailAttachment, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM email_attachments WHERE attachment_id = $1"
        ))
        .bind(attachment_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_attachment(&row),
            None => Err(DomainError::NotFound {
                kind: "email_attachment".to_owned(),
                id: attachment_id.to_string(),
            }),
        }
    }

    async fn list_for_message(
        &self,
        message_id: EmailMessageId,
    ) -> Result<Vec<EmailAttachment>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {ATTACHMENT_COLUMNS} FROM email_attachments
             WHERE email_message_id = $1
             ORDER BY created_at ASC, attachment_id ASC"
        ))
        .bind(message_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_attachment).collect()
    }
}
