// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the identity repositories and auth services.
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
//! container. Each test creates its own database, migrates a fresh schema
//! into it, and drops it afterwards. Database lifecycle helpers live in
//! `common/`.

mod common;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_db::auth::{
    hash_password, verify_password, PgAuthenticationService, PgAuthorizationService,
};
use crm_db::postgres::{PgGrantRepository, PgSessionRepository, PgStaffRepository};
use crm_db::run_migrations;
use crm_domain::error::DomainError;
use crm_domain::ids::{AgencyId, ClientProfileId, GrantId, StaffId};
use crm_domain::repositories::{GrantRepository, SessionRepository, StaffRepository};
use crm_domain::security::{
    Action, Grant, Operation, PasswordCredential, Resource, Scope, SessionToken, StaffStatus,
    StaffUser,
};
use crm_domain::services::{AuthenticationService, AuthorizationService};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

fn test_staff(email: &str, credential: Option<PasswordCredential>) -> StaffUser {
    let now = Utc::now().round_subsecs(6);
    StaffUser {
        staff_id: StaffId::new(),
        agency_id: AgencyId::new(),
        email: email.to_owned(),
        display_name: "Test User".to_owned(),
        status: StaffStatus::Active,
        credential,
        created_at: now,
        updated_at: now,
    }
}

async fn test_credential() -> PasswordCredential {
    let phc = hash_password(b"correct-horse-battery-staple")
        .await
        .expect("hash test password");
    assert!(phc.starts_with("$argon2id$"), "PHC shape, got: {phc}");
    PasswordCredential::new(phc, Utc::now().round_subsecs(6)).expect("PHC-shaped credential")
}

#[tokio::test]
async fn argon2_hash_and_verify_round_trip() {
    let phc = hash_password(b"a-secret-password")
        .await
        .expect("hash succeeds");
    assert!(phc.starts_with("$argon2id$"));
    assert!(
        verify_password(b"a-secret-password", &phc)
            .await
            .expect("verify runs"),
        "correct password verifies"
    );
    assert!(
        !verify_password(b"wrong-password", &phc)
            .await
            .expect("verify runs"),
        "wrong password does not verify"
    );
    // A malformed stored hash is a storage error, never a silent accept.
    assert!(verify_password(b"a-secret-password", "not-a-phc-hash")
        .await
        .is_err());
}

#[tokio::test]
async fn staff_repository_crud_round_trip() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let repo = PgStaffRepository::new(pool.clone());
    let credential = test_credential().await;
    let user = test_staff("op@example.com", Some(credential));

    let stored = repo.create(user.clone()).await.expect("create staff");
    assert_eq!(stored.staff_id, user.staff_id);

    let fetched = repo.get(user.staff_id).await.expect("get staff");
    assert_eq!(fetched.email, "op@example.com");
    assert_eq!(fetched.status, StaffStatus::Active);
    assert!(fetched.credential.is_some());
    // Timestamps survive the microsecond-precision round trip.
    assert_eq!(fetched.created_at, user.created_at);

    let found = repo
        .find_by_email("op@example.com")
        .await
        .expect("find by email")
        .expect("staff found");
    assert_eq!(found.staff_id, user.staff_id);
    assert!(repo
        .find_by_email("unknown@example.com")
        .await
        .expect("find unknown")
        .is_none());

    // Duplicate email is a conflict.
    let err = repo
        .create(test_staff("op@example.com", None))
        .await
        .expect_err("duplicate email must conflict");
    assert!(matches!(err, DomainError::Conflict(_)));

    // Suspend persists.
    let mut suspended = fetched;
    suspended.suspend();
    let updated = repo.update(suspended).await.expect("update staff");
    assert_eq!(updated.status, StaffStatus::Suspended);
    assert_eq!(
        repo.get(user.staff_id).await.expect("refetch").status,
        StaffStatus::Suspended
    );

    // Unknown id is NotFound.
    let err = repo
        .get(StaffId::new())
        .await
        .expect_err("unknown staff must be NotFound");
    assert!(matches!(err, DomainError::NotFound { .. }));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn grant_repository_assign_list_revoke() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let staff_repo = PgStaffRepository::new(pool.clone());
    let grant_repo = PgGrantRepository::new(pool.clone());
    let user = staff_repo
        .create(test_staff("grants@example.com", None))
        .await
        .expect("create staff");
    let agency_id = user.agency_id;
    let profile = ClientProfileId::new();

    let grant = Grant {
        grant_id: GrantId::new(),
        action: Action {
            resource: Resource::Contact,
            operation: Operation::Read,
        },
        scope: Scope::ClientProfile {
            agency_id,
            client_profile_id: profile,
        },
    };
    grant_repo
        .assign(user.staff_id, grant)
        .await
        .expect("assign grant");
    let agency_grant = Grant {
        grant_id: GrantId::new(),
        action: Action {
            resource: Resource::Campaign,
            operation: Operation::Create,
        },
        scope: Scope::Agency { agency_id },
    };
    grant_repo
        .assign(user.staff_id, agency_grant)
        .await
        .expect("assign agency grant");

    let grants = grant_repo
        .list_for_staff(user.staff_id)
        .await
        .expect("list grants");
    assert_eq!(grants.len(), 2);
    assert!(grants.contains(&grant));
    // The profile scope round-trips with its agency and profile ids.
    assert!(grants.iter().any(|g| g.scope
        == Scope::ClientProfile {
            agency_id,
            client_profile_id: profile
        }));

    // Revocation tombstones: the grant disappears from the live list.
    grant_repo
        .revoke(user.staff_id, grant.grant_id)
        .await
        .expect("revoke grant");
    let grants = grant_repo
        .list_for_staff(user.staff_id)
        .await
        .expect("list after revoke");
    assert_eq!(grants.len(), 1);
    assert!(!grants.contains(&grant));
    // Revoking again is a no-op, not an error.
    grant_repo
        .revoke(user.staff_id, grant.grant_id)
        .await
        .expect("re-revoke is idempotent");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn authentication_login_refresh_revoke() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let staff_repo = PgStaffRepository::new(pool.clone());
    let user = staff_repo
        .create(test_staff(
            "login@example.com",
            Some(test_credential().await),
        ))
        .await
        .expect("create staff");
    let auth = PgAuthenticationService::new(pool.clone());

    // Happy path: login issues a session whose token is a UUID rendering.
    let issued = auth
        .authenticate("login@example.com", b"correct-horse-battery-staple")
        .await
        .expect("login succeeds");
    assert_eq!(issued.session.staff_id, user.staff_id);
    assert!(!issued.session.is_revoked());
    // Only the token hash is persisted.
    let sessions = PgSessionRepository::new(pool.clone());
    let stored = sessions
        .get_by_token_hash(&issued.token.token_hash())
        .await
        .expect("lookup by hash")
        .expect("session stored");
    assert_eq!(stored.session_id, issued.session.session_id);

    // Email normalization: case-insensitive login.
    auth.authenticate("LOGIN@EXAMPLE.COM", b"correct-horse-battery-staple")
        .await
        .expect("login is case-insensitive on email");

    // Wrong password and unknown email are both Unauthorized.
    let err = auth
        .authenticate("login@example.com", b"wrong-password")
        .await
        .expect_err("wrong password must fail");
    assert!(matches!(err, DomainError::Unauthorized));
    let err = auth
        .authenticate("nobody@example.com", b"correct-horse-battery-staple")
        .await
        .expect_err("unknown email must fail");
    assert!(matches!(err, DomainError::Unauthorized));

    // Refresh extends the session.
    let refreshed = auth.refresh(&issued.token).await.expect("refresh works");
    assert_eq!(refreshed.session_id, issued.session.session_id);
    assert!(refreshed.expires_at >= issued.session.expires_at);

    // Refresh of an unknown token is Unauthorized.
    let err = auth
        .refresh(&SessionToken::new())
        .await
        .expect_err("unknown token refresh must fail");
    assert!(matches!(err, DomainError::Unauthorized));

    // Revoke, then the token is dead.
    auth.revoke(&issued.token).await.expect("revoke works");
    let err = auth
        .refresh(&issued.token)
        .await
        .expect_err("revoked token refresh must fail");
    assert!(matches!(err, DomainError::Unauthorized));
    // Revoking again is idempotent.
    auth.revoke(&issued.token).await.expect("re-revoke ok");
    // Revoking an unknown token is also fine (no existence leak).
    auth.revoke(&SessionToken::new())
        .await
        .expect("unknown revoke ok");

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn suspended_staff_cannot_authenticate() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let staff_repo = PgStaffRepository::new(pool.clone());
    let mut user = staff_repo
        .create(test_staff(
            "susp@example.com",
            Some(test_credential().await),
        ))
        .await
        .expect("create staff");
    user.suspend();
    staff_repo.update(user).await.expect("suspend persists");

    let auth = PgAuthenticationService::new(pool.clone());
    let err = auth
        .authenticate("susp@example.com", b"correct-horse-battery-staple")
        .await
        .expect_err("suspended login must fail");
    // Indistinguishable from a wrong password: the error must not reveal
    // the account exists or is suspended.
    assert!(matches!(err, DomainError::Unauthorized));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn authorization_grant_gating() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let staff_repo = PgStaffRepository::new(pool.clone());
    let grant_repo = PgGrantRepository::new(pool.clone());
    let user = staff_repo
        .create(test_staff(
            "authz@example.com",
            Some(test_credential().await),
        ))
        .await
        .expect("create staff");
    let profile = ClientProfileId::new();
    let scope = Scope::ClientProfile {
        agency_id: user.agency_id,
        client_profile_id: profile,
    };
    grant_repo
        .assign(
            user.staff_id,
            Grant {
                grant_id: GrantId::new(),
                action: Action {
                    resource: Resource::Contact,
                    operation: Operation::Read,
                },
                scope,
            },
        )
        .await
        .expect("assign read grant");

    let auth = PgAuthenticationService::new(pool.clone());
    let authz = PgAuthorizationService::new(pool.clone());
    let issued = auth
        .authenticate("authz@example.com", b"correct-horse-battery-staple")
        .await
        .expect("login");
    let correlation = Uuid::new_v4();

    // The held grant authorizes.
    let ctx = authz
        .authorize(
            &issued.token,
            &Action {
                resource: Resource::Contact,
                operation: Operation::Read,
            },
            &scope,
            correlation,
        )
        .await
        .expect("held grant authorizes");
    assert_eq!(ctx.actor_id, crm_domain::ids::ActorId::from(user.staff_id));
    assert_eq!(ctx.actor_kind, crm_domain::audit::ActorKind::Human);
    assert_eq!(ctx.client_profile_id, Some(profile));
    assert_eq!(ctx.correlation_id, correlation);

    // An unheld operation is forbidden.
    let err = authz
        .authorize(
            &issued.token,
            &Action {
                resource: Resource::Contact,
                operation: Operation::Delete,
            },
            &scope,
            correlation,
        )
        .await
        .expect_err("unheld operation must be forbidden");
    assert!(matches!(err, DomainError::Forbidden(_)));

    // Another profile is forbidden.
    let other_scope = Scope::ClientProfile {
        agency_id: user.agency_id,
        client_profile_id: ClientProfileId::new(),
    };
    let err = authz
        .authorize(
            &issued.token,
            &Action {
                resource: Resource::Contact,
                operation: Operation::Read,
            },
            &other_scope,
            correlation,
        )
        .await
        .expect_err("other profile must be forbidden");
    assert!(matches!(err, DomainError::Forbidden(_)));

    // Revoking the grant stops authorization immediately.
    let grants = grant_repo
        .list_for_staff(user.staff_id)
        .await
        .expect("list grants");
    grant_repo
        .revoke(user.staff_id, grants[0].grant_id)
        .await
        .expect("revoke");
    let err = authz
        .authorize(
            &issued.token,
            &Action {
                resource: Resource::Contact,
                operation: Operation::Read,
            },
            &scope,
            correlation,
        )
        .await
        .expect_err("revoked grant must forbid");
    assert!(matches!(err, DomainError::Forbidden(_)));

    drop_db(&admin_url, &db_name, pool).await;
}
