// SPDX-License-Identifier: GPL-3.0-only
//! Shared helpers for the `crm-db` integration tests.
//!
//! Every test gets a dedicated database (`crm_t_<pid>_<n>`), so tests stay
//! isolated and may run in parallel. [`fresh_db_raw`] leaves the database
//! empty for tests that drive migrations themselves; [`drop_db`]
//! deterministically tears it down. Test-specific helpers (migrated setup,
//! profile seeding, table assertions) live in each test target so no
//! per-target dead code remains. A test database is only left behind if
//! its test panics; drop strays with `DROP DATABASE "crm_t_<pid>_<n>"`.

use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::PgPool;

use crm_db::{connect, DATABASE_URL_ENV};

/// Counter distinguishing this process's test databases.
static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads `DATABASE_URL`, panicking with setup instructions when unset.
fn admin_url() -> String {
    std::env::var(DATABASE_URL_ENV).unwrap_or_else(|_| {
        panic!(
            "{DATABASE_URL_ENV} must be set for crm-db integration tests; \
             start Postgres with `docker compose up -d` (repo root)"
        )
    })
}

/// Creates a dedicated test database and returns the pool plus the admin
/// URL and db name for teardown. The schema is left empty; callers apply
/// [`run_migrations`] (or drive [`crm_db::MIGRATOR`] directly) themselves.
pub async fn fresh_db_raw() -> (PgPool, String, String) {
    let admin_url = admin_url();
    let admin = connect(&admin_url)
        .await
        .expect("connect to the admin database");
    let db_name = format!(
        "crm_t_{}_{}",
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
    let pool = connect(&url).await.expect("connect to test database");
    (pool, admin_url, db_name)
}

/// Drops a test database created by [`fresh_db_raw`].
pub async fn drop_db(admin_url: &str, db_name: &str, pool: PgPool) {
    pool.close().await;
    let admin = connect(admin_url)
        .await
        .expect("reconnect to the admin database");
    // `pool.close()` is graceful: a just-returned backend can still be
    // registered server-side, which makes DROP DATABASE fail with 55006.
    // Terminate stray backends first so the drop is deterministic.
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
