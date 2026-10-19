// SPDX-License-Identifier: GPL-3.0-only
//! `crm-db`: SQLx/PostgreSQL persistence layer for the Rust CRM.
//!
//! [`postgres`] holds the SQLx repository implementations of the
//! `crm-domain` repository traits, plus the transactional outbox writer.
//! Migrations live in `migrations/` and are embedded at compile time with
//! [`sqlx::migrate!`]; [`run_migrations`] applies them. Connection pools
//! are built from `DATABASE_URL` via [`create_pool`].
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.
//!
//! [`auth`] holds the Argon2id password hashing helpers and the
//! authentication/authorization services over the [`postgres`]
//! repositories.
//!
//! [`audit_log`] implements the [`AuditLogService`] security audit log
//! over the shared `audit_events` table.
//!
//! [`core_crm`] holds the SQLx repositories for the Core CRM vertical
//! slice: client profiles and file/media assets.
//!
//! [`email`] holds the SQLx repositories for the Email vertical slice:
//! messages, threads and attachments.

pub mod admin;
pub mod audit_log;
pub mod auth;
pub mod billing;
pub mod branding;
pub mod campaign;
pub mod core_crm;
pub mod dashboard;
pub mod email;
pub mod feed;
pub mod form;
pub mod jev;
pub mod postgres;
pub mod signing;
pub mod voice;
pub mod workflow;

use thiserror::Error;

/// Environment variable carrying the PostgreSQL connection URL.
pub const DATABASE_URL_ENV: &str = "DATABASE_URL";

/// Example connection URL used in documentation and error messages.
/// Points at the ephemeral Postgres from `docker-compose.yml`; never a
/// real secret.
pub const DATABASE_URL_EXAMPLE: &str = "postgres://postgres:postgres@localhost:5432/crm_test";

/// Errors raised by the persistence layer.
#[derive(Debug, Error)]
pub enum DbError {
    /// A SQLx driver, connection, or query failure.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// A migration failure.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// `DATABASE_URL` is missing.
    #[error("database configuration error: {0}")]
    Config(String),
}

/// Embedded SQLx migrations from `crates/crm-db/migrations/`.
///
/// Each version ships an up script (`NNNN_name.sql`) and a down script
/// (`NNNN_name.down.sql`); [`run_migrations`] applies the ups, while
/// downs run only via explicit [`sqlx::migrate::Migrator::undo`] calls.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Builds a PostgreSQL connection pool from `DATABASE_URL`.
///
/// Returns [`DbError::Config`] when the variable is not set; connection
/// failures surface as [`DbError::Sqlx`]. No value is ever unwrapped.
pub async fn create_pool() -> Result<sqlx::PgPool, DbError> {
    let url = std::env::var(DATABASE_URL_ENV).map_err(|_| {
        DbError::Config(format!(
            "{DATABASE_URL_ENV} must be set, e.g. {DATABASE_URL_EXAMPLE}"
        ))
    })?;
    connect(&url).await
}

/// Builds a PostgreSQL connection pool from an explicit URL.
///
/// Used by tests that create a dedicated database per test run.
///
/// The pool is tuned for the 1/2-core, 2/4 GiB envelope: 5 connections
/// bound the database load, a 5s acquire timeout fails fast instead of
/// queueing requests behind a saturated pool, and idle/lifetime
/// recycling keeps long-lived connections fresh.
pub async fn connect(url: &str) -> Result<sqlx::PgPool, DbError> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .idle_timeout(std::time::Duration::from_secs(300))
        .max_lifetime(std::time::Duration::from_secs(1800))
        .connect(url)
        .await
        .map_err(DbError::Sqlx)
}

/// Applies any pending embedded migrations to the pool's database.
///
/// Migrations are additive and tracked in `_sqlx_migrations`, so calling
/// this on every startup is safe.
pub async fn run_migrations(pool: &sqlx::PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await.map_err(DbError::Migrate)
}
