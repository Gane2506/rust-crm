// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! `crm-core`: shared pure utilities for the Rust CRM.
//!
//! This crate is intentionally dependency-light and side-effect free: no I/O,
//! no networking, no database access. It hosts cross-cutting primitives every
//! other crate can rely on, currently:
//!
//! - [`IdempotencyKey`]: a UUIDv4-backed key identifying a mutating request so
//!   retries never double-apply a side effect (handoff rule: all mutations
//!   require idempotency).
//! - [`Pagination`]: validated 1-based pagination parameters shared by list
//!   endpoints.
//! - [`memory`]: in-memory implementations of the `crm-domain` traits —
//!   vertical-slice stand-ins replaced by SQLx in task 02-DB.
//!
//! # Example
//!
//! ```
//! use crm_core::{IdempotencyKey, Pagination};
//!
//! let key = IdempotencyKey::generate();
//! let parsed: IdempotencyKey = key.to_string().parse().expect("generated key parses");
//! assert_eq!(key, parsed);
//!
//! let page = Pagination::new(2, 25).expect("valid pagination");
//! assert_eq!(page.offset(), 25);
//! ```

use std::{fmt, str::FromStr};

use thiserror::Error;
use uuid::Uuid;

/// In-memory vertical-slice implementations of the `crm-domain`
/// repository and service traits, plus the backend-agnostic
/// [`memory::ContactServiceImpl`].
///
/// **Stand-ins for the 01-FND-02 vertical slice** — the *stores* are
/// replaced by SQLx/PostgreSQL in task 02-DB; the *service* composes over
/// any backend. See the module docs for the exact limitations.
pub mod memory;

/// Email service implementation (task 07-MAIL-02): the generic
/// [`email::EmailServiceImpl`] plus the [`email::StubEmailProvider`]
/// test/dev adapter.
pub mod email;

/// Campaign service implementation (task 08-CAMP-02): the generic
/// [`campaign::CampaignServiceImpl`] over the campaign repositories,
/// contact repository, email service and audit service.
pub mod campaign;

/// Jev agent service implementation (task 09-JEV-02): the generic
/// [`jev::AgentServiceImpl`] over the agent-run, tool-call and handoff
/// repositories, role membership and audit service, plus the
/// [`jev::StubModelAdapter`] test/dev adapter.
pub mod jev;

/// Workflow builder service: lifecycle, validation and the pure
/// simulator over the [`workflow`](crm_domain::workflow) domain.
pub mod workflow;

/// Form builder service: lifecycle, field management and
/// validate-then-persist submission intake over the
/// [`form`](crm_domain::form) domain.
pub mod form;

/// Feed service: profile-scoped activity timeline reads and item
/// annotations (pin, hide, mark read) over the
/// [`feed`](crm_domain::feed) domain.
pub mod feed;

/// Errors raised by [`crm-core`](crate) utilities.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    /// A string could not be parsed as an idempotency key.
    #[error("invalid idempotency key '{value}': {reason}")]
    InvalidIdempotencyKey {
        /// The rejected input.
        value: String,
        /// Why it was rejected.
        reason: String,
    },
    /// Pagination parameters failed validation.
    #[error("invalid pagination: {0}")]
    InvalidPagination(String),
}

/// Idempotency key for mutating requests.
///
/// Backed by a UUIDv4. Clients generate one per mutating call and resend it on
/// retry; the server uses it to recognise replays. See the handoff rule that
/// all mutations require idempotency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(Uuid);

impl IdempotencyKey {
    /// Generate a fresh random (UUIDv4) idempotency key.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Borrow the underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for IdempotencyKey {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<Uuid>()
            .map(Self)
            .map_err(|err| CoreError::InvalidIdempotencyKey {
                value: s.to_owned(),
                reason: err.to_string(),
            })
    }
}

/// Validated 1-based pagination parameters for list endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pagination {
    page: u32,
    per_page: u32,
}

impl Pagination {
    /// Maximum items per page.
    pub const MAX_PER_PAGE: u32 = 100;
    /// Items per page when the caller does not specify.
    pub const DEFAULT_PER_PAGE: u32 = 25;

    /// Build validated pagination: `page` is 1-based, `per_page` must be
    /// within `1..=MAX_PER_PAGE`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidPagination`] when `page` is 0 or `per_page`
    /// is outside the allowed range.
    pub fn new(page: u32, per_page: u32) -> Result<Self, CoreError> {
        if page == 0 {
            return Err(CoreError::InvalidPagination("page must be >= 1".to_owned()));
        }
        if per_page == 0 || per_page > Self::MAX_PER_PAGE {
            return Err(CoreError::InvalidPagination(format!(
                "per_page must be within 1..={}",
                Self::MAX_PER_PAGE
            )));
        }
        Ok(Self { page, per_page })
    }

    /// 1-based page number.
    #[must_use]
    pub const fn page(&self) -> u32 {
        self.page
    }

    /// Items per page.
    #[must_use]
    pub const fn per_page(&self) -> u32 {
        self.per_page
    }

    /// SQL `OFFSET` for the current page.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        (self.page - 1) as u64 * self.per_page as u64
    }
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: Self::DEFAULT_PER_PAGE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generate_produces_unique_keys() {
        let keys: HashSet<IdempotencyKey> = (0..256).map(|_| IdempotencyKey::generate()).collect();
        assert_eq!(keys.len(), 256);
    }

    #[test]
    fn idempotency_key_display_roundtrip() {
        let key = IdempotencyKey::generate();
        let parsed: IdempotencyKey = key.to_string().parse().expect("valid key");
        assert_eq!(key, parsed);
        assert_eq!(key.as_uuid(), parsed.as_uuid());
    }

    #[test]
    fn idempotency_key_rejects_garbage() {
        let err = "not-a-uuid"
            .parse::<IdempotencyKey>()
            .expect_err("must fail");
        assert!(matches!(err, CoreError::InvalidIdempotencyKey { .. }));
        assert!(err.to_string().contains("not-a-uuid"));
    }

    #[test]
    fn pagination_validates_bounds() {
        assert!(Pagination::new(0, 25).is_err());
        assert!(Pagination::new(1, 0).is_err());
        assert!(Pagination::new(1, Pagination::MAX_PER_PAGE + 1).is_err());
        assert!(Pagination::new(1, 1).is_ok());
        assert!(Pagination::new(1, Pagination::MAX_PER_PAGE).is_ok());
    }

    #[test]
    fn pagination_offset_math() {
        let page = Pagination::new(3, 10).expect("valid");
        assert_eq!(page.offset(), 20);
        assert_eq!(page.page(), 3);
        assert_eq!(page.per_page(), 10);
    }

    #[test]
    fn pagination_default_is_first_page() {
        let page = Pagination::default();
        assert_eq!(page.page(), 1);
        assert_eq!(page.per_page(), Pagination::DEFAULT_PER_PAGE);
        assert_eq!(page.offset(), 0);
    }
}
