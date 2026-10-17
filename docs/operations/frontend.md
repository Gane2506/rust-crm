# Frontend operations runbook

How to build, serve, and troubleshoot the Leptos single-page app in
`crates/crm-web`. For the auth model the UI relies on, see
`security.md`; for the audit viewer, `audit-log.md`.

## What it is

`crm-web` is a client-side-rendered Leptos 0.8 single-page application
compiled to `wasm32-unknown-unknown`. There is no handwritten
JavaScript, TypeScript, or Python anywhere in the frontend — the
"100% Rust" rule is enforced by CI's forbidden-source scan. The only
non-Rust file is `crates/crm-web/index.html`, a 12-line logic-free
bootstrap document the browser requires to load the WASM module (kept
by explicit user decision; see the HTML-shell note in `MEMORY.md`).

## Building

```bash
# 1. Compile the WASM module (pinned toolchain, see rust-toolchain.toml)
cargo build -p crm-web --target wasm32-unknown-unknown --release

# 2. Generate the JS glue + WASM package with wasm-bindgen.
#    The CLI version MUST match the lockfile (0.2.129); a mismatch
#    fails at load time with a version error in the browser console.
wasm-bindgen --target web --out-dir <web-dir> \
    target/wasm32-unknown-unknown/release/crm-web.wasm

# 3. Copy index.html into <web-dir> alongside the wasm-bindgen output.
cp crates/crm-web/index.html <web-dir>/index.html
```

CI builds the wasm32 target on every PR (the `build crm-web (wasm32)`
job); the full `wasm-bindgen` packaging happens at deploy time.

## Serving

`crm-api` serves the frontend itself — there is no separate static
server. Set `CRM_WEB_DIR` to the directory holding `index.html` plus
the wasm-bindgen output:

```bash
CRM_WEB_DIR=/opt/rust-crm/web ./target/release/crm-api
```

- `GET /` (and any non-API path) serves `index.html`; the Leptos
  router takes over client-side (SPA fallback via tower-http
  `ServeDir` + `ServeFile`).
- API routes live under `/api/v1/*`; `/healthz` and `/readyz` are
  unauthenticated.
- If `CRM_WEB_DIR` is unset, the API still runs but `/` returns 404 —
  the API-only mode warning in the README.

## The index.html contract

`index.html` is a host document, not application code. It must:

1. Reference the wasm-bindgen JS glue (`crm-web.js`) generated in the
   build step above.
2. Call the WASM `init` (per the wasm-bindgen `--target web` output).
3. Contain no application logic — all UI behavior lives in Rust.

If the bootstrap drifts from what `wasm-bindgen 0.2.129 --target web`
emits (e.g. after a dependency upgrade), the page loads blank; see
troubleshooting.

## Authentication in the UI

- The bearer token lives in a Leptos signal — **memory only**. It is
  never written to `localStorage`, `sessionStorage`, or cookies, so a
  stolen disk image or XSS-persisted storage can't replay it later.
  Closing the tab ends the session client-side (the server session
  still expires on its own TTL).
- Every API call sends `Authorization: Bearer <token>`. A `401` from
  any call means the session expired, was revoked, or the user was
  suspended — the UI drops the token and returns to the login form.
  There is no silent re-login and no refresh endpoint (see
  `threat-model.md`).
- Logout calls `POST /api/v1/auth/logout` (server-side revocation,
  idempotent `204`) and then clears the local token either way.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Blank page, no UI | WASM failed to load | Open the browser console: a wasm-bindgen version mismatch or a 404 on `crm-web.js`/`crm-web_bg.wasm` shows here. Re-run the build steps; confirm the CLI is 0.2.129 and `<web-dir>` holds `index.html` + glue + WASM. |
| Blank page after deploy | `CRM_WEB_DIR` wrong or unset | The API logs the web dir at startup. Point `CRM_WEB_DIR` at the directory from the build step. |
| Login succeeds, then immediate logout loop | Clock skew or revoked session | The token is valid but the server rejects it (`401`). Check server logs for auth failures; re-login. If it persists, the staff row may be suspended — see `security.md`. |
| `403` on a page that used to work | Grant revoked | Grants are the source of truth; the UI has no role cache. Have an operator check the `grants` table. |
| Audit page shows "CHAIN BROKEN" | Tamper or bug | Treat as a security incident — see `audit-log.md` incident response. Do not dismiss it from the UI. |
| Form submits do nothing | JavaScript disabled or WASM blocked | The app requires WASM; there is no server-rendered fallback. Check content-security / extension blocking. |

## Accessibility notes

- All form inputs have `<label for>` associations; the audit filter
  checkbox uses an implicit (wrapping) label.
- Status changes (create success, errors, chain verification) use
  `role="status"` / `role="alert"` live regions.
- The login email field has `autofocus` on first load. Focus is not
  programmatically moved on client-side route changes (no JS focus
  scripting) — screen-reader users land on the new `<h2>` via the
  route render; this is a known limitation, not a bug.
- Color contrast: muted text `#6b7280` on white is 4.8:1; error red
  `#b91c1c` is 5.9:1; both pass WCAG AA for normal text.

## Known limitations

- **No server-side rendering.** First paint waits for WASM download +
  init; there is no SEO-friendly static HTML.
- **No offline support.** The app is a thin client over the API.
- **Audit newest-first is display-only.** The API returns chain order
  (oldest first) with no `order` parameter; the toggle reorders the
  fetched page. True newest-first querying needs a backend `order`
  parameter (future backend task).
- **Asset pagination.** List endpoints cap at 50; the UI has no
  continuation affordance yet.
