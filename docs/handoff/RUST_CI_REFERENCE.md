# Rust CI reference gates

## PR gates
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features` (ephemeral Postgres integration suite)
- `cargo build --workspace --release`
- `cargo build -p crm-web --target wasm32-unknown-unknown --release` (after target/toolchain installed)
- `cargo audit` and `cargo deny check` with pinned configuration
- `cargo metadata` dependency/license inspection; grep/tree-based forbidden first-party source audit for .js/.ts/.py; generated outputs excluded
- Static review of `unsafe` use; fuzz/property tests for parsing, permissions, workflows and signature evidence
- SQL migration forward/backward compatibility rehearsal; contract tests against provider fixtures

## Async correctness
- Tokio cancellation, timeout, bounded channels, semaphore fairness, backpressure, blocking-in-async checks, orderly shutdown, and no lost events under restart.
- Loom for small concurrency primitives where applicable; integration stress for real Tokio tasks.

## Performance gates
Run dedicated reproducible hardware or pinned container limits (shared hosted CI is not a reliable performance baseline). Profiles: 1 vCPU/2 GiB; 2 vCPU/2 GiB; 2 vCPU/4 GiB; optional 8 GiB. Load stages warm-up, sustained, burst, overload, recovery. Record completed requests/sec, p50/p95/p99, errors, CPU throttling, RSS, DB memory, queue length and OOM events. Gate on agreed baselines and no crash; do not require 100% RAM usage.

## Release gates
Signed artifacts, SBOM, migration/restore rehearsal, secret scan, TLS and authorization penetration tests, load test evidence, manual approval for security exceptions.
