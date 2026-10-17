# Contributing to rust-crm

## Workflow

1. Pick a task ID from `docs/handoff/TASKS.md` whose dependencies are done
   (see `docs/handoff/TASK_LEDGER.md`).
2. Branch from `main`: `task/<ID>` — e.g. `git checkout -b task/01-FND-01`.
3. Implement **only** that task. One task per PR.
4. Open a PR against `main` with the completion report (see below).

## PR checklist

- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
- [ ] `cargo test --workspace --all-features` green (unit + integration)
- [ ] `cargo build --workspace --release` green
- [ ] WASM target builds: `cargo build -p crm-web --target wasm32-unknown-unknown --release`
- [ ] `cargo audit` and `cargo deny check` clean (or ADR for exceptions)
- [ ] No new first-party `.js`/`.ts`/`.py` under `crates/`
- [ ] rustdoc on all new public items; doctests where behavior is shown
- [ ] `docs/handoff/TASK_LEDGER.md` updated for the task
- [ ] Completion report in the PR body: task ID, requirement IDs, files
      changed, test results, security impact, resource impact (1/2-core,
      2/4 GiB), migration notes, known limitations, follow-on task IDs

## License

rust-crm is open source from its core under the GNU General Public License
v3.0 (GPL-3.0-only — see `LICENSE`). By contributing, you agree that your
contributions are accepted under the same GPL-3.0-only license. New source
files carry the header `// SPDX-License-Identifier: GPL-3.0-only`.

## Conventions

- Rust 2021 edition, `max_width = 100` (`rustfmt.toml`).
- Error handling via `thiserror`; no `unwrap()` in non-test code; document
  every `unsafe` block with its safety invariants.
- Keep PRs small and coherent; preserve backwards compatibility.
