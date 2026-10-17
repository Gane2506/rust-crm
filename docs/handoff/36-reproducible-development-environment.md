# 36 — Reproducible Development Environment

**Status:** Implementation-ready specification baseline; choices marked *decision required* must be confirmed before implementation.

## Objective and scope
Local and CI parity.

## Required behavior
Pinned toolchain, lockfiles, Rust-based fixtures, deterministic seeds, ephemeral Postgres, reproducible benchmarks.

## Detailed implementation contract
1. Define the domain entities, their lifecycle states, invariants and authorized actors before implementing UI.
2. Expose typed Rust domain services independent of HTTP handlers; use explicit transaction boundaries and SQLx queries.
3. Expose versioned APIs/events with stable IDs, pagination, validation, explicit error codes and idempotent commands.
4. Implement an accessible, responsive Rust/WASM interface with loading, empty, error, pending-approval, success and conflict states.
5. Capture user and agent activity in the unified audit timeline with searchable actor/client/action/time filters.
6. Validate client-profile scope on all reads and writes, including background jobs and search results.
7. Emit telemetry for success, latency, queue depth, failures, resource usage and denied actions.
8. Document external service credentials, limits, webhook authentication and failure/retry paths.

## User experience and edge cases
- Minimize steps: inline edits, clear contextual actions, searchable lists, keyboard navigation and a reversible confirmation for destructive operations.
- Show source and last-touch attribution, the owning client profile, and whether an action was performed by a human or agent wherever applicable.
- Handle duplicate events, concurrent edits, expired tokens, deleted or suspended clients, API outages, rate limits, missing attachments and interrupted sessions.
- For high-impact operations, preview consequences and request an explicit authorized approval; surface actionable errors and recovery steps.

## Acceptance tests
- **AT-36-01:** Authorized happy-path user completes the primary workflow and data persists correctly.
- **AT-36-02:** A role without the needed grant is denied at the API/service layer, including an agent with insufficient delegation.
- **AT-36-03:** Duplicate submissions or webhooks do not create duplicate side effects.
- **AT-36-04:** Failed external operations remain recoverable and auditable.
- **AT-36-05:** All user-visible states remain keyboard accessible and meet the project's latency budget.

## Dependencies and implementation notes
Use the task manifest and requirements registry for sequence; no document overrides the Rust-only, least-privilege, client-profile isolation and audit invariants.

## Normative delivery rules
- **Language**: All first-party executable application logic, server and browser UI, workers, CLI, tests and benchmark harnesses must be authored in Rust. Browser output necessarily includes generated HTML/CSS/WASM; declarative styles and SQL migrations are allowed artifacts, not a second application language. Third-party PostgreSQL and provider services are external systems, not rewritten in Rust. Avoid custom JavaScript.
- **Boundary**: Agency staff operate the application. A client profile is an account/workspace for work performed on behalf of that business. Clients receive outbound messages and reports with expiring, scoped read-only links, not an interactive client account. Public onboarding/signing/payment pages may be accessed via secure tokens without a client dashboard.
- **Security**: Authorize every request, event handler, workflow node and agent tool call on the server. Never infer authorization from hidden UI controls. Encrypt sensitive material, minimize PII, retain tamper-evident audit records and prohibit secrets in logs.
- **Reliability**: Any external side effect requires idempotency keys, retries with jitter, bounded queues, timeout, dead-letter path and reconciliation where applicable. Human approval is a durable paused state, not a transient prompt.
- **Provenance**: Every human and agent mutation carries actor_type, actor_id, client_profile_id, correlation_id, request_id, timestamp, action, before/after reference and result; logs must be append-only with tamper detection. Notes preserve separate human/agent authorship.
- **Quality**: Merge requires Rust-only checks, unit/integration tests, authorization and negative tests, accessibility, operational instrumentation and reproducible acceptance evidence. Never claim that reaching 100% RAM is a pass condition: pass means sustained useful load without OOM or unacceptable tail latency.

