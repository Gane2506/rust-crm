// SPDX-License-Identifier: GPL-3.0-only
//! Campaign aggregate: owner context, channel, schedule, delivery state,
//! steps, prospect pools, opt-outs, attribution and conversions.
//!
//! A campaign is owned either by the agency itself or by one client
//! profile ([`OwnerContext`]). The `schedule` field holds a cron
//! expression and is validated to be non-empty at construction; full cron
//! parsing/validation belongs to the scheduling layer. The `schedule_for`
//! field holds a one-shot launch time used by [`Campaign::schedule`].
//!
//! ## Lifecycle
//!
//! [`DeliveryState`] is forward-only:
//! `Draft → Scheduled → Running → (Paused ⇄ Running) → Completed`, with
//! [`DeliveryState::Cancelled`] reachable from any non-terminal state and
//! [`DeliveryState::Completed`] terminal. Transitions are enforced by the
//! methods on [`Campaign`]; illegal moves fail with
//! [`DomainError::Validation`].
//!
//! ## Opt-outs
//!
//! [`OptOut`] records are immutable: the type exposes no update or delete
//! API. Compliance requires opt-outs to be append-only — an opt-out is
//! never removed, only superseded by a re-consent, which is a separate
//! event recorded elsewhere (consent management is a later task).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{
    CampaignId, CampaignStepId, ClientProfileId, ContactId, ConversionId, OptOutId, ProspectPoolId,
};

/// Who owns a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OwnerContext {
    /// Owned by the agency itself (e.g. house prospecting).
    Agency,
    /// Owned by one client profile.
    ClientProfile(ClientProfileId),
}

/// Outreach channel of a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    /// Email outreach.
    Email,
    /// Phone-call outreach.
    Call,
    /// Mixed email + call sequences.
    Mixed,
}

/// Delivery state of a campaign.
///
/// Forward-only: `Draft → Scheduled → Running ⇄ Paused → Completed`,
/// with `Cancelled` reachable from any non-terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeliveryState {
    /// Created, not yet scheduled.
    Draft,
    /// Scheduled to start at [`Campaign::schedule_for`].
    Scheduled,
    /// Actively delivering.
    Running,
    /// Paused by an operator (durable state, resumable).
    Paused,
    /// Finished delivering. Terminal.
    Completed,
    /// Cancelled before completion. Terminal.
    Cancelled,
}

impl DeliveryState {
    /// Whether the state is terminal (no further transitions allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, DeliveryState::Completed | DeliveryState::Cancelled)
    }
}

/// An outreach campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    /// Stable identifier of this campaign.
    pub campaign_id: CampaignId,
    /// Human-readable name (non-blank, validated at construction).
    pub name: String,
    /// Who owns the campaign (agency or one client profile).
    pub owner_context: OwnerContext,
    /// Reference to the uploaded source file, if the pool came from one.
    pub source_file_ref: Option<String>,
    /// Outreach channel.
    pub channel: Channel,
    /// Identifier of the prospect pool this campaign draws from.
    pub prospect_pool_id: String,
    /// Cron expression for the delivery schedule (validated non-empty).
    pub schedule: String,
    /// One-shot launch time, set by [`Campaign::schedule`]. Distinct
    /// from the recurring cron `schedule` above.
    pub schedule_for: Option<DateTime<Utc>>,
    /// Suppression rules reference (opt-outs, do-not-contact lists).
    pub suppression_policy: String,
    /// Current delivery state.
    pub delivery_state: DeliveryState,
    /// How conversions are attributed to this campaign.
    pub conversion_rules: String,
    /// Days after a touch during which a conversion still attributes.
    pub attribution_window_days: u32,
    /// When the campaign was created.
    pub created_at: DateTime<Utc>,
    /// When the campaign was last modified.
    pub updated_at: DateTime<Utc>,
}

impl Campaign {
    /// Creates a campaign in [`DeliveryState::Draft`].
    ///
    /// Fails with [`DomainError::Validation`] when `name` is blank or
    /// `schedule` is empty: a campaign without a name cannot be managed,
    /// and a campaign without a schedule can never be delivered, so both
    /// are rejected up front rather than failing silently later.
    ///
    /// Takes every field explicitly (like a struct literal with
    /// validation); callers constructing campaigns programmatically
    /// should prefer this over a builder for now.
    ///
    /// ```
    /// use crm_domain::campaign::{Campaign, Channel, DeliveryState, OwnerContext};
    /// use crm_domain::ids::CampaignId;
    ///
    /// let campaign = Campaign::new(
    ///     CampaignId::new(),
    ///     "Q4 Outreach".to_string(),
    ///     OwnerContext::Agency,
    ///     Channel::Email,
    ///     "pool-1".to_string(),
    ///     "0 9 * * MON".to_string(),
    ///     "global-opt-outs".to_string(),
    ///     "first-touch".to_string(),
    ///     30,
    /// )
    /// .expect("valid campaign");
    /// assert_eq!(campaign.delivery_state, DeliveryState::Draft);
    /// assert_eq!(campaign.name, "Q4 Outreach");
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        campaign_id: CampaignId,
        name: String,
        owner_context: OwnerContext,
        channel: Channel,
        prospect_pool_id: String,
        schedule: String,
        suppression_policy: String,
        conversion_rules: String,
        attribution_window_days: u32,
    ) -> Result<Self, DomainError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "campaign name must not be blank".to_string(),
            ));
        }
        if schedule.trim().is_empty() {
            return Err(DomainError::Validation(
                "campaign schedule must be a non-empty cron expression".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            campaign_id,
            name,
            owner_context,
            source_file_ref: None,
            channel,
            prospect_pool_id,
            schedule,
            schedule_for: None,
            suppression_policy,
            delivery_state: DeliveryState::Draft,
            conversion_rules,
            attribution_window_days,
            created_at: now,
            updated_at: now,
        })
    }

    /// Schedules a draft campaign for a one-shot launch.
    ///
    /// `Draft → Scheduled`. The launch time must be in the future; a
    /// past or present time is rejected so campaigns cannot be scheduled
    /// into an ambiguous "should have started" state.
    pub fn schedule(&mut self, schedule_for: DateTime<Utc>) -> Result<(), DomainError> {
        if self.delivery_state != DeliveryState::Draft {
            return Err(DomainError::Validation(format!(
                "only a draft campaign can be scheduled (state: {:?})",
                self.delivery_state
            )));
        }
        if schedule_for <= Utc::now() {
            return Err(DomainError::Validation(
                "campaign schedule_for must be in the future".to_string(),
            ));
        }
        self.schedule_for = Some(schedule_for);
        self.delivery_state = DeliveryState::Scheduled;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Launches the campaign: `Draft → Running` or `Scheduled → Running`.
    ///
    /// A campaign that is already `Running` may be launched again as a
    /// resume: the transition is idempotent (`Running → Running`) so a
    /// re-launch after a crash continues delivery instead of erroring.
    /// The service layer skips recipients whose deterministic
    /// idempotency keys were already sent, so a resume never
    /// double-sends. The state machine stays forward-only: terminal
    /// states still reject launch.
    ///
    /// Requires at least one step — `step_count` is the number of
    /// [`CampaignStep`]s attached to the campaign, supplied by the
    /// service layer (the domain aggregate does not own the step list).
    /// A scheduled campaign whose launch time has passed launches
    /// immediately; the scheduler, not the domain, owns timeliness.
    pub fn launch(&mut self, step_count: usize) -> Result<(), DomainError> {
        if !matches!(
            self.delivery_state,
            DeliveryState::Draft | DeliveryState::Scheduled | DeliveryState::Running
        ) {
            return Err(DomainError::Validation(format!(
                "only a draft, scheduled, or running (resume) campaign can launch (state: {:?})",
                self.delivery_state
            )));
        }
        if step_count == 0 {
            return Err(DomainError::Validation(
                "campaign cannot launch without at least one step".to_string(),
            ));
        }
        self.delivery_state = DeliveryState::Running;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Pauses a running campaign: `Running → Paused`.
    pub fn pause(&mut self) -> Result<(), DomainError> {
        if self.delivery_state != DeliveryState::Running {
            return Err(DomainError::Validation(format!(
                "only a running campaign can be paused (state: {:?})",
                self.delivery_state
            )));
        }
        self.delivery_state = DeliveryState::Paused;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Resumes a paused campaign: `Paused → Running`.
    pub fn resume(&mut self) -> Result<(), DomainError> {
        if self.delivery_state != DeliveryState::Paused {
            return Err(DomainError::Validation(format!(
                "only a paused campaign can be resumed (state: {:?})",
                self.delivery_state
            )));
        }
        self.delivery_state = DeliveryState::Running;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Completes a running or paused campaign: `Running|Paused → Completed`.
    ///
    /// Terminal: a completed campaign cannot transition further.
    pub fn complete(&mut self) -> Result<(), DomainError> {
        if !matches!(
            self.delivery_state,
            DeliveryState::Running | DeliveryState::Paused
        ) {
            return Err(DomainError::Validation(format!(
                "only a running or paused campaign can complete (state: {:?})",
                self.delivery_state
            )));
        }
        self.delivery_state = DeliveryState::Completed;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Cancels the campaign from any non-terminal state.
    ///
    /// Terminal and idempotent: cancelling an already-cancelled
    /// campaign is a no-op. A completed campaign cannot be cancelled —
    /// its outcome is already recorded.
    pub fn cancel(&mut self) -> Result<(), DomainError> {
        if self.delivery_state == DeliveryState::Cancelled {
            return Ok(());
        }
        if self.delivery_state.is_terminal() {
            return Err(DomainError::Validation(format!(
                "campaign in terminal state {:?} cannot be cancelled",
                self.delivery_state
            )));
        }
        self.delivery_state = DeliveryState::Cancelled;
        self.updated_at = Utc::now();
        Ok(())
    }
}

/// Kind of a campaign step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StepKind {
    /// Send an email using a template.
    Email,
    /// Place a phone call using a script.
    Call,
    /// Wait before the next step.
    Wait,
}

/// One step in a campaign's outreach sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignStep {
    /// Stable identifier of this step.
    pub step_id: CampaignStepId,
    /// The campaign this step belongs to.
    pub campaign_id: CampaignId,
    /// What the step does.
    pub kind: StepKind,
    /// Position in the sequence (0-based). Uniqueness within a campaign
    /// is enforced by the service layer, which owns the step list.
    pub position: u32,
    /// Template (email) or script (call) reference. Required for
    /// [`StepKind::Email`] and [`StepKind::Call`].
    pub template_id: Option<String>,
    /// How long to wait. Required for [`StepKind::Wait`].
    pub wait_duration_secs: Option<u64>,
    /// When the step was created.
    pub created_at: DateTime<Utc>,
}

impl CampaignStep {
    /// Creates a campaign step, validating kind-specific requirements:
    /// email/call steps need a template (or script) reference, wait
    /// steps need a positive duration.
    pub fn new(
        campaign_id: CampaignId,
        kind: StepKind,
        position: u32,
        template_id: Option<String>,
        wait_duration_secs: Option<u64>,
    ) -> Result<Self, DomainError> {
        match kind {
            StepKind::Email | StepKind::Call => {
                let template = template_id
                    .map(|t| t.trim().to_owned())
                    .filter(|t| !t.is_empty());
                if template.is_none() {
                    return Err(DomainError::Validation(
                        "email and call steps require a template reference".to_string(),
                    ));
                }
                Ok(Self {
                    step_id: CampaignStepId::new(),
                    campaign_id,
                    kind,
                    position,
                    template_id: template,
                    wait_duration_secs: None,
                    created_at: Utc::now(),
                })
            }
            StepKind::Wait => match wait_duration_secs {
                Some(secs) if secs > 0 => Ok(Self {
                    step_id: CampaignStepId::new(),
                    campaign_id,
                    kind,
                    position,
                    template_id: None,
                    wait_duration_secs: Some(secs),
                    created_at: Utc::now(),
                }),
                _ => Err(DomainError::Validation(
                    "wait steps require a positive wait duration".to_string(),
                )),
            },
        }
    }
}

/// A named set of prospect contacts inside a client profile.
///
/// Pools are the audience a campaign draws from. Members are
/// [`ContactId`]s; membership is deduplicated on add. All members must
/// belong to the pool's client profile — the tenant boundary — which the
/// service layer enforces (the domain cannot see across profiles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProspectPool {
    /// Stable identifier of this pool.
    pub pool_id: ProspectPoolId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Human-readable name (non-blank, validated at construction).
    pub name: String,
    /// Member contacts, deduplicated.
    pub members: Vec<ContactId>,
    /// When the pool was created.
    pub created_at: DateTime<Utc>,
    /// When the pool was last modified.
    pub updated_at: DateTime<Utc>,
}

impl ProspectPool {
    /// Creates an empty prospect pool.
    pub fn new(client_profile_id: ClientProfileId, name: String) -> Result<Self, DomainError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "prospect pool name must not be blank".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            pool_id: ProspectPoolId::new(),
            client_profile_id,
            name,
            members: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Adds a contact to the pool. Deduplicated: adding a member that is
    /// already present is a no-op returning `false`.
    pub fn add_member(&mut self, contact_id: ContactId) -> bool {
        if self.members.contains(&contact_id) {
            return false;
        }
        self.members.push(contact_id);
        self.updated_at = Utc::now();
        true
    }

    /// Removes a contact from the pool. Returns `false` when the contact
    /// was not a member.
    pub fn remove_member(&mut self, contact_id: ContactId) -> bool {
        let before = self.members.len();
        self.members.retain(|id| *id != contact_id);
        if self.members.len() != before {
            self.updated_at = Utc::now();
            true
        } else {
            false
        }
    }
}

/// Channel an opt-out applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptOutChannel {
    /// Email outreach.
    Email,
    /// SMS outreach.
    Sms,
    /// Phone-call outreach.
    Call,
    /// Every channel.
    All,
}

/// A contact's opt-out from outreach on one channel.
///
/// Immutable by design: the type exposes no update or delete API.
/// Compliance requires opt-outs to be append-only — an opt-out is never
/// removed, only superseded by a re-consent, which is a separate event
/// (consent management is a later task). [`OptOut::covers`] answers
/// whether this record suppresses a given channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptOut {
    /// Stable identifier of this opt-out record.
    pub opt_out_id: OptOutId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// The contact who opted out.
    pub contact_id: ContactId,
    /// Which channel the opt-out covers.
    pub channel: OptOutChannel,
    /// Optional reason supplied by the contact.
    pub reason: Option<String>,
    /// When the opt-out was recorded.
    pub created_at: DateTime<Utc>,
}

impl OptOut {
    /// Records an opt-out. There are no mutators: once created, the
    /// record is final.
    pub fn new(
        client_profile_id: ClientProfileId,
        contact_id: ContactId,
        channel: OptOutChannel,
        reason: Option<String>,
    ) -> Self {
        Self {
            opt_out_id: OptOutId::new(),
            client_profile_id,
            contact_id,
            channel,
            reason: reason
                .map(|r| r.trim().to_owned())
                .filter(|r| !r.is_empty()),
            created_at: Utc::now(),
        }
    }

    /// Whether this opt-out suppresses outreach on `channel`. An
    /// [`OptOutChannel::All`] record covers every channel.
    #[must_use]
    pub fn covers(&self, channel: OptOutChannel) -> bool {
        self.channel == OptOutChannel::All || self.channel == channel
    }
}

/// How conversion credit is assigned across touchpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttributionModel {
    /// Credit goes to the first touch in the window.
    FirstTouch,
    /// Credit goes to the last touch in the window.
    LastTouch,
}

/// One outreach touchpoint that may receive conversion credit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touchpoint {
    /// The campaign that produced the touch.
    pub campaign_id: CampaignId,
    /// The step that produced the touch, if known.
    pub step_id: Option<CampaignStepId>,
    /// When the touch occurred.
    pub occurred_at: DateTime<Utc>,
}

/// Assigns conversion credit to one touchpoint.
///
/// Pure function over the candidate touchpoints (the caller applies the
/// campaign's attribution window before calling): [`AttributionModel::FirstTouch`]
/// picks the earliest touch, [`AttributionModel::LastTouch`] the latest.
/// Returns `None` when there are no candidates.
///
/// ```
/// use chrono::Utc;
/// use crm_domain::campaign::{attribute_conversion, AttributionModel, Touchpoint};
/// use crm_domain::ids::CampaignId;
///
/// let earlier = Touchpoint {
///     campaign_id: CampaignId::new(),
///     step_id: None,
///     occurred_at: Utc::now() - chrono::Duration::days(2),
/// };
/// let later = Touchpoint {
///     campaign_id: CampaignId::new(),
///     step_id: None,
///     occurred_at: Utc::now() - chrono::Duration::days(1),
/// };
/// let touches = vec![later.clone(), earlier.clone()];
/// assert_eq!(
///     attribute_conversion(&touches, AttributionModel::FirstTouch),
///     Some(&earlier)
/// );
/// assert_eq!(
///     attribute_conversion(&touches, AttributionModel::LastTouch),
///     Some(&later)
/// );
/// assert_eq!(attribute_conversion(&[], AttributionModel::FirstTouch), None);
/// ```
#[must_use]
pub fn attribute_conversion(
    touchpoints: &[Touchpoint],
    model: AttributionModel,
) -> Option<&Touchpoint> {
    match model {
        AttributionModel::FirstTouch => touchpoints.iter().min_by_key(|t| t.occurred_at),
        AttributionModel::LastTouch => touchpoints.iter().max_by_key(|t| t.occurred_at),
    }
}

/// A recorded conversion: a contact did the thing the campaign wanted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversion {
    /// Stable identifier of this conversion.
    pub conversion_id: ConversionId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// The contact who converted.
    pub contact_id: ContactId,
    /// The campaign credited, if attribution resolved one.
    pub campaign_id: Option<CampaignId>,
    /// Monetary value in cents, if known. Non-negative by construction
    /// (`u64`).
    pub value_cents: Option<u64>,
    /// When the conversion happened.
    pub occurred_at: DateTime<Utc>,
    /// When the record was created.
    pub created_at: DateTime<Utc>,
}

impl Conversion {
    /// Records a conversion. `value_cents` is non-negative by type;
    /// `campaign_id` is `None` when attribution could not resolve a
    /// campaign (e.g. no touchpoints in the window).
    pub fn new(
        client_profile_id: ClientProfileId,
        contact_id: ContactId,
        campaign_id: Option<CampaignId>,
        value_cents: Option<u64>,
        occurred_at: DateTime<Utc>,
    ) -> Self {
        Self {
            conversion_id: ConversionId::new(),
            client_profile_id,
            contact_id,
            campaign_id,
            value_cents,
            occurred_at,
            created_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_campaign(schedule: &str) -> Result<Campaign, DomainError> {
        Campaign::new(
            CampaignId::new(),
            "Q4 Outreach".to_string(),
            OwnerContext::Agency,
            Channel::Mixed,
            "pool-1".to_string(),
            schedule.to_string(),
            "global-opt-outs".to_string(),
            "first-touch".to_string(),
            30,
        )
    }

    #[test]
    fn new_rejects_blank_name() {
        let err = Campaign::new(
            CampaignId::new(),
            "   ".to_string(),
            OwnerContext::Agency,
            Channel::Email,
            "pool-1".to_string(),
            "0 9 * * *".to_string(),
            "policy".to_string(),
            "rules".to_string(),
            30,
        )
        .expect_err("blank name must fail");
        assert!(err.to_string().contains("campaign name must not be blank"));
    }

    #[test]
    fn new_rejects_empty_or_blank_schedule() {
        for schedule in ["", "   ", "\t\n"] {
            let err = Campaign::new(
                CampaignId::new(),
                "Name".to_string(),
                OwnerContext::Agency,
                Channel::Mixed,
                "pool-1".to_string(),
                schedule.to_string(),
                "global-opt-outs".to_string(),
                "first-touch".to_string(),
                30,
            )
            .expect_err("empty schedule must fail");
            assert_eq!(
                err.to_string(),
                "validation failed: campaign schedule must be a non-empty cron expression"
            );
        }
    }

    #[test]
    fn new_accepts_valid_schedule_and_starts_draft() {
        let campaign = new_campaign("0 9 * * MON").expect("valid schedule");
        assert_eq!(campaign.delivery_state, DeliveryState::Draft);
        assert_eq!(campaign.schedule, "0 9 * * MON");
        assert_eq!(campaign.attribution_window_days, 30);
        assert_eq!(campaign.name, "Q4 Outreach");
        assert!(campaign.schedule_for.is_none());
    }

    #[test]
    fn client_profile_owner_context_round_trips() {
        let mut campaign = new_campaign("0 9 * * *").expect("valid schedule");
        campaign.owner_context =
            OwnerContext::ClientProfile(ClientProfileId::from(uuid::Uuid::from_u128(9)));
        let json = serde_json::to_string(&campaign).expect("serialize");
        let back: Campaign = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(campaign, back);
    }

    #[test]
    fn schedule_requires_future_time() {
        let mut campaign = new_campaign("0 9 * * *").expect("valid schedule");
        let past = Utc::now() - chrono::Duration::hours(1);
        assert!(campaign.schedule(past).is_err());
        assert_eq!(campaign.delivery_state, DeliveryState::Draft);

        let future = Utc::now() + chrono::Duration::hours(1);
        campaign.schedule(future).expect("future schedule");
        assert_eq!(campaign.delivery_state, DeliveryState::Scheduled);
        assert_eq!(campaign.schedule_for, Some(future));
        // Scheduling twice is rejected: no longer a draft.
        assert!(campaign.schedule(future).is_err());
    }

    #[test]
    fn lifecycle_advances_forward_only() {
        let mut campaign = new_campaign("0 9 * * *").expect("valid schedule");
        // pause/resume/complete/cancel-targets rejected from Draft.
        assert!(campaign.pause().is_err());
        assert!(campaign.resume().is_err());
        assert!(campaign.complete().is_err());

        // Draft -> Running requires at least one step.
        assert!(campaign.launch(0).is_err());
        campaign.launch(2).expect("launch with steps");
        assert_eq!(campaign.delivery_state, DeliveryState::Running);
        // Re-launching a Running campaign is a resume (idempotent),
        // not an error — the service layer skips already-sent recipients.
        campaign.launch(2).expect("re-launch resumes");
        assert_eq!(campaign.delivery_state, DeliveryState::Running);

        campaign.pause().expect("pause");
        assert_eq!(campaign.delivery_state, DeliveryState::Paused);
        // Paused cannot pause again; resume works.
        assert!(campaign.pause().is_err());
        campaign.resume().expect("resume");
        assert_eq!(campaign.delivery_state, DeliveryState::Running);

        campaign.complete().expect("complete");
        assert_eq!(campaign.delivery_state, DeliveryState::Completed);
        assert!(campaign.delivery_state.is_terminal());
        // Terminal: nothing further.
        assert!(campaign.pause().is_err());
        assert!(campaign.resume().is_err());
        assert!(campaign.complete().is_err());
        assert!(campaign.cancel().is_err());
    }

    #[test]
    fn cancel_reaches_from_any_non_terminal() {
        for setup in [
            DeliveryState::Draft,
            DeliveryState::Scheduled,
            DeliveryState::Running,
            DeliveryState::Paused,
        ] {
            let mut campaign = new_campaign("0 9 * * *").expect("valid schedule");
            match setup {
                DeliveryState::Draft => {}
                DeliveryState::Scheduled => campaign
                    .schedule(Utc::now() + chrono::Duration::hours(1))
                    .expect("schedule"),
                DeliveryState::Running => campaign.launch(1).expect("launch"),
                DeliveryState::Paused => {
                    campaign.launch(1).expect("launch");
                    campaign.pause().expect("pause");
                }
                _ => unreachable!("test setup"),
            }
            campaign.cancel().expect("cancel from non-terminal");
            assert_eq!(campaign.delivery_state, DeliveryState::Cancelled);
            // Idempotent.
            campaign.cancel().expect("cancel is idempotent");
        }
    }

    #[test]
    fn step_kind_validation() {
        let campaign_id = CampaignId::new();
        // Email needs a template.
        assert!(CampaignStep::new(campaign_id, StepKind::Email, 0, None, None).is_err());
        assert!(CampaignStep::new(
            campaign_id,
            StepKind::Email,
            0,
            Some("  ".to_string()),
            None
        )
        .is_err());
        let email = CampaignStep::new(
            campaign_id,
            StepKind::Email,
            0,
            Some("welcome-v3".to_string()),
            None,
        )
        .expect("email step");
        assert_eq!(email.kind, StepKind::Email);

        // Call needs a script reference.
        assert!(CampaignStep::new(campaign_id, StepKind::Call, 1, None, None).is_err());
        CampaignStep::new(
            campaign_id,
            StepKind::Call,
            1,
            Some("discovery-script".to_string()),
            None,
        )
        .expect("call step");

        // Wait needs a positive duration.
        assert!(CampaignStep::new(campaign_id, StepKind::Wait, 2, None, None).is_err());
        assert!(CampaignStep::new(campaign_id, StepKind::Wait, 2, None, Some(0)).is_err());
        let wait = CampaignStep::new(campaign_id, StepKind::Wait, 2, None, Some(86_400))
            .expect("wait step");
        assert_eq!(wait.wait_duration_secs, Some(86_400));
    }

    #[test]
    fn prospect_pool_dedupes_members() {
        let mut pool = ProspectPool::new(ClientProfileId::new(), "Q4 Prospects".to_string())
            .expect("valid pool");
        let a = ContactId::new();
        let b = ContactId::new();
        assert!(pool.add_member(a));
        assert!(!pool.add_member(a));
        assert!(pool.add_member(b));
        assert_eq!(pool.members.len(), 2);
        assert!(pool.remove_member(a));
        assert!(!pool.remove_member(a));
        assert_eq!(pool.members, vec![b]);
    }

    #[test]
    fn prospect_pool_rejects_blank_name() {
        assert!(ProspectPool::new(ClientProfileId::new(), "  ".to_string()).is_err());
    }

    #[test]
    fn opt_out_covers_channels() {
        let email_only = OptOut::new(
            ClientProfileId::new(),
            ContactId::new(),
            OptOutChannel::Email,
            Some("too many emails".to_string()),
        );
        assert!(email_only.covers(OptOutChannel::Email));
        assert!(!email_only.covers(OptOutChannel::Sms));
        assert!(!email_only.covers(OptOutChannel::Call));

        let all = OptOut::new(
            ClientProfileId::new(),
            ContactId::new(),
            OptOutChannel::All,
            None,
        );
        assert!(all.covers(OptOutChannel::Email));
        assert!(all.covers(OptOutChannel::Sms));
        assert!(all.covers(OptOutChannel::Call));
        // Immutability: the struct exposes no mutators — every field is
        // readable but the only constructor is `new`. (Compile-time
        // property; this test pins the public surface.)
        let json = serde_json::to_string(&all).expect("serialize");
        let back: OptOut = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(all, back);
    }

    #[test]
    fn attribution_first_and_last_touch() {
        let first = Touchpoint {
            campaign_id: CampaignId::new(),
            step_id: None,
            occurred_at: Utc::now() - chrono::Duration::days(3),
        };
        let middle = Touchpoint {
            campaign_id: CampaignId::new(),
            step_id: None,
            occurred_at: Utc::now() - chrono::Duration::days(2),
        };
        let last = Touchpoint {
            campaign_id: CampaignId::new(),
            step_id: None,
            occurred_at: Utc::now() - chrono::Duration::days(1),
        };
        let touches = vec![middle.clone(), last.clone(), first.clone()];
        assert_eq!(
            attribute_conversion(&touches, AttributionModel::FirstTouch),
            Some(&first)
        );
        assert_eq!(
            attribute_conversion(&touches, AttributionModel::LastTouch),
            Some(&last)
        );
        assert_eq!(
            attribute_conversion(&[], AttributionModel::FirstTouch),
            None
        );
    }

    #[test]
    fn conversion_records_value() {
        let conversion = Conversion::new(
            ClientProfileId::new(),
            ContactId::new(),
            Some(CampaignId::new()),
            Some(49_99),
            Utc::now(),
        );
        assert_eq!(conversion.value_cents, Some(49_99));
        let unattributed = Conversion::new(
            ClientProfileId::new(),
            ContactId::new(),
            None,
            None,
            Utc::now(),
        );
        assert!(unattributed.campaign_id.is_none());
    }
}
