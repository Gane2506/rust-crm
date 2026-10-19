// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repository for the branding vertical slice (task UI2-03).
//!
//! [`PgBrandingRepository`] implements
//! [`BrandingRepository`](crm_domain::branding::BrandingRepository) over
//! the `0017_branding` table, following the `agency_settings` pattern:
//! one row per version, `version` globally unique and monotonic,
//! `payload` JSONB carrying the theme. Hydration goes through the
//! domain's manual `Deserialize` (validating constructor), so a corrupt
//! row surfaces as a storage error — never as a bypassed invariant.
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available.

use std::future::Future;

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::branding::{BrandingRepository, BrandingTheme, BrandingVersion};
use crm_domain::error::DomainError;
use crm_domain::ids::AgencyId;

use crate::postgres::map_storage_error;

/// PostgreSQL implementation of
/// [`BrandingRepository`](crm_domain::branding::BrandingRepository).
#[derive(Debug, Clone)]
pub struct PgBrandingRepository {
    pool: PgPool,
}

impl PgBrandingRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Hydrates a [`BrandingVersion`] from a `branding` row.
fn hydrate_row(version: i64, payload: serde_json::Value) -> Result<BrandingVersion, DomainError> {
    let version = u64::try_from(version).map_err(|_| {
        DomainError::Validation("storage error: branding version does not fit in u64".to_string())
    })?;
    let theme: BrandingTheme = serde_json::from_value(payload).map_err(|error| {
        DomainError::Validation(format!("storage error: corrupt branding payload: {error}"))
    })?;
    Ok(BrandingVersion { version, theme })
}

// The trait declares `fn -> impl Future<Output = …> + Send + 'a`, so this
// impl must keep that form (an `async fn` does not satisfy the trait).
// `clippy::manual_async_fn` is allowed here with this justification:
// the shape is forced by the trait, not a style choice.
#[allow(clippy::manual_async_fn)]
impl BrandingRepository for PgBrandingRepository {
    fn get<'a>(
        &'a self,
        agency_id: &'a AgencyId,
    ) -> impl Future<Output = Result<Option<BrandingVersion>, DomainError>> + Send + 'a {
        async move {
            let row = sqlx::query(
                "SELECT version, payload FROM branding \
                 WHERE agency_id = $1 ORDER BY version DESC LIMIT 1",
            )
            .bind(agency_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_storage_error)?;
            row.map(|r| {
                let version: i64 = r.try_get("version").map_err(map_storage_error)?;
                let payload: serde_json::Value = r.try_get("payload").map_err(map_storage_error)?;
                hydrate_row(version, payload)
            })
            .transpose()
        }
    }

    fn upsert<'a>(
        &'a self,
        agency_id: &'a AgencyId,
        theme: &'a BrandingTheme,
    ) -> impl Future<Output = Result<BrandingVersion, DomainError>> + Send + 'a {
        async move {
            // Bump the version: max existing + 1, or 1 for the first row.
            // The `version` column is globally UNIQUE, so a concurrent
            // writer loses with a unique violation (surfaced as a storage
            // error) rather than silently overwriting.
            let current: Option<i64> =
                sqlx::query_scalar("SELECT MAX(version) FROM branding WHERE agency_id = $1")
                    .bind(agency_id.as_uuid())
                    .fetch_one(&self.pool)
                    .await
                    .map_err(map_storage_error)?;
            let next = current.unwrap_or(0) + 1;
            let version = u64::try_from(next).map_err(|_| {
                DomainError::Validation(
                    "storage error: branding version does not fit in u64".to_string(),
                )
            })?;
            let payload = serde_json::to_value(theme).map_err(|error| {
                DomainError::Validation(format!(
                    "storage error: cannot serialize branding payload: {error}"
                ))
            })?;
            sqlx::query(
                "INSERT INTO branding (branding_id, agency_id, version, payload) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(Uuid::new_v4())
            .bind(agency_id.as_uuid())
            .bind(next)
            .bind(payload)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
            Ok(BrandingVersion {
                version,
                theme: theme.clone(),
            })
        }
    }
}
