# AGENTS.md — rust-crm implementation contract

Read `README.md`, `docs/handoff/11-requirements-registry.md`,
`docs/handoff/23-task-manifest.md`, `docs/handoff/25-rust-engineering-and-ci.md`
and the relevant feature spec before editing. The spec also travels in
`docs/handoff/` (copied from the original handoff drop).

## Hard rules

- **One task ID per PR/branch.** Branch from `main` as `task/<ID>`
  (e.g. `task/01-FND-01`). Never bundle unrelated tasks.
- **Rust only** for first-party application code, UI, tests and utilities. Do
  not handwrite JavaScript/TypeScript/Python — the CI forbidden-source scan
  fails the build on any `.js`/`.ts`/`.py` under `crates/`.
- Do not suppress compiler, clippy, or security findings without a reviewed
  ADR. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  must stay green.
- All mutations require scoped authorization, audit events, and idempotency
  (`crm-core::IdempotencyKey`).
- Never let Jev or delegated agents exceed explicit role grants.
- Each crate owns its directory: do not edit another task's crate without
  coordinating (e.g. `crates/crm-domain` is owned by its task agent).
- Never deviate from the 51-task plan in `docs/handoff/TASKS.md`; if stuck,
  ask — do not improvise scope.

## Rust discipline (rust-engineer skill)

- No `unwrap()` in non-test code (prefer `expect()` with a message); no
  undocumented `unsafe`; `thiserror`-style error types; rustdoc on all public
  items; doctests for public APIs.
- `cargo fmt` clean, `cargo clippy` clean, tests for every change.

## Required completion report (per PR)

Task ID; requirement IDs; files changed; tests executed with results;
security impact; resource impact (1/2-core, 2/4 GiB); migration notes; known
limitations; follow-on task IDs. Update `docs/handoff/TASK_LEDGER.md`.
