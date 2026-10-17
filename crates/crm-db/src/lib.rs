// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! `crm-db`: persistence layer for the Rust CRM.
//!
//! SQLx repositories, embedded migrations, and the transactional outbox land
//! here in task `02-DB-01` (persistence foundation). Until then this crate
//! exposes the shared [`DbError`] type and the [`postgres`] module stub so
//! downstream crates can depend on `crm-db` without API churn.

pub mod postgres;

use thiserror::Error;

/// Errors raised by the persistence layer.
#[derive(Debug, Error)]
pub enum DbError {
    /// A SQLx driver, connection, or query failure.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// A migration failure.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}
