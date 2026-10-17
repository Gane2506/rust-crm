# AGENTS.md — Rust CRM implementation contract

Read README.md, 11 Requirements Registry, 23 Task Manifest, 25 Rust Engineering and CI, and the relevant feature spec before editing. Implement one task ID per PR. Rust only for first-party application, UI, tests and utilities. Do not handwrite JavaScript/TypeScript/Python. Do not suppress compiler or security findings without a reviewed ADR. All mutations require scoped authorization, audit events and idempotency. Never let Jev or delegated agents exceed explicit role grants. Each PR includes tests, resource impact, rollback notes, and task ledger update. Prefer small coherent changes and preserve backwards compatibility.

## Required completion report
Task ID; requirement IDs; files changed; tests executed with results; security impact; resource impact (1/2-core, 2/4 GiB); migration notes; known limitations; follow-on task IDs.
