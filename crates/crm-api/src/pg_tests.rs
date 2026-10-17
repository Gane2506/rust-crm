// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Postgres-backed API tests for the 02-DB-02 vertical slice.
//!
//! These tests drive the HTTP routes against a real PostgreSQL database
//! (one fresh migrated database per test, dropped afterwards) and prove
//! the production wiring: contacts persist, idempotent replays are served
//! from the `idempotency_keys` table, and creates record verifiable audit
//! events. They run wherever `DATABASE_URL` is set (CI's `test` job and
//! local runs with `docker compose up -d`); without it they skip loudly
//! instead of failing, so `cargo test` stays usable with no database.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use sqlx::PgPool;
use tower::ServiceExt;

use crm_core::IdempotencyKey;
use crm_db::postgres::{PgAuditRepository, PgContactRepository};
use crm_domain::audit::verify_chain;
use crm_domain::ids::{ClientProfileId, ContactId};
use crm_domain::repositories::{AuditRepository, ContactRepository};

use crate::contacts::{AppState, IDEMPOTENCY_KEY_HEADER};

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a fresh migrated database for one test.
///
/// Returns `None` when `DATABASE_URL` is not set; callers skip the
/// test in that case.
async fn fresh_db() -> Option<(PgPool, String, String)> {
    let admin_url = std::env::var(crm_db::DATABASE_URL_ENV).ok()?;
    let admin = crm_db::connect(&admin_url)
        .await
        .expect("connect to the admin database");
    let db_name = format!(
        "crm_api_t_{}_{}",
        std::process::id(),
        DB_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    sqlx::query(&format!("CREATE DATABASE \"{db_name}\""))
        .execute(&admin)
        .await
        .expect("create test database");
    admin.close().await;
    let (base, _) = admin_url
        .rsplit_once('/')
        .expect("DATABASE_URL must end with /<database>");
    let url = format!("{base}/{db_name}");
    let pool = crm_db::connect(&url)
        .await
        .expect("connect to test database");
    crm_db::run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    Some((pool, admin_url, db_name))
}

/// Drops a test database created by [`fresh_db`].
async fn drop_db(admin_url: &str, db_name: &str, pool: PgPool) {
    pool.close().await;
    let admin = crm_db::connect(admin_url)
        .await
        .expect("reconnect to the admin database");
    // `pool.close()` is graceful: a just-returned backend can still be
    // registered server-side, which makes DROP DATABASE fail with
    // 55006. Terminate stray backends first so the drop is
    // deterministic.
    sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind(db_name)
    .execute(&admin)
    .await
    .expect("terminate stray test-database backends");
    sqlx::query(&format!("DROP DATABASE IF EXISTS \"{db_name}\""))
        .execute(&admin)
        .await
        .expect("drop test database");
    admin.close().await;
}

fn pg_router(pool: &PgPool) -> axum::Router {
    crate::app(AppState::from_pool(pool), None)
}

fn contact_body(profile: uuid::Uuid) -> serde_json::Value {
    serde_json::json!({
        "client_profile_id": profile,
        "emails": ["Test@Example.COM "],
        "phones": [" +1-555-0100 "],
        "source": "web-form"
    })
}

fn post_contacts(body: serde_json::Value, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/contacts")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

fn get(uri: String) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("test request builds")
}

async fn read_json(response: Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

#[tokio::test]
async fn pg_create_get_list_round_trip() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();

    let (status, created) = read_json(
        router
            .clone()
            .oneshot(post_contacts(contact_body(profile), None))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["contact_id"]
        .as_str()
        .expect("contact id")
        .to_owned();
    assert_eq!(
        created["normalized_emails"],
        serde_json::json!(["test@example.com"])
    );

    // Read back through the repository: the contact is really in Postgres.
    let stored = PgContactRepository::new(pool.clone())
        .get(ContactId::from(
            id.parse::<uuid::Uuid>().expect("contact id parses"),
        ))
        .await
        .expect("contact persisted in Postgres");
    assert_eq!(stored.contact_id.to_string(), id);

    let (status, fetched) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/contacts/{id}")))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["contact_id"], created["contact_id"]);

    let (status, listed) = read_json(
        router
            .oneshot(get(format!("/api/v1/contacts?client_profile_id={profile}")))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["contacts"].as_array().expect("array").len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_idempotent_post_replays_without_duplicating() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let key = IdempotencyKey::generate().to_string();
    let body = contact_body(profile);

    let (first_status, first) = read_json(
        router
            .clone()
            .oneshot(post_contacts(body.clone(), Some(&key)))
            .await
            .expect("oneshot"),
    )
    .await;
    let (second_status, second) = read_json(
        router
            .clone()
            .oneshot(post_contacts(body, Some(&key)))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first, second, "replay returns the identical response");

    let (_, listed) = read_json(
        router
            .oneshot(get(format!("/api/v1/contacts?client_profile_id={profile}")))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(listed["total"], 1, "one record despite two posts");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_idempotency_replay_survives_new_state() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let profile = uuid::Uuid::new_v4();
    let key = IdempotencyKey::generate().to_string();
    let body = contact_body(profile);

    // First request against one application state ...
    let (_, first) = read_json(
        pg_router(&pool)
            .oneshot(post_contacts(body.clone(), Some(&key)))
            .await
            .expect("oneshot"),
    )
    .await;

    // ... replayed against a *fresh* state over the same database: the
    // idempotency record lives in Postgres, not in process memory.
    let (status, second) = read_json(
        pg_router(&pool)
            .oneshot(post_contacts(body, Some(&key)))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(first, second, "replay works across state instances");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_duplicate_email_returns_409() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let body = contact_body(profile);

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_contacts(body.clone(), None))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, problem) = read_json(
        router
            .oneshot(post_contacts(body, None))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["title"], "Conflict");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_create_records_verifiable_audit_chain() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();

    let (status, _) = read_json(
        router
            .oneshot(post_contacts(contact_body(profile), None))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let events = PgAuditRepository::new(pool.clone())
        .list_for_client(ClientProfileId::from(profile), 10)
        .await
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "contact.create");
    assert!(verify_chain(&events), "single event links from genesis");

    drop_db(&admin_url, &db_name, pool).await;
}
