// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Feed vertical slice (task 12-FEED-02).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.: <redacted>
//! `postgres://postgres:postgres@localhost:5432/crm_test`
//!
//! ```sh
//! docker compose up -d            # ephemeral Postgres 16 on :5432
//! DATABASE_URL=<redacted>
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Each test creates its own database, migrates a fresh schema, and
//! drops it afterwards. The service tests wire the real SQLx
//! repository with a no-op audit service.

mod common;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::feed::FeedServiceImpl;
use crm_db::feed::PgFeedItemRepository;
use crm_db::run_migrations;
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::feed::{compare_feed_items, FeedItem, FeedQuery, FeedSource, FeedVisibility};
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, FeedItemId};
use crm_domain::repositories::FeedItemRepository;
use crm_domain::services::{ActionContext, AuditService, FeedService};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (feed items reference it).
async fn ensure_profile(pool: &PgPool, id: ClientProfileId) {
    sqlx::query(
        "INSERT INTO client_profiles (client_profile_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(id.as_uuid())
    .execute(pool)
    .await
    .expect("insert client profile");
}

/// No-op [`AuditService`] for service tests: audit must never fail the
/// operation under test.
#[derive(Debug, Clone, Default)]
struct NoopAudit;

impl AuditService for NoopAudit {
    async fn record_action(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_kind: &str,
        target_id: &str,
        permission_decision: PermissionDecision,
        result: ActionResult,
        _before_ref: Option<&str>,
        _after_ref: Option<&str>,
    ) -> Result<AuditEvent, DomainError> {
        Ok(AuditEvent {
            event_id: AuditEventId::new(),
            occurred_at: Utc::now().round_subsecs(6),
            actor_kind: ActorKind::System,
            actor_id: ctx.actor_id,
            client_profile_id: ctx.client_profile_id,
            action: action.to_owned(),
            target_kind: target_kind.to_owned(),
            target_id: target_id.to_owned(),
            correlation_id: ctx.correlation_id,
            permission_decision,
            result,
            before_ref: None,
            after_ref: None,
            chain_hash: [0u8; 32],
        })
    }
}

/// Builds the feed repository over `pool`.
fn repos(pool: &PgPool) -> PgFeedItemRepository {
    PgFeedItemRepository::new(pool.clone())
}

/// Builds the real service over `pool` with a no-op audit service.
fn service(pool: &PgPool) -> FeedServiceImpl<PgFeedItemRepository, NoopAudit> {
    FeedServiceImpl::new(repos(pool), NoopAudit)
}

/// Test action context scoped to `client_profile_id`.
fn ctx_for(profile_id: ClientProfileId) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::new(),
        client_profile_id: Some(profile_id),
        correlation_id: Uuid::new_v4(),
    }
}

/// Builds a valid feed item for `profile` with the given source.
fn item(profile: ClientProfileId, source: FeedSource, title: &str) -> FeedItem {
    FeedItem::new(
        profile,
        source,
        title.to_string(),
        None,
        ActorKind::Human,
        "contact".to_string(),
        Uuid::new_v4(),
    )
    .expect("valid feed item")
}

#[tokio::test]
async fn feed_item_repo_crud_round_trips() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let items = repos(&pool);

    // Create + get.
    let created = items
        .create(item(
            profile,
            FeedSource::ContactCreated,
            "Ada became a prospect",
        ))
        .await
        .expect("create feed item");
    let fetched = items
        .get(created.feed_item_id)
        .await
        .expect("get feed item");
    assert_eq!(fetched.title, "Ada became a prospect");
    assert_eq!(fetched.source, FeedSource::ContactCreated);
    assert_eq!(fetched.visibility, FeedVisibility::Normal);
    assert!(!fetched.read);
    assert_eq!(fetched.client_profile_id, profile);

    // Get of a missing id is a 404.
    let missing = items.get(FeedItemId::new()).await;
    assert!(matches!(missing, Err(DomainError::NotFound { .. })));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn feed_list_filters_hidden_and_unread() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let items = repos(&pool);

    let normal = items
        .create(item(profile, FeedSource::NoteAdded, "normal"))
        .await
        .expect("create normal");
    let mut hidden = items
        .create(item(profile, FeedSource::NoteAdded, "hidden"))
        .await
        .expect("create hidden");
    hidden.hide();
    items.set_visibility(hidden).await.expect("hide");
    let mut read = items
        .create(item(profile, FeedSource::NoteAdded, "read"))
        .await
        .expect("create read");
    read.mark_read();
    items.mark_read(read).await.expect("mark read");

    // Default query: hidden excluded.
    let default = items
        .list(
            profile,
            FeedQuery::new(100, false, false).expect("valid query"),
        )
        .await
        .expect("list default");
    assert_eq!(default.len(), 2);
    assert!(default
        .iter()
        .all(|i| i.visibility != FeedVisibility::Hidden));

    // include_hidden opts the hidden item back in.
    let with_hidden = items
        .list(
            profile,
            FeedQuery::new(100, true, false).expect("valid query"),
        )
        .await
        .expect("list with hidden");
    assert_eq!(with_hidden.len(), 3);

    // unread_only drops the read item (and still excludes hidden).
    let unread = items
        .list(
            profile,
            FeedQuery::new(100, false, true).expect("valid query"),
        )
        .await
        .expect("list unread");
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].feed_item_id, normal.feed_item_id);

    // Limit is honored.
    let limited = items
        .list(
            profile,
            FeedQuery::new(1, false, false).expect("valid query"),
        )
        .await
        .expect("list limited");
    assert_eq!(limited.len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn feed_list_orders_pinned_first() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let items = repos(&pool);

    let first = items
        .create(item(profile, FeedSource::NoteAdded, "first"))
        .await
        .expect("create first");
    let second = items
        .create(item(profile, FeedSource::NoteAdded, "second"))
        .await
        .expect("create second");

    // Pin the older item: it must float above the newer one.
    let mut pinned = first.clone();
    pinned.pin().expect("pin");
    items.set_visibility(pinned).await.expect("persist pin");

    let listed = items
        .list(
            profile,
            FeedQuery::new(100, false, false).expect("valid query"),
        )
        .await
        .expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].feed_item_id, first.feed_item_id);
    assert_eq!(listed[1].feed_item_id, second.feed_item_id);
    // The repository order already matches the domain comparator.
    let mut sorted = listed.clone();
    sorted.sort_by(compare_feed_items);
    assert_eq!(listed, sorted);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn feed_service_lifecycle_pin_hide_read() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let items = repos(&pool);
    let service = service(&pool);
    let ctx = ctx_for(profile);

    let created = items
        .create(item(profile, FeedSource::FormSubmitted, "intake arrived"))
        .await
        .expect("create");

    // Mark read (idempotent).
    let read = service
        .mark_read(created.feed_item_id, &ctx)
        .await
        .expect("mark read");
    assert!(read.read);
    let read_again = service
        .mark_read(created.feed_item_id, &ctx)
        .await
        .expect("mark read is idempotent");
    assert!(read_again.read);

    // Pin (idempotent).
    let pinned = service
        .pin_item(created.feed_item_id, &ctx)
        .await
        .expect("pin");
    assert_eq!(pinned.visibility, FeedVisibility::Pinned);

    // Hide is sticky.
    let hidden = service
        .hide_item(created.feed_item_id, &ctx)
        .await
        .expect("hide");
    assert_eq!(hidden.visibility, FeedVisibility::Hidden);

    // A hidden item is excluded from default lists.
    let listed = service
        .list_feed(
            FeedQuery::new(100, false, false).expect("valid query"),
            &ctx,
        )
        .await
        .expect("list feed");
    assert!(listed.is_empty());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn feed_service_rejects_pin_on_hidden_item() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let items = repos(&pool);
    let service = service(&pool);
    let ctx = ctx_for(profile);

    let created = items
        .create(item(profile, FeedSource::NoteAdded, "doomed"))
        .await
        .expect("create");
    service
        .hide_item(created.feed_item_id, &ctx)
        .await
        .expect("hide");

    let pinned = service.pin_item(created.feed_item_id, &ctx).await;
    assert!(matches!(pinned, Err(DomainError::Validation(_))));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn feed_service_enforces_scope_isolation() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;
    let items = repos(&pool);
    let service = service(&pool);
    let ctx_a = ctx_for(profile_a);
    let ctx_b = ctx_for(profile_b);

    let created = items
        .create(item(profile_a, FeedSource::EmailReceived, "a's mail"))
        .await
        .expect("create");

    // Profile B cannot read, pin, hide or mark-read profile A's item.
    for op in ["read", "pin", "hide"] {
        let denied = match op {
            "read" => service.mark_read(created.feed_item_id, &ctx_b).await,
            "pin" => service.pin_item(created.feed_item_id, &ctx_b).await,
            _ => service.hide_item(created.feed_item_id, &ctx_b).await,
        };
        assert!(
            matches!(denied, Err(DomainError::Forbidden(_))),
            "{op} must be forbidden across profiles"
        );
    }

    // Profile B's list is empty; profile A's list has the item.
    let listed_b = service
        .list_feed(
            FeedQuery::new(100, false, false).expect("valid query"),
            &ctx_b,
        )
        .await
        .expect("list b");
    assert!(listed_b.is_empty());
    let listed_a = service
        .list_feed(
            FeedQuery::new(100, false, false).expect("valid query"),
            &ctx_a,
        )
        .await
        .expect("list a");
    assert_eq!(listed_a.len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}
