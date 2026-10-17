// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Core CRM vertical slice (task 05-CRM-02).
//!
//! [`PgClientProfileRepository`] implements
//! [`ClientProfileRepository`](crm_domain::repositories::ClientProfileRepository)
//! over the `client_profiles` table (extended to a real entity table by
//! migration 0005); [`PgAssetRepository`] implements
//! [`AssetRepository`](crm_domain::repositories::AssetRepository) over
//! the `assets` table.
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::asset::{Asset, AssetKind, AssetStatus};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AgencyId, AssetId, ClientProfileId};
use crm_domain::profile::{ClientProfile, ProfileStatus};
use crm_domain::repositories::{AssetRepository, ClientProfileRepository};

use crate::postgres::map_storage_error;

/// Renders a [`ProfileStatus`] as its stable database string.
fn profile_status_as_str(status: ProfileStatus) -> &'static str {
    match status {
        ProfileStatus::Active => "active",
        ProfileStatus::Suspended => "suspended",
        ProfileStatus::Archived => "archived",
    }
}

/// Parses a [`ProfileStatus`] from its database string.
fn profile_status_from_str(raw: &str) -> Result<ProfileStatus, DomainError> {
    match raw {
        "active" => Ok(ProfileStatus::Active),
        "suspended" => Ok(ProfileStatus::Suspended),
        "archived" => Ok(ProfileStatus::Archived),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown profile status '{other}'"
        ))),
    }
}

/// Renders an [`AssetKind`] as its stable database string.
fn asset_kind_as_str(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Document => "document",
        AssetKind::Image => "image",
        AssetKind::Audio => "audio",
        AssetKind::Video => "video",
        AssetKind::Other => "other",
    }
}

/// Parses an [`AssetKind`] from its database string.
fn asset_kind_from_str(raw: &str) -> Result<AssetKind, DomainError> {
    match raw {
        "document" => Ok(AssetKind::Document),
        "image" => Ok(AssetKind::Image),
        "audio" => Ok(AssetKind::Audio),
        "video" => Ok(AssetKind::Video),
        "other" => Ok(AssetKind::Other),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown asset kind '{other}'"
        ))),
    }
}

/// Renders an [`AssetStatus`] as its stable database string.
fn asset_status_as_str(status: AssetStatus) -> &'static str {
    match status {
        AssetStatus::Available => "available",
        AssetStatus::Archived => "archived",
    }
}

/// Parses an [`AssetStatus`] from its database string.
fn asset_status_from_str(raw: &str) -> Result<AssetStatus, DomainError> {
    match raw {
        "available" => Ok(AssetStatus::Available),
        "archived" => Ok(AssetStatus::Archived),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown asset status '{other}'"
        ))),
    }
}

/// Columns selected for a [`ClientProfile`] row.
const PROFILE_COLUMNS: &str = "client_profile_id, agency_id, name, status, created_at, updated_at";

/// Columns selected for an [`Asset`] row.
const ASSET_COLUMNS: &str = "asset_id, client_profile_id, uploaded_by, file_name, mime_type, \
     size_bytes, sha256, kind, status, created_at";

/// SQLx implementation of [`ClientProfileRepository`].
#[derive(Debug, Clone)]
pub struct PgClientProfileRepository {
    pool: PgPool,
}

impl PgClientProfileRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl ClientProfileRepository for PgClientProfileRepository {
    async fn create(&self, profile: ClientProfile) -> Result<ClientProfile, DomainError> {
        // The `ensure_client_profile` upsert may have created a placeholder
        // row (id only) when contacts were created under a caller-supplied
        // id; claim it with the full entity data instead of conflicting.
        sqlx::query(
            "INSERT INTO client_profiles (client_profile_id, agency_id, name, status, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (client_profile_id) DO UPDATE
             SET agency_id = EXCLUDED.agency_id,
                 name = EXCLUDED.name,
                 status = EXCLUDED.status,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(profile.profile_id.as_uuid())
        .bind(profile.agency_id.as_uuid())
        .bind(&profile.name)
        .bind(profile_status_as_str(profile.status))
        .bind(profile.created_at)
        .bind(profile.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(profile)
    }

    async fn get(&self, profile_id: ClientProfileId) -> Result<ClientProfile, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {PROFILE_COLUMNS} FROM client_profiles WHERE client_profile_id = $1"
        ))
        .bind(profile_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_profile(&row),
            None => Err(DomainError::NotFound {
                kind: "client_profile".to_owned(),
                id: profile_id.to_string(),
            }),
        }
    }

    async fn update(&self, profile: ClientProfile) -> Result<ClientProfile, DomainError> {
        let result = sqlx::query(
            "UPDATE client_profiles
             SET agency_id = $2, name = $3, status = $4, updated_at = $5
             WHERE client_profile_id = $1",
        )
        .bind(profile.profile_id.as_uuid())
        .bind(profile.agency_id.as_uuid())
        .bind(&profile.name)
        .bind(profile_status_as_str(profile.status))
        .bind(profile.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "client_profile".to_owned(),
                id: profile.profile_id.to_string(),
            });
        }
        Ok(profile)
    }

    async fn list_for_agency(
        &self,
        agency_id: AgencyId,
        limit: u64,
    ) -> Result<Vec<ClientProfile>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {PROFILE_COLUMNS} FROM client_profiles
             WHERE agency_id = $1
             ORDER BY created_at ASC, client_profile_id ASC
             LIMIT $2"
        ))
        .bind(agency_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_profile).collect()
    }
}

/// Hydrates a `client_profiles` row into a [`ClientProfile`].
///
/// Placeholder rows created by the `ensure_client_profile` upsert carry
/// no agency or name; they surface as a validation error here rather
/// than a half-formed entity — callers must claim them via `create`
/// before use.
fn row_to_profile(row: &sqlx::postgres::PgRow) -> Result<ClientProfile, DomainError> {
    let agency_id: Option<Uuid> = row.try_get("agency_id").map_err(map_storage_error)?;
    let name: Option<String> = row.try_get("name").map_err(map_storage_error)?;
    let (agency_id, name) = match (agency_id, name) {
        (Some(agency_id), Some(name)) => (AgencyId::from(agency_id), name),
        _ => {
            return Err(DomainError::Validation(
                "storage error: client profile row is an unclaimed placeholder".to_owned(),
            ))
        }
    };
    Ok(ClientProfile {
        profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        agency_id,
        name,
        status: profile_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(map_storage_error)?,
    })
}

/// SQLx implementation of [`AssetRepository`].
#[derive(Debug, Clone)]
pub struct PgAssetRepository {
    pool: PgPool,
}

impl PgAssetRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl AssetRepository for PgAssetRepository {
    async fn create(&self, asset: Asset) -> Result<Asset, DomainError> {
        // The vertical slice addresses profiles by caller-supplied id;
        // satisfy the foreign key before inserting the asset.
        crate::postgres::ensure_client_profile(&self.pool, asset.client_profile_id).await?;
        let result = sqlx::query(
            "INSERT INTO assets (asset_id, client_profile_id, uploaded_by, file_name, mime_type, size_bytes, sha256, kind, status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT DO NOTHING",
        )
        .bind(asset.asset_id.as_uuid())
        .bind(asset.client_profile_id.as_uuid())
        .bind(asset.uploaded_by.as_uuid())
        .bind(&asset.file_name)
        .bind(&asset.mime_type)
        .bind(asset.size_bytes as i64)
        .bind(&asset.sha256[..])
        .bind(asset_kind_as_str(asset.kind))
        .bind(asset_status_as_str(asset.status))
        .bind(asset.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            // The unique (client_profile_id, sha256) index fired: these
            // bytes are already registered under this profile — return
            // the existing record instead of a duplicate.
            let existing = self
                .find_by_sha256(asset.client_profile_id, &asset.sha256)
                .await?
                .ok_or_else(|| {
                    DomainError::Validation(
                        "storage error: asset insert conflicted but no row found".to_owned(),
                    )
                })?;
            return Ok(existing);
        }
        Ok(asset)
    }

    async fn get(&self, asset_id: AssetId) -> Result<Asset, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {ASSET_COLUMNS} FROM assets WHERE asset_id = $1"
        ))
        .bind(asset_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_asset(&row),
            None => Err(DomainError::NotFound {
                kind: "asset".to_owned(),
                id: asset_id.to_string(),
            }),
        }
    }

    async fn update(&self, asset: Asset) -> Result<Asset, DomainError> {
        let result = sqlx::query(
            "UPDATE assets
             SET file_name = $2, mime_type = $3, kind = $4, status = $5
             WHERE asset_id = $1",
        )
        .bind(asset.asset_id.as_uuid())
        .bind(&asset.file_name)
        .bind(&asset.mime_type)
        .bind(asset_kind_as_str(asset.kind))
        .bind(asset_status_as_str(asset.status))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "asset".to_owned(),
                id: asset.asset_id.to_string(),
            });
        }
        Ok(asset)
    }

    async fn list_for_client(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Asset>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {ASSET_COLUMNS} FROM assets
             WHERE client_profile_id = $1
             ORDER BY created_at ASC, asset_id ASC
             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_asset).collect()
    }

    async fn find_by_sha256(
        &self,
        client_profile_id: ClientProfileId,
        sha256: &[u8; 32],
    ) -> Result<Option<Asset>, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {ASSET_COLUMNS} FROM assets
             WHERE client_profile_id = $1 AND sha256 = $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(&sha256[..])
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        row.map(|row| row_to_asset(&row)).transpose()
    }
}

/// Hydrates an `assets` row into an [`Asset`].
fn row_to_asset(row: &sqlx::postgres::PgRow) -> Result<Asset, DomainError> {
    let sha256_bytes: Vec<u8> = row.try_get("sha256").map_err(map_storage_error)?;
    let sha256: [u8; 32] = sha256_bytes
        .try_into()
        .map_err(|_| DomainError::Validation("storage error: corrupt asset sha256".to_owned()))?;
    let size_bytes: i64 = row.try_get("size_bytes").map_err(map_storage_error)?;
    Ok(Asset {
        asset_id: AssetId::from(
            row.try_get::<Uuid, _>("asset_id")
                .map_err(map_storage_error)?,
        ),
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        uploaded_by: ActorId::from(
            row.try_get::<Uuid, _>("uploaded_by")
                .map_err(map_storage_error)?,
        ),
        file_name: row.try_get("file_name").map_err(map_storage_error)?,
        mime_type: row.try_get("mime_type").map_err(map_storage_error)?,
        size_bytes: u64::try_from(size_bytes).map_err(|_| {
            DomainError::Validation("storage error: negative asset size".to_owned())
        })?,
        sha256,
        kind: asset_kind_from_str(
            row.try_get::<String, _>("kind")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        status: asset_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
    })
}
