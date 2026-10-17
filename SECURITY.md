# Security policy — rust-crm

## Reporting vulnerabilities

Do not open a public issue for a suspected vulnerability. Report it privately
to the maintainer (see the repo's contact on GitHub: `Gane2506/rust-crm`)
with a description, reproduction steps, and impact. Expect an initial
response within 5 business days; fixes ship as patch releases with a
coordinated disclosure note.

## Secret handling

- Never commit secrets: API keys, tokens, passwords, private keys, or
  connection strings. `.env` is gitignored; CI uses repository secrets.
- Configure the server exclusively via environment variables (e.g.
  `CRM_API_ADDR`, `DATABASE_URL`). The process environment always wins over
  any file.
- Database credentials must be per-environment, least-privilege roles — never
  a superuser in production.
- If a secret is ever committed, treat it as compromised: revoke it
  immediately, purge it from history, and note the rotation in the PR.

## Dependency and supply-chain hygiene

- `cargo audit` and `cargo deny check` run on every CI build; advisories are
  never ignored without a reviewed ADR.
- `deny.toml` pins allowed licenses (MIT/Apache-2.0/BSD-2/3-Clause/ISC/
  MPL-2.0/Unicode-DFS-2016) and denies wildcard dependency requirements.
- No first-party `.js`/`.ts`/`.py` under `crates/` (CI scan); `unsafe` Rust
  is reported in CI and requires documented safety invariants.

## Operational notes

- The API binds `127.0.0.1:3000` by default; expose it publicly only behind
  TLS termination and authentication (tasks 03-SEC-0x).
- Structured JSON logs carry `x-request-id` correlation ids; never log
  secrets, tokens, or PII beyond what the audit schema allows.
