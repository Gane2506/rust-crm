// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Feed vertical slice (task 12-FEED-02).
//!
//! [`PgFeedItemRepository`] implements
//! [`crm_domain::repositories::FeedItemRepository`] over the
//! `feed_items` table (migration 0011).
//!
//! Feed items are write-once facts: the repository persists visibility
//! and read annotations, but item content (source, title, body,
//! actor, reference) never changes after creation. There is no update
//! method and no delete method — hidden items stay in the table.
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::error::DomainError;
use crm_domain::feed::{compare_feed_items, FeedItem, FeedQuery, FeedSource, FeedVisibility};
use crm_domain::ids::{ClientProfileId, FeedItemId};
use crm_domain::repositories::FeedItemRepository;

use crate::postgres::{
    actor_kind_as_str, actor_kind_from_str, ensure_client_profile, map_storage_error,
};

/// Renders a [`FeedSource`] as its stable database string.
fn feed_source_as_str(source: FeedSource) -> &'static str {
    source.as_str()
}

/// Parses a [`FeedSource`] from its database string.
fn feed_source_from_str(raw: &str) -> Result<FeedSource, DomainError> {
    match raw {
        "contact.created" => Ok(FeedSource::ContactCreated),
        "contact.qualified" => Ok(FeedSource::ContactQualified),
        "campaign.launched" => Ok(FeedSource::CampaignLaunched),
        "form.submitted" => Ok(FeedSource::FormSubmitted),
        "email.received" => Ok(FeedSource::EmailReceived),
        "task.due" => Ok(FeedSource::TaskDue),
        "note.added" => Ok(FeedSource::NoteAdded),
        "agent_run.completed" => Ok(FeedSource::AgentRunCompleted),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown feed source '{other}'"
        ))),
    }
}

/// Renders a [`FeedVisibility`] as its stable database string.
fn feed_visibility_as_str(visibility: FeedVisibility) -> &'static str {
    match visibility {
        FeedVisibility::Normal => "normal",
        FeedVisibility::Pinned => "pinned",
        FeedVisibility::Hidden => "hidden",
    }
}

/// Parses a [`FeedVisibility`] from its database string.
fn feed_visibility_from_str(raw: &str) -> Result<FeedVisibility, DomainError> {
    match raw {
        "normal" => Ok(FeedVisibility::Normal),
        "pinned" => Ok(FeedVisibility::Pinned),
        "hidden" => Ok(FeedVisibility::Hidden),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown feed visibility '{other}'"
        ))),
    }
}

/// Columns selected for a [`FeedItem`] row.
const FEED_ITEM_COLUMNS: &str =
    "feed_item_id, client_profile_id, source, title, body, actor_kind, \
     ref_type, ref_id, visibility, read, created_at";

/// Hydrates a `feed_items` row into a [`FeedItem`].
fn row_to_feed_item(row: &sqlx::postgres::PgRow) -> Result<FeedItem, DomainError> {
    Ok(FeedItem {
        feed_item_id: FeedItemId::from(
            row.try_get::<Uuid, _>("feed_item_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        source: feed_source_from_str(
            row.try_get::<String, _>("source")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        title: row.try_get("title").map_err(map_storage_error)?,
        body: row.try_get("body").map_err(map_storage_error)?,
        actor_kind: actor_kind_from_str(
            row.try_get::<String, _>("actor_kind")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        ref_type: row.try_get("ref_type").map_err(map_storage_error)?,
        ref_id: row.try_get("ref_id").map_err(map_storage_error)?,
        visibility: feed_visibility_from_str(
            row.try_get::<String, _>("visibility")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        read: row.try_get("read").map_err(map_storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}

/// SQLx implementation of [`FeedItemRepository`].
#[derive(Debug, Clone)]
pub struct PgFeedItemRepository {
    pool: PgPool,
}

impl PgFeedItemRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl FeedItemRepository for PgFeedItemRepository {
    async fn create(&self, item: FeedItem) -> Result<FeedItem, DomainError> {
        ensure_client_profile(&self.pool, item.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO feed_items (feed_item_id, client_profile_id, source, title, body, \
             actor_kind, ref_type, ref_id, visibility, read, created_at)\n             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(item.feed_item_id.as_uuid())
        .bind(item.client_profile_id.as_uuid())
        .bind(feed_source_as_str(item.source))
        .bind(&item.title)
        .bind(&item.body)
        .bind(actor_kind_as_str(item.actor_kind))
        .bind(&item.ref_type)
        .bind(item.ref_id)
        .bind(feed_visibility_as_str(item.visibility))
        .bind(item.read)
        .bind(item.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(item)
    }

    async fn get(&self, feed_item_id: FeedItemId) -> Result<FeedItem, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {FEED_ITEM_COLUMNS} FROM feed_items WHERE feed_item_id = $1"
        ))
        .bind(feed_item_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_feed_item(&row),
            None => Err(DomainError::NotFound {
                kind: "feed_item".to_owned(),
                id: feed_item_id.to_string(),
            }),
        }
    }

    async fn list(
        &self,
        client_profile_id: ClientProfileId,
        query: FeedQuery,
    ) -> Result<Vec<FeedItem>, DomainError> {
        // The SQL ordering mirrors the domain's `compare_feed_items`
        // (pinned first, then newest-first, then id tiebreak) so the
        // database does the work; the service re-sorts with the pure
        // comparator as a safety net.
        let limit = i64::try_from(query.limit()).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {FEED_ITEM_COLUMNS} FROM feed_items\n             WHERE client_profile_id = $1\n               AND ($2 OR visibility <> 'hidden')\n               AND ($3 OR read = FALSE)\n             ORDER BY CASE visibility WHEN 'pinned' THEN 0 WHEN 'normal' THEN 1 ELSE 2 END ASC,\n                      created_at DESC, feed_item_id DESC\n             LIMIT $4"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(query.include_hidden())
        .bind(!query.unread_only())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        let mut items: Vec<FeedItem> = rows
            .iter()
            .map(row_to_feed_item)
            .collect::<Result<_, _>>()?;
        items.sort_by(compare_feed_items);
        Ok(items)
    }

    async fn set_visibility(&self, item: FeedItem) -> Result<FeedItem, DomainError> {
        let result = sqlx::query(
            "UPDATE feed_items\n             SET visibility = $2\n             WHERE feed_item_id = $1",
        )
        .bind(item.feed_item_id.as_uuid())
        .bind(feed_visibility_as_str(item.visibility))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "feed_item".to_owned(),
                id: item.feed_item_id.to_string(),
            });
        }
        Ok(item)
    }

    async fn mark_read(&self, item: FeedItem) -> Result<FeedItem, DomainError> {
        let result = sqlx::query(
            "UPDATE feed_items\n             SET read = TRUE\n             WHERE feed_item_id = $1",
        )
        .bind(item.feed_item_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "feed_item".to_owned(),
                id: item.feed_item_id.to_string(),
            });
        }
        Ok(item)
    }
}
