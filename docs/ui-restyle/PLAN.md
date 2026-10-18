# UI Restyle — Twenty-style UI in Rust (Leptos)

Tracking issue: #95. User-directed workstream (2026-10-18), outside the 51-task
manifest. The 06-UI workstream delivered functional scaffolding; this rebuilds
the application UI in Twenty CRM's design language, 100% in Rust.

## Design language (reimplemented, not copied)

Twenty's product UI is a Notion-like, light-theme CRM:

- Left navigation rail (~220px): workspace mark at top, icon + label nav items,
  sections for objects (Contacts, Companies→Profiles, Opportunities→Campaigns),
  settings at the bottom.
- Top bar: breadcrumb / object title, view switcher, filter + sort buttons,
  search field, command-menu (⌘K) trigger, user avatar.
- Data tables: compact 13px rows, column headers with field-type icons,
  row hover states, inline status pills, checkbox selection column.
- Record pages: large title at top, two-column body — fields/settings left,
  timeline/activity right.
- Foundations: Inter typeface, blue accent (#2563eb family), page background
  near-white (#f8fafc-ish), white surfaces, 1px #e5e7eb borders, 8px radii,
  generous whitespace.

## Implementation rules

- Rust-only: Leptos 0.8 components, styling via Rust style constants
  (the existing `styles.rs` pattern, greatly expanded). No handwritten
  JavaScript/TypeScript, no CSS framework, no copied Twenty assets.
- Keep every existing route, API call, form, and error/loading/empty state
  working. This is a reskin, not a rewrite of behavior.
- GPL-3.0-only headers on all new Rust files.

## Pages in scope

login, dashboard, contacts, contact detail, contact merge, profiles,
profile detail, assets, audit, plus any workstream page with a route
(campaigns, emails, forms, workflows, billing, signing, calls).

## Verification

- `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features
  -- -D warnings`, `cargo test --workspace --all-features`,
  `cargo build --workspace --release`,
  `cargo build -p crm-web --target wasm32-unknown-unknown --release`,
  `cargo audit`, `cargo deny check`.
- Headless-Chrome screenshots (1440×900) of every page against seeded demo
  data, reviewed before merge — the technique used for
  `docs/images/screenshots/`.
- After merge: refresh README screenshots (separate follow-on, per the user:
  screenshots go up AFTER the UI is built).
