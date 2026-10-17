# Sequential Task Manifest — 51 independently verifiable tasks

Each task is a separate PR/agent assignment. The dependency column is a strict prerequisite, but an engineering lead may parallelize later tasks after reviewing shared data/API boundaries.

| ID | Dependency | Workstream | Task | Deliverable |
|---|---|---|---|---|
| 01-FND-01 | none | Workspace and toolchain | Define domain and interfaces | Rust workspace, pinned compiler, fmt/clippy, WASM build, CI |
| 01-FND-02 | 01-FND-01 | Workspace and toolchain | Implement vertical slice | Rust workspace, pinned compiler, fmt/clippy, WASM build, CI |
| 01-FND-03 | 01-FND-02 | Workspace and toolchain | Verify and operationalize | Rust workspace, pinned compiler, fmt/clippy, WASM build, CI |
| 02-DB-01 | 01-FND-03 | Persistence foundation | Define domain and interfaces | PostgreSQL schema, migrations, transactional outbox, SQLx |
| 02-DB-02 | 02-DB-01 | Persistence foundation | Implement vertical slice | PostgreSQL schema, migrations, transactional outbox, SQLx |
| 02-DB-03 | 02-DB-02 | Persistence foundation | Verify and operationalize | PostgreSQL schema, migrations, transactional outbox, SQLx |
| 03-SEC-01 | 02-DB-03 | Identity and RBAC | Define domain and interfaces | staff sessions, agency/client scoping, grants, agent delegation |
| 03-SEC-02 | 03-SEC-01 | Identity and RBAC | Implement vertical slice | staff sessions, agency/client scoping, grants, agent delegation |
| 03-SEC-03 | 03-SEC-02 | Identity and RBAC | Verify and operationalize | staff sessions, agency/client scoping, grants, agent delegation |
| 04-AUD-01 | 03-SEC-03 | Audit and observability | Define domain and interfaces | append-only audit, tracing, health, anomalies |
| 04-AUD-02 | 04-AUD-01 | Audit and observability | Implement vertical slice | append-only audit, tracing, health, anomalies |
| 04-AUD-03 | 04-AUD-02 | Audit and observability | Verify and operationalize | append-only audit, tracing, health, anomalies |
| 05-CRM-01 | 04-AUD-03 | Core CRM | Define domain and interfaces | client profiles, contacts, sources, dedupe, Assets |
| 05-CRM-02 | 05-CRM-01 | Core CRM | Implement vertical slice | client profiles, contacts, sources, dedupe, Assets |
| 05-CRM-03 | 05-CRM-02 | Core CRM | Verify and operationalize | client profiles, contacts, sources, dedupe, Assets |
| 06-UI-01 | 05-CRM-03 | Design system | Define domain and interfaces | Twenty-inspired Rust/WASM navigation, lists, forms, Kanban |
| 06-UI-02 | 06-UI-01 | Design system | Implement vertical slice | Twenty-inspired Rust/WASM navigation, lists, forms, Kanban |
| 06-UI-03 | 06-UI-02 | Design system | Verify and operationalize | Twenty-inspired Rust/WASM navigation, lists, forms, Kanban |
| 07-MAIL-01 | 06-UI-03 | Email and threaded chat | Define domain and interfaces | Gmail/Zoho adapters, outbound, reply ingestion, attachments |
| 07-MAIL-02 | 07-MAIL-01 | Email and threaded chat | Implement vertical slice | Gmail/Zoho adapters, outbound, reply ingestion, attachments |
| 07-MAIL-03 | 07-MAIL-02 | Email and threaded chat | Verify and operationalize | Gmail/Zoho adapters, outbound, reply ingestion, attachments |
| 08-CAMP-01 | 07-MAIL-03 | Campaigns | Define domain and interfaces | prospect pool, bulk email/calls, opt-out, attribution, conversion |
| 08-CAMP-02 | 08-CAMP-01 | Campaigns | Implement vertical slice | prospect pool, bulk email/calls, opt-out, attribution, conversion |
| 08-CAMP-03 | 08-CAMP-02 | Campaigns | Verify and operationalize | prospect pool, bulk email/calls, opt-out, attribution, conversion |
| 09-JEV-01 | 08-CAMP-03 | Jev agents | Define domain and interfaces | model adapters, fallback, tools, approval and handoff |
| 09-JEV-02 | 09-JEV-01 | Jev agents | Implement vertical slice | model adapters, fallback, tools, approval and handoff |
| 09-JEV-03 | 09-JEV-02 | Jev agents | Verify and operationalize | model adapters, fallback, tools, approval and handoff |
| 10-WORK-01 | 09-JEV-03 | Workflow builder | Define domain and interfaces | typed visual workflow, triggers, versioning and simulator |
| 10-WORK-02 | 10-WORK-01 | Workflow builder | Implement vertical slice | typed visual workflow, triggers, versioning and simulator |
| 10-WORK-03 | 10-WORK-02 | Workflow builder | Verify and operationalize | typed visual workflow, triggers, versioning and simulator |
| 11-FORM-01 | 10-WORK-03 | Form builder | Define domain and interfaces | conditional drag-drop, embed, validation and payments |
| 11-FORM-02 | 11-FORM-01 | Form builder | Implement vertical slice | conditional drag-drop, embed, validation and payments |
| 11-FORM-03 | 11-FORM-02 | Form builder | Verify and operationalize | conditional drag-drop, embed, validation and payments |
| 12-BILL-01 | 11-FORM-03 | Billing | Define domain and interfaces | Stripe one-time/deposit/recurring/annual, reconciliation |
| 12-BILL-02 | 12-BILL-01 | Billing | Implement vertical slice | Stripe one-time/deposit/recurring/annual, reconciliation |
| 12-BILL-03 | 12-BILL-02 | Billing | Verify and operationalize | Stripe one-time/deposit/recurring/annual, reconciliation |
| 13-SIGN-01 | 12-BILL-03 | Contract signing | Define domain and interfaces | Rust-native templates, signing evidence, payment gate |
| 13-SIGN-02 | 13-SIGN-01 | Contract signing | Implement vertical slice | Rust-native templates, signing evidence, payment gate |
| 13-SIGN-03 | 13-SIGN-02 | Contract signing | Verify and operationalize | Rust-native templates, signing evidence, payment gate |
| 14-VOICE-01 | 13-SIGN-03 | Calling | Define domain and interfaces | Twilio/Telnyx provisioning, forwarding, inbound/outbound AI calls |
| 14-VOICE-02 | 14-VOICE-01 | Calling | Implement vertical slice | Twilio/Telnyx provisioning, forwarding, inbound/outbound AI calls |
| 14-VOICE-03 | 14-VOICE-02 | Calling | Verify and operationalize | Twilio/Telnyx provisioning, forwarding, inbound/outbound AI calls |
| 15-DASH-01 | 14-VOICE-03 | Analytics and staff | Define domain and interfaces | master/role/client dashboards, staff drilldown and productivity |
| 15-DASH-02 | 15-DASH-01 | Analytics and staff | Implement vertical slice | master/role/client dashboards, staff drilldown and productivity |
| 15-DASH-03 | 15-DASH-02 | Analytics and staff | Verify and operationalize | master/role/client dashboards, staff drilldown and productivity |
| 16-ADMIN-01 | 15-DASH-03 | Admin and operations | Define domain and interfaces | installer, backup, recovery, upgrades, exports, health |
| 16-ADMIN-02 | 16-ADMIN-01 | Admin and operations | Implement vertical slice | installer, backup, recovery, upgrades, exports, health |
| 16-ADMIN-03 | 16-ADMIN-02 | Admin and operations | Verify and operationalize | installer, backup, recovery, upgrades, exports, health |
| 17-PERF-01 | 16-ADMIN-03 | Hardening and release | Define domain and interfaces | load/chaos/security tests, 1/2-core and 2/4/8-GiB profiles |
| 17-PERF-02 | 17-PERF-01 | Hardening and release | Implement vertical slice | load/chaos/security tests, 1/2-core and 2/4/8-GiB profiles |
| 17-PERF-03 | 17-PERF-02 | Hardening and release | Verify and operationalize | load/chaos/security tests, 1/2-core and 2/4/8-GiB profiles |

## Definition of done for every task
1. Implementation in Rust; no new first-party non-Rust executable source.
2. Unit and integration tests; negative permission tests; idempotency for side effects.
3. Documentation and audit events updated; observable success and failure.
4. CI passes including Rust WASM target and relevant security gates.
5. Record measured CPU/RAM and latency where relevant; update TASK_LEDGER.md.
