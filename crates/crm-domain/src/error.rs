// SPDX-License-Identifier: GPL-3.0-only
//! Domain error type with explicit, actionable error semantics.
//!
//! These variants are the contract's error surface: callers match on them
//! (and on their [`Display`](std::fmt::Display) strings) rather than on
//! free-form messages. **No variant carries secrets** — no tokens,
//! credentials, API keys or raw PII. Identifiers (UUIDs, resource kinds)
//! are safe to include because they are opaque.

use thiserror::Error;

/// Errors raised by domain entities and domain services.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    /// A value or state transition failed validation (e.g. an illegal
    /// lifecycle transition or an empty required field).
    #[error("validation failed: {0}")]
    Validation(String),

    /// A resource could not be found. `kind` names the entity type
    /// (e.g. `"contact"`); `id` is its opaque identifier.
    #[error("not found: {kind} '{id}'")]
    NotFound {
        /// Entity type that was looked up.
        kind: String,
        /// Opaque identifier that was not found.
        id: String,
    },

    /// The operation conflicts with existing state (e.g. a duplicate
    /// idempotency key with a different payload).
    #[error("conflict: {0}")]
    Conflict(String),

    /// The caller could not be authenticated.
    #[error("unauthorized")]
    Unauthorized,

    /// The authenticated caller lacks the grant for this action.
    /// Deny by default: absence of a grant surfaces here.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// The record changed since it was read (optimistic-concurrency or
    /// audit-chain head mismatch). The caller should re-read and retry.
    #[error("concurrency conflict: the record changed since it was read; retry the operation")]
    ConcurrencyConflict,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_strings_are_stable() {
        assert_eq!(
            DomainError::Validation("email is empty".to_string()).to_string(),
            "validation failed: email is empty"
        );
        assert_eq!(
            DomainError::NotFound {
                kind: "contact".to_string(),
                id: "123".to_string(),
            }
            .to_string(),
            "not found: contact '123'"
        );
        assert_eq!(
            DomainError::Conflict("duplicate key".to_string()).to_string(),
            "conflict: duplicate key"
        );
        assert_eq!(DomainError::Unauthorized.to_string(), "unauthorized");
        assert_eq!(
            DomainError::Forbidden("missing grant contacts:write".to_string()).to_string(),
            "forbidden: missing grant contacts:write"
        );
        assert_eq!(
            DomainError::ConcurrencyConflict.to_string(),
            "concurrency conflict: the record changed since it was read; retry the operation"
        );
    }
}
