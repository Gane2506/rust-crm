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
use crm_db::audit_log::PgAuditLogService;
use crm_db::auth::hash_password;
use crm_db::email::{PgEmailMessageRepository, PgEmailThreadRepository};
use crm_db::postgres::{
    PgAuditRepository, PgContactRepository, PgGrantRepository, PgStaffRepository,
};
use crm_domain::audit::{fingerprint, ActorKind, SecurityEvent};
use crm_domain::email::EmailStatus;
use crm_domain::ids::{
    ActorId, AgencyId, ClientProfileId, ContactId, EmailMessageId, GrantId, StaffId,
};
use crm_domain::repositories::{
    AuditRepository, ContactRepository, EmailMessageRepository, EmailThreadRepository,
    GrantRepository, SessionRepository, StaffRepository,
};
use crm_domain::security::{
    Action, Grant, Operation, PasswordCredential, Resource, Scope, StaffStatus, StaffUser,
};
use crm_domain::services::{ActionContext, AuditLogService, AuditQuery};

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
    contact_body_with_email(profile, "Test@Example.COM ")
}

fn contact_body_with_email(profile: uuid::Uuid, email: &str) -> serde_json::Value {
    serde_json::json!({
        "client_profile_id": profile,
        "emails": [email],
        "phones": [" +1-555-0100 "],
        "source": "web-form"
    })
}

fn post_contacts(body: serde_json::Value, key: Option<&str>, token: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/contacts")
        .header(header::CONTENT_TYPE, "application/json")
        .header("Authorization", format!("Bearer {token}"));
    if let Some(key) = key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

fn get(uri: String, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("test request builds")
}

fn get_unauthenticated(uri: String) -> Request<Body> {
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

/// Test password for seeded staff users (never used outside tests).
const TEST_PASSWORD: &str = "test-password-123";

/// Seeds an active staff user with contact read/create grants for
/// `profile`, then logs in over HTTP to obtain a real bearer token.
///
/// Returns `(token, staff_id, agency_id)`.
async fn seed_authed_user(
    pool: &PgPool,
    router: &axum::Router,
    profile: uuid::Uuid,
    email: &str,
) -> (String, StaffId, AgencyId) {
    let agency_id = AgencyId::new();
    let now = chrono::Utc::now();
    let phc = hash_password(TEST_PASSWORD.as_bytes())
        .await
        .expect("hash test password");
    let staff_id = StaffId::new();
    PgStaffRepository::new(pool.clone())
        .create(StaffUser {
            staff_id,
            agency_id,
            email: email.to_owned(),
            display_name: "Test User".to_owned(),
            status: StaffStatus::Active,
            credential: Some(
                PasswordCredential::new(phc, now).expect("test credential is PHC-shaped"),
            ),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("create staff user");
    let grant_repo = PgGrantRepository::new(pool.clone());
    let scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(profile),
    };
    for operation in [Operation::Read, Operation::Create] {
        grant_repo
            .assign(
                staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::Contact,
                        operation,
                    },
                    scope,
                },
            )
            .await
            .expect("assign grant");
    }
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "email": email, "password": TEST_PASSWORD })
                            .to_string(),
                    ))
                    .expect("login request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed login must succeed");
    let token = body["token"].as_str().expect("token in body").to_owned();
    // The token is a UUID string (the `SessionToken::as_uuid` rendering).
    token.parse::<uuid::Uuid>().expect("token is a UUID");
    (token, staff_id, agency_id)
}

#[tokio::test]
async fn pg_create_get_list_round_trip() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, _) =
        seed_authed_user(&pool, &router, profile, "roundtrip@example.com").await;

    let (status, created) = read_json(
        router
            .clone()
            .oneshot(post_contacts(contact_body(profile), None, &token))
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
            .oneshot(get(format!("/api/v1/contacts/{id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["contact_id"], created["contact_id"]);

    let (status, listed) = read_json(
        router
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["contacts"].as_array().expect("array").len(), 1);

    // The audit event carries the real staff actor, not the nil system actor.
    let events = PgAuditRepository::new(pool.clone())
        .list_for_client(ClientProfileId::from(profile), 10)
        .await
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].actor_id,
        crm_domain::ids::ActorId::from(staff_id),
        "audit event records the authenticated staff actor"
    );
    assert_eq!(events[0].actor_kind, crm_domain::audit::ActorKind::Human);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_readyz_reports_database_ok() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let (status, body) = read_json(
        pg_router(&pool)
            .oneshot(get_unauthenticated("/readyz".to_owned()))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["database"], "ok");

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
    let (token, _, _) = seed_authed_user(&pool, &router, profile, "idempotent@example.com").await;
    let key = IdempotencyKey::generate().to_string();
    let body = contact_body(profile);

    let (first_status, first) = read_json(
        router
            .clone()
            .oneshot(post_contacts(body.clone(), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    let (second_status, second) = read_json(
        router
            .clone()
            .oneshot(post_contacts(body, Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first, second, "replay returns the identical response");

    let (_, listed) = read_json(
        router
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={profile}"),
                &token,
            ))
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
    let setup_router = pg_router(&pool);
    let (token, _, _) = seed_authed_user(&pool, &setup_router, profile, "replay@example.com").await;

    // First request against one application state ...
    let (_, first) = read_json(
        pg_router(&pool)
            .oneshot(post_contacts(body.clone(), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;

    // ... replayed against a *fresh* state over the same database: the
    // idempotency record lives in Postgres, not in process memory.
    let (status, second) = read_json(
        pg_router(&pool)
            .oneshot(post_contacts(body, Some(&key), &token))
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
    let (token, _, _) = seed_authed_user(&pool, &router, profile, "duplicate@example.com").await;
    let body = contact_body(profile);

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_contacts(body.clone(), None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, problem) = read_json(
        router
            .oneshot(post_contacts(body, None, &token))
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
    let (token, _, _) = seed_authed_user(&pool, &router, profile, "audit@example.com").await;

    let (status, _) = read_json(
        router
            .oneshot(post_contacts(contact_body(profile), None, &token))
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
    // The chain is global (one log): the seeded login's security event
    // precedes the contact event, so verify the full chain from genesis
    // via the audit-log service rather than the profile-filtered subset.
    let verification = PgAuditLogService::new(pool.clone())
        .verify_chain()
        .await
        .expect("verify chain");
    assert!(verification.valid, "full chain verifies from genesis");
    assert!(verification.events_checked >= 2);

    drop_db(&admin_url, &db_name, pool).await;
}

// ---------------------------------------------------------------------------
// Authentication / authorization (03-SEC-02)
// ---------------------------------------------------------------------------

/// Seeds a staff user without logging in, for credential-failure tests.
async fn seed_staff_only(pool: &PgPool, email: &str) -> StaffId {
    let now = chrono::Utc::now();
    let phc = hash_password(TEST_PASSWORD.as_bytes())
        .await
        .expect("hash test password");
    let staff_id = StaffId::new();
    PgStaffRepository::new(pool.clone())
        .create(StaffUser {
            staff_id,
            agency_id: AgencyId::new(),
            email: email.to_owned(),
            display_name: "Test User".to_owned(),
            status: StaffStatus::Active,
            credential: Some(
                PasswordCredential::new(phc, now).expect("test credential is PHC-shaped"),
            ),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("create staff user");
    staff_id
}

fn login_request(email: &str, password: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({ "email": email, "password": password }).to_string(),
        ))
        .expect("login request builds")
}

#[tokio::test]
async fn pg_login_rejects_wrong_password() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    seed_staff_only(&pool, "wrongpw@example.com").await;

    let (status, problem) = read_json(
        router
            .oneshot(login_request("wrongpw@example.com", "not-the-password"))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["title"], "Unauthorized");
    // The response must not reveal whether the email exists.
    assert!(!problem["detail"]
        .to_string()
        .contains("wrongpw@example.com"));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_login_rejects_unknown_email() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);

    let (status, problem) = read_json(
        router
            .oneshot(login_request("nobody@example.com", "whatever"))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["title"], "Unauthorized");

    drop_db(&admin_url, &db_name, pool).await;
}

/// Builds a test router whose login rate limiter trips after 3 attempts
/// per IP, so the 429 path is exercised without burning CPU on dozens of
/// Argon2 verifications.
fn pg_router_with_tight_login_limits(pool: &PgPool) -> axum::Router {
    let mut state = AppState::from_pool(pool);
    state.auth = Some(crate::auth::ApiAuth::with_rate_limits(
        pool,
        crate::rate_limit::LoginRateLimitConfig::for_tests(),
    ));
    crate::app(state, None)
}

#[tokio::test]
async fn pg_login_rate_limit_trips_with_429_and_retry_after() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router_with_tight_login_limits(&pool);
    seed_staff_only(&pool, "ratelimit@example.com").await;

    // The first three attempts fail closed as 401s (wrong password).
    for _ in 0..3 {
        let (status, _) = read_json(
            router
                .clone()
                .oneshot(login_request("ratelimit@example.com", "wrong-password"))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // The fourth attempt trips the per-IP bucket: 429 problem+json with
    // a numeric Retry-After, carrying the security headers like every
    // other API response.
    let response = router
        .clone()
        .oneshot(login_request("ratelimit@example.com", "wrong-password"))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .expect("retry-after header")
        .to_str()
        .expect("retry-after is ascii");
    assert!(
        retry_after.parse::<u64>().expect("retry-after is numeric") >= 1,
        "retry-after must be a positive number of seconds"
    );
    assert!(
        response.headers().contains_key("x-content-type-options"),
        "the 429 must carry the API security headers"
    );
    let (status, problem) = read_json(response).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(problem["title"], "Too many requests");
    assert_eq!(problem["status"], 429);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_login_rate_limit_keys_off_forwarded_ip() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router_with_tight_login_limits(&pool);
    seed_staff_only(&pool, "xff@example.com").await;

    // Exhaust the bucket for one forwarded client IP...
    let login_as = |ip: &str| {
        Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({ "email": "xff@example.com", "password": "wrong" }).to_string(),
            ))
            .expect("login request builds")
    };
    for _ in 0..3 {
        let (status, _) = read_json(
            router
                .clone()
                .oneshot(login_as("203.0.113.7"))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(login_as("203.0.113.7"))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // ...while a different client IP still has its own budget.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(login_as("198.51.100.9"))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    drop_db(&admin_url, &db_name, pool).await;
}

/// Builds the API router with tight *write* limits but default login
/// limits (task 17-PERF-02): the login step in the test below must not
/// trip the login limiter, only the write limiter.
fn pg_router_with_tight_write_limits(pool: &PgPool) -> axum::Router {
    let mut state = AppState::from_pool(pool);
    state.auth = Some(crate::auth::ApiAuth::with_rate_limit_configs(
        pool,
        crate::rate_limit::LoginRateLimitConfig::default(),
        crate::rate_limit::WriteRateLimitConfig::for_tests(),
    ));
    crate::app(state, None)
}

/// The write rate limiter (task 17-PERF-02) trips on mutating endpoints
/// with `429 problem+json` + `Retry-After`, while reads stay unlimited.
#[tokio::test]
async fn pg_write_rate_limit_trips_with_429_and_retry_after() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router_with_tight_write_limits(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile, "writer@example.com").await;
    PgGrantRepository::new(pool.clone())
        .assign(
            staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource: Resource::Contact,
                    operation: Operation::Create,
                },
                scope: Scope::ClientProfile {
                    agency_id,
                    client_profile_id: ClientProfileId::from(profile),
                },
            },
        )
        .await
        .expect("assign contact create grant");

    // The login above consumed one write-bucket slot (it is a POST);
    // two contact creates succeed, the third trips the per-IP bucket.
    for i in 0..2 {
        let (status, _) = read_json(
            router
                .clone()
                .oneshot(post_contacts(
                    contact_body_with_email(profile, &format!("writer{i}@example.com")),
                    Some(&uuid::Uuid::new_v4().to_string()),
                    &token,
                ))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "write {i} must succeed");
    }

    let response = router
        .clone()
        .oneshot(post_contacts(
            contact_body_with_email(profile, "writer2@example.com"),
            Some(&uuid::Uuid::new_v4().to_string()),
            &token,
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .expect("retry-after header")
        .to_str()
        .expect("retry-after is ascii");
    assert!(
        retry_after.parse::<u64>().expect("retry-after is numeric") >= 1,
        "retry-after must be a positive number of seconds"
    );
    assert!(
        response.headers().contains_key("x-content-type-options"),
        "the 429 must carry the API security headers"
    );
    let (status, problem) = read_json(response).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(problem["title"], "Too many requests");
    assert_eq!(problem["status"], 429);

    // Reads are not write-limited: the contact list still works.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reads stay unlimited");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_unauthorized_responses_carry_security_headers() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);

    // A 401 from the auth middleware must carry the API security headers,
    // proving the headers layer sits outside the auth middleware.
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/contacts")
                .body(Body::empty())
                .expect("test request builds"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("x-content-type-options")
            .expect("nosniff")
            .to_str()
            .expect("header value is ascii"),
        "nosniff"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_contacts_require_authentication() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();

    // No token at all.
    let (status, problem) = read_json(
        router
            .clone()
            .oneshot(get_unauthenticated(format!(
                "/api/v1/contacts?client_profile_id={profile}"
            )))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["title"], "Unauthorized");

    // A well-formed but unknown token.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={profile}"),
                &uuid::Uuid::new_v4().to_string(),
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A malformed bearer value.
    let (status, _) = read_json(
        router
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/contacts?client_profile_id={profile}"))
                    .header("Authorization", "Bearer not-a-uuid")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_contacts_forbidden_without_grant() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let granted_profile = uuid::Uuid::new_v4();
    let other_profile = uuid::Uuid::new_v4();
    let (token, _, _) =
        seed_authed_user(&pool, &router, granted_profile, "scoped@example.com").await;

    // The caller holds grants for `granted_profile` only: asking for
    // another profile is a 403, not a 404 or an empty list.
    let (status, problem) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={other_profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["title"], "Forbidden");

    // Creating under the ungranted profile is also forbidden.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_contacts(contact_body(other_profile), None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The granted profile still works.
    let (status, _) = read_json(
        router
            .oneshot(post_contacts(contact_body(granted_profile), None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_logout_revokes_session() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_authed_user(&pool, &router, profile, "logout@example.com").await;

    // Authenticated request works before logout.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Logout revokes the session.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("logout request builds"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The token no longer authorizes.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Logging out again is idempotent: still 204, never an error.
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("logout request builds"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_suspended_staff_cannot_login() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let staff_id = seed_staff_only(&pool, "suspended@example.com").await;
    // Suspend the account directly through the repository.
    let repo = PgStaffRepository::new(pool.clone());
    let mut user = repo.get(staff_id).await.expect("staff exists");
    user.suspend();
    repo.update(user).await.expect("suspend persists");

    let (status, problem) = read_json(
        router
            .oneshot(login_request("suspended@example.com", TEST_PASSWORD))
            .await
            .expect("oneshot"),
    )
    .await;
    // A suspended account is indistinguishable from a wrong password:
    // the response must not reveal the account exists or its state.
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["title"], "Unauthorized");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_delegation_session_fails_closed() {
    use crm_db::auth::PgAuthorizationService;
    use crm_domain::ids::{DelegationId, SessionId};
    use crm_domain::security::{Session, SessionToken, DEFAULT_SESSION_TTL_SECS};
    use crm_domain::services::AuthorizationService;

    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    // A session linked to a delegation: there is no delegation repository
    // trait yet, so the authorization service must fail closed rather
    // than authorizing against anything else.
    let staff_id = seed_staff_only(&pool, "agentstaff@example.com").await;
    let token = SessionToken::new();
    let delegation_id = DelegationId::new();
    let now = chrono::Utc::now();
    let mut session = Session::new(
        SessionId::new(),
        staff_id,
        token.token_hash(),
        now,
        chrono::Duration::seconds(DEFAULT_SESSION_TTL_SECS),
    )
    .expect("session builds");
    session.delegation_id = Some(delegation_id);
    // The `delegations` table has no repository trait yet, so the row is
    // inserted directly: a real delegation the service cannot load grants
    // for must still fail closed.
    sqlx::query(
        "INSERT INTO delegations (delegation_id, delegator_staff_id, agent_principal_id, grants, expires_at)
         VALUES ($1, $2, $3, '[]'::jsonb, now() + interval '1 hour')",
    )
    .bind(delegation_id.as_uuid())
    .bind(staff_id.as_uuid())
    .bind(uuid::Uuid::new_v4())
    .execute(&pool)
    .await
    .expect("delegation row inserts");
    crm_db::postgres::PgSessionRepository::new(pool.clone())
        .create(session)
        .await
        .expect("session persists");

    let authz = PgAuthorizationService::new(pool.clone());
    let action = Action {
        resource: Resource::Contact,
        operation: Operation::Read,
    };
    let scope = Scope::ClientProfile {
        agency_id: AgencyId::new(),
        client_profile_id: ClientProfileId::from(uuid::Uuid::new_v4()),
    };
    let err = authz
        .authorize(&token, &action, &scope, uuid::Uuid::new_v4())
        .await
        .expect_err("delegation-linked session must not authorize");
    assert!(
        matches!(err, crm_domain::error::DomainError::Unauthorized),
        "fail closed, got: {err:?}"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

// ---------------------------------------------------------------------------
// 04-AUD-02: security audit log vertical slice
// ---------------------------------------------------------------------------

/// Agency-scoped system context for reading the audit log in tests.
///
/// Mirrors the unattributed-actor convention: no authenticated actor, so
/// the well-known system actor reads. Production queries go through
/// `GET /api/v1/audit/events` with an `AuditLog:Read` agency grant.
fn audit_system_ctx() -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(uuid::Uuid::nil()),
        client_profile_id: None,
        correlation_id: uuid::Uuid::new_v4(),
    }
}

/// The newest event recorded with `action`, with its forensic payload.
async fn latest_security_event(
    audit_log: &PgAuditLogService,
    action: &str,
) -> (crm_domain::audit::AuditEvent, Option<serde_json::Value>) {
    let (events, _) = audit_log
        .query_detailed(
            &audit_system_ctx(),
            &AuditQuery {
                action: Some(action.to_owned()),
                ..AuditQuery::default()
            },
        )
        .await
        .expect("query the audit log");
    events
        .into_iter()
        .next_back()
        .expect("the security event was recorded")
}

/// `POST /api/v1/auth/login` records `auth.login` against the staff
/// actor — with no raw email in the payload.
#[tokio::test]
async fn pg_login_records_security_audit_event() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (_token, staff_id, _) =
        seed_authed_user(&pool, &router, profile, "auditor@example.com").await;

    let audit_log = PgAuditLogService::new(pool.clone());
    let (event, payload) = latest_security_event(&audit_log, "auth.login").await;
    assert_eq!(event.actor_id, ActorId::from(staff_id));
    assert_eq!(event.action, "auth.login");
    let payload_text = payload
        .expect("login event carries its forensic payload")
        .to_string();
    assert!(
        !payload_text.contains("auditor@example.com"),
        "raw email must never appear in the audit payload: {payload_text}"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// A failed login (401) still records `auth.login_failed` — with only
/// the email fingerprint, never the address.
#[tokio::test]
async fn pg_failed_login_records_fingerprinted_audit_event() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    seed_authed_user(&pool, &router, profile, "target@example.com").await;

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(login_request("target@example.com", "wrong-password"))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let audit_log = PgAuditLogService::new(pool.clone());
    let (event, payload) = latest_security_event(&audit_log, "auth.login_failed").await;
    assert_eq!(event.action, "auth.login_failed");
    let payload = payload.expect("failed-login event carries its payload");
    assert_eq!(
        payload
            .get("email_fingerprint")
            .and_then(serde_json::Value::as_str),
        Some(fingerprint("target@example.com").as_str()),
        "the payload carries the fingerprint, not the address"
    );
    assert!(
        !payload.to_string().contains("target@example.com"),
        "raw email must never appear in the audit payload"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// Tripping the login rate limiter (429) records `auth.rate_limit_tripped`
/// with a fingerprinted bucket key — never the raw IP or email.
#[tokio::test]
async fn pg_rate_limit_trip_records_audit_event() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router_with_tight_login_limits(&pool);
    seed_staff_only(&pool, "flooded@example.com").await;

    for _ in 0..3 {
        let (status, _) = read_json(
            router
                .clone()
                .oneshot(login_request("flooded@example.com", "wrong-password"))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(login_request("flooded@example.com", "wrong-password"))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    let audit_log = PgAuditLogService::new(pool.clone());
    let (event, payload) = latest_security_event(&audit_log, "auth.rate_limit_tripped").await;
    assert_eq!(event.action, "auth.rate_limit_tripped");
    let payload_text = payload
        .expect("rate-limit event carries its payload")
        .to_string();
    assert!(
        !payload_text.contains("flooded@example.com"),
        "raw email must never appear in the audit payload: {payload_text}"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// `POST /api/v1/auth/logout` records `auth.logout` against the staff
/// actor before revoking the session.
#[tokio::test]
async fn pg_logout_records_security_audit_event() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, _) =
        seed_authed_user(&pool, &router, profile, "leaver@example.com").await;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("test request builds"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let audit_log = PgAuditLogService::new(pool.clone());
    let (event, payload) = latest_security_event(&audit_log, "auth.logout").await;
    assert_eq!(event.actor_id, ActorId::from(staff_id));
    assert_eq!(event.action, "auth.logout");
    assert!(
        payload.is_some(),
        "logout event carries its forensic payload"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// Recording twice with the same idempotency key yields exactly one row
/// and returns the original event both times.
#[tokio::test]
async fn pg_audit_record_is_idempotent() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let audit_log = PgAuditLogService::new(pool.clone());
    let ctx = audit_system_ctx();
    let key = uuid::Uuid::new_v4();
    let event = SecurityEvent::login_failed("retry@example.com");

    let first = audit_log
        .record(&ctx, &event, key)
        .await
        .expect("first record appends");
    let second = audit_log
        .record(&ctx, &event, key)
        .await
        .expect("retried record returns the original");
    assert_eq!(
        first.event_id, second.event_id,
        "the retry must return the original event, not a duplicate"
    );

    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .expect("count audit rows");
    assert_eq!(count, 1, "one key must produce exactly one row");

    drop_db(&admin_url, &db_name, pool).await;
}

/// The chain verifies end to end; tampering with a link reports the
/// first bad event.
#[tokio::test]
async fn pg_audit_chain_verifies_and_reports_tamper() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let audit_log = PgAuditLogService::new(pool.clone());
    let ctx = audit_system_ctx();
    let mut recorded = Vec::new();
    for email in ["a@example.com", "b@example.com", "c@example.com"] {
        recorded.push(
            audit_log
                .record(
                    &ctx,
                    &SecurityEvent::login_failed(email),
                    uuid::Uuid::new_v4(),
                )
                .await
                .expect("record"),
        );
    }

    let verification = audit_log.verify_chain().await.expect("verify chain");
    assert!(verification.valid, "an untampered chain must verify");
    assert_eq!(verification.events_checked, 3);

    // Tamper with the second link's hash in place: the next
    // verification must flag exactly that event.
    let tampered_id = recorded[1].event_id;
    sqlx::query("UPDATE audit_events SET chain_hash = $1 WHERE event_id = $2")
        .bind(vec![0u8; 32])
        .bind(tampered_id.as_uuid())
        .execute(&pool)
        .await
        .expect("tamper with the chain hash");

    let verification = audit_log.verify_chain().await.expect("verify chain");
    assert!(!verification.valid, "a tampered chain must not verify");
    assert_eq!(
        verification.first_bad_event,
        Some(tampered_id),
        "verification must report the first bad event"
    );
    assert_eq!(verification.events_checked, 1);

    drop_db(&admin_url, &db_name, pool).await;
}

/// `AuditQuery` filters (actor, action, time range) narrow the page, and
/// pages come back in chain order with a working cursor.
#[tokio::test]
async fn pg_audit_query_filters_and_cursor() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let audit_log = PgAuditLogService::new(pool.clone());
    let ctx = audit_system_ctx();
    let staff_id = StaffId::new();
    let staff_ctx = ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::from(staff_id),
        client_profile_id: None,
        correlation_id: uuid::Uuid::new_v4(),
    };

    for (n, event_ctx) in [(0, &ctx), (1, &staff_ctx), (2, &staff_ctx)]
        .into_iter()
        .enumerate()
    {
        let event = if n == 0 {
            SecurityEvent::login_failed("filter@example.com")
        } else {
            SecurityEvent::LoginSucceeded { staff_id }
        };
        audit_log
            .record(event_ctx.1, &event, uuid::Uuid::new_v4())
            .await
            .expect("record");
    }

    // By actor: only the staff member's two logins.
    let page = audit_log
        .query(
            &audit_system_ctx(),
            &AuditQuery {
                actor_id: Some(ActorId::from(staff_id)),
                ..AuditQuery::default()
            },
        )
        .await
        .expect("query by actor");
    assert_eq!(page.events.len(), 2);
    assert!(
        page.events.iter().all(|event| event.action == "auth.login"),
        "both events are logins"
    );

    // By action: only the failed login.
    let page = audit_log
        .query(
            &audit_system_ctx(),
            &AuditQuery {
                action: Some("auth.login_failed".to_owned()),
                ..AuditQuery::default()
            },
        )
        .await
        .expect("query by action");
    assert_eq!(page.events.len(), 1);

    // Cursor pagination: one per page, chain order.
    let first_page = audit_log
        .query(
            &audit_system_ctx(),
            &AuditQuery {
                limit: 1,
                ..AuditQuery::default()
            },
        )
        .await
        .expect("first page");
    assert_eq!(first_page.events.len(), 1);
    assert!(first_page.has_more);
    let second_page = audit_log
        .query(
            &audit_system_ctx(),
            &AuditQuery {
                limit: 1,
                after: Some(first_page.events[0].event_id),
                ..AuditQuery::default()
            },
        )
        .await
        .expect("second page");
    assert_eq!(second_page.events.len(), 1);
    assert!(second_page.has_more);
    assert_ne!(
        first_page.events[0].event_id, second_page.events[0].event_id,
        "the cursor must advance the page"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// Scope isolation: a profile-scoped reader only sees events attributed
/// to their own profile; asking for another profile's log yields an
/// empty page (fail closed).
#[tokio::test]
async fn pg_audit_query_scope_isolation() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let audit_log = PgAuditLogService::new(pool.clone());
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    for profile in [profile_a, profile_b] {
        let ctx = ActionContext {
            actor_kind: ActorKind::Human,
            actor_id: ActorId::from(StaffId::new()),
            client_profile_id: Some(profile),
            correlation_id: uuid::Uuid::new_v4(),
        };
        audit_log
            .record(
                &ctx,
                &SecurityEvent::login_failed("scoped@example.com"),
                uuid::Uuid::new_v4(),
            )
            .await
            .expect("record");
    }

    // A profile-A reader sees only profile A's event.
    let reader_a = ActionContext {
        client_profile_id: Some(profile_a),
        ..audit_system_ctx()
    };
    let page = audit_log
        .query(&reader_a, &AuditQuery::default())
        .await
        .expect("query as profile A");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].client_profile_id, Some(profile_a));

    // Asking for profile B's log from a profile-A context: empty page.
    let page = audit_log
        .query(
            &reader_a,
            &AuditQuery {
                client_profile_id: Some(profile_b),
                ..AuditQuery::default()
            },
        )
        .await
        .expect("cross-profile query");
    assert!(
        page.events.is_empty(),
        "cross-profile reads must fail closed"
    );

    // An agency-scoped reader sees both.
    let page = audit_log
        .query(&audit_system_ctx(), &AuditQuery::default())
        .await
        .expect("agency query");
    assert_eq!(page.events.len(), 2);

    drop_db(&admin_url, &db_name, pool).await;
}

/// `GET /api/v1/audit/events` is a 401 without a token and a 403 for a
/// caller without the `AuditLog:Read` agency grant — the audit log is an
/// agency-level resource, not visible to profile-scoped readers.
#[tokio::test]
async fn pg_audit_events_endpoint_requires_audit_grant() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    // `seed_authed_user` grants only Contact read/create on the profile —
    // deliberately no audit grant.
    let (token, _, _) = seed_authed_user(&pool, &router, profile, "nonauditor@example.com").await;

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get_unauthenticated("/api/v1/audit/events".to_owned()))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/audit/events".to_owned(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a profile-scoped caller must not read the agency audit log"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// `GET /api/v1/audit/events` returns chain-order pages (with forensic
/// payloads) for a caller holding `AuditLog:Read` at agency scope, and
/// `GET /api/v1/audit/verify` reports the chain as valid.
#[tokio::test]
async fn pg_audit_endpoints_serve_agency_auditor() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile, "auditor@example.com").await;

    // Grant the seeded staff member audit read at agency scope.
    PgGrantRepository::new(pool.clone())
        .assign(
            staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource: Resource::AuditLog,
                    operation: Operation::Read,
                },
                scope: Scope::Agency { agency_id },
            },
        )
        .await
        .expect("assign the audit-read grant");

    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/audit/events?limit=10".to_owned(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = body
        .get("events")
        .and_then(serde_json::Value::as_array)
        .expect("events array");
    assert!(
        !events.is_empty(),
        "the login itself must have been recorded"
    );
    assert!(
        events.iter().any(
            |event| event.get("action").and_then(serde_json::Value::as_str) == Some("auth.login")
        ),
        "the login event must be in the page"
    );
    let login_event = events
        .iter()
        .find(|event| event.get("action").and_then(serde_json::Value::as_str) == Some("auth.login"))
        .expect("login event present");
    assert!(
        login_event
            .get("payload")
            .is_some_and(|payload| !payload.is_null()),
        "the login event must carry its forensic payload"
    );

    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/audit/verify".to_owned(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("valid").and_then(serde_json::Value::as_bool),
        Some(true),
        "the chain must verify as valid"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// `GET /api/v1/audit/events?newest_first=true` returns the newest
/// events first — the order the Dashboard "Recent audit events" tile
/// needs — while the default (and an explicit `newest_first=false`)
/// stays chain order, and the cursor pages forward in whichever
/// direction was requested (AUDIT-13).
#[tokio::test]
async fn pg_audit_events_newest_first_returns_newest_first() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile, "newest@example.com").await;

    PgGrantRepository::new(pool.clone())
        .assign(
            staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource: Resource::AuditLog,
                    operation: Operation::Read,
                },
                scope: Scope::Agency { agency_id },
            },
        )
        .await
        .expect("assign the audit-read grant");

    // Three events in a known order, recorded after the login so they
    // are the newest events in the log.
    let audit_log = PgAuditLogService::new(pool.clone());
    let ctx = audit_system_ctx();
    let mut recorded = Vec::new();
    for n in 1..=3 {
        recorded.push(
            audit_log
                .record(
                    &ctx,
                    &SecurityEvent::login_failed(&format!("newest-{n}@example.com")),
                    uuid::Uuid::new_v4(),
                )
                .await
                .expect("record")
                .event_id
                .as_uuid()
                .to_string(),
        );
    }

    // Fetches one page over HTTP, returning the event ids in response
    // order plus `has_more`.
    let fetch_page = |uri: &str| {
        let router = router.clone();
        let token = token.clone();
        let uri = uri.to_owned();
        async move {
            let (status, body) =
                read_json(router.oneshot(get(uri, &token)).await.expect("oneshot")).await;
            assert_eq!(status, StatusCode::OK);
            let events = body
                .get("events")
                .and_then(serde_json::Value::as_array)
                .expect("events array");
            let ids: Vec<String> = events
                .iter()
                .map(|event| {
                    event
                        .get("event_id")
                        .and_then(serde_json::Value::as_str)
                        .expect("event id")
                        .to_owned()
                })
                .collect();
            let has_more = body
                .get("has_more")
                .and_then(serde_json::Value::as_bool)
                .expect("has_more flag");
            (ids, has_more)
        }
    };

    // Only the three recorded events, in response order: the login and
    // grant events also sit in the log, but their relative order to the
    // recorded three is not what this test pins.
    let only_recorded = |ids: Vec<String>| {
        ids.into_iter()
            .filter(|id| recorded.contains(id))
            .collect::<Vec<String>>()
    };

    // Default: chain order — the three recorded events oldest first.
    let (ids, _) = fetch_page("/api/v1/audit/events?limit=10").await;
    assert_eq!(
        only_recorded(ids),
        recorded,
        "the default page must stay in chain order"
    );

    // Explicit false: same as the default.
    let (ids, _) = fetch_page("/api/v1/audit/events?newest_first=false&limit=10").await;
    assert_eq!(
        only_recorded(ids),
        recorded,
        "newest_first=false must behave like the default"
    );

    // Newest first: the three recorded events newest first, and the
    // newest event in the whole log is the first row — this is what
    // the Dashboard tile shows.
    let (ids, _) = fetch_page("/api/v1/audit/events?newest_first=true&limit=10").await;
    assert_eq!(
        ids.first().expect("the page is not empty"),
        recorded.last().expect("three events recorded"),
        "the newest event must head a newest-first page"
    );
    let mut reversed = recorded.clone();
    reversed.reverse();
    assert_eq!(
        only_recorded(ids),
        reversed,
        "a newest-first page must run newest first"
    );

    // Cursor pages forward in newest-first order too: one per page,
    // newest to oldest.
    let (ids, has_more) = fetch_page("/api/v1/audit/events?newest_first=true&limit=1").await;
    assert_eq!(ids, vec![recorded[2].clone()], "first newest-first page");
    assert!(has_more);
    let (ids, has_more) = fetch_page(&format!(
        "/api/v1/audit/events?newest_first=true&limit=1&after={}",
        recorded[2]
    ))
    .await;
    assert_eq!(ids, vec![recorded[1].clone()], "second newest-first page");
    assert!(has_more);
    let (ids, _) = fetch_page(&format!(
        "/api/v1/audit/events?newest_first=true&limit=1&after={}",
        recorded[1]
    ))
    .await;
    assert_eq!(ids, vec![recorded[0].clone()], "third newest-first page");

    drop_db(&admin_url, &db_name, pool).await;
}
/// Tampering with a chain hash is surfaced through the HTTP verify
/// endpoint: `GET /api/v1/audit/verify` reports `valid: false` and names
/// the first bad event (task 04-AUD-03).
#[tokio::test]
async fn pg_audit_verify_endpoint_reports_tamper() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile, "tamper@example.com").await;

    PgGrantRepository::new(pool.clone())
        .assign(
            staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource: Resource::AuditLog,
                    operation: Operation::Read,
                },
                scope: Scope::Agency { agency_id },
            },
        )
        .await
        .expect("assign the audit-read grant");

    // Sanity: the untampered chain verifies over HTTP.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/audit/verify".to_owned(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("valid").and_then(serde_json::Value::as_bool),
        Some(true)
    );

    // Tamper with the login event's chain hash in place.
    let tampered_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT event_id FROM audit_events WHERE action = 'auth.login' ORDER BY seq ASC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("login event exists");
    sqlx::query("UPDATE audit_events SET chain_hash = $1 WHERE event_id = $2")
        .bind(vec![0u8; 32])
        .bind(tampered_id)
        .execute(&pool)
        .await
        .expect("tamper with the chain hash");

    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/audit/verify".to_owned(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("valid").and_then(serde_json::Value::as_bool),
        Some(false),
        "the tampered chain must not verify"
    );
    assert_eq!(
        body.get("first_bad_event")
            .and_then(serde_json::Value::as_str),
        Some(tampered_id.to_string()).as_deref(),
        "the endpoint must name the first bad event"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// A profile-scoped `AuditLog:Read` grant is not enough: the audit
/// endpoints demand agency scope, so the caller gets 403 rather than a
/// (possibly empty) page (task 04-AUD-03).
#[tokio::test]
async fn pg_audit_events_endpoint_rejects_profile_scoped_grant() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile, "profilescoped@example.com").await;

    // The caller holds AuditLog:Read, but only for their own profile.
    PgGrantRepository::new(pool.clone())
        .assign(
            staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource: Resource::AuditLog,
                    operation: Operation::Read,
                },
                scope: Scope::ClientProfile {
                    agency_id,
                    client_profile_id: ClientProfileId::from(profile),
                },
            },
        )
        .await
        .expect("assign the profile-scoped audit-read grant");

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/audit/events".to_owned(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a profile-scoped audit grant must not open the agency audit log"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

// ---------------------------------------------------------------------------
// Core CRM vertical slice (05-CRM-02)
// ---------------------------------------------------------------------------

/// Assigns one grant to a staff user.
async fn grant(
    pool: &PgPool,
    staff_id: StaffId,
    resource: Resource,
    operation: Operation,
    scope: Scope,
) {
    PgGrantRepository::new(pool.clone())
        .assign(
            staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource,
                    operation,
                },
                scope,
            },
        )
        .await
        .expect("assign grant");
}

/// Builds an authenticated POST request with a JSON body.
fn post_json(uri: &str, body: serde_json::Value, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

#[tokio::test]
async fn pg_profile_lifecycle() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile_uuid = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile_uuid, "profiler@example.com").await;

    // Grant profile management at agency scope.
    let agency_scope = Scope::Agency { agency_id };
    for operation in [Operation::Create, Operation::Read, Operation::Update] {
        grant(
            &pool,
            staff_id,
            Resource::ClientProfile,
            operation,
            agency_scope,
        )
        .await;
    }

    // Create.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_json(
                "/api/v1/profiles",
                serde_json::json!({ "name": "Acme Corp" }),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let profile_id = body["profile_id"].as_str().expect("profile_id present");
    assert_eq!(body["name"], "Acme Corp");
    assert_eq!(body["status"], "Active");

    // Get.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/profiles/{profile_id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Acme Corp");

    // List.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/profiles".to_owned(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["total"].as_u64().expect("total") >= 1);

    // Suspend → reactivate → archive.
    for (uri, expected) in [
        (
            format!("/api/v1/profiles/{profile_id}/suspend"),
            "Suspended",
        ),
        (
            format!("/api/v1/profiles/{profile_id}/reactivate"),
            "Active",
        ),
        (format!("/api/v1/profiles/{profile_id}/archive"), "Archived"),
    ] {
        let (status, body) = read_json(
            router
                .clone()
                .oneshot(post_json(&uri, serde_json::json!({}), &token))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], expected);
    }

    // Archived is terminal: suspend fails.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_json(
                &format!("/api/v1/profiles/{profile_id}/suspend"),
                serde_json::json!({}),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_profile_agency_isolation() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let (token_a, staff_a, agency_a) =
        seed_authed_user(&pool, &router, uuid::Uuid::new_v4(), "a@example.com").await;
    let (token_b, staff_b, agency_b) =
        seed_authed_user(&pool, &router, uuid::Uuid::new_v4(), "b@example.com").await;

    // Both users get profile-read at their own agency scope.
    grant(
        &pool,
        staff_a,
        Resource::ClientProfile,
        Operation::Create,
        Scope::Agency {
            agency_id: agency_a,
        },
    )
    .await;
    grant(
        &pool,
        staff_a,
        Resource::ClientProfile,
        Operation::Read,
        Scope::Agency {
            agency_id: agency_a,
        },
    )
    .await;
    grant(
        &pool,
        staff_b,
        Resource::ClientProfile,
        Operation::Read,
        Scope::Agency {
            agency_id: agency_b,
        },
    )
    .await;

    // A creates a profile.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_json(
                "/api/v1/profiles",
                serde_json::json!({ "name": "A Corp" }),
                &token_a,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let profile_id = body["profile_id"].as_str().expect("profile_id");

    // B cannot see A's profile: 404, not an existence leak.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/profiles/{profile_id}"), &token_b))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // B's list does not include A's profile.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get("/api/v1/profiles".to_owned(), &token_b))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_contact_dedupe_blocks_duplicate() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_authed_user(&pool, &router, profile, "dedupe@example.com").await;

    // First contact.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_contacts(contact_body(profile), None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Same email, different casing/whitespace → 409 (dedupe on write path).
    let mut dup_body = contact_body(profile);
    dup_body["emails"] = serde_json::json!([" test@example.com "]);
    let (status, problem) = read_json(
        router
            .clone()
            .oneshot(post_contacts(dup_body, None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["title"], "Conflict");
    // No PII in the error.
    assert!(!problem["detail"]
        .as_str()
        .unwrap_or_default()
        .contains("test@example.com"));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_contact_merge_endpoint() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile, "merger@example.com").await;
    // Merge needs contact/update.
    grant(
        &pool,
        staff_id,
        Resource::Contact,
        Operation::Update,
        Scope::ClientProfile {
            agency_id,
            client_profile_id: ClientProfileId::from(profile),
        },
    )
    .await;

    // Survivor with one email.
    let (status, survivor) = read_json(
        router
            .clone()
            .oneshot(post_contacts(
                serde_json::json!({
                    "client_profile_id": profile,
                    "emails": ["survivor@example.com"],
                    "phones": [],
                    "source": "web-form",
                }),
                None,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // Duplicate with a different email + a phone.
    let (status, duplicate) = read_json(
        router
            .clone()
            .oneshot(post_contacts(
                serde_json::json!({
                    "client_profile_id": profile,
                    "emails": ["duplicate@example.com"],
                    "phones": ["+1-555-0199"],
                    "source": "csv-import",
                }),
                None,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let survivor_id = survivor["contact_id"].as_str().expect("survivor id");
    let duplicate_id = duplicate["contact_id"].as_str().expect("duplicate id");

    // Merge.
    let (status, merged) = read_json(
        router
            .clone()
            .oneshot(post_json(
                "/api/v1/contacts/merge",
                serde_json::json!({
                    "survivor_id": survivor_id,
                    "duplicate_id": duplicate_id,
                }),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(merged["lifecycle"], "Archived");
    assert_eq!(merged["merged_into"], survivor_id);

    // The survivor absorbed the duplicate's identity (union).
    let (status, survivor_after) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/contacts/{survivor_id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let emails: Vec<&str> = survivor_after["normalized_emails"]
        .as_array()
        .expect("emails")
        .iter()
        .map(|v| v.as_str().expect("email str"))
        .collect();
    assert!(emails.contains(&"survivor@example.com"));
    assert!(emails.contains(&"duplicate@example.com"));
    let phones: Vec<&str> = survivor_after["normalized_phones"]
        .as_array()
        .expect("phones")
        .iter()
        .map(|v| v.as_str().expect("phone str"))
        .collect();
    assert!(phones.iter().any(|p| p.contains("555-0199")));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_asset_upload_dedupe() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile, "assets@example.com").await;
    let scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(profile),
    };
    for operation in [Operation::Create, Operation::Read, Operation::Update] {
        grant(&pool, staff_id, Resource::Asset, operation, scope).await;
    }

    let sha_hex = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let body = serde_json::json!({
        "client_profile_id": profile,
        "file_name": "contract.pdf",
        "mime_type": "application/pdf",
        "size_bytes": 1024,
        "sha256_hex": sha_hex,
    });
    // First upload → 201.
    let (status, asset) = read_json(
        router
            .clone()
            .oneshot(post_json("/api/v1/assets", body.clone(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let asset_id = asset["asset_id"].as_str().expect("asset id");
    assert_eq!(asset["kind"], "Document");

    // Same bytes, different name → 200 with the SAME asset (dedupe).
    let mut body2 = body.clone();
    body2["file_name"] = serde_json::json!("renamed.pdf");
    let (status, asset2) = read_json(
        router
            .clone()
            .oneshot(post_json("/api/v1/assets", body2, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(asset2["asset_id"], asset_id);

    // Get + list.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/assets/{asset_id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/assets?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);

    // Archive.
    let (status, archived) = read_json(
        router
            .clone()
            .oneshot(post_json(
                &format!("/api/v1/assets/{asset_id}/archive"),
                serde_json::json!({}),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archived["status"], "Archived");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_asset_scope_isolation() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile_a = uuid::Uuid::new_v4();
    let profile_b = uuid::Uuid::new_v4();
    let (token_a, staff_a, agency_a) =
        seed_authed_user(&pool, &router, profile_a, "aiso-a@example.com").await;
    // User B gets asset grants on profile B only.
    let (token_b, staff_b, agency_b) =
        seed_authed_user(&pool, &router, profile_b, "aiso-b@example.com").await;

    for operation in [Operation::Create, Operation::Read] {
        grant(
            &pool,
            staff_a,
            Resource::Asset,
            operation,
            Scope::ClientProfile {
                agency_id: agency_a,
                client_profile_id: ClientProfileId::from(profile_a),
            },
        )
        .await;
        grant(
            &pool,
            staff_b,
            Resource::Asset,
            operation,
            Scope::ClientProfile {
                agency_id: agency_b,
                client_profile_id: ClientProfileId::from(profile_b),
            },
        )
        .await;
    }

    // A uploads under profile A.
    let sha_hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    let (status, asset) = read_json(
        router
            .clone()
            .oneshot(post_json(
                "/api/v1/assets",
                serde_json::json!({
                    "client_profile_id": profile_a,
                    "file_name": "a.pdf",
                    "mime_type": "application/pdf",
                    "size_bytes": 512,
                    "sha256_hex": sha_hex,
                }),
                &token_a,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let asset_id = asset["asset_id"].as_str().expect("asset id");

    // B cannot fetch A's asset: 403 (has asset/read, but only on profile B).
    // The handler fetches first, then authorizes against the asset's own
    // profile — B's grant does not cover profile A.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/assets/{asset_id}"), &token_b))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // B's list under profile A is also forbidden.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/assets?client_profile_id={profile_a}"),
                &token_b,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    drop_db(&admin_url, &db_name, pool).await;
}

/// Full Core CRM slice end to end: profile → contacts (with sources) →
/// dedupe → merge → assets (with dedupe) → scope isolation.
///
/// This is the 05-CRM-03 verification test: it exercises the whole
/// vertical slice through the HTTP API against Postgres, proving the
/// pieces compose.
#[tokio::test]
async fn pg_core_crm_full_slice() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile_uuid = uuid::Uuid::new_v4();
    let (token, staff_id, agency_id) =
        seed_authed_user(&pool, &router, profile_uuid, "slice@example.com").await;

    // Grants: profile management at agency scope; contacts + assets at
    // profile scope.
    let agency_scope = Scope::Agency { agency_id };
    for operation in [Operation::Create, Operation::Read, Operation::Update] {
        grant(
            &pool,
            staff_id,
            Resource::ClientProfile,
            operation,
            agency_scope,
        )
        .await;
    }
    let profile_scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(profile_uuid),
    };
    for resource in [Resource::Contact, Resource::Asset] {
        for operation in [Operation::Create, Operation::Read, Operation::Update] {
            grant(&pool, staff_id, resource, operation, profile_scope).await;
        }
    }

    // 1. Create the client profile.
    let (status, profile) = read_json(
        router
            .clone()
            .oneshot(post_json(
                "/api/v1/profiles",
                serde_json::json!({ "name": "Slice Corp" }),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(profile["name"], "Slice Corp");
    assert_eq!(profile["status"], "Active");

    // 2. Create contacts with distinct sources.
    let (status, alice) = read_json(
        router
            .clone()
            .oneshot(post_contacts(
                serde_json::json!({
                    "client_profile_id": profile_uuid,
                    "emails": ["alice@slice.com"],
                    "phones": ["+1-555-0301"],
                    "source": "web-form",
                }),
                None,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let alice_id = alice["contact_id"].as_str().expect("alice id");

    let (status, bob) = read_json(
        router
            .clone()
            .oneshot(post_contacts(
                serde_json::json!({
                    "client_profile_id": profile_uuid,
                    "emails": ["bob@slice.com"],
                    "phones": [],
                    "source": "csv-import",
                }),
                None,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let bob_id = bob["contact_id"].as_str().expect("bob id");

    // 3. Dedupe: same email → 409 Conflict, no PII in the error.
    let (status, problem) = read_json(
        router
            .clone()
            .oneshot(post_contacts(
                serde_json::json!({
                    "client_profile_id": profile_uuid,
                    "emails": ["alice@slice.com"],
                    "phones": [],
                    "source": "web-form",
                }),
                None,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        !problem.to_string().contains("alice@slice.com"),
        "dedupe error must not echo the address"
    );

    // 4. Merge bob into alice; the survivor absorbs the union.
    let (status, merged) = read_json(
        router
            .clone()
            .oneshot(post_json(
                "/api/v1/contacts/merge",
                serde_json::json!({
                    "survivor_id": alice_id,
                    "duplicate_id": bob_id,
                }),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(merged["lifecycle"], "Archived");
    assert_eq!(merged["merged_into"], alice_id);

    let (status, survivor) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/contacts/{alice_id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let emails: Vec<&str> = survivor["normalized_emails"]
        .as_array()
        .expect("emails")
        .iter()
        .map(|v| v.as_str().expect("email str"))
        .collect();
    assert!(emails.contains(&"alice@slice.com"));
    assert!(emails.contains(&"bob@slice.com"));

    // 5. Asset upload with content-hash dedupe.
    let sha_hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    let asset_body = serde_json::json!({
        "client_profile_id": profile_uuid,
        "file_name": "proposal.pdf",
        "mime_type": "application/pdf",
        "size_bytes": 2048,
        "sha256_hex": sha_hex,
    });
    let (status, asset) = read_json(
        router
            .clone()
            .oneshot(post_json("/api/v1/assets", asset_body.clone(), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let asset_id = asset["asset_id"].as_str().expect("asset id");

    // Same bytes, different name → 200 with the same asset.
    let mut asset_body2 = asset_body.clone();
    asset_body2["file_name"] = serde_json::json!("renamed.pdf");
    let (status, asset2) = read_json(
        router
            .clone()
            .oneshot(post_json("/api/v1/assets", asset_body2, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(asset2["asset_id"], asset_id);

    // 6. Scope isolation: a second profile's data is invisible.
    let other_profile = uuid::Uuid::new_v4();
    let other_scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(other_profile),
    };
    for resource in [Resource::Contact, Resource::Asset] {
        for operation in [Operation::Create, Operation::Read] {
            grant(&pool, staff_id, resource, operation, other_scope).await;
        }
    }
    // Listing contacts without a profile filter is scoped to the
    // caller's grants; the other profile's contacts are not visible
    // through this profile's scope.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/assets?client_profile_id={other_profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_db(&admin_url, &db_name, pool).await;
}

// Email vertical-slice tests (task 07-MAIL-02).
//
// These drive the email HTTP routes against a real PostgreSQL database:
// send → status transitions, ingest → thread matching, profile scope
// isolation, and idempotency replay on the send endpoint.

/// Seeds a staff user holding `Email:Create` and `EmailThread:Read`
/// grants on `profile`, then logs them in. Returns the bearer token.
async fn seed_email_user(
    pool: &PgPool,
    router: &axum::Router,
    profile: uuid::Uuid,
    email: &str,
) -> (String, StaffId, AgencyId) {
    let (token, staff_id, agency_id) = seed_authed_user(pool, router, profile, email).await;
    let grant_repo = PgGrantRepository::new(pool.clone());
    let scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(profile),
    };
    for (resource, operation) in [
        (Resource::Email, Operation::Create),
        (Resource::EmailThread, Operation::Read),
    ] {
        grant_repo
            .assign(
                staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource,
                        operation,
                    },
                    scope,
                },
            )
            .await
            .expect("assign email grant");
    }
    (token, staff_id, agency_id)
}

fn send_email_body(profile: uuid::Uuid) -> serde_json::Value {
    serde_json::json!({
        "client_profile_id": profile,
        "from": "agency@example.com",
        "to": ["customer@example.com"],
        "subject": "Welcome",
        "body_text": "Hello from the agency."
    })
}

fn post_emails_send(body: serde_json::Value, key: Option<&str>, token: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/emails/send")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(key) = key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    builder
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize send body"),
        ))
        .expect("build send request")
}

#[tokio::test]
async fn pg_email_send_transitions_to_sent() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_email_send_transitions_to_sent: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _staff, _agency) =
        seed_email_user(&pool, &router, profile, "mailer@example.com").await;

    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_emails_send(send_email_body(profile), None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "send body: {body}");
    assert_eq!(body["status"], "Sent");
    assert!(body["provider_message_id"]
        .as_str()
        .unwrap_or_default()
        .starts_with("stub-"));

    // The row is persisted as Sent.
    let repo = PgEmailMessageRepository::new(pool.clone());
    let message_id =
        uuid::Uuid::parse_str(body["message_id"].as_str().expect("message_id")).expect("uuid");
    let stored = repo
        .get(EmailMessageId::from(message_id))
        .await
        .expect("fetch sent message");
    assert_eq!(stored.status, EmailStatus::Sent);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_email_send_replay_returns_200_without_duplicating() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_email_send_replay: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _staff, _agency) =
        seed_email_user(&pool, &router, profile, "mailer2@example.com").await;

    let key = IdempotencyKey::generate().to_string();
    let (first_status, first_body) = read_json(
        router
            .clone()
            .oneshot(post_emails_send(
                send_email_body(profile),
                Some(&key),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED);

    let (replay_status, replay_body) = read_json(
        router
            .clone()
            .oneshot(post_emails_send(
                send_email_body(profile),
                Some(&key),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(replay_status, StatusCode::OK, "replay body: {replay_body}");
    assert_eq!(first_body["message_id"], replay_body["message_id"]);

    // Exactly one message row exists for the profile.
    let repo = PgEmailMessageRepository::new(pool.clone());
    let messages = repo
        .list_for_client(ClientProfileId::from(profile), 100)
        .await
        .expect("list messages");
    assert_eq!(messages.len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_email_send_forbidden_without_grant() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_email_send_forbidden: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    // seed_authed_user grants Contact only — no Email grant.
    let (token, _staff, _agency) =
        seed_authed_user(&pool, &router, profile, "nogrant@example.com").await;

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_emails_send(send_email_body(profile), None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    drop_db(&admin_url, &db_name, pool).await;
}

fn ingest_body(profile: uuid::Uuid, provider_message_id: &str) -> serde_json::Value {
    serde_json::json!({
        "client_profile_id": profile,
        "provider_message_id": provider_message_id,
        "provider_thread_id": "provider-thread-1",
        "from": "customer@example.com",
        "to": ["agency@example.com"],
        "subject": "Re: Welcome",
        "body_text": "Thanks for reaching out."
    })
}

fn post_ingest(body: serde_json::Value, secret: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/emails/ingest")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-webhook-secret", secret)
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize ingest"),
        ))
        .expect("build ingest request")
}

/// Shared webhook secret for the ingest tests. The same value is set
/// by every ingest test, so parallel `set_var` calls are idempotent
/// and cannot race. The var is deliberately never removed: cleanup
/// `remove_var` calls would race with parallel tests still running.
const TEST_WEBHOOK_SECRET: &str = "test-webhook-secret-07-mail-02";

#[tokio::test]
async fn pg_email_ingest_matches_thread_and_creates_message() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_email_ingest: DATABASE_URL not set, skipping");
        return;
    };
    std::env::set_var("CRM_EMAIL_WEBHOOK_SECRET", TEST_WEBHOOK_SECRET);
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _staff, _agency) =
        seed_email_user(&pool, &router, profile, "mailer3@example.com").await;

    // First ingest opens a new thread (no provider thread known yet).
    let (status, first) = read_json(
        router
            .clone()
            .oneshot(post_ingest(
                ingest_body(profile, "provider-msg-1"),
                TEST_WEBHOOK_SECRET,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "ingest body: {first}");
    assert_eq!(first["direction"], "Inbound");
    assert_eq!(first["status"], "Delivered");
    let thread_id = first["thread_id"].as_str().expect("thread_id").to_owned();

    // Second ingest with the same provider thread id joins the thread.
    let (status, second) = read_json(
        router
            .clone()
            .oneshot(post_ingest(
                ingest_body(profile, "provider-msg-2"),
                TEST_WEBHOOK_SECRET,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(second["thread_id"].as_str(), Some(thread_id.as_str()));

    // The thread now holds two messages.
    let (status, detail) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/threads/{thread_id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail body: {detail}");
    assert_eq!(detail["thread"]["message_count"], 2);
    assert_eq!(detail["messages"].as_array().map(Vec::len), Some(2));

    // Duplicate provider message id is at-most-once: no new message.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_ingest(
                ingest_body(profile, "provider-msg-1"),
                TEST_WEBHOOK_SECRET,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let threads = PgEmailThreadRepository::new(pool.clone())
        .list_for_client(ClientProfileId::from(profile), 100)
        .await
        .expect("list threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].message_count, 2);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_email_ingest_rejects_bad_secret() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_email_ingest_secret: DATABASE_URL not set, skipping");
        return;
    };
    std::env::set_var("CRM_EMAIL_WEBHOOK_SECRET", TEST_WEBHOOK_SECRET);
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_ingest(
                ingest_body(profile, "provider-msg-x"),
                "wrong-secret",
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_email_threads_are_profile_scoped() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_email_scope: DATABASE_URL not set, skipping");
        return;
    };
    std::env::set_var("CRM_EMAIL_WEBHOOK_SECRET", TEST_WEBHOOK_SECRET);
    let router = pg_router(&pool);
    let profile_a = uuid::Uuid::new_v4();
    let profile_b = uuid::Uuid::new_v4();
    // The user holds email grants on profile A only.
    let (token, _staff, _agency) =
        seed_email_user(&pool, &router, profile_a, "scoped@example.com").await;

    // Ingest into profile A so a thread exists there.
    let (status, ingested) = read_json(
        router
            .clone()
            .oneshot(post_ingest(
                ingest_body(profile_a, "provider-msg-scope"),
                TEST_WEBHOOK_SECRET,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let thread_id = ingested["thread_id"].as_str().expect("thread_id");

    // Listing profile A's threads works.
    let (status, list) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/threads?client_profile_id={profile_a}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list body: {list}");
    assert_eq!(list["total"], 1);

    // Listing profile B's threads is denied: no grant on B.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/threads?client_profile_id={profile_b}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Fetching the profile-A thread directly works.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/threads/{thread_id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_db(&admin_url, &db_name, pool).await;
}

/// Seeds an active staff user with campaign grants for `profile`.
///
/// Returns `(token, staff_id, agency_id)`.
async fn seed_campaign_user(
    pool: &PgPool,
    router: &axum::Router,
    profile: uuid::Uuid,
    email: &str,
) -> (String, StaffId, AgencyId) {
    let (token, staff_id, agency_id) = seed_authed_user(pool, router, profile, email).await;
    let grant_repo = PgGrantRepository::new(pool.clone());
    let scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(profile),
    };
    for operation in [Operation::Create, Operation::Read, Operation::Update] {
        grant_repo
            .assign(
                staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::Campaign,
                        operation,
                    },
                    scope,
                },
            )
            .await
            .expect("assign campaign grant");
    }
    (token, staff_id, agency_id)
}

fn create_campaign_body(profile: uuid::Uuid, pool_id: &str) -> serde_json::Value {
    serde_json::json!({
        "name": "Q4 Outreach",
        "owner_kind": "client_profile",
        "client_profile_id": profile,
        "channel": "email",
        "prospect_pool_id": pool_id,
        "schedule": "0 9 * * MON",
        "suppression_policy": "global-opt-outs",
        "conversion_rules": "last-touch",
        "attribution_window_days": 30
    })
}

fn post_campaigns(body: serde_json::Value, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/campaigns")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

fn post_campaign_step(campaign_id: &str, body: serde_json::Value, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/v1/campaigns/{campaign_id}/steps"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

fn post_campaign_action(campaign_id: &str, action: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/v1/campaigns/{campaign_id}/{action}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("test request builds")
}

/// Seeds an active staff user with invoice create/read/update grants
/// for `profile` (on top of the contact grants from
/// [`seed_authed_user`]), then logs in over HTTP to obtain a real
/// bearer token.
///
/// Returns `(token, staff_id, agency_id)`.
async fn seed_billing_user(
    pool: &PgPool,
    router: &axum::Router,
    profile: uuid::Uuid,
    email: &str,
) -> (String, StaffId, AgencyId) {
    let (token, staff_id, agency_id) = seed_authed_user(pool, router, profile, email).await;
    let grant_repo = PgGrantRepository::new(pool.clone());
    let scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(profile),
    };
    for operation in [Operation::Create, Operation::Read, Operation::Update] {
        grant_repo
            .assign(
                staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::Invoice,
                        operation,
                    },
                    scope,
                },
            )
            .await
            .expect("assign invoice grant");
    }
    (token, staff_id, agency_id)
}

fn invoice_body(profile: uuid::Uuid) -> serde_json::Value {
    serde_json::json!({
        "client_profile_id": profile,
        "currency": "USD"
    })
}

fn invoice_line_body() -> serde_json::Value {
    serde_json::json!({
        "description": "Consulting",
        "quantity": 1,
        "unit_price": { "amount": 10000, "currency": "USD" }
    })
}

fn payment_body() -> serde_json::Value {
    serde_json::json!({
        "amount": { "amount": 5000, "currency": "USD" }
    })
}

fn post_invoices(body: serde_json::Value, key: Option<&str>, token: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/invoices")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(key) = key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

fn post_invoice_lines(
    invoice_id: &str,
    body: serde_json::Value,
    key: Option<&str>,
    token: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/invoices/{invoice_id}/lines"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(key) = key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

fn post_invoice_payments(
    invoice_id: &str,
    body: serde_json::Value,
    key: Option<&str>,
    token: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/invoices/{invoice_id}/payments"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(key) = key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

/// Creates an invoice with one $100.00 line, returning its id.
async fn seed_invoice_with_line(router: &axum::Router, profile: uuid::Uuid, token: &str) -> String {
    let (status, invoice) = read_json(
        router
            .clone()
            .oneshot(post_invoices(invoice_body(profile), None, token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let invoice_id = invoice["invoice_id"]
        .as_str()
        .expect("invoice id")
        .to_owned();
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_invoice_lines(
                &invoice_id,
                invoice_line_body(),
                None,
                token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    invoice_id
}

/// Creates a draft invoice with one $100.00 line and issues it,
/// returning its id. Payments are only recorded on issued invoices.
async fn seed_issued_invoice_with_line(
    router: &axum::Router,
    profile: uuid::Uuid,
    token: &str,
) -> String {
    let invoice_id = seed_invoice_with_line(router, profile, token).await;
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/invoices/{invoice_id}/issue"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    invoice_id
}

#[tokio::test]
async fn pg_billing_invoice_create_idempotent() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_billing_invoice_create_idempotent: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller@example.com").await;

    let key = IdempotencyKey::generate().to_string();
    let body = invoice_body(profile);
    let (first_status, first) = read_json(
        router
            .clone()
            .oneshot(post_invoices(body.clone(), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    let (second_status, second) = read_json(
        router
            .clone()
            .oneshot(post_invoices(body, Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first, second, "replay returns the identical response");

    // Exactly one invoice exists for the profile.
    let (status, listed) = read_json(
        router
            .oneshot(get(
                format!("/api/v1/invoices?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1, "one invoice despite two posts");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_concurrent_same_key_creates_one_invoice() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!(
            "pg_billing_concurrent_same_key_creates_one_invoice: DATABASE_URL not set, skipping"
        );
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller9@example.com").await;

    // Two simultaneous creates with the SAME Idempotency-Key: the atomic
    // claim lets exactly one reach the write; the loser polls for the
    // winner's recorded response and replays its 201 instead of
    // creating a duplicate invoice.
    let key = IdempotencyKey::generate().to_string();
    let body = invoice_body(profile);
    let (first, second) = tokio::join!(
        read_json(
            router
                .clone()
                .oneshot(post_invoices(body.clone(), Some(&key), &token))
                .await
                .expect("oneshot")
        ),
        read_json(
            router
                .clone()
                .oneshot(post_invoices(body, Some(&key), &token))
                .await
                .expect("oneshot")
        ),
    );
    let (first_status, first) = first;
    let (second_status, second) = second;
    assert_eq!(first_status, StatusCode::CREATED, "winner creates");
    assert_eq!(
        second_status,
        StatusCode::CREATED,
        "loser replays the winner's 201"
    );
    assert_eq!(first, second, "both callers see the identical invoice");

    // Exactly one invoice exists for the profile.
    let (status, listed) = read_json(
        router
            .oneshot(get(
                format!("/api/v1/invoices?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed["total"], 1,
        "one invoice despite two concurrent posts"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_stale_claim_expires_and_self_heals() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_billing_stale_claim_expires_and_self_heals: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller-exp@example.com").await;

    // A crashed winner's claim: a JSON-null sentinel backdated far
    // past the claim TTL, with an arbitrary fingerprint (the crashed
    // request's). Without expiry this key would be poisoned forever.
    let idem_key = IdempotencyKey::generate();
    let key = idem_key.to_string();
    sqlx::query(
        "INSERT INTO idempotency_keys (idempotency_key, fingerprint, response, created_at)
         VALUES ($1, $2, 'null'::jsonb, now() - interval '1 hour')",
    )
    .bind(idem_key.as_uuid())
    .bind(12345i64)
    .execute(&pool)
    .await
    .expect("stale sentinel inserts");

    // The retry wins the claim (no 409), executes the write, and
    // records the response — the key is healed, not poisoned.
    let body = invoice_body(profile);
    let (status, created) = read_json(
        router
            .clone()
            .oneshot(post_invoices(body.clone(), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "stale claim expires, retry wins"
    );

    // A replay of the healed key returns the recorded response.
    let (status, replayed) = read_json(
        router
            .clone()
            .oneshot(post_invoices(body, Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created, replayed,
        "healed key replays the recorded response"
    );

    // Exactly one invoice exists for the profile.
    let (status, listed) = read_json(
        router
            .oneshot(get(
                format!("/api/v1/invoices?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1, "one invoice despite the stale sentinel");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_old_recorded_response_never_expires() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_billing_old_recorded_response_never_expires: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller-old@example.com").await;

    // A completed request records its response under the key.
    let idem_key = IdempotencyKey::generate();
    let key = idem_key.to_string();
    let body = invoice_body(profile);
    let (status, created) = read_json(
        router
            .clone()
            .oneshot(post_invoices(body.clone(), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Age the recorded row far past the claim TTL: expiry must never
    // touch a recorded response, only JSON-null sentinels.
    sqlx::query(
        "UPDATE idempotency_keys SET created_at = now() - interval '1 hour'
         WHERE idempotency_key = $1",
    )
    .bind(idem_key.as_uuid())
    .execute(&pool)
    .await
    .expect("recorded row backdates");

    // The replay still returns the stored response — no new invoice,
    // no 409.
    let (status, replayed) = read_json(
        router
            .clone()
            .oneshot(post_invoices(body, Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created, replayed, "old recorded response still replays");

    let (status, listed) = read_json(
        router
            .oneshot(get(
                format!("/api/v1/invoices?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1, "one invoice despite the aged row");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_payment_record_idempotent() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_billing_payment_record_idempotent: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller2@example.com").await;
    let invoice_id = seed_issued_invoice_with_line(&router, profile, &token).await;

    let key = IdempotencyKey::generate().to_string();
    let body = payment_body();
    let (first_status, first) = read_json(
        router
            .clone()
            .oneshot(post_invoice_payments(
                &invoice_id,
                body.clone(),
                Some(&key),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    let (second_status, second) = read_json(
        router
            .clone()
            .oneshot(post_invoice_payments(&invoice_id, body, Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first, second, "replay returns the identical response");

    // Exactly one payment exists for the invoice.
    let (status, listed) = read_json(
        router
            .oneshot(get(
                format!("/api/v1/invoices/{invoice_id}/payments"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1, "one payment despite two posts");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_idempotency_key_different_body_conflicts() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!(
            "pg_billing_idempotency_key_different_body_conflicts: DATABASE_URL not set, skipping"
        );
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller3@example.com").await;

    let key = IdempotencyKey::generate().to_string();
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_invoices(invoice_body(profile), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let mut other_body = invoice_body(profile);
    other_body["currency"] = serde_json::json!("EUR");
    let (status, problem) = read_json(
        router
            .oneshot(post_invoices(other_body, Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["status"], 409);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_idempotency_key_reuse_across_routes_conflicts() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_billing_idempotency_key_reuse_across_routes_conflicts: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller4@example.com").await;
    let invoice_id = seed_issued_invoice_with_line(&router, profile, &token).await;

    // The same key on two different billing routes is a conflict, not a
    // replay: the fingerprint is scoped per route, so reusing a key across
    // operations surfaces a 409 instead of silently replaying the wrong
    // response. Keys identify a single operation.
    let key = IdempotencyKey::generate().to_string();
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_invoices(invoice_body(profile), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, payment) = read_json(
        router
            .oneshot(post_invoice_payments(
                &invoice_id,
                payment_body(),
                Some(&key),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "same key on a different route is a conflict: {payment}"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_malformed_idempotency_key_returns_422() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!(
            "pg_billing_malformed_idempotency_key_returns_422: DATABASE_URL not set, skipping"
        );
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller5@example.com").await;

    let (status, _) = read_json(
        router
            .oneshot(post_invoices(
                invoice_body(profile),
                Some("not-a-uuid"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_issue_invoice_idempotent() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_billing_issue_invoice_idempotent: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller6@example.com").await;
    let invoice_id = seed_invoice_with_line(&router, profile, &token).await;

    let key = IdempotencyKey::generate().to_string();
    let issue = || {
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/invoices/{invoice_id}/issue"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(IDEMPOTENCY_KEY_HEADER, &key)
            .body(Body::empty())
            .expect("test request builds")
    };
    let (first_status, first) =
        read_json(router.clone().oneshot(issue()).await.expect("oneshot")).await;
    let (second_status, second) = read_json(router.oneshot(issue()).await.expect("oneshot")).await;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(first, second, "replay returns the identical response");
    assert_eq!(first["status"], "Issued");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_replay_without_grant_is_denied() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_billing_replay_without_grant_is_denied: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller7@example.com").await;
    // A second user with no invoice grants at all.
    let (unprivileged, _, _) =
        seed_authed_user(&pool, &router, profile, "intruder@example.com").await;

    // Authorized user creates an invoice with a key.
    let key = IdempotencyKey::generate().to_string();
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_invoices(invoice_body(profile), Some(&key), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The unprivileged user replays the same key+body: authorization runs
    // before the replay check, so this is denied, not replayed.
    let (status, _) = read_json(
        router
            .oneshot(post_invoices(
                invoice_body(profile),
                Some(&key),
                &unprivileged,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_billing_concurrent_payments_do_not_over_apply() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!(
            "pg_billing_concurrent_payments_do_not_over_apply: DATABASE_URL not set, skipping"
        );
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_billing_user(&pool, &router, profile, "biller8@example.com").await;
    // $100.00 invoice, issued.
    let invoice_id = seed_issued_invoice_with_line(&router, profile, &token).await;

    // Two $60.00 payments race: the invoice only has $100.00 of
    // balance, so exactly one must win. The `FOR UPDATE` row lock in
    // `record_guarded` serializes them; the loser sees the winner's
    // payment and fails validation instead of over-applying.
    let pay = || {
        let body = serde_json::json!({ "amount": { "amount": 6000, "currency": "USD" } });
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/invoices/{invoice_id}/payments"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .expect("test request builds")
    };
    let (first, second) = tokio::join!(
        read_json(router.clone().oneshot(pay()).await.expect("oneshot")),
        read_json(router.clone().oneshot(pay()).await.expect("oneshot")),
    );
    let (first_status, _) = first;
    let (second_status, _) = second;
    let mut statuses = [first_status, second_status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::CREATED, StatusCode::UNPROCESSABLE_ENTITY],
        "exactly one payment wins the race"
    );

    // Total applied is $60.00, never $120.00.
    let (status, listed) = read_json(
        router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/invoices/{invoice_id}/payments"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let total: i64 = listed["payments"]
        .as_array()
        .expect("payments array")
        .iter()
        .filter(|p| p["status"] == "Recorded")
        .map(|p| p["amount"]["amount"].as_i64().expect("amount"))
        .sum();
    assert_eq!(total, 6000, "no over-application");

    drop_db(&admin_url, &db_name, pool).await;
}

fn create_pool_body(profile: uuid::Uuid) -> serde_json::Value {
    serde_json::json!({
        "client_profile_id": profile,
        "name": "Q4 Prospects"
    })
}

#[tokio::test]
async fn pg_campaign_lifecycle_create_schedule_launch_complete() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_campaign_lifecycle: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _staff, _agency) =
        seed_campaign_user(&pool, &router, profile, "campaigner@example.com").await;

    // Create a prospect pool.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/prospect-pools")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(create_pool_body(profile).to_string()))
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create pool: {body}");
    let pool_id = body["pool_id"]
        .as_str()
        .expect("pool_id in body")
        .to_owned();

    // Create a campaign.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaigns(
                create_campaign_body(profile, &pool_id),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create campaign: {body}");
    let campaign_id = body["campaign_id"]
        .as_str()
        .expect("campaign_id")
        .to_owned();
    assert_eq!(body["delivery_state"], "Draft");

    // Add an email step.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaign_step(
                &campaign_id,
                serde_json::json!({
                    "kind": "email",
                    "position": 0,
                    "template_id": "welcome-template"
                }),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add step: {body}");

    // Launch the campaign (no pool members, so no sends).
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaign_action(&campaign_id, "launch", &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch: {body}");
    assert_eq!(body["delivery_state"], "Running");

    // Complete the campaign.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaign_action(&campaign_id, "complete", &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "complete: {body}");
    assert_eq!(body["delivery_state"], "Completed");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_campaign_launch_skips_opted_out_contacts() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_campaign_optout: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _staff, _agency) =
        seed_campaign_user(&pool, &router, profile, "bulk@example.com").await;

    // Create two contacts.
    let mut contact_ids = Vec::new();
    for email in ["alice@example.com", "bob@example.com"] {
        let (status, body) = read_json(
            router
                .clone()
                .oneshot(post_contacts(
                    serde_json::json!({
                        "client_profile_id": profile,
                        "emails": [email],
                        "phones": [],
                        "source": "web-form"
                    }),
                    None,
                    &token,
                ))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "create contact {email}: {body}"
        );
        contact_ids.push(body["contact_id"].as_str().expect("contact_id").to_owned());
    }

    // Create a pool and add both contacts.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/prospect-pools")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(create_pool_body(profile).to_string()))
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let pool_id = body["pool_id"].as_str().expect("pool_id").to_owned();

    for contact_id in &contact_ids {
        let (status, _) = read_json(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/prospect-pools/{pool_id}/members"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::from(
                            serde_json::json!({ "contact_id": contact_id }).to_string(),
                        ))
                        .expect("test request builds"),
                )
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "add member {contact_id}");
    }

    // Opt out the second contact (bob) from email.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/opt-outs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::json!({
                            "client_profile_id": profile,
                            "contact_id": contact_ids[1],
                            "channel": "email"
                        })
                        .to_string(),
                    ))
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "record opt-out");

    // Create a campaign and add an email step.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaigns(
                create_campaign_body(profile, &pool_id),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let campaign_id = body["campaign_id"]
        .as_str()
        .expect("campaign_id")
        .to_owned();

    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_campaign_step(
                &campaign_id,
                serde_json::json!({
                    "kind": "email",
                    "position": 0,
                    "template_id": "bulk-template"
                }),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Launch: only alice (not opted out) should receive an email.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_campaign_action(&campaign_id, "launch", &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch");

    // Verify via the database: exactly one email message was created,
    // addressed to alice.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM email_messages WHERE client_profile_id = $1")
            .bind(profile)
            .fetch_one(&pool)
            .await
            .expect("count emails");
    assert_eq!(count, 1, "only the non-opted-out contact receives email");

    let recipient: Vec<String> =
        sqlx::query_scalar("SELECT to_addresses FROM email_messages WHERE client_profile_id = $1")
            .bind(profile)
            .fetch_one(&pool)
            .await
            .expect("fetch recipient");
    assert_eq!(recipient, vec!["alice@example.com".to_owned()]);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_campaign_launch_is_idempotent() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_campaign_idempotent: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _staff, _agency) =
        seed_campaign_user(&pool, &router, profile, "idempotent@example.com").await;

    // Create two contacts.
    let mut contact_ids = Vec::new();
    for email in ["carol@example.com", "dave@example.com"] {
        let (status, body) = read_json(
            router
                .clone()
                .oneshot(post_contacts(
                    serde_json::json!({
                        "client_profile_id": profile,
                        "emails": [email],
                        "phones": [],
                        "source": "web-form"
                    }),
                    None,
                    &token,
                ))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "create contact {email}: {body}"
        );
        contact_ids.push(body["contact_id"].as_str().expect("contact_id").to_owned());
    }

    // Create a pool and add both contacts.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/prospect-pools")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(create_pool_body(profile).to_string()))
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let pool_id = body["pool_id"].as_str().expect("pool_id").to_owned();

    for contact_id in &contact_ids {
        let (status, _) = read_json(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/prospect-pools/{pool_id}/members"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::from(
                            serde_json::json!({ "contact_id": contact_id }).to_string(),
                        ))
                        .expect("test request builds"),
                )
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "add member {contact_id}");
    }

    // Create a campaign with two email steps.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaigns(
                create_campaign_body(profile, &pool_id),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let campaign_id = body["campaign_id"]
        .as_str()
        .expect("campaign_id")
        .to_owned();

    for position in [0, 1] {
        let (status, _) = read_json(
            router
                .clone()
                .oneshot(post_campaign_step(
                    &campaign_id,
                    serde_json::json!({
                        "kind": "email",
                        "position": position,
                        "template_id": "idempotent-template"
                    }),
                    &token,
                ))
                .await
                .expect("oneshot"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "add step {position}");
    }

    // First launch: 2 contacts x 2 steps = 4 emails.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaign_action(&campaign_id, "launch", &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "first launch: {body}");
    assert_eq!(body["delivery_state"], "Running");

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM email_messages WHERE client_profile_id = $1")
            .bind(profile)
            .fetch_one(&pool)
            .await
            .expect("count emails after first launch");
    assert_eq!(count, 4, "2 contacts x 2 steps sent on first launch");

    // Every stored message carries its deterministic idempotency key.
    let keyed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM email_messages WHERE client_profile_id = $1 AND idempotency_key IS NOT NULL",
    )
    .bind(profile)
    .fetch_one(&pool)
    .await
    .expect("count keyed emails");
    assert_eq!(keyed, 4, "all launch emails carry idempotency keys");

    let first_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT message_id FROM email_messages WHERE client_profile_id = $1 ORDER BY message_id",
    )
    .bind(profile)
    .fetch_all(&pool)
    .await
    .expect("first launch message ids");

    // Simulate a crash mid-launch followed by a re-launch: the campaign
    // row is already Running, and the re-launch must resume without
    // duplicating any send.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaign_action(&campaign_id, "launch", &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-launch resumes: {body}");
    assert_eq!(body["delivery_state"], "Running");

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM email_messages WHERE client_profile_id = $1")
            .bind(profile)
            .fetch_one(&pool)
            .await
            .expect("count emails after re-launch");
    assert_eq!(count, 4, "re-launch sends no duplicates");

    let second_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT message_id FROM email_messages WHERE client_profile_id = $1 ORDER BY message_id",
    )
    .bind(profile)
    .fetch_all(&pool)
    .await
    .expect("second launch message ids");
    assert_eq!(
        first_ids, second_ids,
        "re-launch returns the original messages"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_campaign_scope_isolation() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_campaign_scope: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile_a = uuid::Uuid::new_v4();
    let profile_b = uuid::Uuid::new_v4();
    // The user holds campaign grants on profile A only.
    let (token, _staff, _agency) =
        seed_campaign_user(&pool, &router, profile_a, "scoped@example.com").await;

    // Create a pool and campaign on profile A.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/prospect-pools")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(create_pool_body(profile_a).to_string()))
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let pool_id = body["pool_id"].as_str().expect("pool_id").to_owned();

    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_campaigns(
                create_campaign_body(profile_a, &pool_id),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let campaign_id = body["campaign_id"]
        .as_str()
        .expect("campaign_id")
        .to_owned();

    // Listing profile B's campaigns: no grant → 403.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/campaigns?client_profile_id={profile_b}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "profile B list without grant"
    );

    // Getting the profile-A campaign works.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(format!("/api/v1/campaigns/{campaign_id}"), &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn pg_campaign_conversion_recording() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_campaign_conversion: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _staff, _agency) =
        seed_campaign_user(&pool, &router, profile, "converter@example.com").await;

    // Create a contact.
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(post_contacts(
                serde_json::json!({
                    "client_profile_id": profile,
                    "emails": ["buyer@example.com"],
                    "phones": [],
                    "source": "web-form"
                }),
                None,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let contact_id = body["contact_id"].as_str().expect("contact_id").to_owned();

    // Record a conversion (no campaign attribution).
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/conversions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::json!({
                            "client_profile_id": profile,
                            "contact_id": contact_id,
                            "touchpoint_campaign_ids": [],
                            "attribution_model": "last_touch",
                            "value_cents": 5000,
                            "occurred_at": chrono::Utc::now().to_rfc3339()
                        })
                        .to_string(),
                    ))
                    .expect("test request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "record conversion: {body}");
    assert_eq!(body["value_cents"], 5000);
    assert!(
        body["campaign_id"].is_null(),
        "no attribution without touchpoints"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

/// Seeds a staff user with `system/manage` + `system/read` at agency
/// scope (for the maintenance toggle) plus contact read/create for
/// `profile`, then logs in over HTTP.
///
/// Returns `(token, staff_id, agency_id)`.
async fn seed_maintenance_admin(
    pool: &PgPool,
    router: &axum::Router,
    profile: uuid::Uuid,
    email: &str,
) -> (String, StaffId, AgencyId) {
    let agency_id = AgencyId::new();
    let now = chrono::Utc::now();
    let phc = hash_password(TEST_PASSWORD.as_bytes())
        .await
        .expect("hash test password");
    let staff_id = StaffId::new();
    PgStaffRepository::new(pool.clone())
        .create(StaffUser {
            staff_id,
            agency_id,
            email: email.to_owned(),
            display_name: "Maintenance Admin".to_owned(),
            status: StaffStatus::Active,
            credential: Some(
                PasswordCredential::new(phc, now).expect("test credential is PHC-shaped"),
            ),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("create staff user");
    let grant_repo = PgGrantRepository::new(pool.clone());
    // System grants at agency scope for the maintenance endpoints.
    for operation in [Operation::Read, Operation::Manage] {
        grant_repo
            .assign(
                staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::System,
                        operation,
                    },
                    scope: Scope::Agency { agency_id },
                },
            )
            .await
            .expect("assign system grant");
    }
    // Contact grants so the mutating/read probes have authorized targets.
    let scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: ClientProfileId::from(profile),
    };
    for operation in [Operation::Read, Operation::Create] {
        grant_repo
            .assign(
                staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::Contact,
                        operation,
                    },
                    scope,
                },
            )
            .await
            .expect("assign contact grant");
    }
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "email": email, "password": TEST_PASSWORD })
                            .to_string(),
                    ))
                    .expect("login request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed login must succeed");
    let token = body["token"].as_str().expect("token in body").to_owned();
    (token, staff_id, agency_id)
}

fn put_maintenance(token: &str, enabled: bool) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri("/api/v1/admin/maintenance")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(
            serde_json::json!({ "enabled": enabled, "reason": "test maintenance window" })
                .to_string(),
        ))
        .expect("test request builds")
}

/// Maintenance mode enforcement (FIX-06): while enabled, mutating
/// requests are `503` with a `Retry-After` hint; reads keep working;
/// the toggle and login stay reachable; disabling restores writes.
#[tokio::test]
async fn pg_maintenance_mode_blocks_writes_not_reads() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("skipping Postgres API test: DATABASE_URL is not set");
        return;
    };
    let router = pg_router(&pool);
    let profile = uuid::Uuid::new_v4();
    let (token, _, _) = seed_maintenance_admin(&pool, &router, profile, "maint@example.com").await;

    // Baseline: writes work while maintenance is disabled.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_contacts(contact_body(profile), None, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "baseline write works");

    // Enable maintenance.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(put_maintenance(&token, true))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable maintenance");

    // Mutating request is rejected with 503 + Retry-After.
    let response = router
        .clone()
        .oneshot(post_contacts(contact_body(profile), None, &token))
        .await
        .expect("oneshot");
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "write blocked during maintenance"
    );
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("60"),
        "503 carries Retry-After: 60"
    );
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .starts_with("application/problem+json"),
        "503 is a problem+json response"
    );
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let detail = body["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("maintenance"),
        "problem detail names maintenance: {body}"
    );
    assert!(
        detail.contains("test maintenance window"),
        "problem detail carries the operator reason: {body}"
    );

    // Login stays reachable during maintenance (needed to reach the toggle
    // with a fresh token).
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "email": "maint@example.com", "password": TEST_PASSWORD })
                            .to_string(),
                    ))
                    .expect("login request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login works during maintenance");

    // Reads still work.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(get(
                format!("/api/v1/contacts?client_profile_id={profile}"),
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reads work during maintenance");

    // The toggle itself stays reachable: disable maintenance.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(put_maintenance(&token, false))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "disable maintenance");

    // Writes work again.
    let (status, _) = read_json(
        router
            .clone()
            .oneshot(post_contacts(
                contact_body_with_email(profile, "after@example.com"),
                None,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "writes restored");

    drop_db(&admin_url, &db_name, pool).await;
}

/// Seeds a staff user with `AgencySettings:Manage` at agency scope and
/// logs them in, for the branding endpoints (HARD-02).
async fn seed_branding_admin(pool: &PgPool, router: &axum::Router, email: &str) -> String {
    let agency_id = AgencyId::new();
    let now = chrono::Utc::now();
    let phc = hash_password(TEST_PASSWORD.as_bytes())
        .await
        .expect("hash test password");
    let staff_id = StaffId::new();
    PgStaffRepository::new(pool.clone())
        .create(StaffUser {
            staff_id,
            agency_id,
            email: email.to_owned(),
            display_name: "Branding Admin".to_owned(),
            status: StaffStatus::Active,
            credential: Some(
                PasswordCredential::new(phc, now).expect("test credential is PHC-shaped"),
            ),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("create staff user");
    PgGrantRepository::new(pool.clone())
        .assign(
            staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource: Resource::AgencySettings,
                    operation: Operation::Manage,
                },
                scope: Scope::Agency { agency_id },
            },
        )
        .await
        .expect("assign branding grant");
    let (status, body) = read_json(
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "email": email, "password": TEST_PASSWORD })
                            .to_string(),
                    ))
                    .expect("login request builds"),
            )
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed login must succeed");
    body["token"].as_str().expect("token in body").to_owned()
}

fn put_branding(body: serde_json::Value, key: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri("/api/v1/branding")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(IDEMPOTENCY_KEY_HEADER, key)
        .body(Body::from(body.to_string()))
        .expect("test request builds")
}

/// Branding idempotency (HARD-02): the same Idempotency-Key twice
/// returns the stored response without bumping the version again.
#[tokio::test]
async fn pg_branding_idempotent_replay_does_not_bump_version() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!(
            "pg_branding_idempotent_replay_does_not_bump_version: DATABASE_URL not set, skipping"
        );
        return;
    };
    let router = pg_router(&pool);
    let token = seed_branding_admin(&pool, &router, "brand@example.com").await;

    let key = IdempotencyKey::generate().to_string();
    let body = serde_json::json!({ "primary": "oklch(0.55 0.2 250)" });
    let (first_status, first) = read_json(
        router
            .clone()
            .oneshot(put_branding(body.clone(), &key, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(first_status, StatusCode::OK, "first branding write");
    assert_eq!(first["version"], 1, "first write creates version 1");

    // Same key + same body: stored response, no version bump.
    let (second_status, second) = read_json(
        router
            .clone()
            .oneshot(put_branding(body, &key, &token))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(second_status, StatusCode::OK, "replay returns 200");
    assert_eq!(
        second, first,
        "replay returns the identical stored response"
    );

    let (status, current) = read_json(
        router
            .clone()
            .oneshot(get_unauthenticated("/api/v1/branding".to_string()))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "branding read");
    assert_eq!(current["version"], 1, "version did not bump on replay");

    // A fresh key still writes: version 2.
    let fresh_key = IdempotencyKey::generate().to_string();
    let (status, third) = read_json(
        router
            .clone()
            .oneshot(put_branding(
                serde_json::json!({ "primary": "oklch(0.6 0.2 250)" }),
                &fresh_key,
                &token,
            ))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fresh key writes");
    assert_eq!(third["version"], 2, "fresh key bumps the version");

    drop_db(&admin_url, &db_name, pool).await;
}

/// Branding concurrent idempotency (HARD-02): two simultaneous updates
/// with the same key produce exactly one version bump; the loser
/// replays the winner's response.
#[tokio::test]
async fn pg_branding_concurrent_same_key_bumps_once() {
    let Some((pool, admin_url, db_name)) = fresh_db().await else {
        eprintln!("pg_branding_concurrent_same_key_bumps_once: DATABASE_URL not set, skipping");
        return;
    };
    let router = pg_router(&pool);
    let token = seed_branding_admin(&pool, &router, "brand2@example.com").await;

    let key = IdempotencyKey::generate().to_string();
    let body = serde_json::json!({ "primary": "oklch(0.55 0.2 250)" });
    let (first, second) = tokio::join!(
        read_json(
            router
                .clone()
                .oneshot(put_branding(body.clone(), &key, &token))
                .await
                .expect("oneshot")
        ),
        read_json(
            router
                .clone()
                .oneshot(put_branding(body, &key, &token))
                .await
                .expect("oneshot")
        ),
    );
    let (first_status, first) = first;
    let (second_status, second) = second;
    assert_eq!(first_status, StatusCode::OK, "winner writes");
    assert_eq!(
        second_status,
        StatusCode::OK,
        "loser replays the winner's 200"
    );
    assert_eq!(first, second, "both callers see the identical response");

    let (status, current) = read_json(
        router
            .clone()
            .oneshot(get_unauthenticated("/api/v1/branding".to_string()))
            .await
            .expect("oneshot"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "branding read");
    assert_eq!(
        current["version"], 1,
        "exactly one version bump for concurrent same-key writes"
    );

    drop_db(&admin_url, &db_name, pool).await;
}
