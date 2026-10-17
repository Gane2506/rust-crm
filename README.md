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

## Development guide

### Prerequisites

- **rustup** with the pinned toolchain. `rust-toolchain.toml` pins Rust
  **1.99.1** (components `rustfmt`, `clippy`; targets include
  `wasm32-unknown-unknown`), so plain `cargo`/`rustc` in this directory use
  the right toolchain automatically once rustup is installed:
  ```sh
  rustup toolchain install 1.99.1
  rustup target add wasm32-unknown-unknown
  ```
- **wasm-bindgen-cli 0.2.129** — the CLI version must match the `wasm-bindgen`
  crate version in `Cargo.lock` (currently 0.2.129):
  ```sh
  cargo install wasm-bindgen-cli --version 0.2.129
  ```
- **cargo-audit** and **cargo-deny** for the supply-chain gates:
  ```sh
  cargo install cargo-audit cargo-deny
  ```

### Build

```sh
# Fast checks (run these before every commit)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features

# Optimized server binary
cargo build --workspace --release

# WASM frontend: compile, bind for the web, stage into web/
cargo build -p crm-web --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir web target/wasm32-unknown-unknown/release/crm-web.wasm
cp crates/crm-web/index.html web/
```

### Run

```sh
# Copy .env.example to .env to customize (optional; defaults work out of the box)
cargo run -p crm-api
```

The app is then available at:

- <http://127.0.0.1:3000/> — the Leptos/WASM frontend (served from
  `CRM_WEB_DIR`, default `./web`)
- <http://127.0.0.1:3000/api/v1/*> — the JSON API

Try the API (the UUID below is the fixed demo tenant used by the vertical
slice until real auth/tenancy lands):

```sh
curl localhost:3000/healthz
# {"status":"ok"}

curl localhost:3000/api/v1/version
# {"version":"0.1.0"}

# Create a contact (Idempotency-Key makes retries safe)
curl -X POST localhost:3000/api/v1/contacts \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: 11111111-1111-4111-8111-111111111111' \
  -d '{"client_profile_id":"123e4567-e89b-42d3-a456-426614174000","emails":["a@example.com"],"phones":["+1-555-0100"],"source":"web-form"}'

# List contacts for the demo tenant
curl "localhost:3000/api/v1/contacts?client_profile_id=123e4567-e89b-42d3-a456-426614174000&limit=20"

# Fetch one contact by id (copy contact_id from the create response)
curl localhost:3000/api/v1/contacts/<contact_id>
```

Errors use RFC 7807 `application/problem+json` (`422` validation,
`404` not found, `409` conflict).

### Environment variables

Copy `.env.example` to `.env` to override the defaults (`.env*` is
gitignored; no secrets are used by this project yet).

| Name           | Default                          | Purpose                                              |
|----------------|----------------------------------|------------------------------------------------------|
| `CRM_API_ADDR` | `127.0.0.1:3000`                 | Socket address the API server binds to               |
| `CRM_WEB_DIR`  | `./web`                          | Directory the built frontend is served from          |
| `RUST_LOG`     | `crm_api=info,tower_http=info`   | `tracing` log filter                                 |

### Verification

CI runs these gates on every PR (see `.github/workflows/ci.yml` and
[`docs/handoff/RUST_CI_REFERENCE.md`](docs/handoff/RUST_CI_REFERENCE.md));
run them locally before pushing:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features        # CI provides Postgres via DATABASE_URL
cargo build --workspace --release
cargo build -p crm-web --target wasm32-unknown-unknown --release
cargo audit
cargo deny check
```

The forbidden-source scan (no `.js`/`.ts`/`.py` under `crates/`) and the
informational `unsafe` usage report run in CI only.

### Task workflow

Work proceeds one task ID per PR, branched from `main` as `task/<ID>`
(e.g. `git checkout -b task/01-FND-03`). The 51-task manifest lives in
[`docs/handoff/TASKS.md`](docs/handoff/TASKS.md); progress is tracked in
[`docs/handoff/TASK_LEDGER.md`](docs/handoff/TASK_LEDGER.md) — update the
ledger row for your task in the PR. Every PR body carries the completion
report: task ID, requirement IDs, files changed, test results, security
impact, resource impact, migration notes, known limitations, follow-on task
IDs. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full checklist.

### Troubleshooting

- **Port already in use** (`address already in use` on startup): another
  `crm-api` instance is running, or something occupies the port. Point the
  server elsewhere: `CRM_API_ADDR=127.0.0.1:3001 cargo run -p crm-api`.
- **wasm-bindgen version mismatch** (`wasm-bindgen` refuses with a version
  error naming two different versions): the installed `wasm-bindgen-cli`
  must equal the `wasm-bindgen` crate version in `Cargo.lock` (0.2.129).
  Reinstall with `cargo install wasm-bindgen-cli --version 0.2.129`.
- **Frontend not served / API-only mode warning**: the `web/` directory is
  missing or empty — the API still runs, but `/` has no frontend to serve.
  Re-run the WASM build steps above to populate it.
- **`rustup`/toolchain not installed** (`cargo: command not found` or
  toolchain errors): install rustup from <https://rustup.rs>, then
  `rustup toolchain install 1.99.1` (rustup also auto-installs the pinned
  toolchain on the first `cargo` run in this directory).

## Build progress

Work proceeds one task per PR under the 51-task manifest in
[`docs/handoff/TASKS.md`](docs/handoff/TASKS.md), following
**issue → draft PR → PR → CI → merge**: each task gets a tracking issue,
work happens on a `task/<ID>` branch with a draft PR open from the start,
and merge happens on green CI plus review. Follow along via
[issues](https://github.com/Gane2506/rust-crm/issues) and
[pull requests](https://github.com/Gane2506/rust-crm/pulls)
(progress is also recorded in `docs/handoff/TASK_LEDGER.md`).

| Workstream | Tasks | Status |
|---|---|---|
| Workspace and toolchain | 01-FND-01…03 | ✅ Done ([#1](https://github.com/Gane2506/rust-crm/pull/1), [#2](https://github.com/Gane2506/rust-crm/pull/2), [#3](https://github.com/Gane2506/rust-crm/pull/3), [#4](https://github.com/Gane2506/rust-crm/pull/4)) |
| Persistence foundation | 02-DB-01…03 | ✅ 02-DB-01 done ([issue #5](https://github.com/Gane2506/rust-crm/issues/5), [PR #6](https://github.com/Gane2506/rust-crm/pull/6)); 🔨 02-DB-02 in progress |
| Identity and RBAC | 03-SEC-01…03 | ⬜ Not started |
| Audit and observability | 04-AUD-01…03 | ⬜ Not started |
| Core CRM | 05-CRM-01…03 | ⬜ Not started |
| Design system | 06-UI-01…03 | ⬜ Not started |
| Email and threaded chat | 07-MAIL-01…03 | ⬜ Not started |
| Campaigns | 08-CAMP-01…03 | ⬜ Not started |
| Jev agents | 09-JEV-01…03 | ⬜ Not started |
| Workflow builder | 10-WORK-01…03 | ⬜ Not started |
| Form builder | 11-FORM-01…03 | ⬜ Not started |
| Billing | 12-BILL-01…03 | ⬜ Not started |
| Contract signing | 13-SIGN-01…03 | ⬜ Not started |
| Calling | 14-VOICE-01…03 | ⬜ Not started |
| Analytics and staff | 15-DASH-01…03 | ⬜ Not started |
| Admin and operations | 16-ADMIN-01…03 | ⬜ Not started |
| Hardening and release | 17-PERF-01…03 | ⬜ Not started |

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
