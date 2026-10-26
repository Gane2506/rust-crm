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

## Screenshots

Real renders of the running app (Leptos/WASM frontend in the Bento UI, seeded demo data).
A per-feature gallery lives below; the original set is kept at
[`docs/images/screenshots/`](docs/images/screenshots/).

### UI gallery

![Login](docs/images/ui2/login.png)
*Login — staff sign in; there is no client login by design.*

![Dashboard](docs/images/ui2/dashboard.png)
*Bento dashboard — live stat tiles (contacts, profiles, audit chain health) and recent audit events.*

![Dashboard (dark mode)](docs/images/ui2/dashboard-dark.png)
*Dashboard in dark mode — the theme toggle restyles every view via CSS custom properties.*

![Contacts](docs/images/ui2/contacts.png)
*Contacts — new-contact form and the seeded contact list.*

![Contacts (dark mode)](docs/images/ui2/contacts-dark.png)
*Contacts in dark mode.*

![Contact detail](docs/images/ui2/contact-detail.png)
*Contact detail — lifecycle, sources, and merge history.*

![Client profiles](docs/images/ui2/profiles.png)
*Client profiles — agency-operated client workspaces.*

![Assets](docs/images/ui2/assets.png)
*Assets — content-addressed files scoped to a client profile.*

![Audit log](docs/images/ui2/audit.png)
*Audit log — tamper-evident security event history with chain verification.*

![Branding studio](docs/images/ui2/branding.png)
*Branding studio — businesses customize logo, colours, radius and font with a live preview.*

### Previous UI (Twenty-style)

![Login gate](docs/images/screenshots/login.png)
*Login gate — staff sign in; there is no client login by design.*

![Dashboard shell](docs/images/screenshots/dashboard.png)
*Dashboard shell — the analytics workstream is still being built, so this
shows the current placeholder.*

![Contacts](docs/images/screenshots/contacts.png)
*Contacts — new-contact form and the seeded contact list.*

![Contact detail](docs/images/screenshots/contact-detail.png)
*Contact detail — lifecycle, sources, and merge history.*

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
# Fast checks (run these before every commit).
# Tests need PostgreSQL: `docker compose up -d` first, with DATABASE_URL set
# (see .env.example). The crm-db integration tests fail loudly without it;
# the crm-api Postgres tests skip.
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

The API server needs PostgreSQL 16. Start the ephemeral database (same
digest-pinned image CI uses), then run:

```sh
docker compose up -d
# Copy .env.example to .env to customize (optional; the DATABASE_URL default
# points at the compose database)
cargo run -p crm-api
```

`crm-api` reads `DATABASE_URL` at startup, applies pending migrations, and
exits non-zero with a logged error when the database is missing or
unreachable — there is no non-durable fallback.

The app is then available at:

- <http://127.0.0.1:3000/> — the Leptos/WASM frontend (served from
  `CRM_WEB_DIR`, default `./web`)
- <http://127.0.0.1:3000/api/v1/*> — the JSON API

Try the API (the UUID below is the fixed demo tenant used by the vertical
slice until real auth/tenancy lands):

```sh
curl localhost:3000/healthz
# {"status":"ok"}

curl localhost:3000/readyz
# {"status":"ok","database":"ok"} — gate orchestrators on /readyz (503 when
# the database is unreachable); /healthz is process liveness only

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

### Seed data (development)

Populate the database with the demo tenant and sample contacts:

```sh
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

Idempotent (re-runs skip existing contacts); without `CRM_SEED_DEV=1` it
prints the plan and writes nothing. Full database runbook (migrations,
rollback, backup/restore, pool guidance, `/readyz`): see
[`docs/operations/database.md`](docs/operations/database.md).

### Environment variables

Copy `.env.example` to `.env` to override the defaults (`.env*` is
gitignored; no secrets are used by this project yet).

| Name           | Default                          | Purpose                                              |
|----------------|----------------------------------|------------------------------------------------------|
| `CRM_API_ADDR` | `127.0.0.1:3000`                 | Socket address the API server binds to               |
| `CRM_WEB_DIR`  | `./web`                          | Directory the built frontend is served from          |
| `RUST_LOG`     | `crm_api=info,tower_http=info`   | `tracing` log filter                                 |
| `DATABASE_URL` | (none — required)                | Postgres connection URL; see `.env.example`          |

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
| Persistence foundation | 02-DB-01…03 | ✅ Done ([#5](https://github.com/Gane2506/rust-crm/issues/5)/[#6](https://github.com/Gane2506/rust-crm/pull/6), [#7](https://github.com/Gane2506/rust-crm/issues/7)/[#8](https://github.com/Gane2506/rust-crm/pull/8), [#9](https://github.com/Gane2506/rust-crm/issues/9)/[#10](https://github.com/Gane2506/rust-crm/pull/10)) |
| Identity and RBAC | 03-SEC-01…03 | ✅ done ([#11](https://github.com/Gane2506/rust-crm/issues/11), [#12](https://github.com/Gane2506/rust-crm/pull/12); [#13](https://github.com/Gane2506/rust-crm/issues/13), [#14](https://github.com/Gane2506/rust-crm/pull/14); [#15](https://github.com/Gane2506/rust-crm/issues/15), [#16](https://github.com/Gane2506/rust-crm/pull/16)) |
| Audit and observability | 04-AUD-01…03 | ✅ done ([#21](https://github.com/Gane2506/rust-crm/issues/21), [#22](https://github.com/Gane2506/rust-crm/pull/22)) |
| Core CRM | 05-CRM-01…03 | ✅ done ([#27](https://github.com/Gane2506/rust-crm/issues/27), [#28](https://github.com/Gane2506/rust-crm/pull/28)) |
| Frontend | 06-UI-01…03 | ✅ done ([#33](https://github.com/Gane2506/rust-crm/issues/33), [#34](https://github.com/Gane2506/rust-crm/pull/34)) |
| Email and threaded chat | 07-MAIL-01… | ✅ workstream complete ([#39](https://github.com/Gane2506/rust-crm/issues/39), [#40](https://github.com/Gane2506/rust-crm/pull/40)) |
| Campaigns and bulk outreach | 08-CAMP-01… | ✅ workstream complete ([#45](https://github.com/Gane2506/rust-crm/issues/45), [#46](https://github.com/Gane2506/rust-crm/pull/46)) |
| Jev agents | 09-JEV-01… | ✅ workstream complete ([#51](https://github.com/Gane2506/rust-crm/issues/51), [#52](https://github.com/Gane2506/rust-crm/pull/52)) |
| Workflow builder | 10-WORK-01… | ✅ 10-WORK-03 done ([#57](https://github.com/Gane2506/rust-crm/issues/57), [#58](https://github.com/Gane2506/rust-crm/pull/58)); **workstream COMPLETE** |
| Form builder | 11-FORM-01… | ✅ 11-FORM-03 done ([#63](https://github.com/Gane2506/rust-crm/issues/63), [#64](https://github.com/Gane2506/rust-crm/pull/64)); **workstream COMPLETE** |
| Feed | 12-FEED-01… | ✅ 12-FEED-03 done ([#69](https://github.com/Gane2506/rust-crm/issues/69), [#70](https://github.com/Gane2506/rust-crm/pull/70)); **workstream COMPLETE** |
| Billing | 12-BILL-01… | ✅ 12-BILL-03 done ([#76](https://github.com/Gane2506/rust-crm/issues/76), [#77](https://github.com/Gane2506/rust-crm/pull/77)); **workstream COMPLETE** |
| Contract signing | 13-SIGN-01… | ✅ 13-SIGN-03 done ([#82](https://github.com/Gane2506/rust-crm/issues/82), [#83](https://github.com/Gane2506/rust-crm/pull/83)); **workstream COMPLETE** |
| Calling | 14-VOICE-01… | ✅ 14-VOICE-03 done ([#88](https://github.com/Gane2506/rust-crm/issues/88), [#89](https://github.com/Gane2506/rust-crm/pull/89)); **workstream COMPLETE** |
| Analytics and staff | 15-DASH-01… | ✅ 15-DASH-03 done ([#99](https://github.com/Gane2506/rust-crm/issues/99), [#100](https://github.com/Gane2506/rust-crm/pull/100)); **workstream COMPLETE** |
| Admin and operations | 16-ADMIN-01…03 | ✅ 16-ADMIN-03 done ([#105](https://github.com/Gane2506/rust-crm/issues/105), [#106](https://github.com/Gane2506/rust-crm/pull/106)); **workstream COMPLETE** |
| Hardening and release | 17-PERF-01…03 | ✅ 17-PERF-03 done ([#111](https://github.com/Gane2506/rust-crm/issues/111), [#112](https://github.com/Gane2506/rust-crm/pull/112)); **workstream COMPLETE** |
| UI 2.0 (Bento + branding) | UI2-01…04 | ✅ UI2-01 done ([#113](https://github.com/Gane2506/rust-crm/issues/113), [#114](https://github.com/Gane2506/rust-crm/pull/114)); ✅ UI2-02 done ([#115](https://github.com/Gane2506/rust-crm/issues/115), [#116](https://github.com/Gane2506/rust-crm/pull/116)); ✅ UI2-03 done ([#117](https://github.com/Gane2506/rust-crm/issues/117), [#118](https://github.com/Gane2506/rust-crm/pull/118)); ✅ UI2-04 done ([#119](https://github.com/Gane2506/rust-crm/issues/119), [#120](https://github.com/Gane2506/rust-crm/pull/120)); **workstream COMPLETE** |
| UI 3.0 (16-style pack) | UI3-01 | ✅ UI3-01 done ([#121](https://github.com/Gane2506/rust-crm/issues/121), [#122](https://github.com/Gane2506/rust-crm/pull/122)); **workstream COMPLETE** |
| FIX hardening | FIX-01…06 | ✅ FIX-01 done ([#123](https://github.com/Gane2506/rust-crm/issues/123), [#129](https://github.com/Gane2506/rust-crm/pull/129)); ✅ FIX-02 done ([#124](https://github.com/Gane2506/rust-crm/issues/124), [#130](https://github.com/Gane2506/rust-crm/pull/130)); ✅ FIX-03 done ([#125](https://github.com/Gane2506/rust-crm/issues/125), [#131](https://github.com/Gane2506/rust-crm/pull/131)); ✅ FIX-04 done ([#126](https://github.com/Gane2506/rust-crm/issues/126), [#132](https://github.com/Gane2506/rust-crm/pull/132)); ✅ FIX-05 done ([#127](https://github.com/Gane2506/rust-crm/issues/127), [#133](https://github.com/Gane2506/rust-crm/pull/133)); ✅ FIX-06 done ([#128](https://github.com/Gane2506/rust-crm/issues/128), [#134](https://github.com/Gane2506/rust-crm/pull/134)); **program COMPLETE** |
| Session persistence | SESSION-01 | ✅ SESSION-01 done ([#135](https://github.com/Gane2506/rust-crm/issues/135), [#136](https://github.com/Gane2506/rust-crm/pull/136)) |
| HARD hardening | HARD-01…06 | ✅ HARD-01 done ([#137](https://github.com/Gane2506/rust-crm/issues/137), [#138](https://github.com/Gane2506/rust-crm/pull/138)); ✅ HARD-02 done ([#139](https://github.com/Gane2506/rust-crm/issues/139), [#140](https://github.com/Gane2506/rust-crm/pull/140)); ✅ HARD-03 done ([#141](https://github.com/Gane2506/rust-crm/issues/141), [#142](https://github.com/Gane2506/rust-crm/pull/142)); ✅ HARD-04 done ([#143](https://github.com/Gane2506/rust-crm/issues/143), [#144](https://github.com/Gane2506/rust-crm/pull/144)); ✅ HARD-05 done ([#145](https://github.com/Gane2506/rust-crm/issues/145), [#146](https://github.com/Gane2506/rust-crm/pull/146)); ✅ HARD-06 done ([#147](https://github.com/Gane2506/rust-crm/issues/147), [#148](https://github.com/Gane2506/rust-crm/pull/148)); **program COMPLETE** (519 tests) |
| Live-defect audit | AUDIT-01…13, R3-01…04 | ✅ AUDIT-01 done ([#149](https://github.com/Gane2506/rust-crm/issues/149), [#152](https://github.com/Gane2506/rust-crm/pull/152)); ✅ AUDIT-02 done ([#150](https://github.com/Gane2506/rust-crm/issues/150), [#154](https://github.com/Gane2506/rust-crm/pull/154)); ✅ AUDIT-03 done ([#151](https://github.com/Gane2506/rust-crm/issues/151), [#153](https://github.com/Gane2506/rust-crm/pull/153)); ✅ AUDIT-04 done ([#155](https://github.com/Gane2506/rust-crm/issues/155), [#156](https://github.com/Gane2506/rust-crm/pull/156)); ✅ AUDIT-05 done ([#157](https://github.com/Gane2506/rust-crm/issues/157), [#158](https://github.com/Gane2506/rust-crm/pull/158)); ✅ AUDIT-06 done ([#159](https://github.com/Gane2506/rust-crm/issues/159), [#160](https://github.com/Gane2506/rust-crm/pull/160)); ✅ AUDIT-07 done ([#161](https://github.com/Gane2506/rust-crm/issues/161), [#167](https://github.com/Gane2506/rust-crm/pull/167)); ✅ AUDIT-08 done ([#162](https://github.com/Gane2506/rust-crm/issues/162), [#166](https://github.com/Gane2506/rust-crm/pull/166)); ✅ AUDIT-09 done ([#163](https://github.com/Gane2506/rust-crm/issues/163), [#165](https://github.com/Gane2506/rust-crm/pull/165)); ✅ AUDIT-10 done ([#164](https://github.com/Gane2506/rust-crm/issues/164), [#168](https://github.com/Gane2506/rust-crm/pull/168)); ✅ AUDIT-11 done ([#169](https://github.com/Gane2506/rust-crm/issues/169), [#172](https://github.com/Gane2506/rust-crm/pull/172)); ✅ AUDIT-12 done ([#170](https://github.com/Gane2506/rust-crm/issues/170), [#173](https://github.com/Gane2506/rust-crm/pull/173)); ✅ AUDIT-13 done ([#171](https://github.com/Gane2506/rust-crm/issues/171), [#174](https://github.com/Gane2506/rust-crm/pull/174)); ✅ R3-03 done ([#177](https://github.com/Gane2506/rust-crm/issues/177), [#178](https://github.com/Gane2506/rust-crm/pull/178)); ✅ R3-01 done ([#175](https://github.com/Gane2506/rust-crm/issues/175), [#179](https://github.com/Gane2506/rust-crm/pull/179)); ✅ R3-02 done ([#176](https://github.com/Gane2506/rust-crm/issues/176), [#180](https://github.com/Gane2506/rust-crm/pull/180)); ✅ R3-04 done ([#181](https://github.com/Gane2506/rust-crm/issues/181), [#183](https://github.com/Gane2506/rust-crm/pull/183)); ✅ SESSION-02 done ([#182](https://github.com/Gane2506/rust-crm/issues/182), [#188](https://github.com/Gane2506/rust-crm/pull/188)); ✅ MAIL-01 done ([#185](https://github.com/Gane2506/rust-crm/issues/185), [#189](https://github.com/Gane2506/rust-crm/pull/189)); ✅ MAIL-02 done ([#190](https://github.com/Gane2506/rust-crm/issues/190), [#191](https://github.com/Gane2506/rust-crm/pull/191)); 🔄 BILL-01 in progress ([#184](https://github.com/Gane2506/rust-crm/issues/184)) — round-2 majors + round-3 audit complete; SESSION-02 + mail UI + orphan-thread fix done, billing UI in progress |

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
