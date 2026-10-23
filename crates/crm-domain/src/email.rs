// SPDX-License-Identifier: GPL-3.0-only
//! Email aggregate: messages, threads and attachments per client profile.
//!
//! The agency sends email on a client's behalf (outbound) and ingests
//! replies (inbound). Every message belongs to one client profile — the
//! tenant boundary — and optionally to a thread that groups a
//! conversation.
//!
//! ## Authorization (deferred)
//!
//! Email operations will authorize against `Resource::Email` (messages)
//! and `Resource::EmailThread` (threads), both profile-scoped. Those
//! variants are **not** added here: the `Resource` → database-string
//! mapping lives in `crates/crm-db` (exhaustive match), and this task
//! is domain-only. 07-MAIL-02 (the vertical slice) adds the variants
//! together with their `"email"` / `"email_thread"` mapping arms
//! atomically, so the workspace keeps compiling.
//!
//! ## Audit
//!
//! Email send/receive/fail are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s with action names
//! `"email.sent"`, `"email.received"` and `"email.failed"` by the
//! service implementation. They are deliberately **not**
//! [`SecurityEvent`](crate::audit::SecurityEvent) variants: that
//! taxonomy covers security-relevant events (auth, RBAC, sessions,
//! rate limits), while email is a business operation audited like
//! contacts and assets.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{
    ActorId, AssetId, ClientProfileId, EmailAttachmentId, EmailMessageId, EmailThreadId,
};

/// Normalizes an email address the same way contacts do: trim and
/// lowercase. The comparison is byte-wise after normalization; no
/// IDNA or plus-address folding is performed.
#[must_use]
pub fn normalize_email(address: &str) -> String {
    address.trim().to_lowercase()
}

/// Validates the structure of one email address: non-empty local part,
/// exactly one `@`, and a domain part containing a dot. This is a
/// structural check, not an RFC 5322 parser — the provider (Gmail/Zoho)
/// performs the authoritative validation at send time.
#[must_use]
pub fn is_valid_email(address: &str) -> bool {
    let normalized = normalize_email(address);
    let mut parts = normalized.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            !local.is_empty() && !domain.is_empty() && domain.contains('.')
        }
        _ => false,
    }
}

/// Strips leading `Re:`, `Fw:`/`Fwd:` prefixes (case-insensitive,
/// repeated) so reply subjects compare equal to their thread subject.
///
/// ```
/// use crm_domain::email::normalize_subject;
///
/// assert_eq!(normalize_subject("Re: Re: Hello"), "hello");
/// assert_eq!(normalize_subject("FWD: invoice"), "invoice");
/// assert_eq!(normalize_subject("  Hello World  "), "hello world");
/// ```
#[must_use]
pub fn normalize_subject(subject: &str) -> String {
    let mut rest = subject.trim();
    loop {
        let lower = rest.to_lowercase();
        let prefix_len = if lower.starts_with("re:") || lower.starts_with("fw:") {
            3
        } else if lower.starts_with("fwd:") {
            4
        } else {
            break;
        };
        rest = rest[prefix_len..].trim_start();
    }
    rest.to_lowercase()
}

/// Direction of an email message relative to the agency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmailDirection {
    /// Sent by the agency on the client's behalf.
    Outbound,
    /// Received from an external party (reply ingestion).
    Inbound,
}

/// Lifecycle of an email message.
///
/// The happy path is `Draft → Queued → Sending → Sent → Delivered`.
/// `Failed` is reachable from any non-terminal state; `Bounced` is
/// reachable from `Sent`. `Failed` and `Bounced` are terminal in the
/// domain: re-sending creates a *new* message (idempotency-keyed), so
/// the audit trail of the failure is preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmailStatus {
    /// Composed but not yet queued for sending.
    Draft,
    /// Queued for the provider adapter to pick up.
    Queued,
    /// Handed to the provider; awaiting provider acknowledgement.
    Sending,
    /// Provider accepted the message.
    Sent,
    /// Provider confirmed delivery.
    Delivered,
    /// Terminal failure (rejected, provider error, …).
    Failed,
    /// Provider reported a bounce after accept.
    Bounced,
}

impl EmailStatus {
    /// Whether the status is terminal (no further transitions allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            EmailStatus::Delivered | EmailStatus::Failed | EmailStatus::Bounced
        )
    }
}

/// One email message inside a client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailMessage {
    /// Stable identifier of this message.
    pub message_id: EmailMessageId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Thread this message belongs to, if threaded.
    pub thread_id: Option<EmailThreadId>,
    /// Sender address (normalized).
    pub from: String,
    /// Recipient addresses (normalized, deduplicated).
    pub to: Vec<String>,
    /// CC addresses (normalized, deduplicated).
    pub cc: Vec<String>,
    /// BCC addresses (normalized, deduplicated).
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body_text: String,
    /// HTML body, if supplied.
    pub body_html: Option<String>,
    /// Which way the message flows.
    pub direction: EmailDirection,
    /// Current lifecycle status.
    pub status: EmailStatus,
    /// Provider's message id (Gmail `id`, Zoho `messageId`), set once
    /// the provider accepts the message. Used for idempotent retries
    /// and reply correlation.
    pub provider_message_id: Option<String>,
    /// Caller-supplied idempotency key for this send. When set,
    /// [`EmailService::send`](crate::services::EmailService::send)
    /// returns the already-stored message instead of sending a
    /// duplicate. Campaign launch mints deterministic keys
    /// (`campaign-{id}-step-{id}-contact-{id}`) so a re-launch after a
    /// crash resumes without re-sending.
    pub idempotency_key: Option<String>,
    /// When the message was sent (outbound) or received (inbound).
    pub sent_at: Option<DateTime<Utc>>,
    /// When the record was created.
    pub created_at: DateTime<Utc>,
}

impl EmailMessage {
    /// Validates and normalizes a recipient list: trims, lowercases,
    /// drops blanks, deduplicates, and rejects structurally invalid
    /// addresses.
    fn normalize_recipients(
        addresses: Vec<String>,
        field: &str,
    ) -> Result<Vec<String>, DomainError> {
        let mut out = Vec::with_capacity(addresses.len());
        for address in addresses {
            let normalized = normalize_email(&address);
            if normalized.is_empty() {
                continue;
            }
            if !is_valid_email(&normalized) {
                return Err(DomainError::Validation(format!(
                    "email {field} contains an invalid address"
                )));
            }
            if !out.contains(&normalized) {
                out.push(normalized);
            }
        }
        Ok(out)
    }

    /// Creates an outbound message in `Draft` status.
    ///
    /// Rejects: no recipients across to/cc/bcc, an invalid address, or
    /// a blank subject *and* blank body. The sender defaults to the
    /// agency's configured from-address when `from` is blank.
    ///
    /// ```
    /// use crm_domain::email::{EmailDirection, EmailMessage, EmailStatus};
    /// use crm_domain::ids::{ActorId, ClientProfileId};
    ///
    /// let message = EmailMessage::new_outbound(
    ///     ClientProfileId::new(),
    ///     None,
    ///     "agency@example.com".to_string(),
    ///     vec!["client@example.com".to_string()],
    ///     vec![],
    ///     vec![],
    ///     "Hello".to_string(),
    ///     "Body text".to_string(),
    ///     None,
    /// )
    /// .expect("valid outbound message");
    /// assert_eq!(message.direction, EmailDirection::Outbound);
    /// assert_eq!(message.status, EmailStatus::Draft);
    /// assert_eq!(message.to, vec!["client@example.com".to_string()]);
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn new_outbound(
        client_profile_id: ClientProfileId,
        thread_id: Option<EmailThreadId>,
        from: String,
        to: Vec<String>,
        cc: Vec<String>,
        bcc: Vec<String>,
        subject: String,
        body_text: String,
        body_html: Option<String>,
    ) -> Result<Self, DomainError> {
        let from = normalize_email(&from);
        if !from.is_empty() && !is_valid_email(&from) {
            return Err(DomainError::Validation(
                "email from address is invalid".to_string(),
            ));
        }
        let to = Self::normalize_recipients(to, "to")?;
        let cc = Self::normalize_recipients(cc, "cc")?;
        let bcc = Self::normalize_recipients(bcc, "bcc")?;
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            return Err(DomainError::Validation(
                "outbound email requires at least one recipient".to_string(),
            ));
        }
        let subject = subject.trim().to_owned();
        let body_text = body_text.trim().to_owned();
        if subject.is_empty() && body_text.is_empty() {
            return Err(DomainError::Validation(
                "email subject and body cannot both be blank".to_string(),
            ));
        }
        Ok(Self {
            message_id: EmailMessageId::new(),
            client_profile_id,
            thread_id,
            from,
            to,
            cc,
            bcc,
            subject,
            body_text,
            body_html: body_html
                .map(|b| b.trim().to_owned())
                .filter(|b| !b.is_empty()),
            direction: EmailDirection::Outbound,
            status: EmailStatus::Draft,
            provider_message_id: None,
            idempotency_key: None,
            sent_at: None,
            created_at: Utc::now(),
        })
    }

    /// Creates an inbound message from reply ingestion.
    ///
    /// Inbound messages arrive already delivered, so they start at
    /// [`EmailStatus::Delivered`] with `sent_at` set to the reception
    /// time. The sender address is required; recipients may be empty
    /// (the agency's inbound address is implied).
    // Nine parameters is the honest shape of an inbound email; grouping
    // them would hide required fields. Mirrors the `new_outbound` allow.
    #[allow(clippy::too_many_arguments)]
    pub fn new_inbound(
        client_profile_id: ClientProfileId,
        thread_id: Option<EmailThreadId>,
        from: String,
        to: Vec<String>,
        subject: String,
        body_text: String,
        body_html: Option<String>,
        provider_message_id: Option<String>,
        received_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let from = normalize_email(&from);
        if !is_valid_email(&from) {
            return Err(DomainError::Validation(
                "inbound email from address is invalid".to_string(),
            ));
        }
        let to = Self::normalize_recipients(to, "to")?;
        let subject = subject.trim().to_owned();
        let body_text = body_text.trim().to_owned();
        if subject.is_empty() && body_text.is_empty() {
            return Err(DomainError::Validation(
                "email subject and body cannot both be blank".to_string(),
            ));
        }
        Ok(Self {
            message_id: EmailMessageId::new(),
            client_profile_id,
            thread_id,
            from,
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            subject,
            body_text,
            body_html: body_html
                .map(|b| b.trim().to_owned())
                .filter(|b| !b.is_empty()),
            direction: EmailDirection::Inbound,
            status: EmailStatus::Delivered,
            provider_message_id,
            idempotency_key: None,
            sent_at: Some(received_at),
            created_at: Utc::now(),
        })
    }

    /// Advances the status forward along the happy path
    /// (`Draft → Queued → Sending → Sent → Delivered`).
    ///
    /// Rejects backward or sideways moves, and any transition out of a
    /// terminal status, with [`DomainError::Validation`].
    pub fn advance_to(&mut self, next: EmailStatus) -> Result<(), DomainError> {
        if self.status.is_terminal() {
            return Err(DomainError::Validation(format!(
                "email status {:?} is terminal",
                self.status
            )));
        }
        // Forward-only on the happy path; Failed/Bounced have dedicated methods.
        let allowed = matches!(
            (self.status, next),
            (EmailStatus::Draft, EmailStatus::Queued)
                | (EmailStatus::Queued, EmailStatus::Sending)
                | (EmailStatus::Sending, EmailStatus::Sent)
                | (EmailStatus::Sent, EmailStatus::Delivered)
        );
        if !allowed {
            return Err(DomainError::Validation(format!(
                "illegal email status transition {:?} -> {:?}",
                self.status, next
            )));
        }
        self.status = next;
        if next == EmailStatus::Sent && self.sent_at.is_none() {
            self.sent_at = Some(Utc::now());
        }
        Ok(())
    }

    /// Marks the message failed from any non-terminal status.
    /// Idempotent: failing an already-failed message is a no-op.
    pub fn mark_failed(&mut self) -> Result<(), DomainError> {
        if self.status == EmailStatus::Failed {
            return Ok(());
        }
        if self.status.is_terminal() {
            return Err(DomainError::Validation(format!(
                "email status {:?} is terminal",
                self.status
            )));
        }
        self.status = EmailStatus::Failed;
        Ok(())
    }

    /// Marks the message bounced. Only `Sent` messages can bounce
    /// (the provider accepted, then reported non-delivery).
    pub fn mark_bounced(&mut self) -> Result<(), DomainError> {
        if self.status != EmailStatus::Sent {
            return Err(DomainError::Validation(format!(
                "only a sent email can bounce (status: {:?})",
                self.status
            )));
        }
        self.status = EmailStatus::Bounced;
        Ok(())
    }

    /// Records the provider's message id once the provider accepts the
    /// message. Set-once: a second call with a different id is
    /// rejected, guarding against double-send correlation bugs.
    pub fn set_provider_message_id(
        &mut self,
        provider_message_id: String,
    ) -> Result<(), DomainError> {
        if let Some(existing) = &self.provider_message_id {
            if existing != &provider_message_id {
                return Err(DomainError::Validation(
                    "provider message id is already set".to_string(),
                ));
            }
            return Ok(());
        }
        self.provider_message_id = Some(provider_message_id);
        Ok(())
    }

    /// All recipient addresses (to + cc + bcc), deduplicated.
    #[must_use]
    pub fn all_recipients(&self) -> Vec<String> {
        let mut out = self.to.clone();
        for addr in self.cc.iter().chain(self.bcc.iter()) {
            if !out.contains(addr) {
                out.push(addr.clone());
            }
        }
        out
    }
}

/// Lifecycle of an email thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThreadStatus {
    /// Active conversation.
    Open,
    /// Resolved; can be reopened by a new inbound reply.
    Closed,
    /// Terminal; kept for audit.
    Archived,
}

/// A conversation grouping email messages inside a client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailThread {
    /// Stable identifier of this thread.
    pub thread_id: EmailThreadId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Provider's thread id (Gmail `threadId`), when known. Used for
    /// exact-match reply correlation.
    pub provider_thread_id: Option<String>,
    /// Thread subject (from the first message).
    pub subject: String,
    /// Normalized participant addresses, deduplicated.
    pub participants: Vec<String>,
    /// Number of messages in the thread.
    pub message_count: u64,
    /// When the latest message arrived/was sent.
    pub last_message_at: DateTime<Utc>,
    /// Current lifecycle status.
    pub status: ThreadStatus,
    /// When the thread was created.
    pub created_at: DateTime<Utc>,
}

impl EmailThread {
    /// Opens a new thread for the first message's subject.
    pub fn new(
        client_profile_id: ClientProfileId,
        provider_thread_id: Option<String>,
        subject: String,
        first_participant: String,
    ) -> Result<Self, DomainError> {
        let subject = subject.trim().to_owned();
        if subject.is_empty() {
            return Err(DomainError::Validation(
                "thread subject must not be empty".to_string(),
            ));
        }
        let participant = normalize_email(&first_participant);
        if !is_valid_email(&participant) {
            return Err(DomainError::Validation(
                "thread participant address is invalid".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            thread_id: EmailThreadId::new(),
            client_profile_id,
            provider_thread_id,
            subject,
            participants: vec![participant],
            message_count: 0,
            last_message_at: now,
            status: ThreadStatus::Open,
            created_at: now,
        })
    }

    /// Records a message on the thread: bumps the count, refreshes
    /// `last_message_at`, and adds new participants. Reopening: an
    /// inbound message on a `Closed` thread reopens it.
    pub fn add_message(
        &mut self,
        direction: EmailDirection,
        participants: &[String],
        at: DateTime<Utc>,
    ) {
        self.message_count += 1;
        if at > self.last_message_at {
            self.last_message_at = at;
        }
        for p in participants {
            let normalized = normalize_email(p);
            if is_valid_email(&normalized) && !self.participants.contains(&normalized) {
                self.participants.push(normalized);
            }
        }
        if direction == EmailDirection::Inbound && self.status == ThreadStatus::Closed {
            self.status = ThreadStatus::Open;
        }
    }

    /// Closes the thread. Idempotent; a closed thread reopens on the
    /// next inbound reply (see [`EmailThread::add_message`]).
    pub fn close(&mut self) -> Result<(), DomainError> {
        if self.status == ThreadStatus::Archived {
            return Err(DomainError::Validation(
                "archived threads cannot be closed".to_string(),
            ));
        }
        self.status = ThreadStatus::Closed;
        Ok(())
    }

    /// Reopens a closed thread. Archived threads stay archived.
    pub fn reopen(&mut self) -> Result<(), DomainError> {
        match self.status {
            ThreadStatus::Closed => {
                self.status = ThreadStatus::Open;
                Ok(())
            }
            ThreadStatus::Open => Ok(()),
            ThreadStatus::Archived => Err(DomainError::Validation(
                "archived threads cannot be reopened".to_string(),
            )),
        }
    }

    /// Archives the thread. Terminal and idempotent.
    pub fn archive(&mut self) {
        self.status = ThreadStatus::Archived;
    }
}

/// Hints extracted by the provider adapter from an inbound message,
/// used to correlate the reply with an existing thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundThreadHints {
    /// Provider thread id (e.g. Gmail `threadId`), when the provider
    /// supplies one.
    pub provider_thread_id: Option<String>,
    /// `In-Reply-To` header: the provider message id being answered.
    pub in_reply_to: Option<String>,
    /// `References` header chain: provider message ids.
    pub references: Vec<String>,
    /// Raw subject line of the inbound message.
    pub subject: String,
}

/// Thread-matching cascade used by reply ingestion.
///
/// The service applies these rules in order; this pure function
/// implements the *subject heuristic* (the last resort), so it stays
/// unit-testable without a database:
///
/// 1. **Exact provider thread id** — the adapter's
///    `provider_thread_id` matches the thread's. Strongest signal;
///    resolved by the repository, not here.
/// 2. **Parent message lookup** — `in_reply_to`/`references` resolve
///    to a known message via `find_by_provider_message_id`; its
///    `thread_id` wins. Also repository-driven.
/// 3. **Subject heuristic** (this function) — the normalized subject
///    (see [`normalize_subject`]) equals the thread's normalized
///    subject, and the thread is the most recently active candidate.
///    Weakest signal: only used when the provider gives no thread id
///    and no parent message is known.
///
/// Returns the most recently active thread whose normalized subject
/// matches, or `None`.
#[must_use]
pub fn match_thread_by_subject<'a>(
    subject: &str,
    candidates: &'a [EmailThread],
) -> Option<&'a EmailThread> {
    let normalized = normalize_subject(subject);
    candidates
        .iter()
        .filter(|t| normalize_subject(&t.subject) == normalized)
        .max_by_key(|t| t.last_message_at)
}

/// A file attached to an email message.
///
/// Attachments reuse the [`Asset`](crate::asset::Asset) domain for
/// content addressing: the bytes live in the asset store (deduped by
/// SHA-256) and the attachment is the link between message and bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAttachment {
    /// Stable identifier of this attachment link.
    pub attachment_id: EmailAttachmentId,
    /// The message carrying the attachment.
    pub email_message_id: EmailMessageId,
    /// Content-addressed bytes in the asset store.
    pub asset_id: AssetId,
    /// Original file name.
    pub file_name: String,
    /// MIME type.
    pub mime_type: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Staff member or agent principal that attached the file
    /// (outbound), or the ingestion principal (inbound).
    pub attached_by: ActorId,
    /// When the attachment was linked.
    pub created_at: DateTime<Utc>,
}

impl EmailAttachment {
    /// Links asset bytes to a message.
    pub fn new(
        email_message_id: EmailMessageId,
        asset_id: AssetId,
        file_name: String,
        mime_type: String,
        size_bytes: u64,
        attached_by: ActorId,
    ) -> Result<Self, DomainError> {
        let file_name = file_name.trim().to_owned();
        if file_name.is_empty() {
            return Err(DomainError::Validation(
                "attachment file name must not be empty".to_string(),
            ));
        }
        let mime_type = mime_type.trim().to_owned();
        if mime_type.is_empty() {
            return Err(DomainError::Validation(
                "attachment MIME type must not be empty".to_string(),
            ));
        }
        if size_bytes == 0 {
            return Err(DomainError::Validation(
                "attachment size must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            attachment_id: EmailAttachmentId::new(),
            email_message_id,
            asset_id,
            file_name,
            mime_type,
            size_bytes,
            attached_by,
            created_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbound_message() -> EmailMessage {
        EmailMessage::new_outbound(
            ClientProfileId::new(),
            None,
            "AGENCY@example.com ".to_string(),
            vec!["Client@Example.COM ".to_string()],
            vec!["cc@example.com".to_string(), "cc@example.com".to_string()],
            vec![],
            "Hello".to_string(),
            "Body".to_string(),
            None,
        )
        .expect("valid outbound message")
    }

    #[test]
    fn email_validation_table() {
        assert!(is_valid_email("a@example.com"));
        assert!(is_valid_email("A@Example.COM "));
        assert!(!is_valid_email("not-an-email"));
        assert!(!is_valid_email("a@b"));
        assert!(!is_valid_email("@example.com"));
        assert!(!is_valid_email("a@"));
        assert!(!is_valid_email("a@@example.com"));
        assert!(!is_valid_email(""));
        assert_eq!(normalize_email("A@Example.COM "), "a@example.com");
    }

    #[test]
    fn outbound_requires_recipient_and_content() {
        let profile = ClientProfileId::new();
        // No recipients.
        assert!(EmailMessage::new_outbound(
            profile,
            None,
            "a@example.com".to_string(),
            vec![],
            vec![],
            vec![],
            "Hi".to_string(),
            "Body".to_string(),
            None,
        )
        .is_err());
        // Blank subject and body.
        assert!(EmailMessage::new_outbound(
            profile,
            None,
            "a@example.com".to_string(),
            vec!["b@example.com".to_string()],
            vec![],
            vec![],
            "  ".to_string(),
            "  ".to_string(),
            None,
        )
        .is_err());
        // Invalid recipient.
        assert!(EmailMessage::new_outbound(
            profile,
            None,
            "a@example.com".to_string(),
            vec!["not-an-email".to_string()],
            vec![],
            vec![],
            "Hi".to_string(),
            "Body".to_string(),
            None,
        )
        .is_err());
    }

    #[test]
    fn outbound_normalizes_and_dedupes_recipients() {
        let message = outbound_message();
        assert_eq!(message.from, "agency@example.com".to_string());
        assert_eq!(message.to, vec!["client@example.com".to_string()]);
        assert_eq!(message.cc, vec!["cc@example.com".to_string()]);
        assert_eq!(
            message.all_recipients(),
            vec![
                "client@example.com".to_string(),
                "cc@example.com".to_string()
            ]
        );
    }

    #[test]
    fn status_advances_forward_only() {
        let mut message = outbound_message();
        // Skip-ahead rejected.
        assert!(message.advance_to(EmailStatus::Sent).is_err());
        // Happy path.
        for next in [
            EmailStatus::Queued,
            EmailStatus::Sending,
            EmailStatus::Sent,
            EmailStatus::Delivered,
        ] {
            message.advance_to(next).expect("forward transition");
        }
        assert!(message.sent_at.is_some());
        // Terminal: no further moves.
        assert!(message.advance_to(EmailStatus::Delivered).is_err());
        assert!(message.mark_failed().is_err());
    }

    #[test]
    fn mark_failed_from_any_non_terminal() {
        for start in [
            EmailStatus::Draft,
            EmailStatus::Queued,
            EmailStatus::Sending,
            EmailStatus::Sent,
        ] {
            let mut message = outbound_message();
            // Drive to the start status.
            let path: &[EmailStatus] = match start {
                EmailStatus::Draft => &[],
                EmailStatus::Queued => &[EmailStatus::Queued],
                EmailStatus::Sending => &[EmailStatus::Queued, EmailStatus::Sending],
                EmailStatus::Sent => {
                    &[EmailStatus::Queued, EmailStatus::Sending, EmailStatus::Sent]
                }
                _ => &[],
            };
            for next in path {
                message.advance_to(*next).expect("setup transition");
            }
            message.mark_failed().expect("mark failed");
            assert_eq!(message.status, EmailStatus::Failed);
            // Idempotent.
            message.mark_failed().expect("failed is idempotent");
        }
    }

    #[test]
    fn only_sent_can_bounce() {
        let mut message = outbound_message();
        assert!(message.mark_bounced().is_err());
        for next in [EmailStatus::Queued, EmailStatus::Sending, EmailStatus::Sent] {
            message.advance_to(next).expect("forward transition");
        }
        message.mark_bounced().expect("sent can bounce");
        assert_eq!(message.status, EmailStatus::Bounced);
        assert!(message.status.is_terminal());
    }

    #[test]
    fn provider_message_id_is_set_once() {
        let mut message = outbound_message();
        message
            .set_provider_message_id("gmail-123".to_string())
            .expect("first set");
        // Same id is a no-op.
        message
            .set_provider_message_id("gmail-123".to_string())
            .expect("same id ok");
        // Different id is rejected.
        assert!(message
            .set_provider_message_id("gmail-456".to_string())
            .is_err());
    }

    #[test]
    fn inbound_starts_delivered() {
        let message = EmailMessage::new_inbound(
            ClientProfileId::new(),
            None,
            "client@example.com".to_string(),
            vec![],
            "Re: Hello".to_string(),
            "Reply body".to_string(),
            None,
            Some("gmail-999".to_string()),
            Utc::now(),
        )
        .expect("valid inbound");
        assert_eq!(message.direction, EmailDirection::Inbound);
        assert_eq!(message.status, EmailStatus::Delivered);
        assert!(message.sent_at.is_some());
        assert!(message.status.is_terminal());
    }

    #[test]
    fn thread_lifecycle() {
        let mut thread = EmailThread::new(
            ClientProfileId::new(),
            Some("gmail-thread-1".to_string()),
            "Hello".to_string(),
            "a@example.com".to_string(),
        )
        .expect("valid thread");
        assert_eq!(thread.status, ThreadStatus::Open);
        assert_eq!(thread.message_count, 0);

        thread.add_message(
            EmailDirection::Outbound,
            &["a@example.com".to_string(), "b@example.com".to_string()],
            Utc::now(),
        );
        assert_eq!(thread.message_count, 1);
        assert_eq!(thread.participants.len(), 2);

        thread.close().expect("close");
        assert_eq!(thread.status, ThreadStatus::Closed);
        // Inbound reopens.
        thread.add_message(
            EmailDirection::Inbound,
            &["c@example.com".to_string()],
            Utc::now(),
        );
        assert_eq!(thread.status, ThreadStatus::Open);

        thread.reopen().expect("reopen idempotent on open");
        thread.archive();
        assert_eq!(thread.status, ThreadStatus::Archived);
        assert!(thread.close().is_err());
        assert!(thread.reopen().is_err());
    }

    #[test]
    fn subject_heuristic_matches_most_recent() {
        let profile = ClientProfileId::new();
        let old = EmailThread {
            last_message_at: Utc::now() - chrono::Duration::hours(2),
            ..EmailThread::new(
                profile,
                None,
                "Invoice".to_string(),
                "a@example.com".to_string(),
            )
            .expect("thread")
        };
        let new = EmailThread {
            last_message_at: Utc::now(),
            ..EmailThread::new(
                profile,
                None,
                "Invoice".to_string(),
                "b@example.com".to_string(),
            )
            .expect("thread")
        };
        let other = EmailThread::new(
            profile,
            None,
            "Different".to_string(),
            "c@example.com".to_string(),
        )
        .expect("thread");

        let candidates = vec![old, new.clone(), other];
        let matched = match_thread_by_subject("Re: invoice", &candidates).expect("match");
        assert_eq!(matched.thread_id, new.thread_id);
        assert!(match_thread_by_subject("No such subject", &candidates).is_none());
    }

    #[test]
    fn attachment_links_asset() {
        let attachment = EmailAttachment::new(
            EmailMessageId::new(),
            AssetId::new(),
            "doc.pdf".to_string(),
            "application/pdf".to_string(),
            512,
            ActorId::new(),
        )
        .expect("valid attachment");
        assert_eq!(attachment.file_name, "doc.pdf");
        assert!(EmailAttachment::new(
            EmailMessageId::new(),
            AssetId::new(),
            "  ".to_string(),
            "application/pdf".to_string(),
            512,
            ActorId::new(),
        )
        .is_err());
    }
}
