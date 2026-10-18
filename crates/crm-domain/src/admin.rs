// SPDX-License-Identifier: GPL-3.0-only
//! Admin and operations domain (task 16-ADMIN-01): staff administration,
//! agency settings, and system operations.
//!
//! This module defines the vocabulary for the administration console:
//! inviting and disabling staff, assigning roles, versioned agency
//! settings, maintenance mode, backups, upgrades, data exports,
//! installation state, and a pure health-summary aggregation.
//!
//! ## Design notes
//!
//! * **Builds on, never duplicates.** Staff identity, sessions, grants
//!   and delegation live in [`crate::security`]; health vocabulary lives
//!   in [`crate::observability`]. This module adds the *administrative*
//!   layer on top: invitations, role assignments, settings versions,
//!   and operational records.
//! * **Validated constructors.** Every aggregate validates its
//!   invariants at construction; fields are private so struct literals
//!   cannot bypass them. Types that cross a serialization boundary
//!   implement `Deserialize` manually through the validating
//!   constructor (never a derived bypass).
//! * **Forward-only lifecycles.** Invitations, backups, exports,
//!   upgrades and installation move forward only; illegal transitions
//!   return [`DomainError::Validation`](crate::error::DomainError::Validation).
//! * **Pinned audit names.** Every admin mutation records an audit
//!   event under one of the `ADMIN_*` action names below; the strings
//!   are part of the audit contract and must never be renamed without
//!   a migration note.
//! * **No I/O.** Like the rest of `crm-domain`, this module is pure:
//!   repositories and the [`AdminService`](crate::services::AdminService)
//!   port live in `repositories.rs` / `services.rs`; SQLx
//!   implementations land in 16-ADMIN-02.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::email::is_valid_email;
use crate::error::DomainError;
use crate::ids::{
    AgencyId, BackupId, ClientProfileId, ExportId, InvitationId, RoleAssignmentId, RoleId, StaffId,
    UpgradeId,
};
use crate::observability::{ComponentHealth, HealthReport};
use crate::security::Scope;

// ---------------------------------------------------------------------------
// Pinned audit action names
// ---------------------------------------------------------------------------

/// Audit action name recorded when a staff invitation is created.
pub const ADMIN_STAFF_INVITED: &str = "admin.staff_invited";
/// Audit action name recorded when a staff invitation is accepted.
pub const ADMIN_INVITATION_ACCEPTED: &str = "admin.invitation_accepted";
/// Audit action name recorded when a staff invitation is revoked.
pub const ADMIN_INVITATION_REVOKED: &str = "admin.invitation_revoked";
/// Audit action name recorded when a staff invitation expires.
pub const ADMIN_INVITATION_EXPIRED: &str = "admin.invitation_expired";
/// Audit action name recorded when a staff account is disabled
/// (suspended).
pub const ADMIN_STAFF_DISABLED: &str = "admin.staff_disabled";
/// Audit action name recorded when a staff account is reactivated.
pub const ADMIN_STAFF_REACTIVATED: &str = "admin.staff_reactivated";
/// Audit action name recorded when a role is assigned to a staff member.
pub const ADMIN_ROLE_ASSIGNED: &str = "admin.role_assigned";
/// Audit action name recorded when a role assignment is revoked.
pub const ADMIN_ROLE_REVOKED: &str = "admin.role_revoked";
/// Audit action name recorded when agency settings are updated (a new
/// version is stored).
pub const ADMIN_SETTINGS_UPDATED: &str = "admin.settings_updated";
/// Audit action name recorded when maintenance mode is enabled.
pub const ADMIN_MAINTENANCE_ENABLED: &str = "admin.maintenance_enabled";
/// Audit action name recorded when maintenance mode is disabled.
pub const ADMIN_MAINTENANCE_DISABLED: &str = "admin.maintenance_disabled";
/// Audit action name recorded when a backup starts.
pub const ADMIN_BACKUP_STARTED: &str = "admin.backup_started";
/// Audit action name recorded when a backup completes.
pub const ADMIN_BACKUP_COMPLETED: &str = "admin.backup_completed";
/// Audit action name recorded when a backup fails.
pub const ADMIN_BACKUP_FAILED: &str = "admin.backup_failed";
/// Audit action name recorded when a data export is requested.
pub const ADMIN_EXPORT_REQUESTED: &str = "admin.export_requested";
/// Audit action name recorded when a data export is ready for download.
pub const ADMIN_EXPORT_READY: &str = "admin.export_ready";
/// Audit action name recorded when a data export fails.
pub const ADMIN_EXPORT_FAILED: &str = "admin.export_failed";
/// Audit action name recorded when a system upgrade starts.
pub const ADMIN_UPGRADE_STARTED: &str = "admin.upgrade_started";
/// Audit action name recorded when a system upgrade completes.
pub const ADMIN_UPGRADE_COMPLETED: &str = "admin.upgrade_completed";
/// Audit action name recorded when a system upgrade fails.
pub const ADMIN_UPGRADE_FAILED: &str = "admin.upgrade_failed";
/// Audit action name recorded when a failed upgrade is rolled back.
pub const ADMIN_UPGRADE_ROLLED_BACK: &str = "admin.upgrade_rolled_back";
/// Audit action name recorded when the installation completes.
pub const ADMIN_INSTALLATION_COMPLETED: &str = "admin.installation_completed";

// ---------------------------------------------------------------------------
// Staff administration: invitations
// ---------------------------------------------------------------------------

/// Lifecycle of a [`StaffInvitation`]: forward-only.
///
/// An invitation is usable only while [`InvitationStatus::Pending`].
/// Acceptance, expiry and revocation are terminal; there is no path
/// back to pending. Token verification is a service-layer concern
/// (like password verification in 03-SEC-02): the domain records the
/// invitation and its transitions, never the secret token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvitationStatus {
    /// Created and awaiting acceptance; usable before `expires_at`.
    Pending,
    /// Accepted by the invited staff member.
    Accepted,
    /// Passed `expires_at` without acceptance.
    Expired,
    /// Revoked by an administrator before acceptance.
    Revoked,
}

/// An invitation for a new staff member to join the agency.
///
/// The invitation names the email address and the roles the new staff
/// member receives on acceptance; the service layer turns the role ids
/// into [`Grant`](crate::security::Grant)s via
/// [`Role::grants_for`](crate::security::Role::grants_for) at accept
/// time, so the invitation always resolves against the *current* role
/// definitions. Construct via [`StaffInvitation::new`] — the email
/// shape, non-empty roles and future expiry are validated there and
/// at deserialization (manual `Deserialize` through
/// [`StaffInvitation::rehydrate`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaffInvitation {
    invitation_id: InvitationId,
    email: String,
    role_ids: Vec<RoleId>,
    invited_by: StaffId,
    status: InvitationStatus,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

impl StaffInvitation {
    /// Invites a staff member: validates the email with
    /// [`is_valid_email`], requires at least one role, and requires
    /// `expires_at` to be in the future relative to `now`.
    ///
    /// ```
    /// use chrono::{Duration, Utc};
    /// use crm_domain::admin::StaffInvitation;
    /// use crm_domain::ids::{RoleId, StaffId};
    ///
    /// let now = Utc::now();
    /// let invitation = StaffInvitation::new(
    ///     "ops@example.com".to_string(),
    ///     vec![RoleId::new()],
    ///     StaffId::new(),
    ///     now + Duration::days(7),
    ///     now,
    /// );
    /// assert!(invitation.is_ok());
    /// // No roles, bad email, or a past expiry are all rejected.
    /// assert!(StaffInvitation::new(
    ///     "not-an-email".to_string(),
    ///     vec![RoleId::new()],
    ///     StaffId::new(),
    ///     now + Duration::days(7),
    ///     now,
    /// )
    /// .is_err());
    /// ```
    pub fn new(
        email: String,
        role_ids: Vec<RoleId>,
        invited_by: StaffId,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        if !is_valid_email(&email) {
            return Err(DomainError::Validation(
                "staff invitation requires a valid email address".to_string(),
            ));
        }
        if role_ids.is_empty() {
            return Err(DomainError::Validation(
                "staff invitation requires at least one role".to_string(),
            ));
        }
        if expires_at <= now {
            return Err(DomainError::Validation(
                "staff invitation expiry must be in the future".to_string(),
            ));
        }
        Ok(Self {
            invitation_id: InvitationId::new(),
            email,
            role_ids,
            invited_by,
            status: InvitationStatus::Pending,
            expires_at,
            created_at: now,
        })
    }

    /// Rehydrates a stored invitation without re-running the
    /// creation-time validations (the row was validated when written).
    /// Corrupt rows surface as validation errors at deserialization,
    /// never as silently accepted invalid state.
    #[must_use]
    pub fn rehydrate(
        invitation_id: InvitationId,
        email: String,
        role_ids: Vec<RoleId>,
        invited_by: StaffId,
        status: InvitationStatus,
        expires_at: DateTime<Utc>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            invitation_id,
            email,
            role_ids,
            invited_by,
            status,
            expires_at,
            created_at,
        }
    }

    /// Accepts the invitation: `Pending` → `Accepted`.
    ///
    /// Rejects when the invitation is not pending, or when `now` is at
    /// or past `expires_at` — the caller should [`StaffInvitation::expire`]
    /// it instead so the terminal state is recorded accurately.
    pub fn accept(&mut self, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != InvitationStatus::Pending {
            return Err(DomainError::Validation(format!(
                "invitation cannot be accepted from status {:?}",
                self.status
            )));
        }
        if now >= self.expires_at {
            return Err(DomainError::Validation(
                "invitation has expired; expire it instead of accepting".to_string(),
            ));
        }
        self.status = InvitationStatus::Accepted;
        Ok(())
    }

    /// Marks the invitation expired: `Pending` → `Expired`.
    pub fn expire(&mut self) -> Result<(), DomainError> {
        if self.status != InvitationStatus::Pending {
            return Err(DomainError::Validation(format!(
                "invitation cannot expire from status {:?}",
                self.status
            )));
        }
        self.status = InvitationStatus::Expired;
        Ok(())
    }

    /// Revokes the invitation: `Pending` → `Revoked`.
    pub fn revoke(&mut self) -> Result<(), DomainError> {
        if self.status != InvitationStatus::Pending {
            return Err(DomainError::Validation(format!(
                "invitation cannot be revoked from status {:?}",
                self.status
            )));
        }
        self.status = InvitationStatus::Revoked;
        Ok(())
    }

    /// Whether the invitation is past its expiry at `now`.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Stable identifier of this invitation.
    #[must_use]
    pub fn invitation_id(&self) -> InvitationId {
        self.invitation_id
    }

    /// The invited email address.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    /// Role ids granted on acceptance.
    #[must_use]
    pub fn role_ids(&self) -> &[RoleId] {
        &self.role_ids
    }

    /// The staff member who sent the invitation.
    #[must_use]
    pub fn invited_by(&self) -> StaffId {
        self.invited_by
    }

    /// Current lifecycle status.
    #[must_use]
    pub fn status(&self) -> InvitationStatus {
        self.status
    }

    /// When the invitation stops being acceptable.
    #[must_use]
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// When the invitation was created.
    #[must_use]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl<'de> Deserialize<'de> for StaffInvitation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawInvitation {
            invitation_id: InvitationId,
            email: String,
            role_ids: Vec<RoleId>,
            invited_by: StaffId,
            status: InvitationStatus,
            expires_at: DateTime<Utc>,
            created_at: DateTime<Utc>,
        }
        let raw = RawInvitation::deserialize(deserializer)?;
        if !is_valid_email(&raw.email) {
            return Err(serde::de::Error::custom(
                "staff invitation requires a valid email address",
            ));
        }
        if raw.role_ids.is_empty() {
            return Err(serde::de::Error::custom(
                "staff invitation requires at least one role",
            ));
        }
        Ok(StaffInvitation::rehydrate(
            raw.invitation_id,
            raw.email,
            raw.role_ids,
            raw.invited_by,
            raw.status,
            raw.expires_at,
            raw.created_at,
        ))
    }
}

// ---------------------------------------------------------------------------
// Staff administration: role assignments
// ---------------------------------------------------------------------------

/// Lifecycle of a [`RoleAssignment`]: a role is either held or it was
/// revoked. Revocation is terminal — re-granting a role creates a new
/// assignment, so the audit trail shows the full grant history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssignmentStatus {
    /// The staff member currently holds the role at the scope.
    Active,
    /// The assignment was revoked; the grants it materialized no
    /// longer authorize.
    Revoked,
}

/// Assigns a named [`Role`](crate::security::Role) to a staff member
/// within a [`Scope`].
///
/// The assignment is the durable record; the service layer
/// materializes the actual [`Grant`](crate::security::Grant)s via
/// [`Role::grants_for`](crate::security::Role::grants_for) when
/// authorizing. Construct via [`RoleAssignment::new`]; deserializes
/// through [`RoleAssignment::rehydrate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleAssignment {
    assignment_id: RoleAssignmentId,
    staff_id: StaffId,
    role_id: RoleId,
    scope: Scope,
    assigned_by: StaffId,
    status: AssignmentStatus,
    assigned_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl RoleAssignment {
    /// Assigns `role_id` to `staff_id` within `scope`.
    ///
    /// A staff member cannot assign a role to themselves — self-grant
    /// would let any staff member escalate their own privileges, so
    /// the domain rejects it and the service layer additionally
    /// requires the assigner to hold the role's permissions.
    ///
    /// ```
    /// use crm_domain::admin::RoleAssignment;
    /// use crm_domain::ids::{AgencyId, RoleId, StaffId};
    /// use crm_domain::security::Scope;
    /// use chrono::Utc;
    ///
    /// let assigner = StaffId::new();
    /// let scope = Scope::Agency { agency_id: AgencyId::new() };
    /// let new_assignment =
    ///     RoleAssignment::new(StaffId::new(), RoleId::new(), scope, assigner, Utc::now());
    /// assert!(new_assignment.is_ok());
    /// // Self-assignment is rejected.
    /// assert!(RoleAssignment::new(assigner, RoleId::new(), scope, assigner, Utc::now()).is_err());
    /// ```
    pub fn new(
        staff_id: StaffId,
        role_id: RoleId,
        scope: Scope,
        assigned_by: StaffId,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        if staff_id == assigned_by {
            return Err(DomainError::Validation(
                "a staff member cannot assign a role to themselves".to_string(),
            ));
        }
        Ok(Self {
            assignment_id: RoleAssignmentId::new(),
            staff_id,
            role_id,
            scope,
            assigned_by,
            status: AssignmentStatus::Active,
            assigned_at: now,
            revoked_at: None,
        })
    }

    /// Rehydrates a stored assignment.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        assignment_id: RoleAssignmentId,
        staff_id: StaffId,
        role_id: RoleId,
        scope: Scope,
        assigned_by: StaffId,
        status: AssignmentStatus,
        assigned_at: DateTime<Utc>,
        revoked_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            assignment_id,
            staff_id,
            role_id,
            scope,
            assigned_by,
            status,
            assigned_at,
            revoked_at,
        }
    }

    /// Revokes the assignment: `Active` → `Revoked`, stamping
    /// `revoked_at`. Idempotent — revoking a revoked assignment is a
    /// no-op so retried commands stay safe.
    pub fn revoke(&mut self, now: DateTime<Utc>) {
        if self.status == AssignmentStatus::Active {
            self.status = AssignmentStatus::Revoked;
            self.revoked_at = Some(now);
        }
    }

    /// Whether the assignment currently confers its role's grants.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == AssignmentStatus::Active
    }

    /// Stable identifier of this assignment.
    #[must_use]
    pub fn assignment_id(&self) -> RoleAssignmentId {
        self.assignment_id
    }

    /// The staff member holding the role.
    #[must_use]
    pub fn staff_id(&self) -> StaffId {
        self.staff_id
    }

    /// The assigned role.
    #[must_use]
    pub fn role_id(&self) -> RoleId {
        self.role_id
    }

    /// The scope the role is held within.
    #[must_use]
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The staff member who made the assignment.
    #[must_use]
    pub fn assigned_by(&self) -> StaffId {
        self.assigned_by
    }

    /// Current lifecycle status.
    #[must_use]
    pub fn status(&self) -> AssignmentStatus {
        self.status
    }

    /// When the role was assigned.
    #[must_use]
    pub fn assigned_at(&self) -> DateTime<Utc> {
        self.assigned_at
    }

    /// When the assignment was revoked, if it was.
    #[must_use]
    pub fn revoked_at(&self) -> Option<DateTime<Utc>> {
        self.revoked_at
    }
}

impl<'de> Deserialize<'de> for RoleAssignment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawAssignment {
            assignment_id: RoleAssignmentId,
            staff_id: StaffId,
            role_id: RoleId,
            scope: Scope,
            assigned_by: StaffId,
            status: AssignmentStatus,
            assigned_at: DateTime<Utc>,
            revoked_at: Option<DateTime<Utc>>,
        }
        let raw = RawAssignment::deserialize(deserializer)?;
        if raw.staff_id == raw.assigned_by {
            return Err(serde::de::Error::custom(
                "a staff member cannot assign a role to themselves",
            ));
        }
        Ok(RoleAssignment::rehydrate(
            raw.assignment_id,
            raw.staff_id,
            raw.role_id,
            raw.scope,
            raw.assigned_by,
            raw.status,
            raw.assigned_at,
            raw.revoked_at,
        ))
    }
}

// ---------------------------------------------------------------------------
// Agency settings
// ---------------------------------------------------------------------------

/// Bounds for the agency's default session TTL setting.
pub mod settings_bounds {
    /// Minimum session TTL an agency may configure: 15 minutes.
    pub const MIN_SESSION_TTL_SECS: u64 = 900;
    /// Maximum session TTL an agency may configure: 7 days (matches
    /// [`crate::security::MAX_SESSION_TTL_SECS`]; an agency can only
    /// tighten the platform default, never loosen it).
    pub const MAX_SESSION_TTL_SECS: u64 = 7 * 24 * 3600;
}

/// Maximum length of the agency display name.
pub const MAX_AGENCY_NAME_LEN: usize = 120;

/// Versioned agency-level configuration.
///
/// Settings are snapshots: [`AgencySettings::update`] never mutates in
/// place — it returns a new snapshot with `version + 1`, so the settings
/// history is append-only and every change is attributable via
/// `updated_by` / `updated_at`. The repository keeps every version;
/// `get_current` returns the highest one. Deserializes through
/// [`AgencySettings::rehydrate`] with the same validations as
/// [`AgencySettings::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgencySettings {
    agency_id: AgencyId,
    version: u64,
    display_name: String,
    support_email: Option<String>,
    session_ttl_secs: u64,
    updated_by: StaffId,
    updated_at: DateTime<Utc>,
}

impl AgencySettings {
    /// Creates the first settings version (`version == 1`).
    ///
    /// Validates: `display_name` is non-blank and at most
    /// [`MAX_AGENCY_NAME_LEN`] characters; `support_email`, when
    /// present, passes [`is_valid_email`]; `session_ttl_secs` lies
    /// within [`settings_bounds::MIN_SESSION_TTL_SECS`] ..
    /// [`settings_bounds::MAX_SESSION_TTL_SECS`].
    ///
    /// ```
    /// use crm_domain::admin::AgencySettings;
    /// use crm_domain::ids::{AgencyId, StaffId};
    /// use chrono::Utc;
    ///
    /// let settings = AgencySettings::new(
    ///     AgencyId::new(),
    ///     "Acme Agency".to_string(),
    ///     Some("support@acme.example".to_string()),
    ///     43_200,
    ///     StaffId::new(),
    ///     Utc::now(),
    /// );
    /// assert!(settings.is_ok());
    /// assert_eq!(settings.unwrap().version(), 1);
    /// ```
    pub fn new(
        agency_id: AgencyId,
        display_name: String,
        support_email: Option<String>,
        session_ttl_secs: u64,
        updated_by: StaffId,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        Self::validated(
            agency_id,
            1,
            display_name,
            support_email,
            session_ttl_secs,
            updated_by,
            now,
        )
    }

    /// Returns a new settings snapshot with `version + 1`.
    ///
    /// `None` fields keep the current value, so callers update only
    /// what changes; every supplied value is validated exactly as in
    /// [`AgencySettings::new`].
    pub fn update(
        &self,
        display_name: Option<String>,
        support_email: Option<Option<String>>,
        session_ttl_secs: Option<u64>,
        updated_by: StaffId,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        Self::validated(
            self.agency_id,
            self.version + 1,
            display_name.unwrap_or_else(|| self.display_name.clone()),
            support_email.unwrap_or_else(|| self.support_email.clone()),
            session_ttl_secs.unwrap_or(self.session_ttl_secs),
            updated_by,
            now,
        )
    }

    /// Rehydrates a stored settings version.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        agency_id: AgencyId,
        version: u64,
        display_name: String,
        support_email: Option<String>,
        session_ttl_secs: u64,
        updated_by: StaffId,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            agency_id,
            version,
            display_name,
            support_email,
            session_ttl_secs,
            updated_by,
            updated_at,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validated(
        agency_id: AgencyId,
        version: u64,
        display_name: String,
        support_email: Option<String>,
        session_ttl_secs: u64,
        updated_by: StaffId,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        if display_name.trim().is_empty() {
            return Err(DomainError::Validation(
                "agency display name must not be blank".to_string(),
            ));
        }
        if display_name.chars().count() > MAX_AGENCY_NAME_LEN {
            return Err(DomainError::Validation(format!(
                "agency display name must be at most {MAX_AGENCY_NAME_LEN} characters"
            )));
        }
        if let Some(email) = &support_email {
            if !is_valid_email(email) {
                return Err(DomainError::Validation(
                    "agency support email must be a valid email address".to_string(),
                ));
            }
        }
        if !(settings_bounds::MIN_SESSION_TTL_SECS..=settings_bounds::MAX_SESSION_TTL_SECS)
            .contains(&session_ttl_secs)
        {
            return Err(DomainError::Validation(format!(
                "session TTL must be between {} and {} seconds",
                settings_bounds::MIN_SESSION_TTL_SECS,
                settings_bounds::MAX_SESSION_TTL_SECS
            )));
        }
        Ok(Self {
            agency_id,
            version,
            display_name,
            support_email,
            session_ttl_secs,
            updated_by,
            updated_at: now,
        })
    }

    /// The agency these settings belong to.
    #[must_use]
    pub fn agency_id(&self) -> AgencyId {
        self.agency_id
    }

    /// Monotonic version; each update increments by one.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Human-readable agency name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Support contact address, when configured.
    #[must_use]
    pub fn support_email(&self) -> Option<&str> {
        self.support_email.as_deref()
    }

    /// Default session TTL for the agency, in seconds.
    #[must_use]
    pub fn session_ttl_secs(&self) -> u64 {
        self.session_ttl_secs
    }

    /// The staff member who wrote this version.
    #[must_use]
    pub fn updated_by(&self) -> StaffId {
        self.updated_by
    }

    /// When this version was written.
    #[must_use]
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl<'de> Deserialize<'de> for AgencySettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawSettings {
            agency_id: AgencyId,
            version: u64,
            display_name: String,
            support_email: Option<String>,
            session_ttl_secs: u64,
            updated_by: StaffId,
            updated_at: DateTime<Utc>,
        }
        let raw = RawSettings::deserialize(deserializer)?;
        // Re-run the constructor validations so a corrupt or tampered
        // row cannot produce an invalid settings snapshot.
        AgencySettings::validated(
            raw.agency_id,
            raw.version,
            raw.display_name,
            raw.support_email,
            raw.session_ttl_secs,
            raw.updated_by,
            raw.updated_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Maintenance mode
// ---------------------------------------------------------------------------

/// Maximum length of the maintenance reason.
pub const MAX_MAINTENANCE_REASON_LEN: usize = 500;

/// Maintenance mode: while enabled, the service layer rejects
/// non-admin mutations with a 503-style [`DomainError`], so operators
/// can run backups, upgrades, or incident response without racing
/// user traffic.
///
/// The mode is a single agency-wide record (not a lifecycle): it is
/// either enabled with a reason or disabled. Every toggle is audited
/// ([`ADMIN_MAINTENANCE_ENABLED`]/[`ADMIN_MAINTENANCE_DISABLED`]).
/// Deserializes through [`MaintenanceMode::rehydrate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaintenanceMode {
    enabled: bool,
    reason: Option<String>,
    set_by: StaffId,
    set_at: DateTime<Utc>,
}

impl MaintenanceMode {
    /// Starts disabled.
    #[must_use]
    pub fn disabled(set_by: StaffId, now: DateTime<Utc>) -> Self {
        Self {
            enabled: false,
            reason: None,
            set_by,
            set_at: now,
        }
    }

    /// Enables maintenance mode with a non-blank reason (what is
    /// happening and, when known, when service resumes).
    ///
    /// ```
    /// use crm_domain::admin::MaintenanceMode;
    /// use crm_domain::ids::StaffId;
    /// use chrono::Utc;
    ///
    /// let mut mode = MaintenanceMode::disabled(StaffId::new(), Utc::now());
    /// assert!(!mode.is_enabled());
    /// mode.enable(
    ///     "Database upgrade, back by 03:00 UTC".to_string(),
    ///     StaffId::new(),
    ///     Utc::now(),
    /// )
    /// .unwrap();
    /// assert!(mode.is_enabled());
    /// ```
    pub fn enable(
        &mut self,
        reason: String,
        set_by: StaffId,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if reason.trim().is_empty() {
            return Err(DomainError::Validation(
                "maintenance reason must not be blank".to_string(),
            ));
        }
        if reason.chars().count() > MAX_MAINTENANCE_REASON_LEN {
            return Err(DomainError::Validation(format!(
                "maintenance reason must be at most {MAX_MAINTENANCE_REASON_LEN} characters"
            )));
        }
        self.enabled = true;
        self.reason = Some(reason);
        self.set_by = set_by;
        self.set_at = now;
        Ok(())
    }

    /// Disables maintenance mode, clearing the reason.
    pub fn disable(&mut self, set_by: StaffId, now: DateTime<Utc>) {
        self.enabled = false;
        self.reason = None;
        self.set_by = set_by;
        self.set_at = now;
    }

    /// Rehydrates the stored maintenance record.
    #[must_use]
    pub fn rehydrate(
        enabled: bool,
        reason: Option<String>,
        set_by: StaffId,
        set_at: DateTime<Utc>,
    ) -> Self {
        Self {
            enabled,
            reason,
            set_by,
            set_at,
        }
    }

    /// Whether maintenance mode is currently enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The reason maintenance was enabled, when it is.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// The staff member who last toggled the mode.
    #[must_use]
    pub fn set_by(&self) -> StaffId {
        self.set_by
    }

    /// When the mode was last toggled.
    #[must_use]
    pub fn set_at(&self) -> DateTime<Utc> {
        self.set_at
    }
}

impl<'de> Deserialize<'de> for MaintenanceMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawMode {
            enabled: bool,
            reason: Option<String>,
            set_by: StaffId,
            set_at: DateTime<Utc>,
        }
        let raw = RawMode::deserialize(deserializer)?;
        if raw.enabled {
            match &raw.reason {
                Some(reason) if !reason.trim().is_empty() => {}
                _ => {
                    return Err(serde::de::Error::custom(
                        "enabled maintenance mode requires a non-blank reason",
                    ))
                }
            }
        }
        Ok(MaintenanceMode::rehydrate(
            raw.enabled,
            raw.reason,
            raw.set_by,
            raw.set_at,
        ))
    }
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

/// Lifecycle of a [`BackupRecord`]: forward-only.
///
/// A backup is scheduled, runs, and then either completes or fails.
/// Terminal states are `Completed` and `Failed`; there is no retry on
/// the same record — a retry schedules a new backup, so every attempt
/// is individually auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackupStatus {
    /// Scheduled but not yet started.
    Scheduled,
    /// Currently running.
    InProgress,
    /// Finished successfully; `size_bytes` is set.
    Completed,
    /// Failed; `failure_reason` is set.
    Failed,
}

/// A database backup attempt.
///
/// The record tracks the attempt, not the backup bytes themselves —
/// those live in external storage and are referenced opaquely. The
/// service layer drives the lifecycle; 16-ADMIN-02 persists it.
/// Deserializes through [`BackupRecord::rehydrate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupRecord {
    backup_id: BackupId,
    status: BackupStatus,
    initiated_by: StaffId,
    scheduled_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    size_bytes: Option<u64>,
    failure_reason: Option<String>,
}

impl BackupRecord {
    /// Schedules a backup: starts in [`BackupStatus::Scheduled`].
    ///
    /// ```
    /// use crm_domain::admin::{BackupRecord, BackupStatus};
    /// use crm_domain::ids::StaffId;
    /// use chrono::Utc;
    ///
    /// let mut backup = BackupRecord::schedule(StaffId::new(), Utc::now());
    /// assert_eq!(backup.status(), BackupStatus::Scheduled);
    /// backup.start(Utc::now()).unwrap();
    /// backup.complete(1_048_576, Utc::now()).unwrap();
    /// assert_eq!(backup.status(), BackupStatus::Completed);
    /// assert_eq!(backup.size_bytes(), Some(1_048_576));
    /// ```
    #[must_use]
    pub fn schedule(initiated_by: StaffId, now: DateTime<Utc>) -> Self {
        Self {
            backup_id: BackupId::new(),
            status: BackupStatus::Scheduled,
            initiated_by,
            scheduled_at: now,
            started_at: None,
            completed_at: None,
            size_bytes: None,
            failure_reason: None,
        }
    }

    /// Starts the backup: `Scheduled` → `InProgress`.
    pub fn start(&mut self, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != BackupStatus::Scheduled {
            return Err(DomainError::Validation(format!(
                "backup cannot start from status {:?}",
                self.status
            )));
        }
        self.status = BackupStatus::InProgress;
        self.started_at = Some(now);
        Ok(())
    }

    /// Completes the backup: `InProgress` → `Completed`, recording
    /// the backup size.
    pub fn complete(&mut self, size_bytes: u64, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != BackupStatus::InProgress {
            return Err(DomainError::Validation(format!(
                "backup cannot complete from status {:?}",
                self.status
            )));
        }
        self.status = BackupStatus::Completed;
        self.size_bytes = Some(size_bytes);
        self.completed_at = Some(now);
        Ok(())
    }

    /// Fails the backup: `InProgress` → `Failed`, recording the
    /// reason (secret-free: never include credentials or paths with
    /// embedded secrets).
    pub fn fail(&mut self, reason: String, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != BackupStatus::InProgress {
            return Err(DomainError::Validation(format!(
                "backup cannot fail from status {:?}",
                self.status
            )));
        }
        if reason.trim().is_empty() {
            return Err(DomainError::Validation(
                "backup failure reason must not be blank".to_string(),
            ));
        }
        self.status = BackupStatus::Failed;
        self.failure_reason = Some(reason);
        self.completed_at = Some(now);
        Ok(())
    }

    /// Rehydrates a stored backup record.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        backup_id: BackupId,
        status: BackupStatus,
        initiated_by: StaffId,
        scheduled_at: DateTime<Utc>,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
        size_bytes: Option<u64>,
        failure_reason: Option<String>,
    ) -> Self {
        Self {
            backup_id,
            status,
            initiated_by,
            scheduled_at,
            started_at,
            completed_at,
            size_bytes,
            failure_reason,
        }
    }

    /// Stable identifier of this backup.
    #[must_use]
    pub fn backup_id(&self) -> BackupId {
        self.backup_id
    }

    /// Current lifecycle status.
    #[must_use]
    pub fn status(&self) -> BackupStatus {
        self.status
    }

    /// The staff member who scheduled the backup.
    #[must_use]
    pub fn initiated_by(&self) -> StaffId {
        self.initiated_by
    }

    /// When the backup was scheduled.
    #[must_use]
    pub fn scheduled_at(&self) -> DateTime<Utc> {
        self.scheduled_at
    }

    /// Backup size in bytes, once completed.
    #[must_use]
    pub fn size_bytes(&self) -> Option<u64> {
        self.size_bytes
    }

    /// Failure reason, when failed.
    #[must_use]
    pub fn failure_reason(&self) -> Option<&str> {
        self.failure_reason.as_deref()
    }
}

impl<'de> Deserialize<'de> for BackupRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawBackup {
            backup_id: BackupId,
            status: BackupStatus,
            initiated_by: StaffId,
            scheduled_at: DateTime<Utc>,
            started_at: Option<DateTime<Utc>>,
            completed_at: Option<DateTime<Utc>>,
            size_bytes: Option<u64>,
            failure_reason: Option<String>,
        }
        let raw = RawBackup::deserialize(deserializer)?;
        Ok(BackupRecord::rehydrate(
            raw.backup_id,
            raw.status,
            raw.initiated_by,
            raw.scheduled_at,
            raw.started_at,
            raw.completed_at,
            raw.size_bytes,
            raw.failure_reason,
        ))
    }
}

// ---------------------------------------------------------------------------
// Data exports
// ---------------------------------------------------------------------------

/// Export file format: the closed set the service can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExportFormat {
    /// Comma-separated values.
    Csv,
    /// JSON document.
    Json,
}

/// Lifecycle of a [`DataExport`]: forward-only.
///
/// An export is requested, produced, and then either becomes ready
/// for download or fails. Ready exports expire after a retention
/// window; expired and failed exports are terminal. Retrying creates
/// a new export so every attempt is auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExportStatus {
    /// Requested but not yet being produced.
    Requested,
    /// Currently being produced.
    InProgress,
    /// Ready for download.
    Ready,
    /// Production failed.
    Failed,
    /// Was ready but passed the retention window.
    Expired,
}

/// A data-portability export: a bounded extract of CRM data in a
/// portable format.
///
/// Exports are scoped: `client_profile_id` is `Some` for a
/// per-profile extract (the data-portability case) and `None` for an
/// agency-wide extract. The export bytes live in external storage;
/// the domain tracks the request lifecycle only. Deserializes through
/// [`DataExport::rehydrate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataExport {
    export_id: ExportId,
    client_profile_id: Option<ClientProfileId>,
    format: ExportFormat,
    status: ExportStatus,
    requested_by: StaffId,
    requested_at: DateTime<Utc>,
    ready_at: Option<DateTime<Utc>>,
    failure_reason: Option<String>,
}

impl DataExport {
    /// Requests an export: starts in [`ExportStatus::Requested`].
    ///
    /// ```
    /// use crm_domain::admin::{DataExport, ExportFormat, ExportStatus};
    /// use crm_domain::ids::{ClientProfileId, StaffId};
    /// use chrono::Utc;
    ///
    /// let mut export = DataExport::request(
    ///     Some(ClientProfileId::new()),
    ///     ExportFormat::Csv,
    ///     StaffId::new(),
    ///     Utc::now(),
    /// );
    /// assert_eq!(export.status(), ExportStatus::Requested);
    /// export.start().unwrap();
    /// export.complete(Utc::now()).unwrap();
    /// assert_eq!(export.status(), ExportStatus::Ready);
    /// ```
    #[must_use]
    pub fn request(
        client_profile_id: Option<ClientProfileId>,
        format: ExportFormat,
        requested_by: StaffId,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            export_id: ExportId::new(),
            client_profile_id,
            format,
            status: ExportStatus::Requested,
            requested_by,
            requested_at: now,
            ready_at: None,
            failure_reason: None,
        }
    }

    /// Starts producing the export: `Requested` → `InProgress`.
    pub fn start(&mut self) -> Result<(), DomainError> {
        if self.status != ExportStatus::Requested {
            return Err(DomainError::Validation(format!(
                "export cannot start from status {:?}",
                self.status
            )));
        }
        self.status = ExportStatus::InProgress;
        Ok(())
    }

    /// Marks the export ready: `InProgress` → `Ready`.
    pub fn complete(&mut self, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != ExportStatus::InProgress {
            return Err(DomainError::Validation(format!(
                "export cannot complete from status {:?}",
                self.status
            )));
        }
        self.status = ExportStatus::Ready;
        self.ready_at = Some(now);
        Ok(())
    }

    /// Fails the export: `InProgress` → `Failed`.
    pub fn fail(&mut self, reason: String) -> Result<(), DomainError> {
        if self.status != ExportStatus::InProgress {
            return Err(DomainError::Validation(format!(
                "export cannot fail from status {:?}",
                self.status
            )));
        }
        if reason.trim().is_empty() {
            return Err(DomainError::Validation(
                "export failure reason must not be blank".to_string(),
            ));
        }
        self.status = ExportStatus::Failed;
        self.failure_reason = Some(reason);
        Ok(())
    }

    /// Expires a ready export: `Ready` → `Expired`.
    pub fn expire(&mut self) -> Result<(), DomainError> {
        if self.status != ExportStatus::Ready {
            return Err(DomainError::Validation(format!(
                "export cannot expire from status {:?}",
                self.status
            )));
        }
        self.status = ExportStatus::Expired;
        Ok(())
    }

    /// Rehydrates a stored export.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        export_id: ExportId,
        client_profile_id: Option<ClientProfileId>,
        format: ExportFormat,
        status: ExportStatus,
        requested_by: StaffId,
        requested_at: DateTime<Utc>,
        ready_at: Option<DateTime<Utc>>,
        failure_reason: Option<String>,
    ) -> Self {
        Self {
            export_id,
            client_profile_id,
            format,
            status,
            requested_by,
            requested_at,
            ready_at,
            failure_reason,
        }
    }

    /// Stable identifier of this export.
    #[must_use]
    pub fn export_id(&self) -> ExportId {
        self.export_id
    }

    /// The client profile the extract covers, or `None` for an
    /// agency-wide extract.
    #[must_use]
    pub fn client_profile_id(&self) -> Option<ClientProfileId> {
        self.client_profile_id
    }

    /// The export file format.
    #[must_use]
    pub fn format(&self) -> ExportFormat {
        self.format
    }

    /// Current lifecycle status.
    #[must_use]
    pub fn status(&self) -> ExportStatus {
        self.status
    }

    /// The staff member who requested the export.
    #[must_use]
    pub fn requested_by(&self) -> StaffId {
        self.requested_by
    }
}

impl<'de> Deserialize<'de> for DataExport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawExport {
            export_id: ExportId,
            client_profile_id: Option<ClientProfileId>,
            format: ExportFormat,
            status: ExportStatus,
            requested_by: StaffId,
            requested_at: DateTime<Utc>,
            ready_at: Option<DateTime<Utc>>,
            failure_reason: Option<String>,
        }
        let raw = RawExport::deserialize(deserializer)?;
        Ok(DataExport::rehydrate(
            raw.export_id,
            raw.client_profile_id,
            raw.format,
            raw.status,
            raw.requested_by,
            raw.requested_at,
            raw.ready_at,
            raw.failure_reason,
        ))
    }
}

// ---------------------------------------------------------------------------
// System upgrades
// ---------------------------------------------------------------------------

/// Lifecycle of a [`SystemUpgrade`]: forward-only.
///
/// An upgrade is scheduled, runs, and then either completes or fails.
/// A failed upgrade may be rolled back exactly once
/// (`Failed` → `RolledBack`); `RolledBack` and `Completed` are
/// terminal. Retrying a failed upgrade schedules a new record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpgradeStatus {
    /// Scheduled but not yet started.
    Scheduled,
    /// Currently being applied.
    InProgress,
    /// Applied successfully.
    Completed,
    /// Application failed; may be rolled back.
    Failed,
    /// A failed upgrade was rolled back.
    RolledBack,
}

/// A system upgrade from one version to another.
///
/// Versions are opaque non-blank strings (e.g. `"1.4.0"`); the domain
/// does not parse semver — ordering and compatibility are the
/// installer's concern. Deserializes through
/// [`SystemUpgrade::rehydrate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemUpgrade {
    upgrade_id: UpgradeId,
    from_version: String,
    to_version: String,
    status: UpgradeStatus,
    scheduled_by: StaffId,
    scheduled_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    failure_reason: Option<String>,
}

impl SystemUpgrade {
    /// Schedules an upgrade: starts in [`UpgradeStatus::Scheduled`].
    ///
    /// Both versions must be non-blank and different — scheduling a
    /// no-op "upgrade" to the running version is rejected.
    ///
    /// ```
    /// use crm_domain::admin::{SystemUpgrade, UpgradeStatus};
    /// use crm_domain::ids::StaffId;
    /// use chrono::Utc;
    ///
    /// let upgrade = SystemUpgrade::schedule(
    ///     "1.4.0".to_string(),
    ///     "1.5.0".to_string(),
    ///     StaffId::new(),
    ///     Utc::now(),
    /// );
    /// assert!(upgrade.is_ok());
    /// assert_eq!(upgrade.unwrap().status(), UpgradeStatus::Scheduled);
    /// // Same-version "upgrade" is rejected.
    /// assert!(SystemUpgrade::schedule(
    ///     "1.5.0".to_string(),
    ///     "1.5.0".to_string(),
    ///     StaffId::new(),
    ///     Utc::now(),
    /// )
    /// .is_err());
    /// ```
    pub fn schedule(
        from_version: String,
        to_version: String,
        scheduled_by: StaffId,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        if from_version.trim().is_empty() || to_version.trim().is_empty() {
            return Err(DomainError::Validation(
                "upgrade versions must not be blank".to_string(),
            ));
        }
        if from_version == to_version {
            return Err(DomainError::Validation(
                "upgrade target must differ from the current version".to_string(),
            ));
        }
        Ok(Self {
            upgrade_id: UpgradeId::new(),
            from_version,
            to_version,
            status: UpgradeStatus::Scheduled,
            scheduled_by,
            scheduled_at: now,
            completed_at: None,
            failure_reason: None,
        })
    }

    /// Starts the upgrade: `Scheduled` → `InProgress`.
    pub fn start(&mut self) -> Result<(), DomainError> {
        if self.status != UpgradeStatus::Scheduled {
            return Err(DomainError::Validation(format!(
                "upgrade cannot start from status {:?}",
                self.status
            )));
        }
        self.status = UpgradeStatus::InProgress;
        Ok(())
    }

    /// Completes the upgrade: `InProgress` → `Completed`.
    pub fn complete(&mut self, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != UpgradeStatus::InProgress {
            return Err(DomainError::Validation(format!(
                "upgrade cannot complete from status {:?}",
                self.status
            )));
        }
        self.status = UpgradeStatus::Completed;
        self.completed_at = Some(now);
        Ok(())
    }

    /// Fails the upgrade: `InProgress` → `Failed`.
    pub fn fail(&mut self, reason: String, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != UpgradeStatus::InProgress {
            return Err(DomainError::Validation(format!(
                "upgrade cannot fail from status {:?}",
                self.status
            )));
        }
        if reason.trim().is_empty() {
            return Err(DomainError::Validation(
                "upgrade failure reason must not be blank".to_string(),
            ));
        }
        self.status = UpgradeStatus::Failed;
        self.failure_reason = Some(reason);
        self.completed_at = Some(now);
        Ok(())
    }

    /// Rolls back a failed upgrade: `Failed` → `RolledBack`.
    pub fn rollback(&mut self, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != UpgradeStatus::Failed {
            return Err(DomainError::Validation(format!(
                "only a failed upgrade can be rolled back (status {:?})",
                self.status
            )));
        }
        self.status = UpgradeStatus::RolledBack;
        self.completed_at = Some(now);
        Ok(())
    }

    /// Rehydrates a stored upgrade.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        upgrade_id: UpgradeId,
        from_version: String,
        to_version: String,
        status: UpgradeStatus,
        scheduled_by: StaffId,
        scheduled_at: DateTime<Utc>,
        completed_at: Option<DateTime<Utc>>,
        failure_reason: Option<String>,
    ) -> Self {
        Self {
            upgrade_id,
            from_version,
            to_version,
            status,
            scheduled_by,
            scheduled_at,
            completed_at,
            failure_reason,
        }
    }

    /// Stable identifier of this upgrade.
    #[must_use]
    pub fn upgrade_id(&self) -> UpgradeId {
        self.upgrade_id
    }

    /// Current lifecycle status.
    #[must_use]
    pub fn status(&self) -> UpgradeStatus {
        self.status
    }

    /// The version being upgraded from.
    #[must_use]
    pub fn from_version(&self) -> &str {
        &self.from_version
    }

    /// The version being upgraded to.
    #[must_use]
    pub fn to_version(&self) -> &str {
        &self.to_version
    }

    /// The staff member who scheduled the upgrade.
    #[must_use]
    pub fn scheduled_by(&self) -> StaffId {
        self.scheduled_by
    }
}

impl<'de> Deserialize<'de> for SystemUpgrade {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawUpgrade {
            upgrade_id: UpgradeId,
            from_version: String,
            to_version: String,
            status: UpgradeStatus,
            scheduled_by: StaffId,
            scheduled_at: DateTime<Utc>,
            completed_at: Option<DateTime<Utc>>,
            failure_reason: Option<String>,
        }
        let raw = RawUpgrade::deserialize(deserializer)?;
        if raw.from_version.trim().is_empty() || raw.to_version.trim().is_empty() {
            return Err(serde::de::Error::custom(
                "upgrade versions must not be blank",
            ));
        }
        if raw.from_version == raw.to_version {
            return Err(serde::de::Error::custom(
                "upgrade target must differ from the current version",
            ));
        }
        Ok(SystemUpgrade::rehydrate(
            raw.upgrade_id,
            raw.from_version,
            raw.to_version,
            raw.status,
            raw.scheduled_by,
            raw.scheduled_at,
            raw.completed_at,
            raw.failure_reason,
        ))
    }
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

/// Installation state: the installer runs exactly once.
///
/// `NotStarted` → `InProgress` → `Completed` is the only path;
/// re-running the installer against a completed installation is
/// rejected by the service layer (the domain has no transition out
/// of `Completed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstallationState {
    /// The installer has never run.
    NotStarted,
    /// The installer is running.
    InProgress,
    /// The installer finished; the system is live.
    Completed,
}

/// First-run installation state.
///
/// A singleton record: the repository holds at most one. The
/// installed version is recorded on completion for upgrade
/// bookkeeping. Deserializes through [`Installation::rehydrate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Installation {
    state: InstallationState,
    version: Option<String>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

impl Installation {
    /// A fresh installation that has never run.
    #[must_use]
    pub fn not_started() -> Self {
        Self {
            state: InstallationState::NotStarted,
            version: None,
            started_at: None,
            completed_at: None,
        }
    }

    /// Begins the installation: `NotStarted` → `InProgress`.
    ///
    /// ```
    /// use crm_domain::admin::{Installation, InstallationState};
    /// use chrono::Utc;
    ///
    /// let mut installation = Installation::not_started();
    /// installation.begin(Utc::now()).unwrap();
    /// assert_eq!(installation.state(), InstallationState::InProgress);
    /// // A second begin is rejected.
    /// assert!(installation.begin(Utc::now()).is_err());
    /// ```
    pub fn begin(&mut self, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.state != InstallationState::NotStarted {
            return Err(DomainError::Validation(format!(
                "installation cannot begin from state {:?}",
                self.state
            )));
        }
        self.state = InstallationState::InProgress;
        self.started_at = Some(now);
        Ok(())
    }

    /// Completes the installation: `InProgress` → `Completed`,
    /// recording the installed version.
    pub fn complete(&mut self, version: String, now: DateTime<Utc>) -> Result<(), DomainError> {
        if self.state != InstallationState::InProgress {
            return Err(DomainError::Validation(format!(
                "installation cannot complete from state {:?}",
                self.state
            )));
        }
        if version.trim().is_empty() {
            return Err(DomainError::Validation(
                "installed version must not be blank".to_string(),
            ));
        }
        self.state = InstallationState::Completed;
        self.version = Some(version);
        self.completed_at = Some(now);
        Ok(())
    }

    /// Rehydrates the stored installation record.
    #[must_use]
    pub fn rehydrate(
        state: InstallationState,
        version: Option<String>,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            state,
            version,
            started_at,
            completed_at,
        }
    }

    /// Current installation state.
    #[must_use]
    pub fn state(&self) -> InstallationState {
        self.state
    }

    /// Whether the system is installed and live.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.state == InstallationState::Completed
    }

    /// The installed version, once completed.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

impl<'de> Deserialize<'de> for Installation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawInstallation {
            state: InstallationState,
            version: Option<String>,
            started_at: Option<DateTime<Utc>>,
            completed_at: Option<DateTime<Utc>>,
        }
        let raw = RawInstallation::deserialize(deserializer)?;
        Ok(Installation::rehydrate(
            raw.state,
            raw.version,
            raw.started_at,
            raw.completed_at,
        ))
    }
}

// ---------------------------------------------------------------------------
// System health summary
// ---------------------------------------------------------------------------

/// An operator-facing aggregate of [`HealthReport`]s.
///
/// Pure aggregation over [`crate::observability::HealthReport`]: the
/// overall status is the worst component status (any `Unhealthy`
/// wins over any `Degraded`, which wins over all-`Healthy`). The
/// per-component reports are carried along so operators can see
/// *what* is degraded, not just that something is. Compute via
/// [`summarize_health`]; this type is a query result, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemHealthSummary {
    overall: ComponentHealth,
    components: Vec<HealthReport>,
    summarized_at: DateTime<Utc>,
}

impl SystemHealthSummary {
    /// The worst component status.
    #[must_use]
    pub fn overall(&self) -> &ComponentHealth {
        &self.overall
    }

    /// The per-component reports that were aggregated.
    #[must_use]
    pub fn components(&self) -> &[HealthReport] {
        &self.components
    }

    /// When the summary was computed.
    #[must_use]
    pub fn summarized_at(&self) -> DateTime<Utc> {
        self.summarized_at
    }

    /// Whether every component is healthy.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self.overall, ComponentHealth::Healthy)
    }
}

/// Aggregates component health reports into an operator-facing
/// summary.
///
/// Worst-status-wins: one `Unhealthy` component makes the system
/// unhealthy regardless of how many components are healthy; one
/// `Degraded` (with no unhealthy) makes it degraded. An empty report
/// set summarizes as healthy — there is nothing known to be wrong.
///
/// ```
/// use chrono::Utc;
/// use crm_domain::admin::summarize_health;
/// use crm_domain::observability::{ComponentHealth, HealthReport};
///
/// let at = Utc::now();
/// let reports = vec![
///     HealthReport { component: "database", health: ComponentHealth::Healthy, checked_at: at },
///     HealthReport {
///         component: "queue",
///         health: ComponentHealth::Degraded { reason: "depth 900/1000".to_string() },
///         checked_at: at,
///     },
/// ];
/// let summary = summarize_health(reports, at);
/// assert!(!summary.is_healthy());
/// assert!(matches!(summary.overall(), ComponentHealth::Degraded { .. }));
/// ```
#[must_use]
pub fn summarize_health(reports: Vec<HealthReport>, now: DateTime<Utc>) -> SystemHealthSummary {
    let overall = if reports
        .iter()
        .any(|r| matches!(r.health, ComponentHealth::Unhealthy { .. }))
    {
        // Carry the first unhealthy reason so the summary names the
        // problem without listing every component.
        let reason = reports
            .iter()
            .find_map(|r| match &r.health {
                ComponentHealth::Unhealthy { reason } => Some(format!("{}: {reason}", r.component)),
                _ => None,
            })
            .unwrap_or_else(|| "a component is unhealthy".to_string());
        ComponentHealth::Unhealthy { reason }
    } else if reports
        .iter()
        .any(|r| matches!(r.health, ComponentHealth::Degraded { .. }))
    {
        let reason = reports
            .iter()
            .find_map(|r| match &r.health {
                ComponentHealth::Degraded { reason } => Some(format!("{}: {reason}", r.component)),
                _ => None,
            })
            .unwrap_or_else(|| "a component is degraded".to_string());
        ComponentHealth::Degraded { reason }
    } else {
        ComponentHealth::Healthy
    };
    SystemHealthSummary {
        overall,
        components: reports,
        summarized_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staff() -> StaffId {
        StaffId::new()
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    // -- StaffInvitation ------------------------------------------------

    #[test]
    fn invitation_lifecycle_is_forward_only() {
        let t = now();
        let mut invitation = StaffInvitation::new(
            "ops@example.com".to_string(),
            vec![RoleId::new()],
            staff(),
            t + chrono::Duration::days(7),
            t,
        )
        .expect("valid invitation");
        assert_eq!(invitation.status(), InvitationStatus::Pending);

        invitation.accept(t).expect("accept while pending");
        assert_eq!(invitation.status(), InvitationStatus::Accepted);
        // Terminal: no path back, and no double accept.
        assert!(invitation.accept(t).is_err());
        assert!(invitation.expire().is_err());
        assert!(invitation.revoke().is_err());
    }

    #[test]
    fn invitation_revoke_and_expire_paths() {
        let t = now();
        let mut revoked = StaffInvitation::new(
            "a@example.com".to_string(),
            vec![RoleId::new()],
            staff(),
            t + chrono::Duration::days(7),
            t,
        )
        .expect("valid invitation");
        revoked.revoke().expect("revoke while pending");
        assert_eq!(revoked.status(), InvitationStatus::Revoked);

        let mut expired = StaffInvitation::new(
            "b@example.com".to_string(),
            vec![RoleId::new()],
            staff(),
            t + chrono::Duration::days(7),
            t,
        )
        .expect("valid invitation");
        expired.expire().expect("expire while pending");
        assert_eq!(expired.status(), InvitationStatus::Expired);
    }

    #[test]
    fn invitation_rejects_expired_accept() {
        let t = now();
        let mut invitation = StaffInvitation::new(
            "c@example.com".to_string(),
            vec![RoleId::new()],
            staff(),
            t + chrono::Duration::seconds(1),
            t,
        )
        .expect("valid invitation");
        // After expiry, accept is rejected; expire records it.
        assert!(invitation.accept(t + chrono::Duration::days(1)).is_err());
        invitation.expire().expect("expire after expiry");
        assert_eq!(invitation.status(), InvitationStatus::Expired);
    }

    #[test]
    fn invitation_deserialization_rejects_invalid_email() {
        let json = serde_json::json!({
            "invitation_id": InvitationId::new(),
            "email": "not-an-email",
            "role_ids": [RoleId::new()],
            "invited_by": staff(),
            "status": "Pending",
            "expires_at": "2027-01-01T00:00:00Z",
            "created_at": "2026-10-18T00:00:00Z",
        });
        let result: Result<StaffInvitation, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn invitation_round_trip_preserves_state() {
        let t = now();
        let invitation = StaffInvitation::new(
            "d@example.com".to_string(),
            vec![RoleId::new()],
            staff(),
            t + chrono::Duration::days(7),
            t,
        )
        .expect("valid invitation");
        let json = serde_json::to_value(&invitation).expect("serializes");
        let again: StaffInvitation = serde_json::from_value(json).expect("deserializes");
        assert_eq!(again, invitation);
    }

    // -- RoleAssignment ---------------------------------------------------

    #[test]
    fn role_assignment_revoke_is_idempotent() {
        let t = now();
        let scope = Scope::Agency {
            agency_id: AgencyId::new(),
        };
        let mut assignment = RoleAssignment::new(StaffId::new(), RoleId::new(), scope, staff(), t)
            .expect("valid assignment");
        assert!(assignment.is_active());
        assignment.revoke(t);
        assert!(!assignment.is_active());
        assert_eq!(assignment.status(), AssignmentStatus::Revoked);
        // Idempotent: second revoke is a no-op, keeps the first stamp.
        assignment.revoke(t + chrono::Duration::hours(1));
        assert_eq!(assignment.revoked_at(), Some(t));
    }

    #[test]
    fn role_assignment_round_trip_preserves_state() {
        let t = now();
        let scope = Scope::Agency {
            agency_id: AgencyId::new(),
        };
        let assignment = RoleAssignment::new(StaffId::new(), RoleId::new(), scope, staff(), t)
            .expect("valid assignment");
        let json = serde_json::to_value(&assignment).expect("serializes");
        let again: RoleAssignment = serde_json::from_value(json).expect("deserializes");
        assert_eq!(again, assignment);
    }

    // -- AgencySettings ----------------------------------------------------

    #[test]
    fn settings_update_versions_monotonically() {
        let t = now();
        let settings = AgencySettings::new(
            AgencyId::new(),
            "Acme".to_string(),
            None,
            43_200,
            staff(),
            t,
        )
        .expect("valid settings");
        assert_eq!(settings.version(), 1);
        let updated = settings
            .update(
                Some("Acme Corp".to_string()),
                None,
                None,
                staff(),
                t + chrono::Duration::hours(1),
            )
            .expect("valid update");
        assert_eq!(updated.version(), 2);
        assert_eq!(updated.display_name(), "Acme Corp");
        // Untouched fields carry over.
        assert_eq!(updated.session_ttl_secs(), 43_200);
    }

    #[test]
    fn settings_rejects_blank_name_and_bad_ttl() {
        let t = now();
        assert!(
            AgencySettings::new(AgencyId::new(), "   ".to_string(), None, 43_200, staff(), t,)
                .is_err()
        );
        assert!(AgencySettings::new(
            AgencyId::new(),
            "Acme".to_string(),
            None,
            60, // below the 15-minute minimum
            staff(),
            t,
        )
        .is_err());
        assert!(AgencySettings::new(
            AgencyId::new(),
            "Acme".to_string(),
            Some("bad-email".to_string()),
            43_200,
            staff(),
            t,
        )
        .is_err());
    }

    #[test]
    fn settings_deserialization_revalidates() {
        let json = serde_json::json!({
            "agency_id": AgencyId::new(),
            "version": 3,
            "display_name": "",
            "support_email": null,
            "session_ttl_secs": 43_200,
            "updated_by": staff(),
            "updated_at": "2026-10-18T00:00:00Z",
        });
        let result: Result<AgencySettings, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    // -- MaintenanceMode ----------------------------------------------------

    #[test]
    fn maintenance_toggle_round_trip() {
        let t = now();
        let mut mode = MaintenanceMode::disabled(staff(), t);
        assert!(!mode.is_enabled());
        mode.enable("Upgrade".to_string(), staff(), t)
            .expect("enable with reason");
        assert!(mode.is_enabled());
        assert_eq!(mode.reason(), Some("Upgrade"));
        mode.disable(staff(), t);
        assert!(!mode.is_enabled());
        assert_eq!(mode.reason(), None);
    }

    #[test]
    fn maintenance_rejects_blank_reason() {
        let t = now();
        let mut mode = MaintenanceMode::disabled(staff(), t);
        assert!(mode.enable("  ".to_string(), staff(), t).is_err());
        assert!(!mode.is_enabled());
    }

    #[test]
    fn maintenance_deserialization_requires_reason_when_enabled() {
        let json = serde_json::json!({
            "enabled": true,
            "reason": null,
            "set_by": staff(),
            "set_at": "2026-10-18T00:00:00Z",
        });
        let result: Result<MaintenanceMode, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    // -- BackupRecord ---------------------------------------------------------

    #[test]
    fn backup_lifecycle_is_forward_only() {
        let t = now();
        let mut backup = BackupRecord::schedule(staff(), t);
        // Cannot complete before starting.
        assert!(backup.complete(100, t).is_err());
        backup.start(t).expect("start while scheduled");
        assert!(backup.start(t).is_err());
        backup.complete(2048, t).expect("complete while running");
        assert_eq!(backup.status(), BackupStatus::Completed);
        assert_eq!(backup.size_bytes(), Some(2048));
        // Terminal: no transitions out of Completed.
        assert!(backup.fail("x".to_string(), t).is_err());
    }

    #[test]
    fn backup_fail_records_reason() {
        let t = now();
        let mut backup = BackupRecord::schedule(staff(), t);
        backup.start(t).expect("start");
        backup
            .fail("disk full".to_string(), t)
            .expect("fail while running");
        assert_eq!(backup.status(), BackupStatus::Failed);
        assert_eq!(backup.failure_reason(), Some("disk full"));
        assert!(backup.fail("  ".to_string(), t).is_err());
    }

    // -- DataExport -----------------------------------------------------------

    #[test]
    fn export_lifecycle_with_expiry() {
        let t = now();
        let mut export = DataExport::request(None, ExportFormat::Json, staff(), t);
        export.start().expect("start while requested");
        export.complete(t).expect("complete while running");
        assert_eq!(export.status(), ExportStatus::Ready);
        export.expire().expect("expire while ready");
        assert_eq!(export.status(), ExportStatus::Expired);
        // Terminal.
        assert!(export.expire().is_err());
    }

    #[test]
    fn export_fail_path() {
        let t = now();
        let mut export =
            DataExport::request(Some(ClientProfileId::new()), ExportFormat::Csv, staff(), t);
        export.start().expect("start");
        export
            .fail("timeout".to_string())
            .expect("fail while running");
        assert_eq!(export.status(), ExportStatus::Failed);
    }

    // -- SystemUpgrade --------------------------------------------------------

    #[test]
    fn upgrade_lifecycle_with_rollback() {
        let t = now();
        let mut upgrade =
            SystemUpgrade::schedule("1.4.0".to_string(), "1.5.0".to_string(), staff(), t)
                .expect("valid upgrade");
        upgrade.start().expect("start while scheduled");
        upgrade
            .fail("migration error".to_string(), t)
            .expect("fail while running");
        assert_eq!(upgrade.status(), UpgradeStatus::Failed);
        upgrade.rollback(t).expect("rollback a failed upgrade");
        assert_eq!(upgrade.status(), UpgradeStatus::RolledBack);
        // Terminal.
        assert!(upgrade.rollback(t).is_err());
    }

    #[test]
    fn upgrade_rejects_same_version() {
        let t = now();
        assert!(
            SystemUpgrade::schedule("1.5.0".to_string(), "1.5.0".to_string(), staff(), t,).is_err()
        );
    }

    #[test]
    fn upgrade_rollback_only_from_failed() {
        let t = now();
        let mut upgrade =
            SystemUpgrade::schedule("1.4.0".to_string(), "1.5.0".to_string(), staff(), t)
                .expect("valid upgrade");
        // Cannot roll back a scheduled upgrade.
        assert!(upgrade.rollback(t).is_err());
    }

    // -- Installation ----------------------------------------------------------

    #[test]
    fn installation_runs_exactly_once() {
        let t = now();
        let mut installation = Installation::not_started();
        assert!(!installation.is_completed());
        installation.begin(t).expect("begin from not-started");
        installation
            .complete("1.0.0".to_string(), t)
            .expect("complete while running");
        assert!(installation.is_completed());
        assert_eq!(installation.version(), Some("1.0.0"));
        // No second run.
        assert!(installation.begin(t).is_err());
        assert!(installation.complete("1.0.1".to_string(), t).is_err());
    }

    // -- Health summary ---------------------------------------------------------

    #[test]
    fn health_summary_worst_wins() {
        let t = now();
        let healthy = HealthReport {
            component: "database",
            health: ComponentHealth::Healthy,
            checked_at: t,
        };
        let degraded = HealthReport {
            component: "queue",
            health: ComponentHealth::Degraded {
                reason: "depth high".to_string(),
            },
            checked_at: t,
        };
        let unhealthy = HealthReport {
            component: "email",
            health: ComponentHealth::Unhealthy {
                reason: "connection refused".to_string(),
            },
            checked_at: t,
        };
        let summary = summarize_health(vec![healthy.clone(), degraded.clone()], t);
        assert!(matches!(
            summary.overall(),
            ComponentHealth::Degraded { .. }
        ));
        let summary = summarize_health(vec![healthy, degraded, unhealthy], t);
        assert!(matches!(
            summary.overall(),
            ComponentHealth::Unhealthy { .. }
        ));
        assert!(!summary.is_healthy());
    }

    #[test]
    fn health_summary_empty_is_healthy() {
        let summary = summarize_health(vec![], now());
        assert!(summary.is_healthy());
        assert!(matches!(summary.overall(), ComponentHealth::Healthy));
    }

    // -- Audit consts -------------------------------------------------------------

    #[test]
    fn audit_action_names_are_pinned() {
        // Pinned like the other workstreams' action names: renaming one
        // breaks audit-trail queries, so the strings are asserted here.
        assert_eq!(ADMIN_STAFF_INVITED, "admin.staff_invited");
        assert_eq!(ADMIN_INVITATION_ACCEPTED, "admin.invitation_accepted");
        assert_eq!(ADMIN_INVITATION_REVOKED, "admin.invitation_revoked");
        assert_eq!(ADMIN_INVITATION_EXPIRED, "admin.invitation_expired");
        assert_eq!(ADMIN_STAFF_DISABLED, "admin.staff_disabled");
        assert_eq!(ADMIN_STAFF_REACTIVATED, "admin.staff_reactivated");
        assert_eq!(ADMIN_ROLE_ASSIGNED, "admin.role_assigned");
        assert_eq!(ADMIN_ROLE_REVOKED, "admin.role_revoked");
        assert_eq!(ADMIN_SETTINGS_UPDATED, "admin.settings_updated");
        assert_eq!(ADMIN_MAINTENANCE_ENABLED, "admin.maintenance_enabled");
        assert_eq!(ADMIN_MAINTENANCE_DISABLED, "admin.maintenance_disabled");
        assert_eq!(ADMIN_BACKUP_STARTED, "admin.backup_started");
        assert_eq!(ADMIN_BACKUP_COMPLETED, "admin.backup_completed");
        assert_eq!(ADMIN_BACKUP_FAILED, "admin.backup_failed");
        assert_eq!(ADMIN_EXPORT_REQUESTED, "admin.export_requested");
        assert_eq!(ADMIN_EXPORT_READY, "admin.export_ready");
        assert_eq!(ADMIN_EXPORT_FAILED, "admin.export_failed");
        assert_eq!(ADMIN_UPGRADE_STARTED, "admin.upgrade_started");
        assert_eq!(ADMIN_UPGRADE_COMPLETED, "admin.upgrade_completed");
        assert_eq!(ADMIN_UPGRADE_FAILED, "admin.upgrade_failed");
        assert_eq!(ADMIN_UPGRADE_ROLLED_BACK, "admin.upgrade_rolled_back");
        assert_eq!(ADMIN_INSTALLATION_COMPLETED, "admin.installation_completed");
    }
}
