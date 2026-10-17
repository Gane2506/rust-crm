# Rust-only Agent-native CRM — AI Development Handoff

**38 numbered specification documents** plus executable-workflow supporting files. Product direction: Twenty-inspired usability (not copied assets), fully Rust-authored application logic and Leptos/WASM interface, Jev orchestration, agency-only management of client profiles, communications, campaigns, calling, signing, payments and reports.

## Key product rules
- Each client profile contains contacts, prospects, campaigns, conversations, files/Assets, call numbers, knowledge base, agents, contracts, payments and analytics. The agency itself is a distinct internal operating context.
- Prospects imported for campaigns are not CRM customers until qualified. Merge contacts by verified identity with a review path for ambiguity. Preserve first and latest source plus all touches.
- Unified threaded email/chat mirrors replies and attachments; inbound replies are linked by provider IDs and email threading headers.
- Phone onboarding uses Twilio/Telnyx available-number search and carrier call forwarding; never requires number porting. Verify forwarding by test call.
- Jev classifies sentiment and intent, routes work, requests approvals, resumes and generates reports. Treat classifications as uncertain, not fact.
- Contracts are agency-to-client only; signed does not equal effective. Effectiveness requires verified payment and any additional template conditions.
- No client CRM login; outbound email and secure read-only report links. Public form, payment and signature flows remain possible without login.
- Embedded forms, workflows and dashboards have visual builders with role-based edit/view rights.

## Implementation sequence
Read `TASKS.md` and `TASK_LEDGER.md`. All task dependencies must be satisfied. Build a vertical slice before expanding breadth.

## Engineering decisions pending validation
Check vendor terms and APIs; decide browser styling policy compatible with Rust-only rule; confirm signature jurisdiction, consent and evidence retention; define provider availability and voice latency targets; define transactional email vs compliant outreach policy. PostgreSQL is a separately installed database engine; it is not Rust software, despite Rust-only application code.
