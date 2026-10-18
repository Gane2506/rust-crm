// SPDX-License-Identifier: GPL-3.0-only
//! Calling service implementation (task 14-VOICE-02).
//!
//! [`VoiceServiceImpl`] implements
//! [`VoiceService`](crm_domain::services::VoiceService) over the
//! PostgreSQL call repository and an injected
//! [`VoiceProvider`](crm_domain::voice::VoiceProvider). It drives the
//! [`Call`](crm_domain::voice::Call) status machine, enforces the
//! consent gate on recordings, and records the pinned audit events.
//!
//! [`StubVoiceProvider`] is the deterministic in-memory
//! [`VoiceProvider`](crm_domain::voice::VoiceProvider) for tests and
//! local development. The Telnyx adapter is a later workstream's
//! concern.
//!
//! Audit failures are swallowed: audit is fail-loud at the HTTP layer
//! and never fails a calling operation (same convention as
//! [`BillingServiceImpl`](crate::billing::BillingServiceImpl)).

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{AssetId, CallId, ClientProfileId};
use crm_domain::repositories::CallRepository;
use crm_domain::services::{ActionContext, AuditService, NewCall, NewRecording, VoiceService};
use crm_domain::voice::{
    Call, CallStatus, PhoneNumber, VoiceProvider, VOICE_CALL_ANSWERED, VOICE_CALL_ENDED,
    VOICE_CALL_PLACED, VOICE_RECORDING_STARTED, VOICE_RECORDING_STOPPED,
};

/// Deterministic in-memory [`VoiceProvider`] for tests and local
/// development.
///
/// `initiate_call` returns `stub-call-N` with a process-wide
/// counter; `hangup` is a no-op (idempotent); `call_status` reports
/// `InProgress` — the service, not the stub, owns the call
/// lifecycle. The Telnyx adapter is a later workstream's concern.
#[derive(Debug, Clone, Default)]
pub struct StubVoiceProvider {
    counter: Arc<AtomicU64>,
}

impl StubVoiceProvider {
    /// Creates the stub with the counter starting at zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(0)),
        }
    }
}

// The trait declares `fn ... -> impl Future<Output = ...> + Send +
// 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl VoiceProvider for StubVoiceProvider {
    fn initiate_call<'a>(
        &'a self,
        _to: &'a PhoneNumber,
    ) -> impl Future<Output = Result<String, DomainError>> + Send + 'a {
        async move {
            let n = self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(format!("stub-call-{n}"))
        }
    }

    fn hangup<'a>(
        &'a self,
        _provider_call_id: &'a str,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a {
        async move { Ok(()) }
    }

    fn call_status<'a>(
        &'a self,
        _provider_call_id: &'a str,
    ) -> impl Future<Output = Result<CallStatus, DomainError>> + Send + 'a {
        async move { Ok(CallStatus::InProgress) }
    }
}

/// PostgreSQL-backed implementation of
/// [`VoiceService`](crm_domain::services::VoiceService).
///
/// Type parameters: `CR` call repository, `P` voice provider, `A`
/// audit service.
#[derive(Debug, Clone)]
pub struct VoiceServiceImpl<CR, P, A> {
    calls: CR,
    provider: P,
    audit: A,
}

impl<CR, P, A> VoiceServiceImpl<CR, P, A> {
    /// Creates the service over the call repository, the voice
    /// provider, and the audit service.
    pub fn new(calls: CR, provider: P, audit: A) -> Self {
        Self {
            calls,
            provider,
            audit,
        }
    }

    /// Fetches a call and enforces that it is visible in the
    /// caller's profile scope, returning [`DomainError::Forbidden`]
    /// on a mismatch. An agency-scoped context (`None`) passes —
    /// grant checks happen upstream, at the HTTP layer.
    async fn scoped_call(&self, call_id: CallId, ctx: &ActionContext) -> Result<Call, DomainError>
    where
        CR: CallRepository,
    {
        let call = self.calls.get(call_id).await?;
        check_scope(ctx, call.client_profile_id)?;
        Ok(call)
    }

    /// Records an audit event for a calling operation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails
    /// the operation).
    async fn audit_voice(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_kind: &str,
        target_id: &str,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                target_kind,
                target_id,
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
    }
}

/// Enforces profile scope on a call: a profile-scoped context may
/// only touch its own profile's calls; an agency-scoped context
/// (`None`) passes because grant checks happen upstream, at the HTTP
/// layer.
fn check_scope(ctx: &ActionContext, client_profile_id: ClientProfileId) -> Result<(), DomainError> {
    match ctx.client_profile_id {
        Some(ctx_profile) if ctx_profile != client_profile_id => Err(DomainError::Forbidden(
            "call is not visible in this profile".to_string(),
        )),
        _ => Ok(()),
    }
}

// The service trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl<CR, P, A> VoiceService for VoiceServiceImpl<CR, P, A>
where
    CR: CallRepository + Send + Sync,
    P: VoiceProvider + Send + Sync,
    A: AuditService + Send + Sync,
{
    fn place_call<'a>(
        &'a self,
        input: NewCall,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Call, DomainError>> + Send + 'a {
        async move {
            check_scope(ctx, input.client_profile_id)?;
            let provider_call_id = self.provider.initiate_call(&input.to).await?;
            let mut call = Call::initiate(
                input.client_profile_id,
                input.contact_id,
                input.direction,
                input.from,
                input.to,
            )?;
            call.set_provider_call_id(provider_call_id);
            let call = self.calls.create(call).await?;
            self.audit_voice(ctx, VOICE_CALL_PLACED, "call", &call.call_id.to_string())
                .await;
            Ok(call)
        }
    }

    fn answer_call<'a>(
        &'a self,
        call_id: CallId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Call, DomainError>> + Send + 'a {
        async move {
            let mut call = self.scoped_call(call_id, ctx).await?;
            call.answer()?;
            let call = self.calls.update(call).await?;
            self.audit_voice(ctx, VOICE_CALL_ANSWERED, "call", &call.call_id.to_string())
                .await;
            Ok(call)
        }
    }

    fn end_call<'a>(
        &'a self,
        call_id: CallId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Call, DomainError>> + Send + 'a {
        async move {
            let mut call = self.scoped_call(call_id, ctx).await?;
            // Move the call to the terminal status the domain allows
            // from its current state.
            match call.status {
                CallStatus::InProgress => call.complete()?,
                CallStatus::Ringing => call.no_answer()?,
                CallStatus::Initiated => call.fail("call ended before connecting".to_string())?,
                CallStatus::Completed
                | CallStatus::Failed
                | CallStatus::NoAnswer
                | CallStatus::Busy => {
                    return Err(DomainError::Validation(format!(
                        "call is already finished in status {:?}",
                        call.status
                    )))
                }
            }
            // The provider hangup is best-effort and idempotent.
            if let Some(provider_call_id) = call.provider_call_id.clone() {
                let _ = self.provider.hangup(&provider_call_id).await;
            }
            let call = self.calls.update(call).await?;
            self.audit_voice(ctx, VOICE_CALL_ENDED, "call", &call.call_id.to_string())
                .await;
            Ok(call)
        }
    }

    fn start_recording<'a>(
        &'a self,
        input: NewRecording,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Call, DomainError>> + Send + 'a {
        async move {
            let mut call = self.scoped_call(input.call_id, ctx).await?;
            // The domain enforces the consent gate: only
            // `RecordingConsent::Granted` on an `InProgress` call may
            // start a recording.
            call.set_recording_consent(input.consent)?;
            call.start_recording()?;
            let call = self.calls.update(call).await?;
            self.audit_voice(
                ctx,
                VOICE_RECORDING_STARTED,
                "call",
                &call.call_id.to_string(),
            )
            .await;
            Ok(call)
        }
    }

    fn stop_recording<'a>(
        &'a self,
        call_id: CallId,
        asset_id: AssetId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Call, DomainError>> + Send + 'a {
        async move {
            let mut call = self.scoped_call(call_id, ctx).await?;
            call.stop_recording(asset_id)?;
            let call = self.calls.update(call).await?;
            self.audit_voice(
                ctx,
                VOICE_RECORDING_STOPPED,
                "call",
                &call.call_id.to_string(),
            )
            .await;
            Ok(call)
        }
    }

    fn list_calls<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<Call>, DomainError>> + Send + 'a {
        async move {
            let profile_id = ctx.client_profile_id.ok_or_else(|| {
                DomainError::Forbidden("call listing requires a client profile scope".to_string())
            })?;
            self.calls.list_by_profile(profile_id, limit).await
        }
    }
}
