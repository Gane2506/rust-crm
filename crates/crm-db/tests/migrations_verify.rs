// SPDX-License-Identifier: GPL-3.0-only
//! Migration up/down verification for the 02-DB-03 operational story.
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.:
//!
//! ```sh
//! docker compose up -d            # ephemeral Postgres 16 on :5432
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Unlike `db_integration` (which migrates immediately), these tests start
//! from an empty database and drive [`crm_db::MIGRATOR`] themselves: every
//! migration must apply cleanly, the journal must record each version in
//! order, and every down migration must remove exactly its own tables.

mod common;

use common::{drop_db, fresh_db_raw};
use crm_db::MIGRATOR;
use sqlx::PgPool;

/// Reports whether `table` exists in the `public` schema.
async fn table_exists(pool: &PgPool, table: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = $1)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .expect("check table existence")
}

/// Tables owned by the migration chain, in dependency order.
const MIGRATED_TABLES: &[&str] = &[
    "client_profiles",
    "contacts",
    "audit_events",
    "outbox",
    "idempotency_keys",
    "staff_users",
    "delegations",
    "sessions",
    "grants",
    "assets",
    "email_threads",
    "email_messages",
    "email_attachments",
    "campaigns",
    "campaign_steps",
    "prospect_pools",
    "pool_members",
    "opt_outs",
    "conversions",
    "agent_runs",
    "tool_calls",
    "handoffs",
    "workflows",
    "workflow_nodes",
    "workflow_edges",
    "workflow_triggers",
    "workflow_versions",
    "forms",
    "form_fields",
    "form_submissions",
    "feed_items",
    "invoices",
    "invoice_lines",
    "payments",
    "signing_templates",
    "contracts",
    "signing_evidence",
    "calls",
    "call_recordings",
];

/// Migration versions the chain currently ships, in apply order.
const MIGRATED_VERSIONS: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

/// Reads the applied versions from the sqlx migration journal.
async fn journal_versions(pool: &sqlx::PgPool) -> Vec<i64> {
    sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .expect("read migration journal")
}

#[tokio::test]
async fn migrations_apply_up_and_roll_back_down() {
    let (pool, admin_url, db_name) = fresh_db_raw().await;

    for table in MIGRATED_TABLES {
        assert!(
            !table_exists(&pool, table).await,
            "fresh database must not contain '{table}' before migrating"
        );
    }

    MIGRATOR.run(&pool).await.expect("migrate up");
    for table in MIGRATED_TABLES {
        assert!(
            table_exists(&pool, table).await,
            "'{table}' must exist after migrating up"
        );
    }
    assert_eq!(
        journal_versions(&pool).await,
        MIGRATED_VERSIONS,
        "journal must record every version in order"
    );

    // Re-running the ups is a no-op, not an error.
    MIGRATOR
        .run(&pool)
        .await
        .expect("re-running ups is a no-op");

    // Rolling back to zero removes every migrated table; the journal
    // table itself stays (sqlx owns it).
    MIGRATOR
        .undo(&pool, 0)
        .await
        .expect("roll back to version 0");
    for table in MIGRATED_TABLES {
        assert!(
            !table_exists(&pool, table).await,
            "'{table}' must be gone after rolling back"
        );
    }
    assert!(
        journal_versions(&pool).await.is_empty(),
        "journal must be empty after full rollback"
    );

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn migrations_reapply_cleanly_after_rollback() {
    let (pool, admin_url, db_name) = fresh_db_raw().await;

    MIGRATOR.run(&pool).await.expect("migrate up");
    MIGRATOR
        .undo(&pool, 0)
        .await
        .expect("roll back to version 0");
    // The chain must survive a full down/up cycle — this is what a
    // botched deploy followed by a re-deploy does.
    MIGRATOR.run(&pool).await.expect("migrate up again");
    for table in MIGRATED_TABLES {
        assert!(
            table_exists(&pool, table).await,
            "'{table}' must exist after re-applying"
        );
    }
    assert_eq!(journal_versions(&pool).await, MIGRATED_VERSIONS);

    drop_db(&admin_url, &db_name, pool).await;
}
