// SPDX-License-Identifier: GPL-3.0-only
//! Staff identity, sessions, RBAC grants and bounded agent delegation.
//!
//! This module is the security foundation the API's authentication and
//! authorization layers (task 03-SEC-02) build on. It is pure domain
//! logic: no I/O, no HTTP, no SQL.
//!
//! ## Invariants
//!
//! * **Deny by default.** [`is_authorized`] returns `false` unless an
//!   explicit [`Grant`] covers the requested action and scope. There are
//!   no negative grants and no implicit allows.
//! * **Tokens are never stored.** A [`SessionToken`] is shown to the
//!   client exactly once at issuance; the persisted [`Session`] carries
//!   only its SHA-256 hash. Stealing the database does not yield live
//!   sessions.
//! * **Delegation ceiling.** A [`Delegation`] can only be built through
//!   [`Delegation::delegate`], which rejects any requested grant the
//!   delegator does not hold. An agent can never exceed its delegator's
//!   authority — enforced at construction, not by convention.
//! * **Suspended means stopped.** A suspended [`StaffUser`] cannot
//!   delegate, and their existing delegations stop being effective
//!   ([`Delegation::is_effective`]).
//! * **No secrets in errors.** [`DomainError`] messages never carry
//!   tokens, password hashes or PII.
//!
//! ## Intended storage shape (later persistence task)
//!
//! * `staff_users` — keyed by `staff_id`; password credential in a
//!   `phc_hash TEXT` column (never a plaintext column).
//! * `sessions` — keyed by `session_id`; `token_hash BYTEA` column; the
//!   raw token is never stored. Rows are never deleted (revocation is a
//!   timestamp) because session history is security-relevant.
//! * `grants` — `grant_id` PK with `staff_id`, resource, operation and
//!   scope columns; assignments are additive.
//! * `delegations` — the validated grant subset, expiry and revocation.

use std::fmt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::DomainError;
use crate::ids::{
    AgencyId, AgentPrincipalId, ClientProfileId, DelegationId, GrantId, RoleId, SessionId, StaffId,
};

/// Default session lifetime: 12 hours from issuance.
///
/// A deployment policy default, not a security proof: task 03-SEC-02 may
/// make it configurable. Sliding refresh ([`crate::services::AuthenticationService::refresh`])
/// extends within this bound per session.
pub const DEFAULT_SESSION_TTL_SECS: i64 = 12 * 3600;

/// Default idle timeout: 30 minutes without activity invalidates the session.
pub const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 30 * 60;

/// Maximum delegation lifetime: 24 hours.
///
/// Bounds the blast radius of a compromised agent principal. Longer-lived
/// authority requires a fresh delegation by the staff user.
pub const MAX_DELEGATION_TTL_SECS: i64 = 24 * 3600;

/// Maximum session lifetime: 7 days.
///
/// Sessions are human-interactive credentials; anything longer-lived
/// should be a fresh login, not an ancient session. [`Session::new`]
/// rejects TTLs above this bound.
pub const MAX_SESSION_TTL_SECS: i64 = 7 * 24 * 3600;

// ---------------------------------------------------------------------------
// Staff identity
// ---------------------------------------------------------------------------

/// Whether a staff account may act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StaffStatus {
    /// The account may authenticate and act within its grants.
    Active,
    /// The account is blocked: authentication fails, existing sessions
    /// stop authorizing, and its delegations stop being effective.
    Suspended,
}

/// A stored password credential.
///
/// Only the password hash is ever held here — the plaintext password
/// crosses the API boundary in 03-SEC-02 and is never stored. `phc_hash`
/// is a PHC-format string (e.g. `$argon2id$v=19$m=65536,t=3,p=4$...`),
/// which embeds the algorithm tag so rotation does not need a separate
/// column. Hashing and constant-time verification are 03-SEC-02's
/// concern; this type defines the shape. Intended storage: `phc_hash
/// TEXT` column — never a plaintext column.
///
/// Fields are private so the PHC shape check in [`PasswordCredential::new`]
/// cannot be bypassed with a struct literal; the `Debug` impl redacts the
/// hash so credentials never leak into logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordCredential {
    phc_hash: String,
    updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for PasswordCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordCredential")
            .field("phc_hash", &"***")
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl PasswordCredential {
    /// Stores a password hash after a PHC shape check.
    ///
    /// Requires the multi-segment PHC structure (`$<id>$<params>$<salt>$<hash>`
    /// with at least three non-empty segments after the id); a bare
    /// `$`-prefixed string is rejected. This validates the shape only —
    /// it does not and cannot validate the cryptography. Never pass a
    /// plaintext password here.
    pub fn new(phc_hash: String, updated_at: DateTime<Utc>) -> Result<Self, DomainError> {
        let mut segments = phc_hash.split('$');
        let starts_with_dollar = segments.next() == Some("");
        let algorithm_id = segments.next().filter(|id| !id.is_empty());
        let tail: Vec<&str> = segments.collect();
        let well_formed = starts_with_dollar
            && algorithm_id.is_some()
            && tail.len() >= 2
            && tail.iter().all(|segment| !segment.is_empty());
        if !well_formed {
            return Err(DomainError::Validation(
                "password credential must be a PHC-format hash string ($<id>$...$<salt>$<hash>)"
                    .to_string(),
            ));
        }
        Ok(Self {
            phc_hash,
            updated_at,
        })
    }

    /// The PHC hash string (opaque to the domain; never logged).
    #[must_use]
    pub fn phc_hash(&self) -> &str {
        &self.phc_hash
    }

    /// When this credential was set (for rotation age policies).
    #[must_use]
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

/// An agency staff member: a human operator of the CRM.
///
/// Staff belong to exactly one [`AgencyId`]; their [`Grant`]s bind them
/// to agency-wide or per-client-profile scopes. Status transitions go
/// through [`StaffUser::suspend`] / [`StaffUser::reactivate`] (idempotent,
/// mirroring [`crate::contact::Contact::transition_to`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaffUser {
    /// Stable identifier of this staff member.
    pub staff_id: StaffId,
    /// The agency this user operates under.
    pub agency_id: AgencyId,
    /// Login name, normalized to lowercase by the service layer.
    pub email: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Whether the account may act.
    pub status: StaffStatus,
    /// Password credential, if set (`None` until enrolled).
    pub credential: Option<PasswordCredential>,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// When the account was last modified.
    pub updated_at: DateTime<Utc>,
}

impl StaffUser {
    /// Suspends the account: authentication fails and delegations stop
    /// being effective. Idempotent — suspending a suspended account is a
    /// no-op so retried commands stay safe.
    pub fn suspend(&mut self) {
        self.status = StaffStatus::Suspended;
    }

    /// Reactivates the account. Idempotent.
    pub fn reactivate(&mut self) {
        self.status = StaffStatus::Active;
    }

    /// Whether the account may authenticate and act.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == StaffStatus::Active
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Opaque session token: the bearer secret the client holds.
///
/// Security properties, all deliberate:
/// * no [`Display`](fmt::Display) and no `Serialize` — the token must
///   never land in logs or stored records; 03-SEC-02 renders it at the
///   login HTTP boundary only;
/// * [`fmt::Debug`] is redacted;
/// * the server persists only [`SessionToken::token_hash`], never the
///   token itself.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionToken(uuid::Uuid);

impl fmt::Debug for SessionToken {
    /// Redacted: tokens must never appear in logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(***)")
    }
}

impl SessionToken {
    /// Creates a new random (UUID v4) token.
    ///
    /// `Default` is deliberately *not* implemented: a random token is not
    /// a neutral fixed value.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// SHA-256 over the token bytes. This — and only this — is persisted
    /// on the [`Session`]. Looking a session up hashes the presented
    /// token and compares hashes in constant time (03-SEC-02).
    #[must_use]
    pub fn token_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.0.as_bytes());
        hasher.finalize().into()
    }

    /// Returns the underlying UUID for the API boundary (login response).
    /// Handle the result as a secret.
    #[must_use]
    pub fn as_uuid(self) -> uuid::Uuid {
        self.0
    }

    /// Reconstructs a token from its UUID form.
    ///
    /// This is the inverse of [`as_uuid`](Self::as_uuid), for the API
    /// boundary only: parsing a presented `Authorization: Bearer <uuid>`
    /// header back into a token so its [`token_hash`](Self::token_hash)
    /// can be looked up. Added for task 03-SEC-02 — the service traits
    /// take `&SessionToken`, so the HTTP layer needs this constructor to
    /// resolve presented bearer tokens. Handle the result as a secret:
    /// never log it, never put it in error messages.
    #[must_use]
    pub fn from_uuid(id: uuid::Uuid) -> Self {
        Self(id)
    }
}

/// A login session.
///
/// The record carries the token *hash*, never the token. `delegation_id`
/// marks agent sessions: they authorize against the delegation's grants,
/// never the delegator's full grant set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Stable identifier of this session.
    pub session_id: SessionId,
    /// The staff user this session belongs to.
    pub staff_id: StaffId,
    /// SHA-256 of the bearer token; the raw token is never persisted.
    pub token_hash: [u8; 32],
    /// When set, this is an agent session acting under the delegation.
    pub delegation_id: Option<DelegationId>,
    /// When the session was issued.
    pub issued_at: DateTime<Utc>,
    /// Absolute expiry; sliding refresh extends it.
    pub expires_at: DateTime<Utc>,
    /// Last observed activity; drives the idle timeout.
    pub last_activity_at: DateTime<Utc>,
    /// When the session was revoked, if ever. Revocation is a timestamp,
    /// not a delete — session history is security-relevant.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Session {
    /// Starts a session for `token_hash`, expiring `ttl` after `issued_at`.
    ///
    /// Rejects non-positive TTLs (a stillborn session is a caller bug)
    /// and TTLs above [`MAX_SESSION_TTL_SECS`] (sessions are
    /// human-interactive credentials; longer lifetimes need a fresh login).
    pub fn new(
        session_id: SessionId,
        staff_id: StaffId,
        token_hash: [u8; 32],
        issued_at: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<Self, DomainError> {
        if ttl <= Duration::zero() {
            return Err(DomainError::Validation(
                "session TTL must be positive".to_string(),
            ));
        }
        if ttl > Duration::seconds(MAX_SESSION_TTL_SECS) {
            return Err(DomainError::Validation(format!(
                "session TTL exceeds the maximum of {MAX_SESSION_TTL_SECS} seconds"
            )));
        }
        Ok(Self {
            session_id,
            staff_id,
            token_hash,
            delegation_id: None,
            issued_at,
            expires_at: issued_at + ttl,
            last_activity_at: issued_at,
            revoked_at: None,
        })
    }

    /// Whether `now` is at or past the absolute expiry.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Whether the session was revoked.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// Whether the session has been idle longer than `idle_timeout`.
    #[must_use]
    pub fn is_idle(&self, now: DateTime<Utc>, idle_timeout: Duration) -> bool {
        now.signed_duration_since(self.last_activity_at) > idle_timeout
    }

    /// Whether the session may authorize right now: not expired, not
    /// revoked, not idle-timed-out.
    #[must_use]
    pub fn is_valid(&self, now: DateTime<Utc>, idle_timeout: Duration) -> bool {
        !self.is_expired(now) && !self.is_revoked() && !self.is_idle(now, idle_timeout)
    }

    /// Revokes the session. Idempotent: the first revocation timestamp is
    /// kept so retried logouts stay safe and history stays accurate.
    pub fn revoke(&mut self, now: DateTime<Utc>) {
        if self.revoked_at.is_none() {
            self.revoked_at = Some(now);
        }
    }

    /// Records activity at `now`. Only moves forward: clock skew must not
    /// resurrect an idle session's activity marker.
    pub fn touch(&mut self, now: DateTime<Utc>) {
        if now > self.last_activity_at {
            self.last_activity_at = now;
        }
    }
}

/// A freshly issued session: the one moment the raw token exists outside
/// the client.
///
/// The token is transmitted to the client exactly once (login response);
/// only [`Session::token_hash`] is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedSession {
    /// The opaque bearer token — handle as a secret, never store.
    pub token: SessionToken,
    /// The persisted session record (token hash only).
    pub session: Session,
}

// ---------------------------------------------------------------------------
// RBAC: resources, operations, scopes, grants, roles
// ---------------------------------------------------------------------------

/// Resource kinds subject to authorization.
///
/// The closed enum keeps the permission model auditable: adding a
/// resource is a deliberate, reviewed change, not a string typo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Resource {
    /// Person records inside a client profile.
    Contact,
    /// E-signature envelopes.
    Contract,
    /// Outreach campaigns.
    Campaign,
    /// Agent tool-call action records.
    AgentAction,
    /// The tamper-evident audit log.
    AuditLog,
    /// Client-profile workspaces (agency-managed).
    ClientProfile,
    /// File/media assets attached to a client profile.
    Asset,
    /// Staff accounts and their grants.
    StaffUser,
    /// Login sessions (revocation, inspection).
    Session,
    /// Email messages inside a client profile (task 07-MAIL-02).
    Email,
    /// Email conversation threads inside a client profile (task 07-MAIL-02).
    EmailThread,
    /// Jev agent runs inside a client profile (task 09-JEV-02). Covers
    /// the run lifecycle (start, approve, cancel, handoff); tool-call
    /// audit records stay under `AgentAction`.
    AgentRun,
    /// Automation workflows inside a client profile (task 10-WORK-02).
    Workflow,
    /// Intake/lead forms inside a client profile (task 11-FORM-02).
    /// Covers the form lifecycle (publish, archive), field management
    /// and submission intake (submit, review).
    Form,
    /// Activity/notification stream inside a client profile (task
    /// 12-FEED-02). Covers feed reads and item annotations
    /// (pin, hide, mark read); item content is write-once.
    FeedItem,
    /// Invoices and payment records inside a client profile (task
    /// 12-BILL-02). Covers the invoice lifecycle (issue, void),
    /// draft-only line items, and payment coverage (record, reverse).
    Invoice,
    /// Calls inside a client profile (task 14-VOICE-02). Covers the
    /// call lifecycle (place, answer, end), consent-gated recordings,
    /// and the provider port.
    Call,
}

/// Operations on a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operation {
    /// Read-only access.
    Read,
    /// Create new records.
    Create,
    /// Modify existing records.
    Update,
    /// Delete or archive records.
    Delete,
    /// Approve a high-impact or agent-proposed action.
    Approve,
    /// Full control within the scope: covers every other operation on
    /// the same resource (see [`Grant::covers`]). Reserved for agency
    /// administrators; never delegated lightly.
    Manage,
}

/// One authorized capability: an operation on a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Action {
    /// What kind of thing is acted on.
    pub resource: Resource,
    /// What is done to it.
    pub operation: Operation,
}

/// The boundary a grant applies within.
///
/// Every scope carries its [`AgencyId`], so cross-agency coverage is
/// impossible by construction in [`Scope::covers`]: scopes of different
/// agencies never cover each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    /// The whole agency: covers every client profile of the agency.
    Agency {
        /// The agency this scope belongs to.
        agency_id: AgencyId,
    },
    /// Exactly one client profile: covers nothing else.
    ClientProfile {
        /// The agency the profile belongs to.
        agency_id: AgencyId,
        /// The single covered profile.
        client_profile_id: ClientProfileId,
    },
}

impl Scope {
    /// The agency this scope belongs to.
    #[must_use]
    pub fn agency_id(&self) -> AgencyId {
        match self {
            Scope::Agency { agency_id } | Scope::ClientProfile { agency_id, .. } => *agency_id,
        }
    }

    /// Whether this scope includes `other`.
    ///
    /// * An agency scope covers the agency itself and each of its client
    ///   profiles.
    /// * A client-profile scope covers only that exact profile.
    /// * Scopes of different agencies never cover each other.
    ///
    /// ```
    /// use crm_domain::ids::{AgencyId, ClientProfileId};
    /// use crm_domain::security::Scope;
    /// use uuid::Uuid;
    ///
    /// let agency = AgencyId::from(Uuid::from_u128(1));
    /// let other_agency = AgencyId::from(Uuid::from_u128(9));
    /// let profile = ClientProfileId::from(Uuid::from_u128(2));
    ///
    /// let agency_scope = Scope::Agency { agency_id: agency };
    /// let profile_scope = Scope::ClientProfile { agency_id: agency, client_profile_id: profile };
    /// let foreign_scope = Scope::ClientProfile { agency_id: other_agency, client_profile_id: profile };
    ///
    /// assert!(agency_scope.covers(&profile_scope));
    /// assert!(agency_scope.covers(&agency_scope));
    /// assert!(profile_scope.covers(&profile_scope));
    /// // Narrower never covers wider; cross-agency never covers.
    /// assert!(!profile_scope.covers(&agency_scope));
    /// assert!(!agency_scope.covers(&foreign_scope));
    /// ```
    #[must_use]
    pub fn covers(&self, other: &Scope) -> bool {
        match (self, other) {
            (Scope::Agency { agency_id: a }, Scope::Agency { agency_id: b }) => a == b,
            (Scope::Agency { agency_id: a }, Scope::ClientProfile { agency_id: b, .. }) => a == b,
            (
                Scope::ClientProfile {
                    agency_id: a,
                    client_profile_id: x,
                },
                Scope::ClientProfile {
                    agency_id: b,
                    client_profile_id: y,
                },
            ) => a == b && x == y,
            (Scope::ClientProfile { .. }, Scope::Agency { .. }) => false,
        }
    }
}

/// One authorized capability within one scope: the unit of least privilege.
///
/// Grants are additive and narrow: prefer many small grants over one
/// wide one. There are no negative grants — deny by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Grant {
    /// Stable identifier of this grant.
    pub grant_id: GrantId,
    /// The authorized capability.
    pub action: Action,
    /// The boundary it applies within.
    pub scope: Scope,
}

impl Grant {
    /// Whether this grant authorizes `action` within `scope`.
    ///
    /// [`Operation::Manage`] on a resource covers every operation on that
    /// resource within the scope; any other operation covers only itself.
    /// The scope must cover (be equal to or wider than) the requested one.
    #[must_use]
    pub fn covers(&self, action: &Action, scope: &Scope) -> bool {
        let operation_covered =
            self.action.operation == Operation::Manage || self.action.operation == action.operation;
        self.action.resource == action.resource && operation_covered && self.scope.covers(scope)
    }
}

/// A named, reusable bundle of permissions.
///
/// A role carries no scope itself: [`Role::grants_for`] materializes one
/// [`Grant`] per permission within the scope the role is assigned at, so
/// the same role (e.g. "marketer") can be granted per client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    /// Stable identifier of this role.
    pub role_id: RoleId,
    /// Human-readable name, e.g. `"marketer"`.
    pub name: String,
    /// The capabilities bundled by this role.
    pub permissions: Vec<Action>,
}

impl Role {
    /// Materializes one [`Grant`] per permission within `scope`.
    ///
    /// Pure and total: the same role assigned at two scopes yields two
    /// disjoint grant sets.
    #[must_use]
    pub fn grants_for(&self, scope: Scope) -> Vec<Grant> {
        self.permissions
            .iter()
            .map(|action| Grant {
                grant_id: GrantId::new(),
                action: *action,
                scope,
            })
            .collect()
    }
}

/// Pure permission check: deny by default.
///
/// Returns `true` iff at least one grant in `grants` covers (`action`,
/// `scope`). No grants — or no covering grant — is a denial, never an
/// implicit allow. This is the check the authorization service applies
/// server-side before every mutation; it is pure so it unit-tests
/// without I/O.
///
/// ```
/// use crm_domain::ids::{AgencyId, ClientProfileId, GrantId};
/// use crm_domain::security::{is_authorized, Action, Grant, Operation, Resource, Scope};
/// use uuid::Uuid;
///
/// let agency = AgencyId::from(Uuid::from_u128(1));
/// let profile = ClientProfileId::from(Uuid::from_u128(2));
/// let scope = Scope::ClientProfile { agency_id: agency, client_profile_id: profile };
/// let read_contacts = Action { resource: Resource::Contact, operation: Operation::Read };
/// let grant = Grant { grant_id: GrantId::new(), action: read_contacts, scope };
///
/// // The grant authorizes exactly what it covers — nothing more.
/// assert!(is_authorized(&[grant], &read_contacts, &scope));
/// let update_contacts = Action { resource: Resource::Contact, operation: Operation::Update };
/// assert!(!is_authorized(&[grant], &update_contacts, &scope));
/// // And nothing at all without grants: deny by default.
/// assert!(!is_authorized(&[], &read_contacts, &scope));
/// ```
#[must_use]
pub fn is_authorized(grants: &[Grant], action: &Action, scope: &Scope) -> bool {
    grants.iter().any(|grant| grant.covers(action, scope))
}

// ---------------------------------------------------------------------------
// Bounded agent delegation
// ---------------------------------------------------------------------------

/// One capability requested for delegation: an action within a scope.
///
/// The requested scope may be *narrower* than a held grant's scope (e.g.
/// delegating one client profile out of an agency-wide grant) but never
/// wider — [`Scope::covers`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestedGrant {
    /// The capability to delegate.
    pub action: Action,
    /// The boundary to delegate it within.
    pub scope: Scope,
}

/// Bounded delegation of a staff user's grants to an agent principal.
///
/// ## The ceiling invariant
///
/// A delegation can **never** exceed the delegator's own grants. This is
/// enforced by construction, not by convention:
/// * the fields are private — there is no way to build or mutate a
///   `Delegation` except through [`Delegation::delegate`] (new
///   delegations) and [`Delegation::rehydrate`] (loading a stored
///   record that was validated at creation);
/// * `delegate` rejects every requested grant that is not covered by at
///   least one of the delegator's grants with [`DomainError::Forbidden`];
/// * only a staff user can delegate ([`StaffId`], not another
///   `Delegation`), so authority cannot be transitively re-delegated;
/// * the delegator must be [`StaffStatus::Active`], and
///   [`Delegation::is_effective`] additionally requires the delegator to
///   *still* be active — suspending the staff user stops their agents.
///
/// Delegations also carry an explicit expiry (bounded by
/// [`MAX_DELEGATION_TTL_SECS`]) and revocation is a timestamp, not a
/// delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    delegation_id: DelegationId,
    delegator: StaffId,
    agent: AgentPrincipalId,
    grants: Vec<Grant>,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl Delegation {
    /// Delegates a subset of the delegator's grants to an agent principal.
    ///
    /// Every requested grant must be covered by at least one of
    /// `delegator_grants` — same action (or `Manage`) within an equal or
    /// wider scope — otherwise [`DomainError::Forbidden`]. The requested
    /// scope may narrow a held grant (agency → one client profile) but
    /// never widen it.
    ///
    /// Also rejected: an inactive delegator ([`DomainError::Forbidden`]),
    /// an empty request, a non-future expiry, or a TTL beyond
    /// [`MAX_DELEGATION_TTL_SECS`] ([`DomainError::Validation`]).
    ///
    /// ```
    /// use chrono::{DateTime, Duration};
    /// use crm_domain::ids::{AgencyId, AgentPrincipalId, ClientProfileId, StaffId};
    /// use crm_domain::security::{
    ///     Action, Delegation, Grant, Operation, RequestedGrant, Resource, Scope,
    ///     StaffStatus, StaffUser,
    /// };
    /// use uuid::Uuid;
    ///
    /// let now = DateTime::from_timestamp(1_700_000_000, 0).expect("fixed time");
    /// let agency = AgencyId::from(Uuid::from_u128(1));
    /// let profile = ClientProfileId::from(Uuid::from_u128(2));
    /// let staff = StaffUser {
    ///     staff_id: StaffId::from(Uuid::from_u128(3)),
    ///     agency_id: agency,
    ///     email: "op@example.com".to_string(),
    ///     display_name: "Op".to_string(),
    ///     status: StaffStatus::Active,
    ///     credential: None,
    ///     created_at: now,
    ///     updated_at: now,
    /// };
    /// let read_contacts = Action { resource: Resource::Contact, operation: Operation::Read };
    /// let held = Grant {
    ///     grant_id: crm_domain::ids::GrantId::new(),
    ///     action: read_contacts,
    ///     scope: Scope::Agency { agency_id: agency },
    /// };
    /// let agent = AgentPrincipalId::new();
    /// let requested = vec![RequestedGrant {
    ///     action: read_contacts,
    ///     scope: Scope::ClientProfile { agency_id: agency, client_profile_id: profile },
    /// }];
    /// let expires_at = now + Duration::seconds(3600);
    ///
    /// // Held (agency-wide read) covers the narrowed request: allowed.
    /// let delegation = Delegation::delegate(&staff, &[held], agent, requested, expires_at, now)
    ///     .expect("covered grant delegates");
    /// assert_eq!(delegation.grants().len(), 1);
    ///
    /// // Requesting something not held is impossible: rejected, not truncated.
    /// let write_contacts = Action { resource: Resource::Contact, operation: Operation::Update };
    /// let overreach = vec![RequestedGrant {
    ///     action: write_contacts,
    ///     scope: Scope::ClientProfile { agency_id: agency, client_profile_id: profile },
    /// }];
    /// let err = Delegation::delegate(&staff, &[held], agent, overreach, expires_at, now)
    ///     .expect_err("must not delegate unheld grants");
    /// assert!(matches!(err, crm_domain::DomainError::Forbidden(_)));
    /// ```
    pub fn delegate(
        delegator: &StaffUser,
        delegator_grants: &[Grant],
        agent: AgentPrincipalId,
        requested: Vec<RequestedGrant>,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        if !delegator.is_active() {
            return Err(DomainError::Forbidden(
                "delegator account is not active".to_string(),
            ));
        }
        if requested.is_empty() {
            return Err(DomainError::Validation(
                "delegation must include at least one grant".to_string(),
            ));
        }
        if expires_at <= now {
            return Err(DomainError::Validation(
                "delegation expiry must be in the future".to_string(),
            ));
        }
        if expires_at.signed_duration_since(now) > Duration::seconds(MAX_DELEGATION_TTL_SECS) {
            return Err(DomainError::Validation(format!(
                "delegation TTL exceeds the maximum of {MAX_DELEGATION_TTL_SECS} seconds"
            )));
        }
        let mut grants = Vec::with_capacity(requested.len());
        for req in &requested {
            // The service layer must pass the delegator's own grants, but
            // fail closed on a mismatch anyway: a held grant outside the
            // delegator's agency indicates a caller bug, and must never
            // become a cross-agency delegation.
            let held = delegator_grants.iter().any(|grant| {
                grant.scope.agency_id() == delegator.agency_id
                    && grant.covers(&req.action, &req.scope)
            });
            if !held {
                return Err(DomainError::Forbidden(format!(
                    "cannot delegate {action:?} in {scope:?}: not held by the delegator",
                    action = req.action,
                    scope = req.scope
                )));
            }
            grants.push(Grant {
                grant_id: GrantId::new(),
                action: req.action,
                scope: req.scope,
            });
        }
        Ok(Self {
            delegation_id: DelegationId::new(),
            delegator: delegator.staff_id,
            agent,
            grants,
            expires_at,
            revoked_at: None,
        })
    }

    /// Rehydrates a delegation from stored fields (repository implementations).
    ///
    /// The record must have been created through [`Delegation::delegate`]:
    /// rehydration does not re-check the ceiling because the stored grants
    /// are exactly what was validated at creation. Do not use this to
    /// construct new delegations.
    ///
    /// Defense in depth for the persistence layer: repositories SHOULD
    /// re-validate the ceiling at load time (each stored grant covered by
    /// the delegator's current grants) and refuse to rehydrate records
    /// that fail it, so a tampered or corrupt row can never mint authority.
    /// Belt and suspenders — authorization additionally checks
    /// [`Delegation::effective_grants`] against current grants, so even a
    /// skipped revalidation cannot confer revoked capabilities.
    #[must_use]
    pub fn rehydrate(
        delegation_id: DelegationId,
        delegator: StaffId,
        agent: AgentPrincipalId,
        grants: Vec<Grant>,
        expires_at: DateTime<Utc>,
        revoked_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            delegation_id,
            delegator,
            agent,
            grants,
            expires_at,
            revoked_at,
        }
    }

    /// Stable identifier of this delegation.
    #[must_use]
    pub fn delegation_id(&self) -> DelegationId {
        self.delegation_id
    }

    /// The staff user who delegated.
    #[must_use]
    pub fn delegator(&self) -> StaffId {
        self.delegator
    }

    /// The agent principal receiving the grants.
    #[must_use]
    pub fn agent(&self) -> AgentPrincipalId {
        self.agent
    }

    /// The delegated grants: a subset of the delegator's grants, fixed at
    /// delegation time. Agents authorize against exactly this set.
    #[must_use]
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// When the delegation expires.
    #[must_use]
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// Whether the delegation reached its expiry.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Whether the delegation was revoked.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// Whether the delegation currently confers authority: the delegator
    /// is still active and the delegation is neither expired nor revoked.
    ///
    /// Pure over (`delegator_status`, `now`) so the rule unit-tests
    /// without I/O.
    #[must_use]
    pub fn is_effective(&self, delegator_status: StaffStatus, now: DateTime<Utc>) -> bool {
        delegator_status == StaffStatus::Active && !self.is_expired(now) && !self.is_revoked()
    }

    /// Returns the delegation's grants that the delegator still holds.
    ///
    /// Grant revocation cascades: a delegation only ever confers the
    /// capabilities the delegator currently holds, so revoking a staff
    /// grant (see `GrantRepository::revoke`) immediately stops delegated
    /// agents from acting on the revoked capability — no separate
    /// delegation cleanup is needed. Authorization must check the agent's
    /// action against this set, never against [`Delegation::grants`] alone.
    #[must_use]
    pub fn effective_grants(&self, current_grants: &[Grant]) -> Vec<Grant> {
        self.grants
            .iter()
            .filter(|grant| {
                current_grants
                    .iter()
                    .any(|held| held.covers(&grant.action, &grant.scope))
            })
            .copied()
            .collect()
    }

    /// Revokes the delegation. Idempotent: the first revocation timestamp
    /// is kept so retried revocations stay safe.
    pub fn revoke(&mut self, now: DateTime<Utc>) {
        if self.revoked_at.is_none() {
            self.revoked_at = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("fixed timestamp")
    }

    fn agency() -> AgencyId {
        AgencyId::from(Uuid::from_u128(1))
    }

    fn other_agency() -> AgencyId {
        AgencyId::from(Uuid::from_u128(9))
    }

    fn profile_a() -> ClientProfileId {
        ClientProfileId::from(Uuid::from_u128(10))
    }

    fn profile_b() -> ClientProfileId {
        ClientProfileId::from(Uuid::from_u128(11))
    }

    fn agency_scope() -> Scope {
        Scope::Agency {
            agency_id: agency(),
        }
    }

    fn profile_scope(profile: ClientProfileId) -> Scope {
        Scope::ClientProfile {
            agency_id: agency(),
            client_profile_id: profile,
        }
    }

    fn action(resource: Resource, operation: Operation) -> Action {
        Action {
            resource,
            operation,
        }
    }

    fn grant(action: Action, scope: Scope) -> Grant {
        Grant {
            grant_id: GrantId::new(),
            action,
            scope,
        }
    }

    fn active_staff() -> StaffUser {
        let now = fixed_now();
        StaffUser {
            staff_id: StaffId::new(),
            agency_id: agency(),
            email: "op@example.com".to_string(),
            display_name: "Op".to_string(),
            status: StaffStatus::Active,
            credential: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn delegate_ok(
        staff: &StaffUser,
        held: &[Grant],
        requested: Vec<RequestedGrant>,
    ) -> Delegation {
        Delegation::delegate(
            staff,
            held,
            AgentPrincipalId::new(),
            requested,
            fixed_now() + Duration::seconds(3600),
            fixed_now(),
        )
        .expect("delegation should succeed")
    }

    // ---- deny by default -------------------------------------------------

    #[test]
    fn deny_by_default_with_no_grants() {
        let scope = profile_scope(profile_a());
        let read = action(Resource::Contact, Operation::Read);
        assert!(!is_authorized(&[], &read, &scope));
    }

    #[test]
    fn grant_covers_exact_action_and_scope() {
        let scope = profile_scope(profile_a());
        let read = action(Resource::Contact, Operation::Read);
        let grants = vec![grant(read, scope)];
        assert!(is_authorized(&grants, &read, &scope));
    }

    #[test]
    fn cross_profile_access_is_denied() {
        let read = action(Resource::Contact, Operation::Read);
        let grants = vec![grant(read, profile_scope(profile_a()))];
        // Same action, different client profile: denied.
        assert!(!is_authorized(&grants, &read, &profile_scope(profile_b())));
    }

    #[test]
    fn cross_agency_access_is_denied() {
        let read = action(Resource::Contact, Operation::Read);
        let grants = vec![grant(read, agency_scope())];
        let foreign = Scope::ClientProfile {
            agency_id: other_agency(),
            client_profile_id: profile_a(),
        };
        assert!(!is_authorized(&grants, &read, &foreign));
        assert!(!is_authorized(
            &grants,
            &read,
            &Scope::Agency {
                agency_id: other_agency()
            }
        ));
    }

    #[test]
    fn agency_scope_covers_client_profiles() {
        let read = action(Resource::Contact, Operation::Read);
        let grants = vec![grant(read, agency_scope())];
        assert!(is_authorized(&grants, &read, &profile_scope(profile_a())));
        assert!(is_authorized(&grants, &read, &profile_scope(profile_b())));
        assert!(is_authorized(&grants, &read, &agency_scope()));
        // But a profile-scoped grant never covers the wider agency scope.
        let narrow = vec![grant(read, profile_scope(profile_a()))];
        assert!(!is_authorized(&narrow, &read, &agency_scope()));
    }

    #[test]
    fn manage_covers_all_operations_on_its_resource() {
        let scope = profile_scope(profile_a());
        let manage = action(Resource::Contact, Operation::Manage);
        let grants = vec![grant(manage, scope)];
        for op in [
            Operation::Read,
            Operation::Create,
            Operation::Update,
            Operation::Delete,
            Operation::Approve,
            Operation::Manage,
        ] {
            assert!(
                is_authorized(&grants, &action(Resource::Contact, op), &scope),
                "manage should cover {op:?}"
            );
        }
        // Manage on one resource does not spill to another.
        assert!(!is_authorized(
            &grants,
            &action(Resource::Contract, Operation::Read),
            &scope
        ));
    }

    #[test]
    fn read_grant_does_not_cover_write() {
        let scope = profile_scope(profile_a());
        let grants = vec![grant(action(Resource::Contact, Operation::Read), scope)];
        assert!(!is_authorized(
            &grants,
            &action(Resource::Contact, Operation::Update),
            &scope
        ));
    }

    // ---- sessions --------------------------------------------------------

    fn session_at(issued_at: DateTime<Utc>) -> Session {
        Session::new(
            SessionId::new(),
            StaffId::new(),
            [7u8; 32],
            issued_at,
            Duration::seconds(DEFAULT_SESSION_TTL_SECS),
        )
        .expect("valid session")
    }

    #[test]
    fn session_new_rejects_non_positive_ttl() {
        let err = Session::new(
            SessionId::new(),
            StaffId::new(),
            [0u8; 32],
            fixed_now(),
            Duration::zero(),
        )
        .expect_err("zero TTL must fail");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn session_new_rejects_overlong_ttl() {
        let err = Session::new(
            SessionId::new(),
            StaffId::new(),
            [0u8; 32],
            fixed_now(),
            Duration::seconds(MAX_SESSION_TTL_SECS + 1),
        )
        .expect_err("overlong TTL must fail");
        assert!(matches!(err, DomainError::Validation(_)));
        // Exactly at the maximum is allowed.
        Session::new(
            SessionId::new(),
            StaffId::new(),
            [0u8; 32],
            fixed_now(),
            Duration::seconds(MAX_SESSION_TTL_SECS),
        )
        .expect("max TTL is allowed");
    }

    #[test]
    fn session_expiry_is_enforced() {
        let issued = fixed_now();
        let session = session_at(issued);
        let idle = Duration::seconds(DEFAULT_IDLE_TIMEOUT_SECS);
        assert!(session.is_valid(issued, idle));
        assert!(!session.is_expired(issued));
        let past_expiry = issued + Duration::seconds(DEFAULT_SESSION_TTL_SECS);
        assert!(session.is_expired(past_expiry));
        assert!(!session.is_valid(past_expiry, idle));
    }

    #[test]
    fn session_revocation_is_idempotent_and_sticky() {
        let issued = fixed_now();
        let mut session = session_at(issued);
        let idle = Duration::seconds(DEFAULT_IDLE_TIMEOUT_SECS);
        assert!(!session.is_revoked());

        let revoked_at = issued + Duration::seconds(60);
        session.revoke(revoked_at);
        assert!(session.is_revoked());
        assert!(!session.is_valid(revoked_at, idle));

        // Second revoke keeps the first timestamp.
        session.revoke(revoked_at + Duration::seconds(60));
        assert_eq!(session.revoked_at, Some(revoked_at));
    }

    #[test]
    fn session_idle_timeout_and_touch() {
        let issued = fixed_now();
        let mut session = session_at(issued);
        let idle = Duration::seconds(60);

        let active = issued + Duration::seconds(30);
        assert!(!session.is_idle(active, idle));
        assert!(session.is_valid(active, idle));

        let idle_at = issued + Duration::seconds(61);
        assert!(session.is_idle(idle_at, idle));
        assert!(!session.is_valid(idle_at, idle));

        // Activity refreshes the marker...
        session.touch(idle_at);
        assert!(!session.is_idle(idle_at, idle));
        // ...but touch never moves the marker backwards (clock skew).
        session.touch(issued);
        assert_eq!(session.last_activity_at, idle_at);
    }

    #[test]
    fn session_token_hash_is_sha256_and_debug_is_redacted() {
        let token = SessionToken::new();
        let mut hasher = Sha256::new();
        hasher.update(token.as_uuid().as_bytes());
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(token.token_hash(), expected);

        // Two tokens never share a hash.
        assert_ne!(SessionToken::new().token_hash(), token.token_hash());

        // The token must never appear in logs.
        let debug = format!("{token:?}");
        assert_eq!(debug, "SessionToken(***)");
        assert!(!debug.contains(&token.as_uuid().to_string()));
    }

    // ---- delegation: the ceiling invariant --------------------------------

    #[test]
    fn delegation_happy_path_delegates_subset() {
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        let update = action(Resource::Contact, Operation::Update);
        let held = vec![
            grant(read, agency_scope()),
            grant(update, profile_scope(profile_a())),
        ];
        let requested = vec![
            RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            },
            RequestedGrant {
                action: update,
                scope: profile_scope(profile_a()),
            },
        ];
        let delegation = delegate_ok(&staff, &held, requested);
        assert_eq!(delegation.grants().len(), 2);
        assert_eq!(delegation.delegator(), staff.staff_id);
        assert!(!delegation.is_revoked());
        assert!(delegation.is_effective(StaffStatus::Active, fixed_now()));
        // The agent authorizes against the delegated subset.
        assert!(is_authorized(
            delegation.grants(),
            &read,
            &profile_scope(profile_a())
        ));
    }

    #[test]
    fn delegation_cannot_exceed_grants_action() {
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        let held = vec![grant(read, agency_scope())];
        // Requesting Update while holding only Read: forbidden.
        let requested = vec![RequestedGrant {
            action: action(Resource::Contact, Operation::Update),
            scope: profile_scope(profile_a()),
        }];
        let err = Delegation::delegate(
            &staff,
            &held,
            AgentPrincipalId::new(),
            requested,
            fixed_now() + Duration::seconds(60),
            fixed_now(),
        )
        .expect_err("must not delegate unheld action");
        assert!(matches!(err, DomainError::Forbidden(_)));
    }

    #[test]
    fn delegation_cannot_exceed_grants_scope() {
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        // Held for profile A only; requesting profile B: forbidden.
        let held = vec![grant(read, profile_scope(profile_a()))];
        let requested = vec![RequestedGrant {
            action: read,
            scope: profile_scope(profile_b()),
        }];
        let err = Delegation::delegate(
            &staff,
            &held,
            AgentPrincipalId::new(),
            requested,
            fixed_now() + Duration::seconds(60),
            fixed_now(),
        )
        .expect_err("must not delegate unwheld scope");
        assert!(matches!(err, DomainError::Forbidden(_)));
    }

    #[test]
    fn delegation_may_narrow_but_never_widen_scope() {
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        // Agency-wide held grant narrows to one profile: allowed.
        let held = vec![grant(read, agency_scope())];
        let delegation = delegate_ok(
            &staff,
            &held,
            vec![RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            }],
        );
        assert!(is_authorized(
            delegation.grants(),
            &read,
            &profile_scope(profile_a())
        ));
        // ...but the narrowed delegation does not cover profile B.
        assert!(!is_authorized(
            delegation.grants(),
            &read,
            &profile_scope(profile_b())
        ));
    }

    #[test]
    fn delegation_manage_held_covers_narrower_request() {
        let staff = active_staff();
        let manage = action(Resource::Contact, Operation::Manage);
        let held = vec![grant(manage, agency_scope())];
        // Manage covers a Read request...
        let delegation = delegate_ok(
            &staff,
            &held,
            vec![RequestedGrant {
                action: action(Resource::Contact, Operation::Read),
                scope: profile_scope(profile_a()),
            }],
        );
        assert_eq!(delegation.grants().len(), 1);
    }

    #[test]
    fn delegation_rejects_empty_request_past_expiry_and_overlong_ttl() {
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        let held = vec![grant(read, agency_scope())];
        let agent = AgentPrincipalId::new();

        let err = Delegation::delegate(
            &staff,
            &held,
            agent,
            vec![],
            fixed_now() + Duration::seconds(60),
            fixed_now(),
        )
        .expect_err("empty request must fail");
        assert!(matches!(err, DomainError::Validation(_)));

        let err = Delegation::delegate(
            &staff,
            &held,
            agent,
            vec![RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            }],
            fixed_now(),
            fixed_now(),
        )
        .expect_err("non-future expiry must fail");
        assert!(matches!(err, DomainError::Validation(_)));

        let err = Delegation::delegate(
            &staff,
            &held,
            agent,
            vec![RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            }],
            fixed_now() + Duration::seconds(MAX_DELEGATION_TTL_SECS + 1),
            fixed_now(),
        )
        .expect_err("overlong TTL must fail");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn delegation_effective_grants_intersects_revoked() {
        // Revoking a staff grant must cascade: the delegation only confers
        // what the delegator still holds.
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        let update = action(Resource::Contact, Operation::Update);
        let held = vec![grant(read, agency_scope()), grant(update, agency_scope())];
        let delegation = delegate_ok(
            &staff,
            &held,
            vec![
                RequestedGrant {
                    action: read,
                    scope: profile_scope(profile_a()),
                },
                RequestedGrant {
                    action: update,
                    scope: profile_scope(profile_a()),
                },
            ],
        );
        // Both conferred while held.
        assert_eq!(delegation.effective_grants(&held).len(), 2);
        // After the update grant is revoked, only read remains effective.
        let after_revoke = vec![grant(read, agency_scope())];
        let effective = delegation.effective_grants(&after_revoke);
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].action, read);
        // Authorizing update against the effective set is denied.
        assert!(!is_authorized(
            &effective,
            &update,
            &profile_scope(profile_a())
        ));
    }

    #[test]
    fn delegation_rejects_cross_agency_held_grant() {
        // A held grant outside the delegator's agency is a caller bug;
        // delegation must fail closed rather than cross agencies.
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        let foreign_scope = Scope::Agency {
            agency_id: AgencyId::new(),
        };
        let held = vec![grant(read, foreign_scope)];
        let err = Delegation::delegate(
            &staff,
            &held,
            AgentPrincipalId::new(),
            vec![RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            }],
            fixed_now() + Duration::seconds(60),
            fixed_now(),
        )
        .expect_err("cross-agency held grant must fail");
        assert!(matches!(err, DomainError::Forbidden(_)));
    }

    #[test]
    fn delegation_expiry_and_revocation_end_effectiveness() {
        let staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        let held = vec![grant(read, agency_scope())];
        let mut delegation = delegate_ok(
            &staff,
            &held,
            vec![RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            }],
        );
        let now = fixed_now();
        assert!(delegation.is_effective(StaffStatus::Active, now));

        // Past expiry: no longer effective.
        let past_expiry = delegation.expires_at() + Duration::seconds(1);
        assert!(delegation.is_expired(past_expiry));
        assert!(!delegation.is_effective(StaffStatus::Active, past_expiry));

        // Revocation ends it early, idempotently.
        delegation.revoke(now);
        assert!(delegation.is_revoked());
        assert!(!delegation.is_effective(StaffStatus::Active, now));
        delegation.revoke(now + Duration::seconds(10));
        assert!(!delegation.is_effective(StaffStatus::Active, now));
    }

    #[test]
    fn suspended_staff_cannot_delegate_and_disables_existing_delegations() {
        let mut staff = active_staff();
        let read = action(Resource::Contact, Operation::Read);
        let held = vec![grant(read, agency_scope())];

        staff.suspend();
        assert!(!staff.is_active());
        let err = Delegation::delegate(
            &staff,
            &held,
            AgentPrincipalId::new(),
            vec![RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            }],
            fixed_now() + Duration::seconds(60),
            fixed_now(),
        )
        .expect_err("suspended staff must not delegate");
        assert!(matches!(err, DomainError::Forbidden(_)));

        // A delegation created while active stops being effective once
        // the delegator is suspended.
        staff.reactivate();
        let delegation = delegate_ok(
            &staff,
            &held,
            vec![RequestedGrant {
                action: read,
                scope: profile_scope(profile_a()),
            }],
        );
        assert!(delegation.is_effective(StaffStatus::Active, fixed_now()));
        assert!(!delegation.is_effective(StaffStatus::Suspended, fixed_now()));
    }

    // ---- staff lifecycle ---------------------------------------------------

    #[test]
    fn staff_suspend_and_reactivate_are_idempotent() {
        let mut staff = active_staff();
        assert!(staff.is_active());
        staff.suspend();
        assert!(!staff.is_active());
        staff.suspend();
        assert!(!staff.is_active());
        staff.reactivate();
        assert!(staff.is_active());
        staff.reactivate();
        assert!(staff.is_active());
    }

    #[test]
    fn password_credential_rejects_non_phc_shape() {
        let now = fixed_now();
        let err = PasswordCredential::new("plaintext-password".to_string(), now)
            .expect_err("plaintext must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
        let err = PasswordCredential::new(String::new(), now).expect_err("empty must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
        // A `$`-prefixed non-PHC string must be rejected by the shape check.
        let err = PasswordCredential::new("$uperSecret!".to_string(), now)
            .expect_err("bare $-prefixed string must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
        let err = PasswordCredential::new("$argon2id$only-two-segments".to_string(), now)
            .expect_err("truncated PHC must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
        let cred =
            PasswordCredential::new("$argon2id$v=19$m=65536,t=3,p=4$salt$hash".to_string(), now)
                .expect("PHC shape accepted");
        assert!(cred.phc_hash().starts_with("$argon2id"));
        // The Debug impl must not leak the hash.
        assert!(!format!("{cred:?}").contains("$argon2id"));
    }

    // ---- roles --------------------------------------------------------------

    #[test]
    fn role_materializes_scoped_grants() {
        let role = Role {
            role_id: RoleId::new(),
            name: "marketer".to_string(),
            permissions: vec![
                action(Resource::Contact, Operation::Read),
                action(Resource::Campaign, Operation::Create),
            ],
        };
        let scope = profile_scope(profile_a());
        let grants = role.grants_for(scope);
        assert_eq!(grants.len(), 2);
        assert!(is_authorized(
            &grants,
            &action(Resource::Contact, Operation::Read),
            &scope
        ));
        assert!(is_authorized(
            &grants,
            &action(Resource::Campaign, Operation::Create),
            &scope
        ));
        // Same role, other profile: disjoint.
        assert!(!is_authorized(
            &grants,
            &action(Resource::Contact, Operation::Read),
            &profile_scope(profile_b())
        ));
    }
}
