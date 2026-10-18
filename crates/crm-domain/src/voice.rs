// SPDX-License-Identifier: GPL-3.0-only
//! Calling domain: calls, consent-gated recordings, the provider port,
//! and the service/repository ports.
//!
//! Staff place calls to contacts (outbound) and receive them
//! (inbound). Every call is a [`Call`] with a forward-only
//! [`CallStatus`] machine. A finished call is summarized as an
//! immutable [`CallLog`].
//!
//! ## Consent model
//!
//! Recording a call is **consent-gated** — this is a legal
//! requirement, not a UX nicety. The domain enforces three rules:
//!
//! 1. [`CallRecording`] can only start when
//!    [`RecordingConsent::Granted`] was recorded first; any other
//!    consent state rejects `start_recording` with
//!    [`DomainError::Validation`](crate::error::DomainError::Validation).
//! 2. Consent is captured explicitly per call via
//!    [`Call::set_recording_consent`]; silence is `Unknown`, and
//!    `Unknown` does not authorize recording.
//! 3. A recording references an [`AssetId`](crate::ids::AssetId): the
//!    audio bytes live in the content-addressed asset store, never
//!    inline, so retention and deletion follow the asset lifecycle.
//!
//! The service layer is responsible for surfacing the consent prompt
//! to staff; the domain only guarantees that recording cannot happen
//! without a recorded grant.
//!
//! ## Provider abstraction
//!
//! [`VoiceProvider`] is the port the service calls to reach the
//! telephony network. It is provider-agnostic by design; the Telnyx
//! adapter is a later workstream's concern (see the
//! [`VoiceProvider`] docs).
//!
//! ## Authorization
//!
//! Voice operations authorize against `Resource::Call`
//! (profile-scoped; the variant lands in task 14-VOICE-02 together
//! with the persistence layer).
//!
//! ## Audit
//!
//! Call mutations are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s by the service
//! implementation, with pinned action names ([`VOICE_CALL_PLACED`]
//! … [`VOICE_RECORDING_STOPPED`]) documented on
//! [`VoiceService`](crate::services::VoiceService). They are
//! deliberately **not** [`SecurityEvent`](crate::audit::SecurityEvent)
//! variants: that taxonomy covers security-relevant events, while
//! calling is a business operation audited like invoices and
//! campaigns.

use std::future::Future;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{AssetId, CallId, ClientProfileId, ContactId};

/// Audit action name recorded when an outbound call is placed.
pub const VOICE_CALL_PLACED: &str = "voice.call_placed";
/// Audit action name recorded when a call is answered.
pub const VOICE_CALL_ANSWERED: &str = "voice.call_answered";
/// Audit action name recorded when a call ends (any terminal status).
pub const VOICE_CALL_ENDED: &str = "voice.call_ended";
/// Audit action name recorded when recording starts on a call.
pub const VOICE_RECORDING_STARTED: &str = "voice.recording_started";
/// Audit action name recorded when recording stops on a call.
pub const VOICE_RECORDING_STOPPED: &str = "voice.recording_stopped";

/// A validated telephone number.
///
/// E.164-ish: optional leading `+`, then 7–15 ASCII digits. The
/// in-memory contact layer only trims phones (see
/// `crm_core::memory::normalize_phone`, where full E.164
/// normalization was deferred to this workstream); the domain type
/// here is the strict entry point for dialing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct PhoneNumber {
    /// Digits with the optional leading `+`, as validated.
    value: String,
}

impl<'de> Deserialize<'de> for PhoneNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawPhone {
            value: String,
        }
        let raw = RawPhone::deserialize(deserializer)?;
        PhoneNumber::new(raw.value).map_err(serde::de::Error::custom)
    }
}

impl PhoneNumber {
    /// Validates and normalizes a raw phone number.
    ///
    /// Rejects: blank input; anything but an optional leading `+`
    /// followed by digits; fewer than 7 or more than 15 digits.
    ///
    /// ```
    /// use crm_domain::voice::PhoneNumber;
    ///
    /// let number = PhoneNumber::new("+14155550199".to_string()).expect("valid number");
    /// assert_eq!(number.as_str(), "+14155550199");
    /// assert!(PhoneNumber::new("not-a-number".to_string()).is_err());
    /// ```
    pub fn new(raw: String) -> Result<Self, DomainError> {
        let value = raw.trim().to_string();
        if value.is_empty() {
            return Err(DomainError::Validation(
                "phone number must not be blank".to_string(),
            ));
        }
        let digits = value.strip_prefix('+').unwrap_or(&value);
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(DomainError::Validation(format!(
                "phone number {value:?} must contain only digits with an optional leading '+'"
            )));
        }
        if !(7..=15).contains(&digits.len()) {
            return Err(DomainError::Validation(format!(
                "phone number {value:?} must have 7 to 15 digits"
            )));
        }
        Ok(PhoneNumber { value })
    }

    /// The validated number, digits with optional leading `+`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Which way a call goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CallDirection {
    /// Staff dialed the contact.
    Outbound,
    /// The contact dialed in.
    Inbound,
}

impl CallDirection {
    /// Machine-readable name of the direction.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CallDirection::Outbound => "outbound",
            CallDirection::Inbound => "inbound",
        }
    }
}

/// Forward-only lifecycle of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CallStatus {
    /// The call was initiated (dialed or received) and is being set up.
    Initiated,
    /// The far end is ringing.
    Ringing,
    /// Both parties are connected.
    InProgress,
    /// The call ended normally.
    Completed,
    /// The call failed (network error, provider error).
    Failed,
    /// The far end never answered.
    NoAnswer,
    /// The far end was busy.
    Busy,
}

impl CallStatus {
    /// Machine-readable name of the status.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CallStatus::Initiated => "initiated",
            CallStatus::Ringing => "ringing",
            CallStatus::InProgress => "in_progress",
            CallStatus::Completed => "completed",
            CallStatus::Failed => "failed",
            CallStatus::NoAnswer => "no_answer",
            CallStatus::Busy => "busy",
        }
    }

    /// Whether the status is terminal (the call is over and the state
    /// machine cannot advance further).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            CallStatus::Completed | CallStatus::Failed | CallStatus::NoAnswer | CallStatus::Busy
        )
    }
}

/// The consent state for recording one call.
///
/// `Unknown` is the default — silence is not consent, and the domain
/// rejects recording unless consent was explicitly granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecordingConsent {
    /// The called party consented to recording.
    Granted,
    /// The called party declined recording.
    Denied,
    /// No consent was captured.
    Unknown,
}

/// The recording attached to a [`Call`].
///
/// Consent-gated: a recording can only start after
/// [`RecordingConsent::Granted`] was recorded on the call. The audio
/// itself is an [`AssetId`](crate::ids::AssetId) reference — the
/// bytes live in the asset store, never inline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecording {
    /// Consent state for this recording.
    pub consent: RecordingConsent,
    /// When recording started (`None` until it does).
    pub started_at: Option<DateTime<Utc>>,
    /// Whether recording is still running.
    pub active: bool,
    /// The audio asset, once recording has stopped.
    pub asset_id: Option<AssetId>,
}

/// A call: who, which way, and where it is in the lifecycle.
///
/// Created via [`Call::initiate`]; the status machine only moves
/// forward and terminal statuses cannot be left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    /// Stable identifier of this call.
    pub call_id: CallId,
    /// The client profile this call belongs to (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// The contact on the other end, when known (inbound calls may
    /// not match a contact).
    pub contact_id: Option<ContactId>,
    /// Which way the call goes.
    pub direction: CallDirection,
    /// The dialing party.
    pub from: PhoneNumber,
    /// The dialed party.
    pub to: PhoneNumber,
    /// Current lifecycle status.
    pub status: CallStatus,
    /// When the call was initiated.
    pub started_at: DateTime<Utc>,
    /// When the call reached a terminal status.
    pub ended_at: Option<DateTime<Utc>>,
    /// Seconds the parties were connected (set on completion).
    pub duration_secs: Option<u64>,
    /// Why the call failed, when [`CallStatus::Failed`].
    pub failure_reason: Option<String>,
    /// Opaque provider-side reference (e.g. the Telnyx call id).
    pub provider_call_id: Option<String>,
    /// The recording attached to this call, if any.
    pub recording: Option<CallRecording>,
}

impl Call {
    /// Initiates a call: creates it in [`CallStatus::Initiated`].
    ///
    /// ```
    /// use crm_domain::ids::{ClientProfileId, ContactId};
    /// use crm_domain::voice::{Call, CallDirection, CallStatus, PhoneNumber};
    ///
    /// let call = Call::initiate(
    ///     ClientProfileId::new(),
    ///     Some(ContactId::new()),
    ///     CallDirection::Outbound,
    ///     PhoneNumber::new("+14155550100".to_string()).expect("valid"),
    ///     PhoneNumber::new("+14155550199".to_string()).expect("valid"),
    /// )
    /// .expect("valid call");
    /// assert_eq!(call.status, CallStatus::Initiated);
    /// ```
    pub fn initiate(
        client_profile_id: ClientProfileId,
        contact_id: Option<ContactId>,
        direction: CallDirection,
        from: PhoneNumber,
        to: PhoneNumber,
    ) -> Result<Self, DomainError> {
        Ok(Call {
            call_id: CallId::new(),
            client_profile_id,
            contact_id,
            direction,
            from,
            to,
            status: CallStatus::Initiated,
            started_at: Utc::now(),
            ended_at: None,
            duration_secs: None,
            failure_reason: None,
            provider_call_id: None,
            recording: None,
        })
    }

    /// Records the opaque provider-side call reference.
    pub fn set_provider_call_id(&mut self, provider_call_id: String) {
        self.provider_call_id = Some(provider_call_id);
    }

    /// `Initiated → Ringing`: the far end is ringing.
    pub fn ring(&mut self) -> Result<(), DomainError> {
        self.require_status(CallStatus::Initiated, "ring")?;
        self.status = CallStatus::Ringing;
        Ok(())
    }

    /// `Ringing → InProgress`: the far end answered.
    pub fn answer(&mut self) -> Result<(), DomainError> {
        self.require_status(CallStatus::Ringing, "answer")?;
        self.status = CallStatus::InProgress;
        Ok(())
    }

    /// `InProgress → Completed`: the call ended normally.
    ///
    /// Sets `ended_at` and computes `duration_secs` from
    /// `started_at` (clamped at zero — clocks can skew).
    pub fn complete(&mut self) -> Result<(), DomainError> {
        self.require_status(CallStatus::InProgress, "complete")?;
        // A recording left running is stopped implicitly: the audio
        // asset is finalized by the provider, referenced later.
        if let Some(recording) = self.recording.as_mut() {
            recording.active = false;
        }
        let ended_at = Utc::now();
        let duration_secs = ended_at
            .signed_duration_since(self.started_at)
            .num_seconds()
            .max(0) as u64;
        self.ended_at = Some(ended_at);
        self.duration_secs = Some(duration_secs);
        self.status = CallStatus::Completed;
        Ok(())
    }

    /// `Initiated | Ringing → Failed`: the call failed.
    ///
    /// Rejects: a blank reason (the log must say what happened).
    pub fn fail(&mut self, reason: String) -> Result<(), DomainError> {
        if self.status != CallStatus::Initiated && self.status != CallStatus::Ringing {
            return Err(DomainError::Validation(format!(
                "cannot fail a call in status {:?}",
                self.status
            )));
        }
        if reason.trim().is_empty() {
            return Err(DomainError::Validation(
                "call failure reason must not be blank".to_string(),
            ));
        }
        self.failure_reason = Some(reason.trim().to_string());
        self.ended_at = Some(Utc::now());
        self.status = CallStatus::Failed;
        Ok(())
    }

    /// `Ringing → NoAnswer`: the far end never answered.
    pub fn no_answer(&mut self) -> Result<(), DomainError> {
        self.require_status(CallStatus::Ringing, "mark no-answer")?;
        self.ended_at = Some(Utc::now());
        self.status = CallStatus::NoAnswer;
        Ok(())
    }

    /// `Ringing → Busy`: the far end was busy.
    pub fn busy(&mut self) -> Result<(), DomainError> {
        self.require_status(CallStatus::Ringing, "mark busy")?;
        self.ended_at = Some(Utc::now());
        self.status = CallStatus::Busy;
        Ok(())
    }

    /// Records the called party's recording consent.
    ///
    /// Consent can be captured while the call is active
    /// (`Initiated`/`Ringing`/`InProgress`) — typically at the start
    /// of the call. It cannot be changed once the call is over.
    pub fn set_recording_consent(&mut self, consent: RecordingConsent) -> Result<(), DomainError> {
        if self.status.is_terminal() {
            return Err(DomainError::Validation(
                "cannot record consent on a finished call".to_string(),
            ));
        }
        match self.recording.as_mut() {
            Some(recording) => {
                // Once a recording has started, its consent is pinned:
                // changing it afterwards would rewrite the legal basis.
                // Before any recording started, consent may still be
                // updated (e.g. the party was asked again mid-call).
                if recording.started_at.is_some() && recording.consent != consent {
                    return Err(DomainError::Validation(
                        "cannot change recording consent after recording started".to_string(),
                    ));
                }
                recording.consent = consent;
            }
            None => {
                self.recording = Some(CallRecording {
                    consent,
                    started_at: None,
                    active: false,
                    asset_id: None,
                });
            }
        }
        Ok(())
    }

    /// Starts recording: requires [`RecordingConsent::Granted`] and an
    /// [`CallStatus::InProgress`] call.
    pub fn start_recording(&mut self) -> Result<(), DomainError> {
        let recording = self.recording.as_mut().ok_or_else(|| {
            DomainError::Validation("recording consent was not captured".to_string())
        })?;
        if recording.consent != RecordingConsent::Granted {
            return Err(DomainError::Validation(
                "call recording requires explicit consent".to_string(),
            ));
        }
        if self.status != CallStatus::InProgress {
            return Err(DomainError::Validation(
                "recording can only start on a connected call".to_string(),
            ));
        }
        if recording.active {
            return Err(DomainError::Validation(
                "recording is already active".to_string(),
            ));
        }
        recording.started_at = Some(Utc::now());
        recording.active = true;
        Ok(())
    }

    /// Stops recording, attaching the audio asset.
    ///
    /// Rejects: no active recording.
    pub fn stop_recording(&mut self, asset_id: AssetId) -> Result<(), DomainError> {
        let recording = self
            .recording
            .as_mut()
            .ok_or_else(|| DomainError::Validation("no recording on this call".to_string()))?;
        if !recording.active {
            return Err(DomainError::Validation(
                "no active recording to stop".to_string(),
            ));
        }
        recording.active = false;
        recording.asset_id = Some(asset_id);
        Ok(())
    }

    /// Builds the immutable [`CallLog`] for a finished call.
    ///
    /// Rejects: non-terminal calls (the log is the record of a
    /// finished call).
    pub fn to_log(&self) -> Result<CallLog, DomainError> {
        if !self.status.is_terminal() {
            return Err(DomainError::Validation(
                "call log can only be built for a finished call".to_string(),
            ));
        }
        Ok(CallLog {
            call_id: self.call_id,
            client_profile_id: self.client_profile_id,
            contact_id: self.contact_id,
            direction: self.direction,
            from: self.from.clone(),
            to: self.to.clone(),
            status: self.status,
            started_at: self.started_at,
            ended_at: self.ended_at,
            duration_secs: self.duration_secs.unwrap_or(0),
            failure_reason: self.failure_reason.clone(),
            recording_asset_id: self
                .recording
                .as_ref()
                .and_then(|recording| recording.asset_id),
        })
    }

    /// Rejects the transition unless the call is in `expected`.
    fn require_status(&self, expected: CallStatus, transition: &str) -> Result<(), DomainError> {
        if self.status != expected {
            return Err(DomainError::Validation(format!(
                "cannot {transition} a call in status {:?}",
                self.status
            )));
        }
        Ok(())
    }
}

/// The immutable record of a finished call.
///
/// Built via [`Call::to_log`] — there are no mutators, so a log is a
/// write-once fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallLog {
    /// The call this log describes.
    pub call_id: CallId,
    /// The client profile this call belongs to (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// The contact on the other end, when known.
    pub contact_id: Option<ContactId>,
    /// Which way the call went.
    pub direction: CallDirection,
    /// The dialing party.
    pub from: PhoneNumber,
    /// The dialed party.
    pub to: PhoneNumber,
    /// Terminal status of the call.
    pub status: CallStatus,
    /// When the call was initiated.
    pub started_at: DateTime<Utc>,
    /// When the call reached its terminal status.
    pub ended_at: Option<DateTime<Utc>>,
    /// Seconds the parties were connected (0 for unanswered calls).
    pub duration_secs: u64,
    /// Why the call failed, when [`CallStatus::Failed`].
    pub failure_reason: Option<String>,
    /// The recording's audio asset, when a recording was made.
    pub recording_asset_id: Option<AssetId>,
}

/// The telephony provider port.
///
/// Provider-agnostic by design: the service talks to this trait and
/// never to a concrete API. The Telnyx adapter is a later
/// workstream's concern; a stub provider serves tests and the local
/// seed until then. The provider reference is an opaque string —
/// the domain never parses it.
pub trait VoiceProvider {
    /// Dials `to`, returning the provider-side call reference.
    fn initiate_call<'a>(
        &'a self,
        to: &'a PhoneNumber,
    ) -> impl Future<Output = Result<String, DomainError>> + Send + 'a;

    /// Hangs up the provider-side call (idempotent).
    fn hangup<'a>(
        &'a self,
        provider_call_id: &'a str,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a;

    /// Reads the provider's view of the call's status.
    fn call_status<'a>(
        &'a self,
        provider_call_id: &'a str,
    ) -> impl Future<Output = Result<CallStatus, DomainError>> + Send + 'a;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(raw: &str) -> PhoneNumber {
        PhoneNumber::new(raw.to_string()).expect("valid phone number")
    }

    fn outbound_call() -> Call {
        Call::initiate(
            ClientProfileId::new(),
            Some(ContactId::new()),
            CallDirection::Outbound,
            number("+14155550100"),
            number("+14155550199"),
        )
        .expect("valid call")
    }

    #[test]
    fn phone_number_rejects_invalid() {
        for bad in [
            "",
            "   ",
            "not-a-number",
            "+1",
            "+1234567890123456",
            "555-0199",
        ] {
            assert!(
                PhoneNumber::new(bad.to_string()).is_err(),
                "{bad:?} must be rejected"
            );
        }
        assert!(PhoneNumber::new("+14155550199".to_string()).is_ok());
        assert!(PhoneNumber::new("4155550199".to_string()).is_ok());
    }

    #[test]
    fn phone_number_deserialize_rejects_invalid() {
        let bad: Result<PhoneNumber, _> = serde_json::from_str(r#"{"value": "abc"}"#);
        assert!(bad.is_err());
        let ok: PhoneNumber =
            serde_json::from_str(r#"{"value": "+14155550199"}"#).expect("valid phone JSON");
        assert_eq!(ok.as_str(), "+14155550199");
    }

    #[test]
    fn call_lifecycle_advances_forward_only() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        assert_eq!(call.status, CallStatus::Ringing);
        // Cannot answer before ringing, cannot complete before answering.
        let mut fresh = outbound_call();
        assert!(fresh.answer().is_err());
        assert!(fresh.complete().is_err());
        fresh.ring().expect("ring");
        fresh.answer().expect("answer");
        assert_eq!(fresh.status, CallStatus::InProgress);
        assert!(fresh.ring().is_err());
        fresh.complete().expect("complete");
        assert_eq!(call.status, CallStatus::Ringing);
        assert_eq!(fresh.status, CallStatus::Completed);
        assert!(fresh.ended_at.is_some());
        // Terminal: nothing moves afterwards.
        assert!(fresh.answer().is_err());
        assert!(fresh.complete().is_err());
        assert!(fresh.fail("x".to_string()).is_err());
    }

    #[test]
    fn call_fail_requires_a_reason() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        assert!(call.fail("  ".to_string()).is_err());
        call.fail("provider timeout".to_string()).expect("fail");
        assert_eq!(call.status, CallStatus::Failed);
        assert_eq!(call.failure_reason.as_deref(), Some("provider timeout"));
    }

    #[test]
    fn no_answer_and_busy_are_terminal() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        call.no_answer().expect("no answer");
        assert_eq!(call.status, CallStatus::NoAnswer);
        assert!(call.status.is_terminal());
        assert!(call.answer().is_err());

        let mut call = outbound_call();
        call.ring().expect("ring");
        call.busy().expect("busy");
        assert_eq!(call.status, CallStatus::Busy);
        assert!(call.status.is_terminal());
    }

    #[test]
    fn recording_requires_granted_consent() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        call.answer().expect("answer");
        // No consent captured at all.
        assert!(call.start_recording().is_err());
        // Unknown consent does not authorize recording.
        call.set_recording_consent(RecordingConsent::Unknown)
            .expect("set consent");
        assert!(call.start_recording().is_err());
        // Denied consent does not authorize recording.
        call.set_recording_consent(RecordingConsent::Denied)
            .expect("set consent");
        assert!(call.start_recording().is_err());
        // Granted consent authorizes recording.
        call.set_recording_consent(RecordingConsent::Granted)
            .expect("set consent");
        call.start_recording().expect("start recording");
        assert!(call.start_recording().is_err()); // already active
        call.stop_recording(AssetId::new()).expect("stop recording");
        let recording = call.recording.as_ref().expect("recording");
        assert!(!recording.active);
        assert!(recording.asset_id.is_some());
    }

    #[test]
    fn consent_may_be_updated_before_recording_starts() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        call.answer().expect("answer");
        call.set_recording_consent(RecordingConsent::Unknown)
            .expect("set consent");
        // The party was asked again mid-call and granted: allowed,
        // because no recording has started yet.
        call.set_recording_consent(RecordingConsent::Granted)
            .expect("consent update is allowed");
        call.start_recording().expect("start recording");
    }

    #[test]
    fn recording_requires_a_connected_call() {
        let mut call = outbound_call();
        call.set_recording_consent(RecordingConsent::Granted)
            .expect("set consent");
        // Not answered yet — recording is rejected.
        assert!(call.start_recording().is_err());
    }

    #[test]
    fn consent_cannot_change_after_recording_started() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        call.answer().expect("answer");
        call.set_recording_consent(RecordingConsent::Granted)
            .expect("set consent");
        call.start_recording().expect("start recording");
        assert!(call
            .set_recording_consent(RecordingConsent::Denied)
            .is_err());
    }

    #[test]
    fn consent_cannot_be_set_on_a_finished_call() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        call.no_answer().expect("no answer");
        assert!(call
            .set_recording_consent(RecordingConsent::Granted)
            .is_err());
    }

    #[test]
    fn call_log_requires_a_finished_call() {
        let call = outbound_call();
        assert!(call.to_log().is_err());
        let mut call = outbound_call();
        call.ring().expect("ring");
        call.answer().expect("answer");
        call.complete().expect("complete");
        let log = call.to_log().expect("log");
        assert_eq!(log.status, CallStatus::Completed);
        assert_eq!(log.direction, CallDirection::Outbound);
        assert!(log.contact_id.is_some());
    }

    #[test]
    fn call_log_carries_the_recording_asset() {
        let mut call = outbound_call();
        call.ring().expect("ring");
        call.answer().expect("answer");
        call.set_recording_consent(RecordingConsent::Granted)
            .expect("set consent");
        call.start_recording().expect("start recording");
        let asset = AssetId::new();
        call.stop_recording(asset).expect("stop recording");
        call.complete().expect("complete");
        let log = call.to_log().expect("log");
        assert_eq!(log.recording_asset_id, Some(asset));
    }

    #[test]
    fn status_names_are_stable() {
        assert_eq!(CallStatus::Initiated.as_str(), "initiated");
        assert_eq!(CallStatus::Ringing.as_str(), "ringing");
        assert_eq!(CallStatus::InProgress.as_str(), "in_progress");
        assert_eq!(CallStatus::Completed.as_str(), "completed");
        assert_eq!(CallStatus::Failed.as_str(), "failed");
        assert_eq!(CallStatus::NoAnswer.as_str(), "no_answer");
        assert_eq!(CallStatus::Busy.as_str(), "busy");
        assert_eq!(CallDirection::Outbound.as_str(), "outbound");
        assert_eq!(CallDirection::Inbound.as_str(), "inbound");
    }
}
