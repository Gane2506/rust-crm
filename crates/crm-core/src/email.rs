// SPDX-License-Identifier: GPL-3.0-only
//! Email service implementation (task 07-MAIL-02).
//!
//! [`EmailServiceImpl`] implements
//! [`EmailService`](crm_domain::services::EmailService) over generic
//! repository, provider and audit ports — the same composition pattern
//! as [`ContactServiceImpl`](crate::memory::ContactServiceImpl). The
//! production wiring (SQLx repositories + stub provider) is assembled
//! by the API layer; real Gmail/Zoho adapters land in a later task.
//!
//! [`StubEmailProvider`] is a test/dev
//! [`EmailProvider`](crm_domain::services::EmailProvider) that records
//! sent messages in memory and mints fake provider ids. It performs no
//! network I/O.

use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::email::{match_thread_by_subject, EmailDirection, EmailMessage, EmailThread};
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, EmailThreadId};
use crm_domain::repositories::{
    EmailAttachmentRepository, EmailMessageRepository, EmailThreadRepository,
};
use crm_domain::services::{
    ActionContext, AuditService, EmailProvider, EmailService, InboundEmail, NewEmailMessage,
    ProviderSendOutcome,
};

/// Generic [`EmailService`] implementation.
///
/// Type parameters: `M` message repository, `T` thread repository,
/// `AT` attachment repository, `P` provider adapter, `A` audit service.
pub struct EmailServiceImpl<M, T, AT, P, A> {
    messages: M,
    threads: T,
    attachments: AT,
    provider: P,
    audit: A,
}

impl<M, T, AT, P, A> EmailServiceImpl<M, T, AT, P, A> {
    /// Creates the service over the given ports.
    pub fn new(messages: M, threads: T, attachments: AT, provider: P, audit: A) -> Self {
        Self {
            messages,
            threads,
            attachments,
            provider,
            audit,
        }
    }

    /// Borrows the message repository (read path for the HTTP layer).
    #[must_use]
    pub fn messages(&self) -> &M {
        &self.messages
    }

    /// Borrows the thread repository (read path for the HTTP layer).
    #[must_use]
    pub fn threads(&self) -> &T {
        &self.threads
    }

    /// Borrows the attachment repository (read path for the HTTP layer).
    #[must_use]
    pub fn attachments(&self) -> &AT {
        &self.attachments
    }

    /// Enforces client-profile scope: a profile-scoped context may only
    /// touch its own profile; an agency-scoped context (`None`) may
    /// touch any profile in the agency (grant checks happen upstream).
    fn check_scope(ctx: &ActionContext, profile: ClientProfileId) -> Result<(), DomainError> {
        match ctx.client_profile_id {
            Some(ctx_profile) if ctx_profile != profile => Err(DomainError::Forbidden(
                "operation crosses client-profile scope".to_owned(),
            )),
            _ => Ok(()),
        }
    }
}

impl<M, T, AT, P, A> EmailService for EmailServiceImpl<M, T, AT, P, A>
where
    M: EmailMessageRepository + Send + Sync,
    T: EmailThreadRepository + Send + Sync,
    AT: EmailAttachmentRepository + Send + Sync,
    P: EmailProvider + Send + Sync,
    A: AuditService + Send + Sync,
{
    async fn send(
        &self,
        input: NewEmailMessage,
        ctx: &ActionContext,
    ) -> Result<EmailMessage, DomainError> {
        Self::check_scope(ctx, input.client_profile_id)?;

        // Idempotent replay: a non-empty key that was already stored
        // returns the original message instead of sending twice.
        // Campaign launch mints deterministic keys so a re-launch after
        // a crash resumes without re-sending to already-mailed contacts.
        if !input.idempotency_key.is_empty() {
            if let Some(existing) = self
                .messages
                .find_by_idempotency_key(input.client_profile_id, &input.idempotency_key)
                .await?
            {
                return Ok(existing);
            }
        }

        // Validate and create the draft.
        let mut message = EmailMessage::new_outbound(
            input.client_profile_id,
            input.thread_id,
            input.from,
            input.to,
            input.cc,
            input.bcc,
            input.subject,
            input.body_text,
            input.body_html,
        )?;
        message.idempotency_key = if input.idempotency_key.is_empty() {
            None
        } else {
            Some(input.idempotency_key)
        };
        message.advance_to(crm_domain::email::EmailStatus::Queued)?;
        // Resolve the thread: a reply appends to its thread; a fresh
        // compose opens one. Before this, a send with thread_id None
        // orphaned the message — no thread row, so the threads list
        // could never show it and the sent mail was unfindable in the
        // UI (MAIL-01 live verification). Mirrors ingest_reply's
        // "no match: open a new thread".
        let thread = match input.thread_id {
            Some(thread_id) => Some(self.threads.get(thread_id).await?),
            None => None,
        };
        let mut thread = match thread {
            Some(thread) => thread,
            None => {
                let first_participant = if !message.from.is_empty() {
                    message.from.clone()
                } else {
                    message
                        .to
                        .first()
                        .or(message.cc.first())
                        .or(message.bcc.first())
                        .cloned()
                        .unwrap_or_default()
                };
                let subject = if message.subject.is_empty() {
                    "(no subject)".to_string()
                } else {
                    message.subject.clone()
                };
                let thread =
                    EmailThread::new(input.client_profile_id, None, subject, first_participant)?;
                self.threads.create(thread).await?
            }
        };
        // A passed-in thread must belong to the profile being mailed
        // in; a fresh one trivially does (same profile id).
        Self::check_scope(ctx, thread.client_profile_id)?;
        message.thread_id = Some(thread.thread_id);
        let mut message = self.messages.create(message).await?;

        // Hand to the provider.
        message.advance_to(crm_domain::email::EmailStatus::Sending)?;
        message = self.messages.update(message).await?;
        let outcome = match self.provider.send_message(&message).await {
            Ok(outcome) => outcome,
            Err(error) => {
                // Provider failure: terminal Failed, audit, propagate.
                // The message row is preserved for the audit trail;
                // re-sending creates a new idempotency-keyed message.
                // The failed message stays in its thread (visible,
                // auditable); count it so the thread's message count
                // stays true.
                let _ = message.mark_failed();
                let message = self.messages.update(message).await?;
                thread.add_message(EmailDirection::Outbound, &message.to, chrono::Utc::now());
                let _ = self.threads.update(thread).await;
                let _ = self
                    .audit
                    .record_action(
                        ctx,
                        "email.failed",
                        "email",
                        &message.message_id.to_string(),
                        PermissionDecision::Allow,
                        ActionResult::Failure,
                        None,
                        Some(&error.to_string()),
                    )
                    .await;
                return Err(error);
            }
        };
        message.set_provider_message_id(outcome.provider_message_id)?;
        message.advance_to(crm_domain::email::EmailStatus::Sent)?;
        let message = self.messages.update(message).await?;

        // Record the provider's threading (if any) and the message on
        // the thread, so the sent mail shows up in the threads list.
        // (The old code looked the thread up by provider id and then
        // set the same id on it — a no-op that never linked anything.)
        if let Some(provider_thread_id) = outcome.provider_thread_id {
            thread.provider_thread_id = Some(provider_thread_id);
        }
        thread.add_message(
            EmailDirection::Outbound,
            &message.to,
            message.sent_at.unwrap_or_else(chrono::Utc::now),
        );
        let _ = self.threads.update(thread).await;

        let _ = self
            .audit
            .record_action(
                ctx,
                "email.sent",
                "email",
                &message.message_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
        Ok(message)
    }

    async fn ingest_reply(
        &self,
        inbound: InboundEmail,
        ctx: &ActionContext,
    ) -> Result<EmailMessage, DomainError> {
        Self::check_scope(ctx, inbound.client_profile_id)?;

        // At-most-once ingestion: a known provider message id returns
        // the existing message instead of a duplicate.
        if let Some(existing) = self
            .messages
            .find_by_provider_message_id(inbound.client_profile_id, &inbound.provider_message_id)
            .await?
        {
            return Ok(existing);
        }

        // Thread-matching cascade.
        let mut thread: Option<EmailThread> = None;

        // 1. Exact provider thread id.
        if let Some(provider_thread_id) = &inbound.hints.provider_thread_id {
            thread = self
                .threads
                .find_by_provider_thread_id(inbound.client_profile_id, provider_thread_id)
                .await?;
        }

        // 2. Parent message lookup via In-Reply-To / References.
        if thread.is_none() {
            let mut parent_ids = Vec::new();
            if let Some(in_reply_to) = &inbound.hints.in_reply_to {
                parent_ids.push(in_reply_to.clone());
            }
            parent_ids.extend(inbound.hints.references.iter().cloned());
            for parent_id in parent_ids {
                if let Some(parent) = self
                    .messages
                    .find_by_provider_message_id(inbound.client_profile_id, &parent_id)
                    .await?
                {
                    if let Some(thread_id) = parent.thread_id {
                        thread = Some(self.threads.get(thread_id).await?);
                        break;
                    }
                }
            }
        }

        // 3. Subject heuristic (last resort).
        if thread.is_none() {
            let candidates = self
                .threads
                .list_for_client(inbound.client_profile_id, 100)
                .await?;
            thread = match_thread_by_subject(&inbound.hints.subject, &candidates).cloned();
        }

        // No match: open a new thread.
        let mut thread = match thread {
            Some(thread) => thread,
            None => {
                let thread = EmailThread::new(
                    inbound.client_profile_id,
                    inbound.hints.provider_thread_id.clone(),
                    inbound.hints.subject.clone(),
                    inbound.from.clone(),
                )?;
                self.threads.create(thread).await?
            }
        };

        // Create the inbound message (starts Delivered).
        let mut message = EmailMessage::new_inbound(
            inbound.client_profile_id,
            Some(thread.thread_id),
            inbound.from.clone(),
            inbound.to,
            inbound.hints.subject.clone(),
            inbound.body_text,
            inbound.body_html,
            Some(inbound.provider_message_id),
            inbound.received_at,
        )?;
        message = self.messages.create(message).await?;

        // Update the thread: count, participants, last-message time;
        // inbound on a closed thread reopens it.
        thread.add_message(
            crm_domain::email::EmailDirection::Inbound,
            &[message.from.clone()],
            message.sent_at.unwrap_or_else(chrono::Utc::now),
        );
        let _ = self.threads.update(thread).await;

        let _ = self
            .audit
            .record_action(
                ctx,
                "email.received",
                "email",
                &message.message_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
        Ok(message)
    }

    async fn get_thread(
        &self,
        thread_id: EmailThreadId,
        ctx: &ActionContext,
    ) -> Result<EmailThread, DomainError> {
        let thread = self.threads.get(thread_id).await?;
        Self::check_scope(ctx, thread.client_profile_id)?;
        Ok(thread)
    }

    async fn list_threads(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
        ctx: &ActionContext,
    ) -> Result<Vec<EmailThread>, DomainError> {
        Self::check_scope(ctx, client_profile_id)?;
        self.threads.list_for_client(client_profile_id, limit).await
    }
}

/// Test/dev [`EmailProvider`]: records sent messages in memory and
/// mints fake provider ids. Performs no network I/O.
///
/// Real Gmail/Zoho adapters require provider credentials and land in a
/// later task; this stub lets the vertical slice (and its tests)
/// exercise the send path end to end.
#[derive(Debug, Clone, Default)]
pub struct StubEmailProvider {
    sent: Arc<Mutex<Vec<EmailMessage>>>,
}

impl StubEmailProvider {
    /// Creates a fresh stub with an empty sent-message log.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns the messages sent through this stub, in send order.
    /// Test inspection only.
    #[must_use]
    pub fn sent_messages(&self) -> Vec<EmailMessage> {
        self.sent.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl EmailProvider for StubEmailProvider {
    async fn send_message(
        &self,
        message: &EmailMessage,
    ) -> Result<ProviderSendOutcome, DomainError> {
        let provider_message_id = format!("stub-{}", Uuid::new_v4());
        if let Ok(mut sent) = self.sent.lock() {
            sent.push(message.clone());
        }
        Ok(ProviderSendOutcome {
            provider_message_id,
            provider_thread_id: None,
        })
    }

    async fn fetch_message(&self, _provider_message_id: &str) -> Result<InboundEmail, DomainError> {
        Err(DomainError::Validation(
            "stub email provider cannot fetch messages".to_owned(),
        ))
    }
}
