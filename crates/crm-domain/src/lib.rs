// SPDX-License-Identifier: GPL-3.0-only
//! # crm-domain
//!
//! Domain model and interfaces for the Rust CRM.
//!
//! This crate defines the entities, value objects, state machines and
//! service/repository interfaces that the rest of the system is built on.
//! It has no I/O, no HTTP and no SQL: persistence lives in the repository
//! implementations (task 02-DB), transport lives in the API crate.
//!
//! ## Cross-cutting invariants
//!
//! * **Stable identifiers.** Every entity carries a typed UUID newtype
//!   ([`ids`]). IDs are opaque, serializable and never reused.
//! * **Explicit state machines.** Lifecycle transitions ([`Contact`],
//!   [`Contract`]) are validated by the entity itself; illegal transitions
//!   return [`DomainError::Validation`] instead of silently succeeding.
//! * **Tamper-evident audit.** Every mutation produces an [`AuditEvent`]
//!   chained by SHA-256 over the previous head and the event's canonical
//!   bytes (see [`audit`]). The log is append-only; denied actions are
//!   recorded too.
//! * **Deny by default.** Services authorize every operation against an
//!   explicit [`PermissionDecision`](audit::PermissionDecision); absence of
//!   a grant is a denial, never an implicit allow.
//! * **No secrets in errors.** [`DomainError`] messages never carry tokens,
//!   credentials or PII.

#![warn(missing_docs)]
#![deny(unsafe_code)]

/// Agent action records: delegated authority, authorization and outcome.
pub mod agent;
/// Tamper-evident audit events and the hash-chain helpers.
pub mod audit;
/// Campaign aggregate: owner context, channel, schedule and delivery state.
pub mod campaign;
/// Contact aggregate: normalized identity, lifecycle and consent records.
pub mod contact;
/// Contract aggregate: signing state machine and the payment gate.
pub mod contract;
/// Domain error type with explicit, secret-free error semantics.
pub mod error;
/// Typed UUID newtypes used as stable entity identifiers.
pub mod ids;
/// Repository traits (persistence ports; SQLx implementations land in 02-DB).
pub mod repositories;
/// Service traits (domain operations with authorization and audit).
pub mod services;

pub use agent::AgentAction;
pub use audit::AuditEvent;
pub use campaign::Campaign;
pub use contact::Contact;
pub use contract::Contract;
pub use error::DomainError;
