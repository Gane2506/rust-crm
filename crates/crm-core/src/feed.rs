// SPDX-License-Identifier: GPL-3.0-only
//! Feed service implementation (task 12-FEED-02).
//!
//! [`FeedServiceImpl`] implements
//! [`FeedService`](crm_domain::services::FeedService) over generic
//! feed-item and audit ports — the same composition pattern as
//! [`FormServiceImpl`](crate::form::FormServiceImpl). The production
//! wiring (SQLx repositories) is assembled by the API layer.
//!
//! Key behaviors:
//! * feed items are write-once facts: the service never edits item
//!   content — it only lists, filters, orders and annotates;
//! * [`FeedServiceImpl::list_feed`] re-sorts with the pure
//!   [`compare_feed_items`](crm_domain::feed::compare_feed_items)
//!   comparator as a safety net over the repository ordering;
//! * every operation records an
//!   [`AuditEvent`](crm_domain::audit::AuditEvent) with its pinned
//!   action name;
//! * audit failures are swallowed: audit is fail-loud at the HTTP
//!   layer, never fails the operation.

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::feed::{compare_feed_items, FeedItem, FeedQuery};
use crm_domain::ids::{ClientProfileId, FeedItemId};
use crm_domain::repositories::FeedItemRepository;
use crm_domain::services::{ActionContext, AuditService, FeedService};

/// Generic [`FeedService`] implementation.
///
/// Type parameters: `R` feed-item repository, `A` audit service.
pub struct FeedServiceImpl<R, A> {
    items: R,
    audit: A,
}

impl<R, A> FeedServiceImpl<R, A> {
    /// Creates the service over the given ports.
    pub fn new(items: R, audit: A) -> Self {
        Self { items, audit }
    }

    /// Enforces profile scope: a profile-scoped context may only touch
    /// feed items of its own profile; an agency-scoped context (`None`)
    /// may touch any profile's items (grant checks happen upstream, at
    /// the HTTP layer).
    fn check_scope(
        ctx: &ActionContext,
        client_profile_id: ClientProfileId,
    ) -> Result<(), DomainError> {
        match ctx.client_profile_id {
            Some(ctx_profile) if ctx_profile != client_profile_id => Err(DomainError::Forbidden(
                "operation crosses client-profile scope".to_owned(),
            )),
            _ => Ok(()),
        }
    }

    /// Records an audit event for a feed operation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails
    /// the operation).
    async fn audit_feed(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_id: &str,
        result: ActionResult,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                "feed_item",
                target_id,
                PermissionDecision::Allow,
                result,
                None,
                None,
            )
            .await;
    }

    /// Loads a feed item and enforces the caller's profile scope on it.
    async fn load_scoped_item(
        &self,
        feed_item_id: FeedItemId,
        ctx: &ActionContext,
    ) -> Result<FeedItem, DomainError>
    where
        R: FeedItemRepository + Send + Sync,
    {
        let item = self.items.get(feed_item_id).await?;
        Self::check_scope(ctx, item.client_profile_id)?;
        Ok(item)
    }
}

impl<R, A> FeedService for FeedServiceImpl<R, A>
where
    R: FeedItemRepository + Send + Sync,
    A: AuditService + Send + Sync,
{
    async fn list_feed(
        &self,
        query: FeedQuery,
        ctx: &ActionContext,
    ) -> Result<Vec<FeedItem>, DomainError> {
        let profile_id = ctx.client_profile_id.ok_or_else(|| {
            DomainError::Forbidden("feed reads require a client-profile scope".to_owned())
        })?;
        let mut items = self.items.list(profile_id, query).await?;
        // Safety net: the repository orders in SQL (mirroring the
        // domain comparator), but the domain ordering is the source of
        // truth — re-sort here so the contract holds regardless of the
        // port implementation.
        items.sort_by(compare_feed_items);
        self.audit_feed(
            ctx,
            "feed.listed",
            &profile_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(items)
    }

    async fn mark_read(
        &self,
        feed_item_id: FeedItemId,
        ctx: &ActionContext,
    ) -> Result<FeedItem, DomainError> {
        let mut item = self.load_scoped_item(feed_item_id, ctx).await?;
        item.mark_read();
        let item = self.items.mark_read(item).await?;
        self.audit_feed(
            ctx,
            "feed.item_read",
            &feed_item_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(item)
    }

    async fn pin_item(
        &self,
        feed_item_id: FeedItemId,
        ctx: &ActionContext,
    ) -> Result<FeedItem, DomainError> {
        let mut item = self.load_scoped_item(feed_item_id, ctx).await?;
        item.pin()?;
        let item = self.items.set_visibility(item).await?;
        self.audit_feed(
            ctx,
            "feed.item_pinned",
            &feed_item_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(item)
    }

    async fn hide_item(
        &self,
        feed_item_id: FeedItemId,
        ctx: &ActionContext,
    ) -> Result<FeedItem, DomainError> {
        let mut item = self.load_scoped_item(feed_item_id, ctx).await?;
        item.hide();
        let item = self.items.set_visibility(item).await?;
        self.audit_feed(
            ctx,
            "feed.item_hidden",
            &feed_item_id.to_string(),
            ActionResult::Success,
        )
        .await;
        Ok(item)
    }
}
