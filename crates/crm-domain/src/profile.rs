// SPDX-License-Identifier: GPL-3.0-only
//! Client profile aggregate: the agency-managed workspace lifecycle.
//!
//! A client profile is an account/workspace for work performed on behalf
//! of one business. Agency staff operate the application; clients never
//! log in — they receive outbound messages and reports through expiring,
//! scoped read-only links. The profile is the tenant boundary for all
//! reads and writes (contacts, assets, contracts, campaigns).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{AgencyId, ClientProfileId};

/// Lifecycle status of a client profile.
///
/// `Suspended` pauses all work under the profile (reads stay visible to
/// staff for audit, writes are rejected by the service layer);
/// `Archived` is terminal — nothing reactivates an archived profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProfileStatus {
    /// Operating normally.
    Active,
    /// Paused by the agency; reactivatable.
    Suspended,
    /// Retired; terminal.
    Archived,
}

/// An agency-managed workspace for one client's business.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientProfile {
    /// Stable identifier of this profile.
    pub profile_id: ClientProfileId,
    /// The agency operating this profile.
    pub agency_id: AgencyId,
    /// Display name of the client's business.
    pub name: String,
    /// Current lifecycle status.
    pub status: ProfileStatus,
    /// When the profile was created.
    pub created_at: DateTime<Utc>,
    /// When the profile was last modified.
    pub updated_at: DateTime<Utc>,
}

impl ClientProfile {
    /// Creates a new active profile for `agency_id`.
    ///
    /// The name must be non-empty after trimming; leading/trailing
    /// whitespace is stripped.
    ///
    /// ```
    /// use crm_domain::ids::AgencyId;
    /// use crm_domain::profile::{ClientProfile, ProfileStatus};
    ///
    /// let profile = ClientProfile::new("Acme Corp".to_string(), AgencyId::new())
    ///     .expect("valid profile");
    /// assert_eq!(profile.status, ProfileStatus::Active);
    /// assert_eq!(profile.name, "Acme Corp");
    ///
    /// assert!(ClientProfile::new("   ".to_string(), AgencyId::new()).is_err());
    /// ```
    pub fn new(name: String, agency_id: AgencyId) -> Result<Self, DomainError> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "client profile name must not be empty".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Self {
            profile_id: ClientProfileId::new(),
            agency_id,
            name,
            status: ProfileStatus::Active,
            created_at: now,
            updated_at: now,
        })
    }

    /// Suspends an active profile, pausing work under it.
    ///
    /// Idempotent: suspending a suspended profile is a no-op. Archiving
    /// is terminal — an archived profile cannot be suspended.
    pub fn suspend(&mut self) -> Result<(), DomainError> {
        match self.status {
            ProfileStatus::Active => {
                self.status = ProfileStatus::Suspended;
                self.updated_at = Utc::now();
                Ok(())
            }
            ProfileStatus::Suspended => Ok(()),
            ProfileStatus::Archived => Err(DomainError::Validation(
                "cannot suspend an archived client profile".to_string(),
            )),
        }
    }

    /// Reactivates a suspended profile.
    ///
    /// Idempotent: reactivating an active profile is a no-op. An archived
    /// profile can never be reactivated.
    pub fn reactivate(&mut self) -> Result<(), DomainError> {
        match self.status {
            ProfileStatus::Suspended => {
                self.status = ProfileStatus::Active;
                self.updated_at = Utc::now();
                Ok(())
            }
            ProfileStatus::Active => Ok(()),
            ProfileStatus::Archived => Err(DomainError::Validation(
                "cannot reactivate an archived client profile".to_string(),
            )),
        }
    }

    /// Archives the profile, retiring it permanently.
    ///
    /// Allowed from `Active` or `Suspended`; idempotent on `Archived`.
    /// The row is never deleted — history stays queryable for audit.
    pub fn archive(&mut self) -> Result<(), DomainError> {
        match self.status {
            ProfileStatus::Archived => Ok(()),
            ProfileStatus::Active | ProfileStatus::Suspended => {
                self.status = ProfileStatus::Archived;
                self.updated_at = Utc::now();
                Ok(())
            }
        }
    }

    /// Whether the profile is operating normally.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == ProfileStatus::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_profile() -> ClientProfile {
        ClientProfile::new("Acme Corp".to_string(), AgencyId::new()).expect("valid profile")
    }

    #[test]
    fn new_profile_is_active_with_timestamps() {
        let profile = test_profile();
        assert_eq!(profile.status, ProfileStatus::Active);
        assert!(profile.is_active());
        assert!(profile.updated_at >= profile.created_at);
    }

    #[test]
    fn new_rejects_blank_name() {
        for name in ["", "   ", "\t\n "] {
            let err = ClientProfile::new(name.to_string(), AgencyId::new())
                .expect_err("blank name must fail");
            assert_eq!(
                err.to_string(),
                "validation failed: client profile name must not be empty"
            );
        }
    }

    #[test]
    fn new_trims_name() {
        let profile = ClientProfile::new("  Acme Corp  ".to_string(), AgencyId::new())
            .expect("valid profile");
        assert_eq!(profile.name, "Acme Corp");
    }

    #[test]
    fn suspend_reactivate_cycle() {
        let mut profile = test_profile();
        profile.suspend().expect("suspend active");
        assert_eq!(profile.status, ProfileStatus::Suspended);
        assert!(!profile.is_active());

        // Idempotent.
        profile.suspend().expect("suspend is idempotent");

        profile.reactivate().expect("reactivate suspended");
        assert_eq!(profile.status, ProfileStatus::Active);

        // Idempotent.
        profile.reactivate().expect("reactivate is idempotent");
    }

    #[test]
    fn archive_is_terminal() {
        let mut profile = test_profile();
        profile.archive().expect("archive active");
        assert_eq!(profile.status, ProfileStatus::Archived);

        // Idempotent.
        profile.archive().expect("archive is idempotent");

        assert!(profile.suspend().is_err(), "suspend after archive fails");
        assert!(
            profile.reactivate().is_err(),
            "reactivate after archive fails"
        );
        // Failed transitions must not mutate.
        assert_eq!(profile.status, ProfileStatus::Archived);
    }

    #[test]
    fn archive_from_suspended() {
        let mut profile = test_profile();
        profile.suspend().expect("suspend");
        profile.archive().expect("archive suspended");
        assert_eq!(profile.status, ProfileStatus::Archived);
    }
}
