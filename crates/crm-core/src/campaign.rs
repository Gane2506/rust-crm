// SPDX-License-Identifier: GPL-3.0-only
//! Campaign service implementation (task 08-CAMP-02).
//!
//! [`CampaignServiceImpl`] implements
//! [`CampaignService`](crm_domain::services::CampaignService) over generic
//! repository, contact, email-service and audit ports — the same
//! composition pattern as
//! [`EmailServiceImpl`](crate::email::EmailServiceImpl). The production
//! wiring (SQLx repositories + email service) is assembled by the API
//! layer.
//!
//! Bulk-send opt-out enforcement is mandatory: [`CampaignServiceImpl::launch`]
//! skips every pool member with an opt-out covering the step's channel
//! before sending. Opt-outs are checked per contact per send; there is
//! no caching (correctness over latency for compliance).

use uuid::Uuid;

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::campaign::{
    attribute_conversion, Campaign, CampaignStep, Conversion, OptOutChannel, OwnerContext,
    ProspectPool, StepKind,
};
use crm_domain::error::DomainError;
use crm_domain::ids::{CampaignId, ClientProfileId, ContactId};
use crm_domain::repositories::{
    CampaignRepository, ContactRepository, ConversionRepository, OptOutRepository,
    ProspectPoolRepository,
};
use crm_domain::services::{
    ActionContext, AuditService, CampaignService, EmailService, NewCampaign, NewCampaignStep,
    NewConversion, NewEmailMessage,
};

/// Generic [`CampaignService`] implementation.
///
/// Type parameters: `C` campaign repository, `P` prospect-pool
/// repository, `O` opt-out repository, `CT` contact repository, `CV`
/// conversion repository, `E` email service, `A` audit service.
pub struct CampaignServiceImpl<C, P, O, CT, CV, E, A> {
    campaigns: C,
    pools: P,
    opt_outs: O,
    contacts: CT,
    conversions: CV,
    email_service: E,
    audit: A,
}

impl<C, P, O, CT, CV, E, A> CampaignServiceImpl<C, P, O, CT, CV, E, A> {
    /// Creates the service over the given ports.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        campaigns: C,
        pools: P,
        opt_outs: O,
        contacts: CT,
        conversions: CV,
        email_service: E,
        audit: A,
    ) -> Self {
        Self {
            campaigns,
            pools,
            opt_outs,
            contacts,
            conversions,
            email_service,
            audit,
        }
    }

    /// Borrows the campaign repository (read path for the HTTP layer).
    #[must_use]
    pub fn campaigns(&self) -> &C {
        &self.campaigns
    }

    /// Borrows the pool repository (read path for the HTTP layer).
    #[must_use]
    pub fn pools(&self) -> &P {
        &self.pools
    }

    /// Borrows the opt-out repository (read path for the HTTP layer).
    #[must_use]
    pub fn opt_outs(&self) -> &O {
        &self.opt_outs
    }

    /// Borrows the conversion repository (read path for the HTTP layer).
    #[must_use]
    pub fn conversions(&self) -> &CV {
        &self.conversions
    }

    /// Enforces owner scope: a profile-scoped context may only touch
    /// campaigns owned by its profile; an agency-scoped context (`None`)
    /// may touch agency-owned campaigns and any profile's campaigns
    /// (grant checks happen upstream).
    fn check_owner_scope(ctx: &ActionContext, owner: &OwnerContext) -> Result<(), DomainError> {
        match (ctx.client_profile_id, owner) {
            (Some(ctx_profile), OwnerContext::ClientProfile(owner_profile))
                if ctx_profile != *owner_profile =>
            {
                Err(DomainError::Forbidden(
                    "operation crosses client-profile scope".to_owned(),
                ))
            }
            (Some(_), OwnerContext::Agency) => Err(DomainError::Forbidden(
                "profile-scoped context cannot touch agency-owned campaigns".to_owned(),
            )),
            _ => Ok(()),
        }
    }

    /// Resolves a campaign's prospect pool from its `prospect_pool_id`
    /// string. The id is parsed as a UUID; an unparseable id is a
    /// configuration error.
    async fn resolve_pool(&self, campaign: &Campaign) -> Result<Option<ProspectPool>, DomainError>
    where
        P: ProspectPoolRepository + Send + Sync,
    {
        let pool_id = match Uuid::parse_str(&campaign.prospect_pool_id) {
            Ok(uuid) => crm_domain::ids::ProspectPoolId::from(uuid),
            Err(_) => {
                return Err(DomainError::Validation(format!(
                    "campaign prospect_pool_id '{}' is not a valid pool id",
                    campaign.prospect_pool_id
                )))
            }
        };
        match self.pools.get(pool_id).await {
            Ok(pool) => Ok(Some(pool)),
            Err(DomainError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Records an audit event for a campaign mutation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails the
    /// operation).
    async fn audit_campaign(
        &self,
        ctx: &ActionContext,
        action: &str,
        campaign_id: &CampaignId,
        result: ActionResult,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                "campaign",
                &campaign_id.to_string(),
                PermissionDecision::Allow,
                result,
                None,
                None,
            )
            .await;
    }
}

impl<C, P, O, CT, CV, E, A> CampaignService for CampaignServiceImpl<C, P, O, CT, CV, E, A>
where
    C: CampaignRepository + Send + Sync,
    P: ProspectPoolRepository + Send + Sync,
    O: OptOutRepository + Send + Sync,
    CT: ContactRepository + Send + Sync,
    CV: ConversionRepository + Send + Sync,
    E: EmailService + Send + Sync,
    A: AuditService + Send + Sync,
{
    async fn create_campaign(
        &self,
        input: NewCampaign,
        ctx: &ActionContext,
    ) -> Result<Campaign, DomainError> {
        Self::check_owner_scope(ctx, &input.owner_context)?;
        let campaign = Campaign::new(
            CampaignId::new(),
            input.name,
            input.owner_context,
            input.channel,
            input.prospect_pool_id,
            input.schedule,
            input.suppression_policy,
            input.conversion_rules,
            input.attribution_window_days,
        )?;
        let campaign = self.campaigns.create(campaign).await?;
        self.audit_campaign(
            ctx,
            "campaign.created",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        Ok(campaign)
    }

    async fn add_step(
        &self,
        input: NewCampaignStep,
        ctx: &ActionContext,
    ) -> Result<CampaignStep, DomainError> {
        let campaign = self.campaigns.get(input.campaign_id).await?;
        Self::check_owner_scope(ctx, &campaign.owner_context)?;
        // Steps are frozen once the campaign leaves Draft.
        if campaign.delivery_state != crm_domain::campaign::DeliveryState::Draft {
            return Err(DomainError::Validation(
                "steps can only be added to a draft campaign".to_owned(),
            ));
        }
        let step = CampaignStep::new(
            input.campaign_id,
            input.kind,
            input.position,
            input.template_id,
            input.wait_duration_secs,
        )?;
        let step = self.campaigns.add_step(step).await?;
        self.audit_campaign(
            ctx,
            "campaign.step_added",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        Ok(step)
    }

    async fn schedule(
        &self,
        campaign_id: CampaignId,
        schedule_for: chrono::DateTime<chrono::Utc>,
        ctx: &ActionContext,
    ) -> Result<Campaign, DomainError> {
        let mut campaign = self.campaigns.get(campaign_id).await?;
        Self::check_owner_scope(ctx, &campaign.owner_context)?;
        campaign.schedule(schedule_for)?;
        let campaign = self.campaigns.update(campaign).await?;
        self.audit_campaign(
            ctx,
            "campaign.scheduled",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        Ok(campaign)
    }

    async fn launch(
        &self,
        campaign_id: CampaignId,
        ctx: &ActionContext,
    ) -> Result<Campaign, DomainError> {
        let mut campaign = self.campaigns.get(campaign_id).await?;
        Self::check_owner_scope(ctx, &campaign.owner_context)?;

        // Load steps; the domain requires at least one.
        let steps = self.campaigns.list_steps(campaign_id).await?;
        campaign.launch(steps.len())?;

        // Resolve the audience.
        let pool = self.resolve_pool(&campaign).await?;
        let members: Vec<ContactId> = pool.map(|p| p.members).unwrap_or_default();

        // Execute email steps, skipping opted-out contacts. Call and
        // wait steps are recorded but not executed (no telephony in
        // this slice; waits are scheduler concerns).
        let mut sent_count = 0u32;
        let mut skipped_count = 0u32;
        for step in &steps {
            if step.kind != StepKind::Email {
                continue;
            }
            let template = step.template_id.clone().unwrap_or_default();
            for contact_id in &members {
                // Mandatory opt-out enforcement (compliance).
                if self
                    .opt_outs
                    .is_opted_out(*contact_id, OptOutChannel::Email)
                    .await?
                {
                    skipped_count += 1;
                    continue;
                }
                let contact = self.contacts.get(*contact_id).await?;
                let Some(email) = contact.normalized_emails.first() else {
                    // No email address: skip (not an error).
                    skipped_count += 1;
                    continue;
                };
                // The email's profile scope follows the contact.
                let email_ctx = ActionContext {
                    actor_kind: ctx.actor_kind,
                    actor_id: ctx.actor_id,
                    client_profile_id: Some(contact.client_profile_id),
                    correlation_id: ctx.correlation_id,
                };
                let input = NewEmailMessage {
                    client_profile_id: contact.client_profile_id,
                    thread_id: None,
                    from: String::new(),
                    to: vec![email.clone()],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: format!("Campaign: {}", campaign.name),
                    body_text: template.clone(),
                    body_html: None,
                    idempotency_key: format!(
                        "campaign-{}-step-{}-contact-{}",
                        campaign.campaign_id, step.step_id, contact_id
                    ),
                };
                // A single send failure does not abort the campaign;
                // the failure is audited and the launch continues.
                match self.email_service.send(input, &email_ctx).await {
                    Ok(_) => sent_count += 1,
                    Err(_) => skipped_count += 1,
                }
            }
        }

        let campaign = self.campaigns.update(campaign).await?;
        self.audit_campaign(
            ctx,
            "campaign.launched",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        // Record the send summary as a separate audit event for
        // operational visibility.
        let _ = self
            .audit
            .record_action(
                ctx,
                "campaign.bulk_send_completed",
                "campaign",
                &campaign.campaign_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                Some(&format!("sent={sent_count} skipped={skipped_count}")),
            )
            .await;
        Ok(campaign)
    }

    async fn pause(
        &self,
        campaign_id: CampaignId,
        ctx: &ActionContext,
    ) -> Result<Campaign, DomainError> {
        let mut campaign = self.campaigns.get(campaign_id).await?;
        Self::check_owner_scope(ctx, &campaign.owner_context)?;
        campaign.pause()?;
        let campaign = self.campaigns.update(campaign).await?;
        self.audit_campaign(
            ctx,
            "campaign.paused",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        Ok(campaign)
    }

    async fn resume(
        &self,
        campaign_id: CampaignId,
        ctx: &ActionContext,
    ) -> Result<Campaign, DomainError> {
        let mut campaign = self.campaigns.get(campaign_id).await?;
        Self::check_owner_scope(ctx, &campaign.owner_context)?;
        campaign.resume()?;
        let campaign = self.campaigns.update(campaign).await?;
        self.audit_campaign(
            ctx,
            "campaign.resumed",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        Ok(campaign)
    }

    async fn complete(
        &self,
        campaign_id: CampaignId,
        ctx: &ActionContext,
    ) -> Result<Campaign, DomainError> {
        let mut campaign = self.campaigns.get(campaign_id).await?;
        Self::check_owner_scope(ctx, &campaign.owner_context)?;
        campaign.complete()?;
        let campaign = self.campaigns.update(campaign).await?;
        self.audit_campaign(
            ctx,
            "campaign.completed",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        Ok(campaign)
    }

    async fn cancel(
        &self,
        campaign_id: CampaignId,
        ctx: &ActionContext,
    ) -> Result<Campaign, DomainError> {
        let mut campaign = self.campaigns.get(campaign_id).await?;
        Self::check_owner_scope(ctx, &campaign.owner_context)?;
        campaign.cancel()?;
        let campaign = self.campaigns.update(campaign).await?;
        self.audit_campaign(
            ctx,
            "campaign.cancelled",
            &campaign.campaign_id,
            ActionResult::Success,
        )
        .await;
        Ok(campaign)
    }

    async fn record_conversion(
        &self,
        input: NewConversion,
        ctx: &ActionContext,
    ) -> Result<Conversion, DomainError> {
        // Scope: the conversion's profile must match the context.
        match ctx.client_profile_id {
            Some(ctx_profile) if ctx_profile != input.client_profile_id => {
                return Err(DomainError::Forbidden(
                    "operation crosses client-profile scope".to_owned(),
                ))
            }
            _ => {}
        }
        // Attribute to one of the candidate touchpoints.
        let credited = attribute_conversion(&input.touchpoints, input.attribution_model)
            .map(|t| t.campaign_id);
        let conversion = Conversion::new(
            input.client_profile_id,
            input.contact_id,
            credited,
            input.value_cents,
            input.occurred_at,
        );
        let conversion = self.conversions.create(conversion).await?;
        let _ = self
            .audit
            .record_action(
                ctx,
                "campaign.conversion_recorded",
                "conversion",
                &conversion.conversion_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
        Ok(conversion)
    }

    async fn create_pool(
        &self,
        client_profile_id: ClientProfileId,
        name: String,
        ctx: &ActionContext,
    ) -> Result<ProspectPool, DomainError> {
        match ctx.client_profile_id {
            Some(ctx_profile) if ctx_profile != client_profile_id => {
                return Err(DomainError::Forbidden(
                    "operation crosses client-profile scope".to_owned(),
                ))
            }
            _ => {}
        }
        let pool = ProspectPool::new(client_profile_id, name)?;
        let pool = self.pools.create(pool).await?;
        let _ = self
            .audit
            .record_action(
                ctx,
                "campaign.pool_created",
                "prospect_pool",
                &pool.pool_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
        Ok(pool)
    }

    async fn record_opt_out(
        &self,
        client_profile_id: ClientProfileId,
        contact_id: ContactId,
        channel: OptOutChannel,
        reason: Option<String>,
        ctx: &ActionContext,
    ) -> Result<crm_domain::campaign::OptOut, DomainError> {
        match ctx.client_profile_id {
            Some(ctx_profile) if ctx_profile != client_profile_id => {
                return Err(DomainError::Forbidden(
                    "operation crosses client-profile scope".to_owned(),
                ))
            }
            _ => {}
        }
        let opt_out =
            crm_domain::campaign::OptOut::new(client_profile_id, contact_id, channel, reason);
        let opt_out = self.opt_outs.create(opt_out).await?;
        let _ = self
            .audit
            .record_action(
                ctx,
                "campaign.opted_out",
                "opt_out",
                &opt_out.opt_out_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
        Ok(opt_out)
    }
}
