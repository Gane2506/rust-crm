// SPDX-License-Identifier: GPL-3.0-only
//! Authentication and authorization services over the SQLx repositories.
//!
//! [`PgAuthenticationService`] implements
//! [`AuthenticationService`](crm_domain::services::AuthenticationService):
//! Argon2id password verification, session issuance, sliding refresh and
//! revocation. [`PgAuthorizationService`] implements
//! [`AuthorizationService`](crm_domain::services::AuthorizationService):
//! the server-side enforcement point every request passes through.
//!
//! ## Security notes
//!
//! * Passwords are hashed with Argon2id (`argon2` crate, default params);
//!   verification is constant-time inside the crate (via `subtle`) —
//!   never hand-rolled. Both run on the blocking pool because hashing is
//!   CPU-intensive and must not stall the async runtime.
//! * Plaintext passwords never enter the domain: only the PHC string
//!   crosses into [`PasswordCredential::new`](crm_domain::security::PasswordCredential::new).
//! * Unknown emails (and credential-less accounts) still pay for one
//!   Argon2 verification against a dummy hash, so a timing probe cannot
//!   distinguish "no such account" from "wrong password".
//! * Tokens are never logged and never appear in error messages; only
//!   [`SessionToken::token_hash`] is persisted or compared.
//! * Delegation-linked sessions fail closed: there is no delegation
//!   repository trait yet, so an agent session cannot be validated and
//!   authorizes nothing. The `delegations` table is forward schema for the
//!   agent-delegation workstream.

use std::sync::OnceLock;

use argon2::password_hash;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{Duration, Utc};
use sqlx::PgPool;

use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, SessionId};
use crm_domain::repositories::{GrantRepository, SessionRepository, StaffRepository};
use crm_domain::security::{
    is_authorized, Action, IssuedSession, Scope, Session, SessionToken, StaffUser,
    DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_SESSION_TTL_SECS, MAX_SESSION_TTL_SECS,
};
use crm_domain::services::{ActionContext, AuthenticationService, AuthorizationService};

use crate::postgres::{PgGrantRepository, PgSessionRepository, PgStaffRepository};

/// Hashes `password` with Argon2id, returning the PHC-format string
/// (`$argon2id$v=19$...`) suitable for
/// [`PasswordCredential::new`](crm_domain::security::PasswordCredential::new).
///
/// Runs on the blocking pool: Argon2 is deliberately CPU- and
/// memory-hard and must not stall the async runtime.
pub async fn hash_password(password: &[u8]) -> Result<String, DomainError> {
    let password = password.to_vec();
    tokio::task::spawn_blocking(move || hash_password_blocking(&password))
        .await
        .map_err(|_| DomainError::Validation("password hashing was cancelled".to_owned()))?
}

/// Verifies `password` against the stored PHC `phc_hash`.
///
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch. The comparison
/// is constant-time inside `argon2` (via the `subtle` crate); a malformed
/// stored hash is a storage error, never a silent accept. Runs on the
/// blocking pool.
pub async fn verify_password(password: &[u8], phc_hash: &str) -> Result<bool, DomainError> {
    let password = password.to_vec();
    let phc_hash = phc_hash.to_owned();
    tokio::task::spawn_blocking(move || verify_password_blocking(&password, &phc_hash))
        .await
        .map_err(|_| DomainError::Validation("password verification was cancelled".to_owned()))?
}

/// Synchronous Argon2id hash; see [`hash_password`].
fn hash_password_blocking(password: &[u8]) -> Result<String, DomainError> {
    use argon2::password_hash::rand_core::OsRng;
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password, &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| DomainError::Validation(format!("password hashing failed: {error}")))
}

/// Synchronous Argon2id verify; see [`verify_password`].
fn verify_password_blocking(password: &[u8], phc_hash: &str) -> Result<bool, DomainError> {
    let parsed = PasswordHash::new(phc_hash).map_err(|error| {
        DomainError::Validation(format!("stored password hash is malformed: {error}"))
    })?;
    match Argon2::default().verify_password(password, &parsed) {
        Ok(()) => Ok(true),
        Err(password_hash::Error::Password) => Ok(false),
        Err(error) => Err(DomainError::Validation(format!(
            "password verification failed: {error}"
        ))),
    }
}

/// A valid Argon2id hash used only to equalize timing when the account
/// does not exist (or has no credential): the rejection still pays for
/// one full verification, so a timing probe cannot distinguish "no such
/// account" from "wrong password".
fn dummy_phc_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password_blocking(b"dummy-password-for-unknown-accounts")
            .expect("dummy password hash must generate")
    })
}

/// Verifies `password` against the dummy hash, discarding the result.
///
/// Used on the unknown-account path so its timing matches a real
/// verification. Any internal error is swallowed: the caller is already
/// returning [`DomainError::Unauthorized`].
async fn verify_against_dummy(password: &[u8]) {
    let _ = verify_password(password, dummy_phc_hash()).await;
}

/// Normalizes a login name the way the service stores it: trimmed and
/// lowercased, so `Op@Example.com` and `op@example.com` are one account.
fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// A validated session with its staff account: the result of resolving a
/// presented bearer token.
///
/// The middleware uses this to gate requests; the authorization service
/// re-resolves per call through the trait so every check is fresh.
#[derive(Debug, Clone)]
pub struct ValidatedSession {
    /// The live session record.
    pub session: Session,
    /// The staff account the session belongs to (active).
    pub staff: StaffUser,
}

/// SQLx implementation of
/// [`AuthenticationService`](crm_domain::services::AuthenticationService).
#[derive(Debug, Clone)]
pub struct PgAuthenticationService {
    staff: PgStaffRepository,
    sessions: PgSessionRepository,
}

impl PgAuthenticationService {
    /// Creates the service over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self {
            staff: PgStaffRepository::new(pool.clone()),
            sessions: PgSessionRepository::new(pool),
        }
    }

    /// Resolves a presented bearer token to its live session and active
    /// staff account.
    ///
    /// Returns [`DomainError::Unauthorized`] when the token is unknown,
    /// expired, revoked or idle-timed-out, or its staff account is not
    /// active. Used by the API auth middleware; the token itself is never
    /// logged.
    pub async fn validate_token(
        &self,
        token: &SessionToken,
    ) -> Result<ValidatedSession, DomainError> {
        let now = Utc::now();
        let idle_timeout = Duration::seconds(DEFAULT_IDLE_TIMEOUT_SECS);
        let session = self
            .sessions
            .get_by_token_hash(&token.token_hash())
            .await?
            .ok_or(DomainError::Unauthorized)?;
        if !session.is_valid(now, idle_timeout) {
            return Err(DomainError::Unauthorized);
        }
        let staff = self.staff.get(session.staff_id).await.map_err(|error| {
            // The session references a deleted account: fail closed as
            // unauthorized, not as a storage 404.
            match error {
                DomainError::NotFound { .. } => DomainError::Unauthorized,
                other => other,
            }
        })?;
        if !staff.is_active() {
            return Err(DomainError::Unauthorized);
        }
        Ok(ValidatedSession { session, staff })
    }
}

impl AuthenticationService for PgAuthenticationService {
    async fn authenticate(
        &self,
        username: &str,
        password: &[u8],
    ) -> Result<IssuedSession, DomainError> {
        let email = normalize_email(username);
        let user = self.staff.find_by_email(&email).await?;
        let Some(user) = user else {
            // Timing-equalized rejection: never reveal whether the account
            // exists.
            verify_against_dummy(password).await;
            return Err(DomainError::Unauthorized);
        };
        // Verify the password before checking status or credential presence:
        // a suspended or credential-less account must cost the same time
        // and return the same error as a wrong password, so the response
        // reveals nothing about the account's existence or state.
        let password_ok = match &user.credential {
            Some(credential) => verify_password(password, credential.phc_hash()).await?,
            None => {
                verify_against_dummy(password).await;
                false
            }
        };
        if !password_ok || !user.is_active() {
            return Err(DomainError::Unauthorized);
        }
        let token = SessionToken::new();
        let now = Utc::now();
        let session = Session::new(
            SessionId::new(),
            user.staff_id,
            token.token_hash(),
            now,
            Duration::seconds(DEFAULT_SESSION_TTL_SECS),
        )?;
        let session = self.sessions.create(session).await?;
        Ok(IssuedSession { token, session })
    }

    async fn refresh(&self, token: &SessionToken) -> Result<Session, DomainError> {
        let validated = self.validate_token(token).await?;
        let mut session = validated.session;
        let now = Utc::now();
        // Sliding refresh extends the expiry, never shortens it, and never
        // past `issued_at + MAX_SESSION_TTL_SECS`: sessions are
        // human-interactive credentials, not long-lived tokens.
        let cap = session.issued_at + Duration::seconds(MAX_SESSION_TTL_SECS);
        let candidate = now + Duration::seconds(DEFAULT_SESSION_TTL_SECS);
        session.expires_at = session.expires_at.max(candidate.min(cap));
        session.touch(now);
        self.sessions.update(session).await
    }

    async fn revoke(&self, token: &SessionToken) -> Result<(), DomainError> {
        // Idempotent: an unknown token succeeds silently — logout must
        // never leak whether a token existed.
        let Some(session) = self.sessions.get_by_token_hash(&token.token_hash()).await? else {
            return Ok(());
        };
        let mut session = session;
        session.revoke(Utc::now());
        self.sessions.update(session).await?;
        Ok(())
    }
}

/// SQLx implementation of
/// [`AuthorizationService`](crm_domain::services::AuthorizationService):
/// the server-side enforcement point.
#[derive(Debug, Clone)]
pub struct PgAuthorizationService {
    authentication: PgAuthenticationService,
    sessions: PgSessionRepository,
    grants: PgGrantRepository,
}

impl PgAuthorizationService {
    /// Creates the service over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self {
            authentication: PgAuthenticationService::new(pool.clone()),
            sessions: PgSessionRepository::new(pool.clone()),
            grants: PgGrantRepository::new(pool),
        }
    }
}

impl AuthorizationService for PgAuthorizationService {
    async fn authorize(
        &self,
        token: &SessionToken,
        action: &Action,
        scope: &Scope,
        correlation_id: uuid::Uuid,
    ) -> Result<ActionContext, DomainError> {
        let validated = self.authentication.validate_token(token).await?;
        let session = validated.session;
        let staff: StaffUser = validated.staff;

        // Delegation-linked sessions fail closed: there is no delegation
        // repository trait yet, so an agent session's grants cannot be
        // resolved — and the tightened contract forbids authorizing it
        // against anything else. The `delegations` table is forward schema
        // for the agent-delegation workstream.
        if session.delegation_id.is_some() {
            return Err(DomainError::Unauthorized);
        }

        // Record activity for the idle timeout. A failed touch fails the
        // request: silently skipping it would let sessions live forever.
        let mut session = session;
        session.touch(Utc::now());
        self.sessions.update(session).await?;

        let grants = self.grants.list_for_staff(staff.staff_id).await?;
        if !is_authorized(&grants, action, scope) {
            return Err(DomainError::Forbidden(format!(
                "no grant covers {action:?} in {scope:?}"
            )));
        }

        Ok(ActionContext {
            actor_kind: crm_domain::audit::ActorKind::Human,
            actor_id: ActorId::from(staff.staff_id),
            client_profile_id: match scope {
                Scope::ClientProfile {
                    client_profile_id, ..
                } => Some(*client_profile_id),
                Scope::Agency { .. } => None,
            },
            correlation_id,
        })
    }
}
