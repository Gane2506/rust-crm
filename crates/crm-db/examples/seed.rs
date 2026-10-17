// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Dev-only database seed for the Rust CRM.
//!
//! Inserts the demo client profile plus a handful of sample contacts through
//! the real service layer (so normalization, dedupe and audit events behave
//! exactly as in production).
//!
//! Run:
//!
//! ```sh
//! docker compose up -d   # ephemeral Postgres 16 on :5432
//! CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
//!     cargo run -p crm-db --example seed
//! ```
//!
//! The seed is **idempotent**: re-running it skips contacts whose normalized
//! email already exists. Without `CRM_SEED_DEV=1` it prints the plan and
//! exits without writing anything, so it can never seed a production
//! database by accident. Migrations run first, so a fresh database works.

use uuid::Uuid;

use crm_core::memory::ContactServiceImpl;
use crm_db::postgres::{PgAuditRepository, PgAuditService, PgContactRepository};
use crm_db::{run_migrations, DATABASE_URL_ENV, DATABASE_URL_EXAMPLE};
use crm_domain::audit::ActorKind;
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, ClientProfileId};
use crm_domain::services::ActionContext;
use crm_domain::services::{ContactService, NewContact};

/// Demo tenant also used by the `crm-web` vertical slice until real
/// auth/tenancy lands (task 03-SEC-01).
const DEMO_PROFILE_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

/// Sample contacts: (emails, phones, source).
const SEED_CONTACTS: &[(&[&str], &[&str], &str)] = &[
    (&["ada@example.com"], &["+1-555-0101"], "web-form"),
    (&["grace@example.com"], &["+1-555-0102"], "csv-import"),
    (
        &["linus@example.com", "torvalds@example.com"],
        &["+1-555-0103"],
        "web-form",
    ),
    (&["margaret@example.com"], &[], "referral"),
    (&["alan@example.com"], &["+1-555-0105"], "phone-call"),
];

/// Seeds the demo profile and sample contacts. Returns `(created, skipped)`:
/// skipped contacts already existed (idempotent re-runs).
async fn seed(pool: &sqlx::PgPool) -> Result<(usize, usize), DomainError> {
    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let service = ContactServiceImpl::new(
        PgContactRepository::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let mut created = 0;
    let mut skipped = 0;
    for (emails, phones, source) in SEED_CONTACTS {
        let input = NewContact {
            client_profile_id: profile,
            emails: emails.iter().map(ToString::to_string).collect(),
            phones: phones.iter().map(ToString::to_string).collect(),
            source: (*source).to_owned(),
        };
        match service.create_contact(input, &ctx).await {
            Ok(_) => {
                created += 1;
            }
            // Re-running the seed meets the previous run's emails; the
            // dedupe conflict means "already seeded", not a failure.
            Err(DomainError::Conflict(_)) => {
                skipped += 1;
            }
            Err(other) => return Err(other),
        }
    }
    Ok((created, skipped))
}

/// Env var gating writes: `CRM_SEED_DEV=1` is the explicit opt-in that
/// lets this example write to the database.
const SEED_DEV_ENV: &str = "CRM_SEED_DEV";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("seed failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let url = std::env::var(DATABASE_URL_ENV)
        .map_err(|_| format!("{DATABASE_URL_ENV} must be set, e.g. {DATABASE_URL_EXAMPLE}"))?;

    if std::env::var(SEED_DEV_ENV).as_deref() != Ok("1") {
        println!("Dry run (set {SEED_DEV_ENV}=1 to write). Plan:");
        println!("  database: {url}");
        println!("  1. apply pending migrations");
        println!("  2. ensure demo client profile {DEMO_PROFILE_ID}");
        println!(
            "  3. create {} sample contacts (idempotent)",
            SEED_CONTACTS.len()
        );
        return Ok(());
    }

    let pool = crm_db::connect(&url)
        .await
        .map_err(|error| format!("connect to database: {error}"))?;
    run_migrations(&pool)
        .await
        .map_err(|error| format!("apply migrations: {error}"))?;
    let (created, skipped) = seed(&pool)
        .await
        .map_err(|error| format!("seed contacts: {error}"))?;
    println!("seed complete: {created} contacts created, {skipped} already present");
    Ok(())
}
