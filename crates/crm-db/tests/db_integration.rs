// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the SQLx repositories.
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.:
//!
//! ```sh
//! docker compose up -d            # ephemeral Postgres 16 on :5432
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
//!     cargo test -p crm-db --all-features
//! ```
//!
//! CI's `test` job provides the same database via a Postgres 16 service
//! container. Each test creates its own database (`crm_t_<pid>_<n>`),
//! migrates a fresh schema into it, and drops it afterwards, so tests are
//! fully isolated and may run in parallel. A test database is only left
//! behind if its test panics; drop strays with
//! `DROP DATABASE "crm_t_<pid>_<n>"`.
//!
//! Database lifecycle helpers live in `common/` (shared with the
//! migration-verification tests).

mod common;

use chrono::{DateTime, SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_db::postgres::{publish_outbox, OutboxMessage, PgAuditRepository, PgContactRepository};
use crm_db::run_migrations;
use crm_domain::audit::{
    compute_chain_hash, verify_chain, ActionResult, ActorKind, AuditEvent, PermissionDecision,
    GENESIS_CHAIN_HASH,
};
use crm_domain::contact::{ConsentRecord, Contact, Lifecycle};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId, ContactId};
use crm_domain::repositories::{AuditRepository, ContactRepository};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (contacts and audit events reference it).
async fn ensure_profile(pool: &PgPool, id: ClientProfileId) {
    sqlx::query(
        "INSERT INTO client_profiles (client_profile_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(id.as_uuid())
    .execute(pool)
    .await
    .expect("insert client profile");
}

fn test_contact(profile: ClientProfileId) -> Contact {
    Contact {
        contact_id: ContactId::new(),
        client_profile_id: profile,
        normalized_emails: vec!["a@example.com".to_owned()],
        normalized_phones: vec!["+15550100".to_owned()],
        lifecycle: Lifecycle::Prospect,
        first_source: "web-form".to_owned(),
        latest_source: "web-form".to_owned(),
        source_touches: vec!["web-form".to_owned()],
        consent_records: vec![ConsentRecord {
            channel: "email".to_owned(),
            granted_at: DateTime::from_timestamp(1_700_000_000, 0).expect("fixed timestamp"),
            source: "web-form".to_owned(),
        }],
        merged_into: None,
    }
}

fn test_event(profile: ClientProfileId) -> AuditEvent {
    AuditEvent {
        event_id: AuditEventId::new(),
        // Postgres `timestamptz` stores microsecond precision; round the
        // fixture so round-trip equality holds.
        occurred_at: Utc::now().round_subsecs(6),
        actor_kind: ActorKind::Human,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        action: "contact.create".to_owned(),
        target_kind: "contact".to_owned(),
        target_id: "contact-1".to_owned(),
        correlation_id: Uuid::new_v4(),
        permission_decision: PermissionDecision::Allow,
        result: ActionResult::Success,
        before_ref: None,
        after_ref: Some("v1".to_owned()),
        chain_hash: [0u8; 32], // replaced by `chain`
    }
}

/// Links `event` to `prev`, mirroring what a service does before append.
fn chain(mut event: AuditEvent, prev: &[u8; 32]) -> AuditEvent {
    event.chain_hash = compute_chain_hash(prev, &event.canonical_bytes());
    event
}

#[tokio::test]
async fn migrations_are_rerunnable() {
    let (pool, admin_url, db_name) = fresh_db().await;
    // Applying twice must be a no-op, not an error.
    run_migrations(&pool)
        .await
        .expect("re-running migrations must succeed");
    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn contact_crud_round_trip() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgContactRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let contact = test_contact(profile);
    let stored = repo.create(contact.clone()).await.expect("create");
    assert_eq!(stored, contact, "create returns the stored contact");

    let fetched = repo.get(contact.contact_id).await.expect("get");
    assert_eq!(fetched, contact, "every domain field round-trips");
    assert_eq!(fetched.consent_records.len(), 1);

    let mut updated = fetched;
    updated.lifecycle = Lifecycle::Qualified;
    updated.latest_source = "phone-call".to_owned();
    repo.update(updated.clone()).await.expect("update");
    let refetched = repo
        .get(contact.contact_id)
        .await
        .expect("get after update");
    assert_eq!(refetched.lifecycle, Lifecycle::Qualified);
    assert_eq!(refetched.latest_source, "phone-call");

    let err = repo
        .get(ContactId::new())
        .await
        .expect_err("unknown id must be NotFound");
    assert!(matches!(err, DomainError::NotFound { .. }));

    let err = repo
        .update(test_contact(profile))
        .await
        .expect_err("updating a missing contact must be NotFound");
    assert!(matches!(err, DomainError::NotFound { .. }));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn contact_duplicate_id_is_conflict() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgContactRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let contact = test_contact(profile);
    repo.create(contact.clone()).await.expect("first create");
    let err = repo
        .create(contact)
        .await
        .expect_err("second create with the same id");
    assert!(matches!(err, DomainError::Conflict(_)));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn contact_list_is_tenant_scoped() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgContactRepository::new(pool.clone());
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;

    let contact_a = test_contact(profile_a);
    let contact_b = test_contact(profile_b);
    repo.create(contact_a.clone()).await.expect("create a");
    repo.create(contact_b.clone()).await.expect("create b");

    let listed = repo.list_for_client(profile_a, 20).await.expect("list");
    assert_eq!(listed.len(), 1, "no rows may leak across profiles");
    assert_eq!(listed[0].contact_id, contact_a.contact_id);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn find_by_normalized_email_matches_exact() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgContactRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    let other = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    ensure_profile(&pool, other).await;

    let contact = test_contact(profile);
    repo.create(contact.clone()).await.expect("create");

    let found = repo
        .find_by_normalized_email(profile, "a@example.com")
        .await
        .expect("lookup");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].contact_id, contact.contact_id);

    // Same email in another profile does not match: tenant scope holds.
    let cross = repo
        .find_by_normalized_email(other, "a@example.com")
        .await
        .expect("cross-profile lookup");
    assert!(cross.is_empty());

    let missing = repo
        .find_by_normalized_email(profile, "nobody@example.com")
        .await
        .expect("unknown email lookup");
    assert!(missing.is_empty());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn audit_chain_append_and_verify() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgAuditRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    assert_eq!(
        repo.latest_chain_hash().await.expect("empty head"),
        None,
        "empty log links from genesis"
    );

    let first = chain(test_event(profile), &GENESIS_CHAIN_HASH);
    repo.append(first.clone()).await.expect("append first");
    assert_eq!(
        repo.latest_chain_hash().await.expect("head"),
        Some(first.chain_hash)
    );

    let second = chain(test_event(profile), &first.chain_hash);
    repo.append(second.clone()).await.expect("append second");

    // Newest first per the trait contract.
    let events = repo.list_for_client(profile, 10).await.expect("list");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_id, second.event_id);
    assert_eq!(events[1].event_id, first.event_id);

    // The stored chain verifies from genesis (oldest first for verify).
    let oldest_first: Vec<AuditEvent> = events.into_iter().rev().collect();
    assert!(verify_chain(&oldest_first));

    let fetched = repo.get(first.event_id).await.expect("get");
    assert_eq!(fetched, first);

    let err = repo
        .get(AuditEventId::new())
        .await
        .expect_err("unknown event must be NotFound");
    assert!(matches!(err, DomainError::NotFound { .. }));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn audit_broken_link_is_rejected() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgAuditRepository::new(pool.clone());
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let first = chain(test_event(profile), &GENESIS_CHAIN_HASH);
    repo.append(first.clone()).await.expect("append first");

    // Forged link: claims genesis as predecessor although the head moved on.
    let forged = chain(test_event(profile), &GENESIS_CHAIN_HASH);
    let err = repo
        .append(forged)
        .await
        .expect_err("broken chain link must be rejected");
    assert!(matches!(err, DomainError::ConcurrencyConflict));

    // Duplicate event id with a valid chain still conflicts on the PK.
    let second = chain(test_event(profile), &first.chain_hash);
    repo.append(second.clone()).await.expect("append second");
    let mut dupe = test_event(profile);
    dupe.event_id = second.event_id;
    let dupe = chain(dupe, &second.chain_hash);
    let err = repo
        .append(dupe)
        .await
        .expect_err("duplicate event id must conflict");
    assert!(matches!(err, DomainError::Conflict(_)));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn outbox_insert_stays_unpublished() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;

    let message = OutboxMessage {
        id: Uuid::new_v4(),
        aggregate_type: "contact".to_owned(),
        aggregate_id: profile.as_uuid(),
        event_type: "contact.created".to_owned(),
        payload: serde_json::json!({ "source": "web-form" }),
    };
    // Staged inside a transaction, as a service would do alongside its
    // domain writes.
    let mut tx = pool.begin().await.expect("begin transaction");
    publish_outbox(&mut *tx, &message)
        .await
        .expect("stage outbox message");
    tx.commit().await.expect("commit transaction");

    let unpublished: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM outbox WHERE id = $1 AND published_at IS NULL")
            .bind(message.id)
            .fetch_one(&pool)
            .await
            .expect("read staged message");
    assert_eq!(unpublished, 1, "message waits for the relay");

    drop_db(&admin_url, &db_name, pool).await;
}
