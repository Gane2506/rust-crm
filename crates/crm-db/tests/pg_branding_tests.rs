// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the branding vertical slice (task UI2-03).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.
//! `postgres://postgres:postgres@localhost:5432/crm_test`
//!
//! ```sh
//! DATABASE_URL=<redacted>
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Each test creates its own database, migrates a fresh schema, and
//! drops it afterwards.

mod common;

use common::{drop_db, fresh_db_raw};
use crm_db::branding::PgBrandingRepository;
use crm_db::run_migrations;
use crm_domain::branding::{BrandingRepository, BrandingTheme};
use crm_domain::ids::AgencyId;

fn test_theme() -> BrandingTheme {
    BrandingTheme::new(
        Some("https://example.com/logo.png".to_string()),
        Some("oklch(0.55 0.2 250)".to_string()),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .expect("valid test theme")
}

#[tokio::test]
async fn branding_get_returns_none_when_unset() {
    let (pool, _admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool).await.expect("migrate");
    let repo = PgBrandingRepository::new(pool.clone());
    let agency = AgencyId::new();

    let result = repo.get(&agency).await.expect("get");
    assert!(result.is_none());

    drop_db(&_admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn branding_upsert_versions() {
    let (pool, _admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool).await.expect("migrate");
    let repo = PgBrandingRepository::new(pool.clone());
    let agency = AgencyId::new();

    let v1 = repo
        .upsert(&agency, &test_theme())
        .await
        .expect("upsert v1");
    assert_eq!(v1.version, 1);
    assert_eq!(v1.theme.primary(), Some("oklch(0.55 0.2 250)"));

    let v2 = repo
        .upsert(&agency, &BrandingTheme::empty())
        .await
        .expect("upsert v2");
    assert_eq!(v2.version, 2);

    let current = repo.get(&agency).await.expect("get").expect("some");
    assert_eq!(current.version, 2);
    assert_eq!(current.theme.primary(), None);

    drop_db(&_admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn branding_is_agency_scoped() {
    let (pool, _admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool).await.expect("migrate");
    let repo = PgBrandingRepository::new(pool.clone());

    let a1 = AgencyId::new();
    let a2 = AgencyId::new();
    repo.upsert(&a1, &test_theme()).await.expect("upsert a1");

    assert!(repo.get(&a2).await.expect("get a2").is_none());

    drop_db(&_admin_url, &db_name, pool).await;
}
