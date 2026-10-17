// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Operator migration CLI for the Rust CRM.
//!
//! The API server applies pending migrations automatically at startup;
//! this example is for operators who need to inspect or roll back the
//! migration chain explicitly.
//!
//! ```sh
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
//!     cargo run -p crm-db --example migrate -- status
//! DATABASE_URL=... cargo run -p crm-db --example migrate -- up
//! DATABASE_URL=... cargo run -p crm-db --example migrate -- down 1
//! ```
//!
//! `down <target>` runs the down migrations back to (and excluding)
//! `<target>`; `down 0` rolls back everything. Down migrations are
//! destructive — the required explicit target version is the guard
//! against accidental use.

use crm_db::{run_migrations, DATABASE_URL_ENV, DATABASE_URL_EXAMPLE, MIGRATOR};

/// Prints usage and exits non-zero.
fn usage() -> ! {
    eprintln!("usage: migrate <status|up|down <target-version>>");
    std::process::exit(2);
}

/// Lists applied migration versions, oldest first.
async fn status(pool: &sqlx::PgPool) -> Result<(), String> {
    let journal_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = '_sqlx_migrations')",
    )
    .fetch_one(pool)
    .await
    .map_err(|error| format!("check migration journal: {error}"))?;
    if !journal_exists {
        println!("no migrations applied");
        return Ok(());
    }
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .map_err(|error| format!("read migration journal: {error}"))?;
    if versions.is_empty() {
        println!("no migrations applied");
    } else {
        for version in &versions {
            let description: String =
                sqlx::query_scalar("SELECT description FROM _sqlx_migrations WHERE version = $1")
                    .bind(version)
                    .fetch_one(pool)
                    .await
                    .map_err(|error| format!("read migration journal: {error}"))?;
            println!("{version:>4}  {description}");
        }
    }
    Ok(())
}

/// Applies pending migrations (same as server startup).
async fn up(pool: &sqlx::PgPool) -> Result<(), String> {
    run_migrations(pool)
        .await
        .map_err(|error| format!("apply migrations: {error}"))?;
    println!("migrations applied");
    Ok(())
}

/// Rolls back to `target`, running down migrations newest-first.
async fn down(pool: &sqlx::PgPool, target: i64) -> Result<(), String> {
    MIGRATOR
        .undo(pool, target)
        .await
        .map_err(|error| format!("roll back to {target}: {error}"))?;
    println!("rolled back to version {target}");
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("migrate failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let url = std::env::var(DATABASE_URL_ENV)
        .map_err(|_| format!("{DATABASE_URL_ENV} must be set, e.g. {DATABASE_URL_EXAMPLE}"))?;
    let pool = crm_db::connect(&url)
        .await
        .map_err(|error| format!("connect to database: {error}"))?;

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("status") => status(&pool).await,
        Some("up") => up(&pool).await,
        Some("down") => {
            let target = args.next().ok_or_else(|| {
                "down requires an explicit target version, e.g. `down 1` (`down 0` rolls back everything)".to_owned()
            })?;
            let target: i64 = target
                .parse()
                .map_err(|_| format!("invalid target version '{target}'"))?;
            down(&pool, target).await
        }
        _ => usage(),
    }
}
