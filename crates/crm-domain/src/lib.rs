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
//! * **Identity and least privilege.** Staff sessions, RBAC grants and
//!   bounded agent delegation ([`security`]): tokens are never stored
//!   (only their hash), and a delegation can never exceed its delegator's
//!   grants — enforced by construction.
//! * **No secrets in errors.** [`DomainError`] messages never carry tokens,
//!   credentials or PII.

#![warn(missing_docs)]
#![deny(unsafe_code)]

/// Agent action records: delegated authority, authorization and outcome.
pub mod agent;
/// Asset aggregate: file/media attachments owned by a client profile.
pub mod asset;
/// Tamper-evident audit events and the hash-chain helpers.
pub mod audit;
/// Billing domain: invoices and payment records for agency clients
/// (task 12-BILL-01).
pub mod billing;
/// Campaign aggregate: owner context, channel, schedule, delivery state,
/// steps, prospect pools, opt-outs, attribution and conversions.
pub mod campaign;
/// Contact aggregate: normalized identity, lifecycle and consent records.
pub mod contact;
/// Contract aggregate: signing state machine and the payment gate.
pub mod contract;
/// Email aggregate: messages, threads and attachments per client profile.
pub mod email;
/// Domain error type with explicit, secret-free error semantics.
pub mod error;
/// Feed domain: per-profile activity stream, visibility rules and
/// ordering (task 12-FEED-01).
pub mod feed;
/// Form builder domain: typed intake/lead forms, field validation and
/// submissions (task 11-FORM-01).
pub mod form;
/// Typed UUID newtypes used as stable entity identifiers.
pub mod ids;
/// Jev agent domain: runs, model adapters, tools, approval gates and
/// handoffs (task 09-JEV-01).
pub mod jev;
/// Domain-level observability hooks: tracing, health and anomaly signals.
pub mod observability;
/// Client profile aggregate: agency-managed workspace lifecycle.
pub mod profile;
/// Repository traits (persistence ports; SQLx implementations land in 02-DB).
pub mod repositories;
/// Staff identity, sessions, RBAC grants and bounded agent delegation.
pub mod security;
/// Service traits (domain operations with authorization and audit).
pub mod services;
/// Workflow builder domain: typed visual workflows, triggers,
/// versioning and the pure simulator (task 10-WORK-01).
pub mod workflow;

pub use agent::AgentAction;
pub use asset::{Asset, AssetKind, AssetStatus};
pub use audit::{AuditEvent, SecurityEvent};
pub use billing::{
    Invoice, InvoiceLine, InvoiceStatus, Money, Payment, PaymentStatus, BILLING_INVOICE_CREATED,
    BILLING_INVOICE_ISSUED, BILLING_INVOICE_VOIDED, BILLING_LINE_ADDED, BILLING_PAYMENT_RECORDED,
    BILLING_PAYMENT_REVERSED,
};
pub use campaign::{
    attribute_conversion, AttributionModel, Campaign, CampaignStep, Channel, Conversion,
    DeliveryState, OptOut, OptOutChannel, OwnerContext, ProspectPool, StepKind, Touchpoint,
};
pub use contact::{Contact, ContactSource, DedupeSignal, Lifecycle};
pub use contract::Contract;
pub use email::{
    EmailAttachment, EmailDirection, EmailMessage, EmailStatus, EmailThread, InboundThreadHints,
    ThreadStatus,
};
pub use error::DomainError;
pub use feed::{
    compare_feed_items, FeedItem, FeedQuery, FeedSource, FeedVisibility, FEED_ITEM_HIDDEN,
    FEED_ITEM_PINNED, FEED_ITEM_READ, FEED_LISTED,
};
pub use form::{
    Form, FormAnswer, FormField, FormFieldType, FormStatus, FormSubmission, SubmissionStatus,
};
pub use jev::{
    AgentRun, AgentRunStatus, AgentTool, ApprovalDecision, ApprovalGate, FallbackChain, Handoff,
    ModelAdapter, ModelCompletion, ToolCall, ToolCallRequest, ToolCallStatus,
};
pub use profile::{ClientProfile, ProfileStatus};
pub use security::{
    Action, Delegation, Grant, Operation, Resource, Role, Scope, Session, SessionToken, StaffUser,
};
pub use workflow::{
    SimulationStep, SimulationStepStatus, SimulationTrace, Simulator, Trigger, TriggerType,
    Workflow, WorkflowEdge, WorkflowNode, WorkflowNodeType, WorkflowStatus, WorkflowVersion,
};
