# rust-crm

Rust-only, agent-native CRM — **open source from its core under the GNU
General Public License v3.0 (GPL-3.0-only)**: an agency operating system
covering client profiles, contacts, campaigns, communications, calling,
contracts, payments and reports, orchestrated by Jev agents.

Product direction is Twenty-inspired usability (not copied assets), with all
application logic and UI authored in Rust: an Axum/Tokio/SQLx/PostgreSQL
backend and a Leptos/WASM frontend. The full product spec is the 38-document
AI handoff under [`docs/handoff/`](docs/handoff/) (start with `README.md`,
`TASKS.md` and `RUST_CI_REFERENCE.md` there).

## Architecture map

```text
crates/
  crm-api/      Axum HTTP server: /healthz, /api/v1/*, RFC 7807 errors,
                x-request-id correlation, graceful shutdown
  crm-web/      Leptos 0.8 CSR app (wasm32): dashboard + contacts shells
  crm-core/     Pure shared utilities: idempotency keys, pagination
  crm-db/       SQLx persistence layer (repositories land in 02-DB-01)
  crm-domain/   Domain model and interfaces (owned by its task agent)
```

## Quickstart

```sh
# 1. Install the pinned toolchain (see rust-toolchain.toml)
rustup toolchain install  # then: rustup default <pinned>

# 2. Check formatting, lints, tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features

# 3. Run the API (binds 127.0.0.1:3000; override with CRM_API_ADDR)
cargo run -p crm-api
curl localhost:3000/healthz        # {"status":"ok"}
curl localhost:3000/api/v1/version # {"version":"0.1.0"}

# 4. Build the WASM frontend
rustup target add wasm32-unknown-unknown
cargo build -p crm-web --target wasm32-unknown-unknown --release
```

PostgreSQL is a separately installed database engine (not Rust software);
integration tests use an ephemeral instance via `DATABASE_URL`.

## Task manifest

Work proceeds one task ID per PR under the 51-task manifest in
[`docs/handoff/TASKS.md`](docs/handoff/TASKS.md) (progress in
`docs/handoff/TASK_LEDGER.md`). Current task: **01-FND-01** — workspace,
pinned toolchain, fmt/clippy, WASM build, CI.

## CI gates (summary)

`cargo fmt --check` · `cargo clippy --workspace --all-targets --all-features
-D warnings` · `cargo test --workspace --all-features` (ephemeral Postgres) ·
`cargo build --workspace --release` · `cargo build -p crm-web --target
wasm32-unknown-unknown --release` · `cargo audit` · `cargo deny check` ·
forbidden first-party source scan (no `.js`/`.ts`/`.py` under `crates/`) ·
informational `unsafe` usage report. Full reference:
[`docs/handoff/RUST_CI_REFERENCE.md`](docs/handoff/RUST_CI_REFERENCE.md).

## License

GPL-3.0-only — see [LICENSE](LICENSE).
