// SPDX-License-Identifier: GPL-3.0-only
//! Asset aggregate: file/media attachments owned by a client profile.
//!
//! Assets are the files the agency handles on a client's behalf —
//! documents, images, audio, video. Each asset is content-addressed by
//! its SHA-256 so re-uploads of the same bytes dedupe instead of
//! duplicating storage. Assets are never deleted: archival is terminal
//! and the row stays for audit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{ActorId, AssetId, ClientProfileId};

/// Kind of file asset, derived from the MIME type at registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetKind {
    /// PDFs, office documents, text files.
    Document,
    /// Raster/vector images.
    Image,
    /// Audio recordings and clips.
    Audio,
    /// Video files.
    Video,
    /// Anything else.
    Other,
}

impl AssetKind {
    /// Classifies a MIME type into a kind. Matching is case-insensitive
    /// on the type prefix; unknown types become [`AssetKind::Other`].
    ///
    /// ```
    /// use crm_domain::asset::AssetKind;
    ///
    /// assert_eq!(AssetKind::from_mime("image/png"), AssetKind::Image);
    /// assert_eq!(AssetKind::from_mime("application/pdf"), AssetKind::Document);
    /// assert_eq!(AssetKind::from_mime("audio/mpeg"), AssetKind::Audio);
    /// assert_eq!(AssetKind::from_mime("video/mp4"), AssetKind::Video);
    /// assert_eq!(AssetKind::from_mime("application/x-unknown"), AssetKind::Other);
    /// ```
    #[must_use]
    pub fn from_mime(mime_type: &str) -> Self {
        let lower = mime_type.trim().to_lowercase();
        if lower.starts_with("image/") {
            Self::Image
        } else if lower.starts_with("audio/") {
            Self::Audio
        } else if lower.starts_with("video/") {
            Self::Video
        } else if lower.starts_with("text/")
            || lower.starts_with("application/pdf")
            || lower.starts_with("application/msword")
            || lower.starts_with("application/vnd.openxmlformats")
            || lower.starts_with("application/vnd.ms-")
        {
            Self::Document
        } else {
            Self::Other
        }
    }
}

/// Lifecycle of an asset.
///
/// `Archived` is terminal — assets are never deleted, so the bytes and
/// their provenance stay retrievable for audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetStatus {
    /// Stored and retrievable.
    Available,
    /// Retired from use; terminal.
    Archived,
}

/// A file attached to a client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// Stable identifier of this asset.
    pub asset_id: AssetId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Staff member or agent principal that uploaded the file.
    pub uploaded_by: ActorId,
    /// Original file name, e.g. `"contract-draft.pdf"`.
    pub file_name: String,
    /// MIME type supplied at upload, e.g. `"application/pdf"`.
    pub mime_type: String,
    /// Size in bytes; must be greater than zero.
    pub size_bytes: u64,
    /// SHA-256 of the file bytes — the content address used for
    /// upload dedupe and integrity checks.
    pub sha256: [u8; 32],
    /// Kind derived from the MIME type.
    pub kind: AssetKind,
    /// Current lifecycle status.
    pub status: AssetStatus,
    /// When the asset was registered.
    pub created_at: DateTime<Utc>,
}

impl Asset {
    /// Registers a new available asset.
    ///
    /// Rejects a blank file name or MIME type and a zero size. The kind
    /// is derived from the MIME type via [`AssetKind::from_mime`].
    ///
    /// ```
    /// use crm_domain::asset::Asset;
    /// use crm_domain::ids::{ActorId, ClientProfileId};
    ///
    /// let asset = Asset::new(
    ///     ClientProfileId::new(),
    ///     ActorId::new(),
    ///     "contract-draft.pdf".to_string(),
    ///     "application/pdf".to_string(),
    ///     1024,
    ///     [7u8; 32],
    /// )
    /// .expect("valid asset");
    /// assert_eq!(asset.kind, crm_domain::asset::AssetKind::Document);
    ///
    /// assert!(Asset::new(
    ///     ClientProfileId::new(),
    ///     ActorId::new(),
    ///     "empty.bin".to_string(),
    ///     "application/octet-stream".to_string(),
    ///     0,
    ///     [7u8; 32],
    /// )
    /// .is_err());
    /// ```
    pub fn new(
        client_profile_id: ClientProfileId,
        uploaded_by: ActorId,
        file_name: String,
        mime_type: String,
        size_bytes: u64,
        sha256: [u8; 32],
    ) -> Result<Self, DomainError> {
        let file_name = file_name.trim().to_owned();
        if file_name.is_empty() {
            return Err(DomainError::Validation(
                "asset file name must not be empty".to_string(),
            ));
        }
        let mime_type = mime_type.trim().to_owned();
        if mime_type.is_empty() {
            return Err(DomainError::Validation(
                "asset MIME type must not be empty".to_string(),
            ));
        }
        if size_bytes == 0 {
            return Err(DomainError::Validation(
                "asset size must be greater than zero".to_string(),
            ));
        }
        let kind = AssetKind::from_mime(&mime_type);
        Ok(Self {
            asset_id: AssetId::new(),
            client_profile_id,
            uploaded_by,
            file_name,
            mime_type,
            size_bytes,
            sha256,
            kind,
            status: AssetStatus::Available,
            created_at: Utc::now(),
        })
    }

    /// Archives the asset, retiring it from use.
    ///
    /// Idempotent: archiving an archived asset is a no-op. The row and
    /// its bytes are never deleted.
    pub fn archive(&mut self) -> Result<(), DomainError> {
        self.status = AssetStatus::Archived;
        Ok(())
    }

    /// Whether the asset is retrievable for use.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.status == AssetStatus::Available
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_asset() -> Asset {
        Asset::new(
            ClientProfileId::new(),
            ActorId::new(),
            "photo.png".to_string(),
            "image/png".to_string(),
            2048,
            [9u8; 32],
        )
        .expect("valid asset")
    }

    #[test]
    fn mime_classification_table() {
        use AssetKind::{Audio, Document, Image, Other, Video};
        let cases = [
            ("image/png", Image),
            ("IMAGE/JPEG", Image),
            ("application/pdf", Document),
            ("text/csv", Document),
            (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                Document,
            ),
            ("audio/mpeg", Audio),
            ("video/mp4", Video),
            ("application/octet-stream", Other),
            ("", Other),
        ];
        for (mime, expected) in cases {
            assert_eq!(AssetKind::from_mime(mime), expected, "mime {mime}");
        }
    }

    #[test]
    fn new_asset_is_available() {
        let asset = test_asset();
        assert_eq!(asset.status, AssetStatus::Available);
        assert!(asset.is_available());
        assert_eq!(asset.kind, AssetKind::Image);
    }

    #[test]
    fn new_rejects_invalid_input() {
        let profile = ClientProfileId::new();
        let actor = ActorId::new();
        // Blank file name.
        assert!(Asset::new(
            profile,
            actor,
            "  ".to_string(),
            "image/png".to_string(),
            10,
            [1u8; 32]
        )
        .is_err());
        // Blank MIME type.
        assert!(Asset::new(
            profile,
            actor,
            "a.png".to_string(),
            "  ".to_string(),
            10,
            [1u8; 32]
        )
        .is_err());
        // Zero size.
        assert!(Asset::new(
            profile,
            actor,
            "a.png".to_string(),
            "image/png".to_string(),
            0,
            [1u8; 32]
        )
        .is_err());
    }

    #[test]
    fn archive_is_idempotent_and_terminal() {
        let mut asset = test_asset();
        asset.archive().expect("archive");
        assert_eq!(asset.status, AssetStatus::Archived);
        assert!(!asset.is_available());
        // Idempotent.
        asset.archive().expect("archive is idempotent");
        assert_eq!(asset.status, AssetStatus::Archived);
    }
}
