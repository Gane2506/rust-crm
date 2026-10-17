// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Campaigns vertical slice (task 08-CAMP-02).
//!
//! [`PgCampaignRepository`], [`PgProspectPoolRepository`],
//! [`PgOptOutRepository`] and [`PgConversionRepository`] implement the
//! [`crm_domain::repositories`] campaign traits over the `campaigns`,
//! `campaign_steps`, `prospect_pools`, `pool_members`, `opt_outs` and
//! `conversions` tables (migration 0007).
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::campaign::{
    Campaign, CampaignStep, Channel, Conversion, DeliveryState, OptOut,
    OptOutChannel, OwnerContext, ProspectPool, StepKind,
};
use crm_domain::error::DomainError;
use crm_domain::ids::{
    CampaignId, CampaignStepId, ClientProfileId, ContactId, ConversionId, OptOutId, ProspectPoolId,
};
use crm_domain::repositories::{
    CampaignRepository, ConversionRepository, OptOutRepository, ProspectPoolRepository,
};

use crate::postgres::map_storage_error;

/// Renders a [`Channel`] as its stable database string.
fn channel_as_str(channel: Channel) -> &'static str {
    match channel {
        Channel::Email => "email",
        Channel::Call => "call",
        Channel::Mixed => "mixed",
    }
}

/// Parses a [`Channel`] from its database string.
fn channel_from_str(raw: &str) -> Result<Channel, DomainError> {
    match raw {
        "email" => Ok(Channel::Email),
        "call" => Ok(Channel::Call),
        "mixed" => Ok(Channel::Mixed),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown campaign channel '{other}'"
        ))),
    }
}

/// Renders a [`DeliveryState`] as its stable database string.
fn delivery_state_as_str(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Draft => "draft",
        DeliveryState::Scheduled => "scheduled",
        DeliveryState::Running => "running",
        DeliveryState::Paused => "paused",
        DeliveryState::Completed => "completed",
        DeliveryState::Cancelled => "cancelled",
    }
}

/// Parses a [`DeliveryState`] from its database string.
fn delivery_state_from_str(raw: &str) -> Result<DeliveryState, DomainError> {
    match raw {
        "draft" => Ok(DeliveryState::Draft),
        "scheduled" => Ok(DeliveryState::Scheduled),
        "running" => Ok(DeliveryState::Running),
        "paused" => Ok(DeliveryState::Paused),
        "completed" => Ok(DeliveryState::Completed),
        "cancelled" => Ok(DeliveryState::Cancelled),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown delivery state '{other}'"
        ))),
    }
}

/// Renders a [`StepKind`] as its stable database string.
fn step_kind_as_str(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Email => "email",
        StepKind::Call => "call",
        StepKind::Wait => "wait",
    }
}

/// Parses a [`StepKind`] from its database string.
fn step_kind_from_str(raw: &str) -> Result<StepKind, DomainError> {
    match raw {
        "email" => Ok(StepKind::Email),
        "call" => Ok(StepKind::Call),
        "wait" => Ok(StepKind::Wait),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown step kind '{other}'"
        ))),
    }
}

/// Renders an [`OptOutChannel`] as its stable database string.
fn opt_out_channel_as_str(channel: OptOutChannel) -> &'static str {
    match channel {
        OptOutChannel::Email => "email",
        OptOutChannel::Sms => "sms",
        OptOutChannel::Call => "call",
        OptOutChannel::All => "all",
    }
}

/// Parses an [`OptOutChannel`] from its database string.
fn opt_out_channel_from_str(raw: &str) -> Result<OptOutChannel, DomainError> {
    match raw {
        "email" => Ok(OptOutChannel::Email),
        "sms" => Ok(OptOutChannel::Sms),
        "call" => Ok(OptOutChannel::Call),
        "all" => Ok(OptOutChannel::All),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown opt-out channel '{other}'"
        ))),
    }
}

/// Columns selected for a [`Campaign`] row.
const CAMPAIGN_COLUMNS: &str = "campaign_id, client_profile_id, owner_kind, name, channel, \
     prospect_pool_id, schedule, schedule_for, suppression_policy, delivery_state, \
     conversion_rules, attribution_window_days, created_at, updated_at";

/// Columns selected for a [`CampaignStep`] row.
const STEP_COLUMNS: &str =
    "step_id, campaign_id, kind, position, template_id, wait_seconds, created_at";

/// Columns selected for a [`ProspectPool`] row (members come from `pool_members`).
const POOL_COLUMNS: &str = "pool_id, client_profile_id, name, created_at, updated_at";

/// Columns selected for an [`OptOut`] row.
const OPT_OUT_COLUMNS: &str =
    "opt_out_id, client_profile_id, contact_id, channel, reason, created_at";

/// Columns selected for a [`Conversion`] row.
const CONVERSION_COLUMNS: &str = "conversion_id, client_profile_id, contact_id, campaign_id, \
     value_cents, occurred_at, created_at";

/// Hydrates a `campaigns` row into a [`Campaign`].
fn row_to_campaign(row: &sqlx::postgres::PgRow) -> Result<Campaign, DomainError> {
    let client_profile_id: Option<Uuid> = row
        .try_get("client_profile_id")
        .map_err(map_storage_error)?;
    let owner_kind: String = row.try_get("owner_kind").map_err(map_storage_error)?;
    let owner_context = match owner_kind.as_str() {
        "agency" => OwnerContext::Agency,
        "client_profile" => {
            let profile_id = client_profile_id.ok_or_else(|| {
                DomainError::Validation(
                    "storage error: client_profile campaign without client_profile_id".to_owned(),
                )
            })?;
            OwnerContext::ClientProfile(ClientProfileId::from(profile_id))
        }
        other => {
            return Err(DomainError::Validation(format!(
                "storage error: unknown owner kind '{other}'"
            )))
        }
    };
    let schedule_for: Option<DateTime<Utc>> =
        row.try_get("schedule_for").map_err(map_storage_error)?;
    let attribution_window_days: i32 = row
        .try_get("attribution_window_days")
        .map_err(map_storage_error)?;
    Ok(Campaign {
        campaign_id: CampaignId::from(
            row.try_get::<Uuid, _>("campaign_id")
                .map_err(map_storage_error)?,
        ),
        name: row.try_get("name").map_err(map_storage_error)?,
        owner_context,
        source_file_ref: None,
        channel: channel_from_str(
            row.try_get::<String, _>("channel")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        prospect_pool_id: row.try_get("prospect_pool_id").map_err(map_storage_error)?,
        schedule: row.try_get("schedule").map_err(map_storage_error)?,
        schedule_for,
        suppression_policy: row
            .try_get("suppression_policy")
            .map_err(map_storage_error)?,
        delivery_state: delivery_state_from_str(
            row.try_get::<String, _>("delivery_state")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        conversion_rules: row.try_get("conversion_rules").map_err(map_storage_error)?,
        attribution_window_days: u32::try_from(attribution_window_days).map_err(|_| {
            DomainError::Validation("storage error: negative attribution_window_days".to_owned())
        })?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates a `campaign_steps` row into a [`CampaignStep`].
fn row_to_step(row: &sqlx::postgres::PgRow) -> Result<CampaignStep, DomainError> {
    let position: i32 = row.try_get("position").map_err(map_storage_error)?;
    let wait_seconds: Option<i64> = row.try_get("wait_seconds").map_err(map_storage_error)?;
    Ok(CampaignStep {
        step_id: CampaignStepId::from(
            row.try_get::<Uuid, _>("step_id")
                .map_err(map_storage_error)?,
        ),
        campaign_id: CampaignId::from(
            row.try_get::<Uuid, _>("campaign_id")
                .map_err(map_storage_error)?,
        ),
        kind: step_kind_from_str(
            row.try_get::<String, _>("kind")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        position: u32::try_from(position).map_err(|_| {
            DomainError::Validation("storage error: negative step position".to_owned())
        })?,
        template_id: row.try_get("template_id").map_err(map_storage_error)?,
        wait_duration_secs: wait_seconds
            .map(|s| {
                u64::try_from(s).map_err(|_| {
                    DomainError::Validation("storage error: negative wait_seconds".to_owned())
                })
            })
            .transpose()?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates a `prospect_pools` row plus its member ids into a [`ProspectPool`].
fn row_to_pool(
    row: &sqlx::postgres::PgRow,
    members: Vec<ContactId>,
) -> Result<ProspectPool, DomainError> {
    Ok(ProspectPool {
        pool_id: ProspectPoolId::from(
            row.try_get::<Uuid, _>("pool_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        name: row.try_get("name").map_err(map_storage_error)?,
        members,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates an `opt_outs` row into an [`OptOut`].
fn row_to_opt_out(row: &sqlx::postgres::PgRow) -> Result<OptOut, DomainError> {
    Ok(OptOut {
        opt_out_id: OptOutId::from(
            row.try_get::<Uuid, _>("opt_out_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        contact_id: ContactId::from(
            row.try_get::<Uuid, _>("contact_id")
                .map_err(map_storage_error)?,
        ),
        channel: opt_out_channel_from_str(
            row.try_get::<String, _>("channel")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        reason: row.try_get("reason").map_err(map_storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// Hydrates a `conversions` row into a [`Conversion`].
fn row_to_conversion(row: &sqlx::postgres::PgRow) -> Result<Conversion, DomainError> {
    let campaign_id: Option<Uuid> = row.try_get("campaign_id").map_err(map_storage_error)?;
    let value_cents: Option<i64> = row.try_get("value_cents").map_err(map_storage_error)?;
    Ok(Conversion {
        conversion_id: ConversionId::from(
            row.try_get::<Uuid, _>("conversion_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        contact_id: ContactId::from(
            row.try_get::<Uuid, _>("contact_id")
                .map_err(map_storage_error)?,
        ),
        campaign_id: campaign_id.map(CampaignId::from),
        value_cents: value_cents
            .map(|v| {
                u64::try_from(v).map_err(|_| {
                    DomainError::Validation("storage error: negative value_cents".to_owned())
                })
            })
            .transpose()?,
        occurred_at: row
            .try_get::<DateTime<Utc>, _>("occurred_at")
            .map_err(map_storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// Splits an [`OwnerContext`] into its storage form: the `owner_kind`
/// discriminator plus the nullable `client_profile_id`.
fn owner_context_to_storage(owner: &OwnerContext) -> (&'static str, Option<Uuid>) {
    match owner {
        OwnerContext::Agency => ("agency", None),
        OwnerContext::ClientProfile(profile_id) => ("client_profile", Some(profile_id.as_uuid())),
    }
}

/// SQLx implementation of [`CampaignRepository`].
#[derive(Debug, Clone)]
pub struct PgCampaignRepository {
    pool: PgPool,
}

impl PgCampaignRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl CampaignRepository for PgCampaignRepository {
    async fn create(&self, campaign: Campaign) -> Result<Campaign, DomainError> {
        let (owner_kind, client_profile_id) = owner_context_to_storage(&campaign.owner_context);
        if let Some(profile_id) = client_profile_id {
            crate::postgres::ensure_client_profile(&self.pool, ClientProfileId::from(profile_id))
                .await?;
        }
        sqlx::query(
            "INSERT INTO campaigns (campaign_id, client_profile_id, owner_kind, name, channel, \
             prospect_pool_id, schedule, schedule_for, suppression_policy, delivery_state, \
             conversion_rules, attribution_window_days, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(campaign.campaign_id.as_uuid())
        .bind(client_profile_id)
        .bind(owner_kind)
        .bind(&campaign.name)
        .bind(channel_as_str(campaign.channel))
        .bind(&campaign.prospect_pool_id)
        .bind(&campaign.schedule)
        .bind(campaign.schedule_for)
        .bind(&campaign.suppression_policy)
        .bind(delivery_state_as_str(campaign.delivery_state))
        .bind(&campaign.conversion_rules)
        .bind(i32::try_from(campaign.attribution_window_days).unwrap_or(i32::MAX))
        .bind(campaign.created_at)
        .bind(campaign.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(campaign)
    }

    async fn get(&self, campaign_id: CampaignId) -> Result<Campaign, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {CAMPAIGN_COLUMNS} FROM campaigns WHERE campaign_id = $1"
        ))
        .bind(campaign_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_campaign(&row),
            None => Err(DomainError::NotFound {
                kind: "campaign".to_owned(),
                id: campaign_id.to_string(),
            }),
        }
    }

    async fn update(&self, campaign: Campaign) -> Result<Campaign, DomainError> {
        let (owner_kind, client_profile_id) = owner_context_to_storage(&campaign.owner_context);
        let result = sqlx::query(
            "UPDATE campaigns
             SET client_profile_id = $2, owner_kind = $3, name = $4, channel = $5,
                 prospect_pool_id = $6, schedule = $7, schedule_for = $8,
                 suppression_policy = $9, delivery_state = $10, conversion_rules = $11,
                 attribution_window_days = $12, updated_at = $13
             WHERE campaign_id = $1",
        )
        .bind(campaign.campaign_id.as_uuid())
        .bind(client_profile_id)
        .bind(owner_kind)
        .bind(&campaign.name)
        .bind(channel_as_str(campaign.channel))
        .bind(&campaign.prospect_pool_id)
        .bind(&campaign.schedule)
        .bind(campaign.schedule_for)
        .bind(&campaign.suppression_policy)
        .bind(delivery_state_as_str(campaign.delivery_state))
        .bind(&campaign.conversion_rules)
        .bind(i32::try_from(campaign.attribution_window_days).unwrap_or(i32::MAX))
        .bind(campaign.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "campaign".to_owned(),
                id: campaign.campaign_id.to_string(),
            });
        }
        Ok(campaign)
    }

    async fn list_for_owner(
        &self,
        owner: &OwnerContext,
        limit: u64,
    ) -> Result<Vec<Campaign>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let (owner_kind, client_profile_id) = owner_context_to_storage(owner);
        let rows = sqlx::query(&format!(
            "SELECT {CAMPAIGN_COLUMNS} FROM campaigns
             WHERE owner_kind = $1
               AND (client_profile_id = $2 OR ($2 IS NULL AND client_profile_id IS NULL))
             ORDER BY created_at DESC, campaign_id DESC
             LIMIT $3"
        ))
        .bind(owner_kind)
        .bind(client_profile_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_campaign).collect()
    }

    async fn list_by_profile(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Campaign>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {CAMPAIGN_COLUMNS} FROM campaigns
             WHERE client_profile_id = $1
             ORDER BY created_at DESC, campaign_id DESC
             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_campaign).collect()
    }

    async fn add_step(&self, step: CampaignStep) -> Result<CampaignStep, DomainError> {
        sqlx::query(
            "INSERT INTO campaign_steps (step_id, campaign_id, kind, position, template_id, \
             wait_seconds, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(step.step_id.as_uuid())
        .bind(step.campaign_id.as_uuid())
        .bind(step_kind_as_str(step.kind))
        .bind(i32::try_from(step.position).unwrap_or(i32::MAX))
        .bind(&step.template_id)
        .bind(
            step.wait_duration_secs
                .map(|s| i64::try_from(s).unwrap_or(i64::MAX)),
        )
        .bind(step.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(step)
    }

    async fn list_steps(&self, campaign_id: CampaignId) -> Result<Vec<CampaignStep>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {STEP_COLUMNS} FROM campaign_steps
             WHERE campaign_id = $1
             ORDER BY position ASC, step_id ASC"
        ))
        .bind(campaign_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_step).collect()
    }
}

/// SQLx implementation of [`ProspectPoolRepository`].
#[derive(Debug, Clone)]
pub struct PgProspectPoolRepository {
    pool: PgPool,
}

impl PgProspectPoolRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Loads the member contact ids for one pool.
    async fn load_members(&self, pool_id: ProspectPoolId) -> Result<Vec<ContactId>, DomainError> {
        let rows = sqlx::query(
            "SELECT contact_id FROM pool_members WHERE pool_id = $1 ORDER BY added_at ASC",
        )
        .bind(pool_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter()
            .map(|row| {
                row.try_get::<Uuid, _>("contact_id")
                    .map(ContactId::from)
                    .map_err(map_storage_error)
            })
            .collect()
    }
}

impl ProspectPoolRepository for PgProspectPoolRepository {
    async fn create(&self, pool: ProspectPool) -> Result<ProspectPool, DomainError> {
        crate::postgres::ensure_client_profile(&self.pool, pool.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO prospect_pools (pool_id, client_profile_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(pool.pool_id.as_uuid())
        .bind(pool.client_profile_id.as_uuid())
        .bind(&pool.name)
        .bind(pool.created_at)
        .bind(pool.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(pool)
    }

    async fn get(&self, pool_id: ProspectPoolId) -> Result<ProspectPool, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {POOL_COLUMNS} FROM prospect_pools WHERE pool_id = $1"
        ))
        .bind(pool_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => {
                let members = self.load_members(pool_id).await?;
                row_to_pool(&row, members)
            }
            None => Err(DomainError::NotFound {
                kind: "prospect_pool".to_owned(),
                id: pool_id.to_string(),
            }),
        }
    }

    async fn update(&self, pool: ProspectPool) -> Result<ProspectPool, DomainError> {
        let result = sqlx::query(
            "UPDATE prospect_pools
             SET name = $2, updated_at = $3
             WHERE pool_id = $1",
        )
        .bind(pool.pool_id.as_uuid())
        .bind(&pool.name)
        .bind(pool.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "prospect_pool".to_owned(),
                id: pool.pool_id.to_string(),
            });
        }
        // Sync membership: delete all, re-insert. Simple and correct;
        // pools are small and membership changes are infrequent.
        sqlx::query("DELETE FROM pool_members WHERE pool_id = $1")
            .bind(pool.pool_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        for member in &pool.members {
            sqlx::query("INSERT INTO pool_members (pool_id, contact_id) VALUES ($1, $2)")
                .bind(pool.pool_id.as_uuid())
                .bind(member.as_uuid())
                .execute(&self.pool)
                .await
                .map_err(map_storage_error)?;
        }
        Ok(pool)
    }

    async fn list_by_profile(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<ProspectPool>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {POOL_COLUMNS} FROM prospect_pools
             WHERE client_profile_id = $1
             ORDER BY created_at DESC, pool_id DESC
             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        let mut pools = Vec::with_capacity(rows.len());
        for row in &rows {
            let pool_id = ProspectPoolId::from(
                row.try_get::<Uuid, _>("pool_id")
                    .map_err(map_storage_error)?,
            );
            let members = self.load_members(pool_id).await?;
            pools.push(row_to_pool(row, members)?);
        }
        Ok(pools)
    }

    async fn add_member(
        &self,
        pool_id: ProspectPoolId,
        contact_id: ContactId,
    ) -> Result<ProspectPool, DomainError> {
        // ON CONFLICT DO NOTHING: the unique constraint enforces the
        // domain's dedupe-on-add invariant at the storage layer.
        sqlx::query(
            "INSERT INTO pool_members (pool_id, contact_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(pool_id.as_uuid())
        .bind(contact_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        self.get(pool_id).await
    }

    async fn remove_member(
        &self,
        pool_id: ProspectPoolId,
        contact_id: ContactId,
    ) -> Result<ProspectPool, DomainError> {
        sqlx::query("DELETE FROM pool_members WHERE pool_id = $1 AND contact_id = $2")
            .bind(pool_id.as_uuid())
            .bind(contact_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        self.get(pool_id).await
    }
}

/// SQLx implementation of [`OptOutRepository`].
///
/// Append-only: there is deliberately no update or delete path.
#[derive(Debug, Clone)]
pub struct PgOptOutRepository {
    pool: PgPool,
}

impl PgOptOutRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl OptOutRepository for PgOptOutRepository {
    async fn create(&self, opt_out: OptOut) -> Result<OptOut, DomainError> {
        crate::postgres::ensure_client_profile(&self.pool, opt_out.client_profile_id).await?;
        // ON CONFLICT DO NOTHING: re-recording the same opt-out is
        // idempotent (compliance: the first record's timestamp stands).
        sqlx::query(
            "INSERT INTO opt_outs (opt_out_id, client_profile_id, contact_id, channel, reason, \
             created_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (client_profile_id, contact_id, channel) DO NOTHING",
        )
        .bind(opt_out.opt_out_id.as_uuid())
        .bind(opt_out.client_profile_id.as_uuid())
        .bind(opt_out.contact_id.as_uuid())
        .bind(opt_out_channel_as_str(opt_out.channel))
        .bind(&opt_out.reason)
        .bind(opt_out.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(opt_out)
    }

    async fn get(&self, opt_out_id: OptOutId) -> Result<Option<OptOut>, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {OPT_OUT_COLUMNS} FROM opt_outs WHERE opt_out_id = $1"
        ))
        .bind(opt_out_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(|row| row_to_opt_out(&row)).transpose()
    }

    async fn list_by_contact(&self, contact_id: ContactId) -> Result<Vec<OptOut>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {OPT_OUT_COLUMNS} FROM opt_outs
             WHERE contact_id = $1
             ORDER BY created_at DESC, opt_out_id DESC"
        ))
        .bind(contact_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_opt_out).collect()
    }

    async fn is_opted_out(
        &self,
        contact_id: ContactId,
        channel: OptOutChannel,
    ) -> Result<bool, DomainError> {
        // An `All` record suppresses every channel; otherwise the
        // channel must match exactly.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM opt_outs
             WHERE contact_id = $1 AND (channel = $2 OR channel = 'all')",
        )
        .bind(contact_id.as_uuid())
        .bind(opt_out_channel_as_str(channel))
        .fetch_one(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(count > 0)
    }
}

/// SQLx implementation of [`ConversionRepository`].
///
/// Append-only: corrections are new records, never updates.
#[derive(Debug, Clone)]
pub struct PgConversionRepository {
    pool: PgPool,
}

impl PgConversionRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl ConversionRepository for PgConversionRepository {
    async fn create(&self, conversion: Conversion) -> Result<Conversion, DomainError> {
        crate::postgres::ensure_client_profile(&self.pool, conversion.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO conversions (conversion_id, client_profile_id, contact_id, campaign_id, \
             value_cents, occurred_at, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(conversion.conversion_id.as_uuid())
        .bind(conversion.client_profile_id.as_uuid())
        .bind(conversion.contact_id.as_uuid())
        .bind(conversion.campaign_id.map(|id| id.as_uuid()))
        .bind(
            conversion
                .value_cents
                .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        )
        .bind(conversion.occurred_at)
        .bind(conversion.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(conversion)
    }

    async fn get(&self, conversion_id: ConversionId) -> Result<Conversion, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {CONVERSION_COLUMNS} FROM conversions WHERE conversion_id = $1"
        ))
        .bind(conversion_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_conversion(&row),
            None => Err(DomainError::NotFound {
                kind: "conversion".to_owned(),
                id: conversion_id.to_string(),
            }),
        }
    }

    async fn list_by_campaign(
        &self,
        campaign_id: CampaignId,
        limit: u64,
    ) -> Result<Vec<Conversion>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {CONVERSION_COLUMNS} FROM conversions
             WHERE campaign_id = $1
             ORDER BY occurred_at DESC, conversion_id DESC
             LIMIT $2"
        ))
        .bind(campaign_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_conversion).collect()
    }

    async fn list_by_contact(
        &self,
        contact_id: ContactId,
        limit: u64,
    ) -> Result<Vec<Conversion>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {CONVERSION_COLUMNS} FROM conversions
             WHERE contact_id = $1
             ORDER BY occurred_at DESC, conversion_id DESC
             LIMIT $2"
        ))
        .bind(contact_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_conversion).collect()
    }
}
