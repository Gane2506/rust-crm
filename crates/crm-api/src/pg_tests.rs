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
use crm_db::auth::hash_password;
use crm_db::postgres::{
    PgAuditRepository, PgContactRepository, PgGrantRepository, PgStaffRepository,
};
use crm_domain::audit::verify_chain;
use crm_domain::ids::{AgencyId, ClientProfileId, ContactId, GrantId, StaffId};
use crm_domain::repositories::{
    AuditRepository, ContactRepository, GrantRepository, SessionRepository, StaffRepository,
};
use crm_domain::security::{
    Action, Grant, Operation, PasswordCredential, Resource, Scope, StaffStatus, StaffUser,
};

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
    assert!(verify_chain(&events), "single event links from genesis");

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
