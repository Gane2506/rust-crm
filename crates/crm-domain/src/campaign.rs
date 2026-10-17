// SPDX-License-Identifier: GPL-3.0-only
//! Campaign aggregate: owner context, channel, schedule and delivery state.
//!
//! A campaign is owned either by the agency itself or by one client
//! profile ([`OwnerContext`]). The `schedule` field holds a cron
//! expression and is validated to be non-empty at construction; full cron
//! parsing/validation belongs to the scheduling layer.

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{CampaignId, ClientProfileId};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeliveryState {
    /// Created, not yet scheduled.
    Draft,
    /// Scheduled to start.
    Scheduled,
    /// Actively delivering.
    Running,
    /// Paused by an operator (durable state, resumable).
    Paused,
    /// Finished delivering.
    Completed,
    /// Cancelled before completion.
    Cancelled,
}

/// An outreach campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    /// Stable identifier of this campaign.
    pub campaign_id: CampaignId,
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
    /// Suppression rules reference (opt-outs, do-not-contact lists).
    pub suppression_policy: String,
    /// Current delivery state.
    pub delivery_state: DeliveryState,
    /// How conversions are attributed to this campaign.
    pub conversion_rules: String,
    /// Days after a touch during which a conversion still attributes.
    pub attribution_window_days: u32,
}

impl Campaign {
    /// Creates a campaign in [`DeliveryState::Draft`].
    ///
    /// Fails with [`DomainError::Validation`] when `schedule` is empty or
    /// blank: a campaign without a schedule can never be delivered, so it
    /// is rejected up front rather than failing silently later.
    ///
    /// Takes every field explicitly (like a struct literal with
    /// validation); callers constructing campaigns programmatically
    /// should prefer this over a builder for now.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        campaign_id: CampaignId,
        owner_context: OwnerContext,
        channel: Channel,
        prospect_pool_id: String,
        schedule: String,
        suppression_policy: String,
        conversion_rules: String,
        attribution_window_days: u32,
    ) -> Result<Self, DomainError> {
        if schedule.trim().is_empty() {
            return Err(DomainError::Validation(
                "campaign schedule must be a non-empty cron expression".to_string(),
            ));
        }
        Ok(Self {
            campaign_id,
            owner_context,
            source_file_ref: None,
            channel,
            prospect_pool_id,
            schedule,
            suppression_policy,
            delivery_state: DeliveryState::Draft,
            conversion_rules,
            attribution_window_days,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_campaign(schedule: &str) -> Result<Campaign, DomainError> {
        Campaign::new(
            CampaignId::new(),
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
    fn new_rejects_empty_or_blank_schedule() {
        for schedule in ["", "   ", "\t\n"] {
            let err = new_campaign(schedule).expect_err("empty schedule must fail");
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
}
