// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Dev-only database seed for the Rust CRM.
//!
//! Inserts the demo client profile plus a handful of sample contacts through
//! the real service layer (so normalization, dedupe and audit events behave
//! exactly as in production). It also creates the demo staff user
//! (`demo@example.com`) with a documented dev-only password and grants for
//! the demo profile, so the auth vertical slice (task 03-SEC-02) is tryable
//! locally: log in at `POST /api/v1/auth/login` and call the contacts
//! endpoints with `Authorization: Bearer <token>`.
//!
//! Run:
//!
//! ```sh
//! docker compose up -d   # ephemeral Postgres 16 on :5432
//! CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
//!     cargo run -p crm-db --example seed
//! ```
//!
//! The seed is **idempotent**: re-running it skips contacts whose normalized
//! email already exists and staff whose email already exists. Without
//! `CRM_SEED_DEV=1` it prints the plan and exits without writing anything,
//! so it can never seed a production database by accident. Migrations run
//! first, so a fresh database works.
//!
//! The demo password below is **dev-only** and printed on every run; never
//! use it outside local development.

use uuid::Uuid;

use std::collections::HashMap;

use crm_core::billing::BillingServiceImpl;
use crm_core::campaign::CampaignServiceImpl;
use crm_core::dashboard::DashboardServiceImpl;
use crm_core::email::{EmailServiceImpl, StubEmailProvider};
use crm_core::memory::ContactServiceImpl;
use crm_core::signing::SigningServiceImpl;
use crm_core::voice::{StubVoiceProvider, VoiceServiceImpl};
use crm_db::auth::hash_password;
use crm_db::billing::{PgInvoiceRepository, PgPaymentRepository};
use crm_db::campaign::{
    PgCampaignRepository, PgConversionRepository, PgOptOutRepository, PgProspectPoolRepository,
};
use crm_db::core_crm::{PgAssetRepository, PgClientProfileRepository};
use crm_db::dashboard::{PgDashboardRepository, PgReportRepository};
use crm_db::email::{
    PgEmailAttachmentRepository, PgEmailMessageRepository, PgEmailThreadRepository,
};
use crm_db::postgres::{
    PgAuditRepository, PgAuditService, PgContactRepository, PgGrantRepository, PgStaffRepository,
};
use crm_db::signing::{PgContractRepository, PgSigningTemplateRepository};
use crm_db::voice::PgCallRepository;
use crm_db::{run_migrations, DATABASE_URL_ENV, DATABASE_URL_EXAMPLE};
use crm_domain::asset::Asset;
use crm_domain::audit::ActorKind;
use crm_domain::billing::Money;
use crm_domain::campaign::{Channel as CampaignChannel, OptOutChannel, OwnerContext, StepKind};
use crm_domain::contract::PaymentRequirement;
use crm_domain::dashboard::{
    DashboardWidget, DateRange, MetricAggregation, MetricDefinition, MetricEntity, WidgetPosition,
    WidgetType,
};
use crm_domain::email::InboundThreadHints;
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AgencyId, ClientProfileId, ContactId, GrantId, StaffId};
use crm_domain::profile::{ClientProfile, ProfileStatus};
use crm_domain::repositories::{
    AssetRepository, CallRepository, ClientProfileRepository, ContactRepository,
    ContractRepository, EmailMessageRepository, EmailThreadRepository, GrantRepository,
    InvoiceRepository, ProspectPoolRepository, StaffRepository,
};
use crm_domain::security::{Action, Grant, Operation, PasswordCredential, Resource, Scope};
use crm_domain::services::ActionContext;
use crm_domain::services::{
    BillingService, CampaignService, ContactService, DashboardService, EmailService, InboundEmail,
    NewCall, NewCampaign, NewCampaignStep, NewContact, NewContract, NewDashboard, NewEmailMessage,
    NewInvoice, NewInvoiceLine, NewPayment, NewReport, NewSigningTemplate, SigningService,
    UpdateDashboard, VoiceService,
};
use crm_domain::voice::{CallDirection, PhoneNumber};

/// Demo tenant also used by the `crm-web` vertical slice until real
/// auth/tenancy lands (task 03-SEC-01).
const DEMO_PROFILE_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

/// Demo agency the seeded staff user operates under.
const DEMO_AGENCY_ID: &str = "aaaaaaaa-0000-4000-8000-000000000001";

/// Demo staff login (dev-only).
const DEMO_STAFF_EMAIL: &str = "demo@example.com";
/// Dev-only password for the demo staff user. Printed on every seed run;
/// never use outside local development.
const DEMO_STAFF_PASSWORD: &str = "crm-demo-password";

/// Sample contacts: (emails, phones, source).
const SEED_CONTACTS: &[(&[&str], &[&str], &str)] = &[
    (&["ada@example.com"], &["+1-555-0101"], "web-form"),
    (&["grace@example.com"], &["+1-555-0102"], "csv-import"),
    (
        &["linus@example.com", "torvalds@example.com"],
        &["+1-555-0103"],
        "web-form",
    ),
    (&["margaret@example.com"], &[], "referral"),
    (&["alan@example.com"], &["+1-555-0105"], "phone-call"),
];

/// Seeds the demo profile and sample contacts. Returns `(created, skipped)`:
/// skipped contacts already existed (idempotent re-runs).
async fn seed(pool: &sqlx::PgPool) -> Result<(usize, usize), DomainError> {
    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let service = ContactServiceImpl::new(
        PgContactRepository::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let mut created = 0;
    let mut skipped = 0;
    for (emails, phones, source) in SEED_CONTACTS {
        let input = NewContact {
            client_profile_id: profile,
            emails: emails.iter().map(ToString::to_string).collect(),
            phones: phones.iter().map(ToString::to_string).collect(),
            source: (*source).to_owned(),
        };
        match service.create_contact(input, &ctx).await {
            Ok(_) => {
                created += 1;
            }
            // Re-running the seed meets the previous run's emails; the
            // dedupe conflict means "already seeded", not a failure.
            Err(DomainError::Conflict(_)) => {
                skipped += 1;
            }
            Err(other) => return Err(other),
        }
    }
    Ok((created, skipped))
}

/// Claims the demo client profile with its real agency and name.
///
/// The contact seed's `ensure_client_profile` upsert may have created a
/// placeholder row (id only); the repository's `create` claims it via
/// `ON CONFLICT DO UPDATE`, so this is idempotent. Also registers a
/// demo asset (idempotent via the content-hash unique index).
async fn seed_profile(pool: &sqlx::PgPool, agency_id: AgencyId) -> Result<(), DomainError> {
    let profile_id =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let now = chrono::Utc::now();
    let profile_repo = PgClientProfileRepository::new(pool.clone());
    profile_repo
        .create(ClientProfile {
            profile_id,
            agency_id,
            name: "Demo Client".to_owned(),
            status: ProfileStatus::Active,
            created_at: now,
            updated_at: now,
        })
        .await?;

    // Demo asset: a fixed content hash keeps re-runs idempotent — the
    // repository returns the existing row on hash conflict.
    let asset_repo = PgAssetRepository::new(pool.clone());
    let asset = Asset::new(
        profile_id,
        ActorId::from(Uuid::nil()),
        "demo-welcome.pdf".to_owned(),
        "application/pdf".to_owned(),
        1024,
        [42u8; 32],
    )?;
    asset_repo.create(asset).await?;
    Ok(())
}

/// Seeds one demo email thread on the demo profile: an inbound opener
/// (creates the thread via the ingest cascade), an outbound reply sent
/// through the stub provider, and a second inbound reply joined via the
/// `in_reply_to` parent lookup. Idempotent: skipped when the seed's
/// provider message id already exists.
async fn seed_email(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    use chrono::Utc;

    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let messages = PgEmailMessageRepository::new(pool.clone());
    // Idempotency anchor: the first seeded inbound message.
    if messages
        .find_by_provider_message_id(profile, "demo-seed-inbound-1")
        .await?
        .is_some()
    {
        return Ok(false);
    }

    let service = EmailServiceImpl::new(
        messages,
        PgEmailThreadRepository::new(pool.clone()),
        PgEmailAttachmentRepository::new(pool.clone()),
        StubEmailProvider::new(),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    // 1. Inbound opener: no thread hints match, so the ingest cascade
    //    opens a new thread.
    let opener = service
        .ingest_reply(
            InboundEmail {
                client_profile_id: profile,
                hints: InboundThreadHints {
                    provider_thread_id: None,
                    in_reply_to: None,
                    references: Vec::new(),
                    subject: "Welcome to Demo Client".to_owned(),
                },
                from: "client@example.com".to_owned(),
                to: vec!["agency@example.com".to_owned()],
                body_text: "Hi — looking forward to working with you.".to_owned(),
                body_html: None,
                provider_message_id: "demo-seed-inbound-1".to_owned(),
                received_at: Utc::now(),
            },
            &ctx,
        )
        .await?;
    let thread_id = opener.thread_id.expect("ingest always threads the message");

    // 2. Outbound reply in the same thread, via the stub provider.
    let sent = service
        .send(
            NewEmailMessage {
                client_profile_id: profile,
                thread_id: Some(thread_id),
                from: "agency@example.com".to_owned(),
                to: vec!["client@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Re: Welcome to Demo Client".to_owned(),
                body_text: "Likewise! Here's what happens next.".to_owned(),
                body_html: None,
                idempotency_key: Uuid::new_v4().to_string(),
            },
            &ctx,
        )
        .await?;

    // 3. Second inbound reply, joined via the `in_reply_to` parent
    //    lookup against the sent message's provider id.
    service
        .ingest_reply(
            InboundEmail {
                client_profile_id: profile,
                hints: InboundThreadHints {
                    provider_thread_id: None,
                    in_reply_to: sent.provider_message_id.clone(),
                    references: Vec::new(),
                    subject: "Re: Welcome to Demo Client".to_owned(),
                },
                from: "client@example.com".to_owned(),
                to: vec!["agency@example.com".to_owned()],
                body_text: "Sounds good — talk next week.".to_owned(),
                body_html: None,
                provider_message_id: "demo-seed-inbound-2".to_owned(),
                received_at: Utc::now(),
            },
            &ctx,
        )
        .await?;

    // `send` links the message to the thread but does not bump the
    // thread counters (only ingest does) — fix up the demo row so the
    // seeded thread reads consistently.
    let threads = PgEmailThreadRepository::new(pool.clone());
    let mut thread = threads.get(thread_id).await?;
    thread.message_count = 3;
    for participant in ["agency@example.com", "client@example.com"] {
        if !thread.participants.contains(&participant.to_owned()) {
            thread.participants.push(participant.to_owned());
        }
    }
    threads.update(thread).await?;
    Ok(true)
}

/// Seeds one draft campaign with an email step, one prospect pool
/// holding the demo contacts, and one opt-out.
///
/// Idempotent: if the demo pool already exists for the profile, the
/// whole step is skipped. The campaign is left in `Draft` — launching
/// it is the operator's explicit action (launch sends mail, even via
/// the stub provider's in-memory log).
async fn seed_campaign(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    const DEMO_POOL_NAME: &str = "demo-seed-pool";
    const DEMO_CAMPAIGN_NAME: &str = "demo-seed-campaign";

    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let pools = PgProspectPoolRepository::new(pool.clone());
    // Idempotency anchor: the demo pool.
    if pools
        .list_by_profile(profile, 100)
        .await?
        .iter()
        .any(|p| p.name == DEMO_POOL_NAME)
    {
        return Ok(false);
    }

    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    // Pool members: the seeded demo contacts, looked up by email.
    let contacts = PgContactRepository::new(pool.clone());
    let mut member_ids: Vec<ContactId> = Vec::new();
    let mut grace_id: Option<ContactId> = None;
    for email in ["ada@example.com", "grace@example.com", "linus@example.com"] {
        let mut found = contacts.find_by_normalized_email(profile, email).await?;
        if let Some(contact) = found.pop() {
            if email == "grace@example.com" {
                grace_id = Some(contact.contact_id);
            }
            member_ids.push(contact.contact_id);
        }
    }

    let email_service = EmailServiceImpl::new(
        PgEmailMessageRepository::new(pool.clone()),
        PgEmailThreadRepository::new(pool.clone()),
        PgEmailAttachmentRepository::new(pool.clone()),
        StubEmailProvider::new(),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let service = CampaignServiceImpl::new(
        PgCampaignRepository::new(pool.clone()),
        pools,
        PgOptOutRepository::new(pool.clone()),
        PgContactRepository::new(pool.clone()),
        PgConversionRepository::new(pool.clone()),
        email_service,
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );

    let created_pool = service
        .create_pool(profile, DEMO_POOL_NAME.to_owned(), &ctx)
        .await?;
    let pool_id = created_pool.pool_id;
    let pool_repos = PgProspectPoolRepository::new(pool.clone());
    for contact_id in &member_ids {
        // Dedupe is enforced by the composite PK; ignore conflicts.
        let _ = pool_repos.add_member(pool_id, *contact_id).await;
    }

    let campaign = service
        .create_campaign(
            NewCampaign {
                name: DEMO_CAMPAIGN_NAME.to_owned(),
                owner_context: OwnerContext::ClientProfile(profile),
                channel: CampaignChannel::Email,
                prospect_pool_id: pool_id.to_string(),
                schedule: "0 9 * * *".to_owned(),
                suppression_policy: "opt-outs".to_owned(),
                conversion_rules: "last-touch".to_owned(),
                attribution_window_days: 30,
                idempotency_key: Uuid::new_v4().to_string(),
            },
            &ctx,
        )
        .await?;
    service
        .add_step(
            NewCampaignStep {
                campaign_id: campaign.campaign_id,
                kind: StepKind::Email,
                position: 0,
                template_id: Some("demo-welcome-template".to_owned()),
                wait_duration_secs: None,
            },
            &ctx,
        )
        .await?;

    // grace@example.com opts out of email: launching the demo campaign
    // later must skip her.
    if let Some(grace_id) = grace_id {
        let _ = service
            .record_opt_out(
                profile,
                grace_id,
                OptOutChannel::Email,
                Some("demo seed opt-out".to_owned()),
                &ctx,
            )
            .await;
    }
    Ok(true)
}

/// Seeds one completed demo agent run on the demo profile, driven
/// through the real agent service (start → complete). Idempotent:
/// skipped when a `demo-triage` run already exists.
async fn seed_agent_run(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    use crm_core::jev::AgentServiceImpl;
    use crm_db::jev::{
        PgAgentRunRepository, PgHandoffRepository, PgRoleMembership, PgToolCallRepository,
    };
    use crm_domain::jev::FallbackChain;
    use crm_domain::repositories::AgentRunRepository;
    use crm_domain::services::{AgentService, NewAgentRun};

    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let runs = PgAgentRunRepository::new(pool.clone());
    // Idempotency anchor: an existing demo-triage run.
    if runs
        .list_by_profile(profile, 100)
        .await?
        .iter()
        .any(|r| r.agent_kind == "demo-triage")
    {
        return Ok(false);
    }

    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let service = AgentServiceImpl::new(
        runs,
        PgToolCallRepository::new(pool.clone()),
        PgHandoffRepository::new(pool.clone()),
        PgRoleMembership::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );

    let chain = FallbackChain::new(vec!["stub".to_string(), "local".to_string()])
        .expect("demo fallback chain is valid");
    let run = service
        .start_run(
            NewAgentRun {
                client_profile_id: profile,
                agent_kind: "demo-triage".to_string(),
                fallback_chain: chain,
                input: serde_json::json!({"task": "demo triage run"}),
            },
            &ctx,
        )
        .await?;
    service
        .complete_run(
            run.run_id,
            serde_json::json!({"done": true, "note": "seeded demo run"}),
            &ctx,
        )
        .await?;
    Ok(true)
}

/// Seeds one draft demo workflow on the demo profile: a manual
/// trigger node, an action node, a delay node, and edges connecting
/// trigger → action → delay. Idempotent via the demo workflow name.
/// Stays in `Draft` — publishing is the operator's explicit action.
async fn seed_workflow(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    use crm_core::workflow::WorkflowServiceImpl;
    use crm_db::workflow::{
        PgTriggerRepository, PgWorkflowEdgeRepository, PgWorkflowNodeRepository,
        PgWorkflowRepository, PgWorkflowVersionRepository,
    };
    use crm_domain::repositories::WorkflowRepository;
    use crm_domain::services::{
        NewTrigger, NewWorkflow, NewWorkflowEdge, NewWorkflowNode, WorkflowService,
    };
    use crm_domain::workflow::{TriggerType, WorkflowNodeType};

    const DEMO_WORKFLOW_NAME: &str = "demo-seed-workflow";

    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let workflows = PgWorkflowRepository::new(pool.clone());
    // Idempotency anchor: the demo workflow.
    if workflows
        .list_by_profile(profile, 100)
        .await?
        .iter()
        .any(|w| w.name == DEMO_WORKFLOW_NAME)
    {
        return Ok(false);
    }

    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let service = WorkflowServiceImpl::new(
        workflows,
        PgWorkflowNodeRepository::new(pool.clone()),
        PgWorkflowEdgeRepository::new(pool.clone()),
        PgTriggerRepository::new(pool.clone()),
        PgWorkflowVersionRepository::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );

    let workflow = service
        .create_workflow(
            NewWorkflow {
                client_profile_id: profile,
                name: DEMO_WORKFLOW_NAME.to_string(),
            },
            &ctx,
        )
        .await?;

    let trigger = service
        .add_node(
            NewWorkflowNode {
                workflow_id: workflow.workflow_id,
                node_type: WorkflowNodeType::TriggerNode,
                config: serde_json::json!({"trigger": "manual"}),
                position: (0, 0),
            },
            &ctx,
        )
        .await?;
    let action = service
        .add_node(
            NewWorkflowNode {
                workflow_id: workflow.workflow_id,
                node_type: WorkflowNodeType::ActionNode,
                config: serde_json::json!({"action": "email.send"}),
                position: (200, 0),
            },
            &ctx,
        )
        .await?;
    let delay = service
        .add_node(
            NewWorkflowNode {
                workflow_id: workflow.workflow_id,
                node_type: WorkflowNodeType::DelayNode,
                config: serde_json::json!({"duration_secs": 300}),
                position: (400, 0),
            },
            &ctx,
        )
        .await?;

    for (from, to) in [
        (trigger.node_id, action.node_id),
        (action.node_id, delay.node_id),
    ] {
        service
            .add_edge(
                NewWorkflowEdge {
                    workflow_id: workflow.workflow_id,
                    from_node: from,
                    to_node: to,
                    label: None,
                },
                &ctx,
            )
            .await?;
    }

    service
        .set_trigger(
            NewTrigger {
                workflow_id: workflow.workflow_id,
                trigger_type: TriggerType::ManualTrigger,
                config: serde_json::json!({}),
            },
            &ctx,
        )
        .await?;
    Ok(true)
}

/// Seeds one published demo form on the demo profile: a required
/// text field ("Full name"), a required email field ("Work email")
/// and an optional select field ("Plan"), plus one reviewed demo
/// submission. Idempotent via the demo form name. The form is
/// published at seed time (unlike the demo workflow, which stays
/// draft) so the demo submission validates against a live form.
async fn seed_form(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    use crm_core::form::FormServiceImpl;
    use crm_db::form::{PgFormFieldRepository, PgFormRepository, PgFormSubmissionRepository};
    use crm_domain::form::{FormAnswer, FormFieldType};
    use crm_domain::repositories::FormRepository;
    use crm_domain::services::{FormService, NewForm, NewFormField, NewFormSubmission};

    const DEMO_FORM_NAME: &str = "demo-seed-form";

    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let forms = PgFormRepository::new(pool.clone());
    // Idempotency anchor: the demo form.
    if forms
        .list_by_profile(profile, 100)
        .await?
        .iter()
        .any(|f| f.name == DEMO_FORM_NAME)
    {
        return Ok(false);
    }

    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let service = FormServiceImpl::new(
        forms,
        PgFormFieldRepository::new(pool.clone()),
        PgFormSubmissionRepository::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );

    let form = service
        .create_form(
            NewForm {
                client_profile_id: profile,
                name: DEMO_FORM_NAME.to_string(),
            },
            &ctx,
        )
        .await?;

    let name_field = service
        .add_field(
            NewFormField {
                form_id: form.form_id,
                field_type: FormFieldType::TextField,
                label: "Full name".to_string(),
                required: true,
                position: 0,
                config: serde_json::json!({}),
            },
            &ctx,
        )
        .await?;
    let email_field = service
        .add_field(
            NewFormField {
                form_id: form.form_id,
                field_type: FormFieldType::EmailField,
                label: "Work email".to_string(),
                required: true,
                position: 1,
                config: serde_json::json!({}),
            },
            &ctx,
        )
        .await?;
    service
        .add_field(
            NewFormField {
                form_id: form.form_id,
                field_type: FormFieldType::SelectField,
                label: "Plan".to_string(),
                required: false,
                position: 2,
                config: serde_json::json!({"options": ["basic", "pro"]}),
            },
            &ctx,
        )
        .await?;

    service.publish_form(form.form_id, &ctx).await?;

    let submission = service
        .submit(
            NewFormSubmission {
                form_id: form.form_id,
                submitted_by_contact: None,
                submitted_by_label: "Demo User".to_string(),
                answers: vec![
                    FormAnswer {
                        field_id: name_field.field_id,
                        value: "Demo User".to_string(),
                    },
                    FormAnswer {
                        field_id: email_field.field_id,
                        value: "demo-user@example.com".to_string(),
                    },
                ],
            },
            &ctx,
        )
        .await?;
    service
        .review_submission(submission.submission_id, &ctx)
        .await?;
    Ok(true)
}

/// Seeds demo feed items on the demo profile: one pinned, one
/// normal, one read, one hidden — exercising the feed ordering and
/// visibility bands. Written through `PgFeedItemRepository`
/// directly: there is no producer service yet (a 12-FEED-02
/// follow-on will have workstreams emit items). Idempotent via the
/// profile's item list (skipped when any feed items exist).
async fn seed_feed(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    use crm_db::feed::PgFeedItemRepository;
    use crm_domain::audit::ActorKind as FeedActorKind;
    use crm_domain::feed::{FeedItem, FeedQuery, FeedSource};
    use crm_domain::repositories::FeedItemRepository;

    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let items = PgFeedItemRepository::new(pool.clone());
    // Idempotency anchor: the profile's feed.
    if !items
        .list(
            profile,
            FeedQuery::new(1, true, false).expect("valid query"),
        )
        .await?
        .is_empty()
    {
        return Ok(false);
    }

    let mk = |source: FeedSource, title: &str, ref_type: &str| {
        FeedItem::new(
            profile,
            source,
            title.to_string(),
            None,
            FeedActorKind::System,
            ref_type.to_string(),
            Uuid::new_v4(),
        )
    };

    // Pinned: floats to the top of reads.
    let mut pinned = mk(
        FeedSource::ContactCreated,
        "Ada Lovelace became a prospect",
        "contact",
    )?;
    pinned.pin().expect("pin is valid");
    items.create(pinned).await?;

    // Normal: plain timeline item.
    items
        .create(mk(
            FeedSource::FormSubmitted,
            "Demo User submitted demo-seed-form",
            "form_submission",
        )?)
        .await?;

    // Read: shared read flag set.
    let mut read = mk(
        FeedSource::EmailReceived,
        "Email received from ada@example.com",
        "email_message",
    )?;
    read.mark_read();
    items.create(read).await?;

    // Hidden: excluded from default reads, sticky.
    let mut hidden = mk(
        FeedSource::NoteAdded,
        "Internal note (hidden from the default timeline)",
        "note",
    )?;
    hidden.hide();
    items.create(hidden).await?;

    Ok(true)
}

/// Seeds a demo invoice through the real `BillingServiceImpl` (task
/// 12-BILL-03): a USD draft invoice with two lines, issued, with a
/// partial payment recorded (balance remains, so it stays `Issued`).
/// Written through the service — never raw SQL — so lifecycle,
/// coverage, and audit behavior match production. Idempotent: skipped
/// when any invoices already exist on the demo profile.
async fn seed_billing(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let invoices = PgInvoiceRepository::new(pool.clone());
    // Idempotency anchor: the profile's invoices.
    if !invoices.list_by_profile(profile, 1).await?.is_empty() {
        return Ok(false);
    }

    let service = BillingServiceImpl::new(
        invoices,
        PgPaymentRepository::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let usd = |amount: i64| Money::new(amount, "USD".to_string()).expect("valid money");
    let invoice = service
        .create_invoice(
            NewInvoice {
                client_profile_id: profile,
                currency: "USD".to_string(),
            },
            &ctx,
        )
        .await?;
    service
        .add_line(
            NewInvoiceLine {
                invoice_id: invoice.invoice_id,
                description: "Monthly retainer".to_string(),
                quantity: 1,
                unit_price: usd(25_000),
            },
            &ctx,
        )
        .await?;
    service
        .add_line(
            NewInvoiceLine {
                invoice_id: invoice.invoice_id,
                description: "Onboarding".to_string(),
                quantity: 1,
                unit_price: usd(15_000),
            },
            &ctx,
        )
        .await?;
    service.issue_invoice(invoice.invoice_id, &ctx).await?;
    // Partial payment: $200 of the $400 total, so the invoice stays
    // `Issued` with a $200 balance.
    service
        .record_payment(
            NewPayment {
                invoice_id: invoice.invoice_id,
                amount: usd(20_000),
            },
            &ctx,
        )
        .await?;

    Ok(true)
}

/// Seeds a demo signing template and a draft contract on the demo
/// profile through the real `SigningServiceImpl`. Idempotent: skipped
/// when any contracts already exist on the demo profile.
async fn seed_signing(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let contracts = PgContractRepository::new(pool.clone());
    // Idempotency anchor: the profile's contracts.
    if !contracts.list_for_client(profile, 1).await?.is_empty() {
        return Ok(false);
    }

    let service = SigningServiceImpl::new(
        contracts,
        PgSigningTemplateRepository::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let template = service
        .create_template(
            NewSigningTemplate {
                name: "MSA".to_string(),
                body: "Master Services Agreement between {{agency_name}} and {{client_name}}.\
                       \n\nServices begin {{start_date}}."
                    .to_string(),
            },
            &ctx,
        )
        .await?;
    let mut variables = HashMap::new();
    variables.insert("agency_name".to_string(), "Acme Agency".to_string());
    variables.insert("client_name".to_string(), "Demo Client".to_string());
    variables.insert("start_date".to_string(), "2026-11-01".to_string());
    service
        .create_contract(
            NewContract {
                client_profile_id: profile,
                template_id: template.template_id,
                variables,
                payment_requirement: PaymentRequirement::None,
                signers: vec![("Demo Signer".to_string(), "signer@example.com".to_string())],
            },
            &ctx,
        )
        .await?;

    Ok(true)
}

/// Seeds a demo outbound call on the demo profile through the real
/// `VoiceServiceImpl`. Idempotent: skipped when any calls already
/// exist on the demo profile.
///
/// The call is placed and then ended. Ending an `Initiated` call
/// lands it in `Failed` ("call ended before connecting") — nothing
/// drives `Initiated → Ringing` over the service surface yet (see
/// `docs/operations/voice.md`), so a finished demo call is a failed
/// one until the provider-status polling ships.
async fn seed_voice(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let calls = PgCallRepository::new(pool.clone());
    // Idempotency anchor: the profile's calls.
    if !calls.list_by_profile(profile, 1).await?.is_empty() {
        return Ok(false);
    }

    let service = VoiceServiceImpl::new(
        calls,
        StubVoiceProvider::new(),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: Some(profile),
        correlation_id: Uuid::new_v4(),
    };

    let call = service
        .place_call(
            NewCall {
                client_profile_id: profile,
                contact_id: None,
                direction: CallDirection::Outbound,
                from: PhoneNumber::new("+14155550100".to_string())
                    .expect("demo from number parses"),
                to: PhoneNumber::new("+14155550199".to_string()).expect("demo to number parses"),
            },
            &ctx,
        )
        .await?;
    service.end_call(call.call_id, &ctx).await?;

    Ok(true)
}

/// Seeds an agency-level demo dashboard ("Agency overview") with two
/// widgets — a `Count` of contacts over the last 30 days and a
/// `Breakdown` of contacts by lifecycle — plus a demo report ("New
/// contacts by lifecycle", same metric, no parameters), through the real
/// `DashboardServiceImpl` (so scope rules, widget validation, and audit
/// behavior match production). Idempotent: skipped when any
/// agency-level dashboards already exist. The report is owned by the
/// demo staff user.
async fn seed_dashboard(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    let dashboards = PgDashboardRepository::new(pool.clone());
    let reports = PgReportRepository::new(pool.clone());
    let service = DashboardServiceImpl::new(
        dashboards,
        reports,
        pool.clone(),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::System,
        actor_id: ActorId::from(Uuid::nil()),
        client_profile_id: None,
        correlation_id: Uuid::new_v4(),
    };

    // Idempotency anchor: the specific demo artifacts, not just any
    // agency-level dashboard — so a retry after a mid-seed failure
    // recreates the missing pieces instead of skipping them.
    let mut created = false;
    let dashboard = match service
        .list_dashboards(100, &ctx)
        .await?
        .into_iter()
        .find(|d| d.name() == "Agency overview")
    {
        Some(existing) => existing,
        None => {
            created = true;
            service
                .create_dashboard(
                    NewDashboard {
                        name: "Agency overview".to_string(),
                        client_profile_id: None,
                    },
                    &ctx,
                )
                .await?
        }
    };

    let contacts_count = MetricDefinition::new(
        MetricEntity::Contact,
        MetricAggregation::count(),
        None,
        DateRange::Last30Days,
    )
    .expect("demo contacts count metric is valid");
    let contacts_by_lifecycle = MetricDefinition::new(
        MetricEntity::Contact,
        MetricAggregation::count(),
        Some("lifecycle".to_string()),
        DateRange::Last30Days,
    )
    .expect("demo contacts breakdown metric is valid");

    let mut widgets_to_add = Vec::new();
    for (title, widget_type, metric, position) in [
        (
            "Contacts (30d)",
            WidgetType::Count,
            contacts_count,
            WidgetPosition::new(0, 0),
        ),
        (
            "Contacts by lifecycle",
            WidgetType::Breakdown,
            contacts_by_lifecycle,
            WidgetPosition::new(1, 0),
        ),
    ] {
        if !dashboard.widgets().iter().any(|w| w.title() == title) {
            widgets_to_add.push(
                DashboardWidget::new(title.to_string(), widget_type, metric, position)
                    .expect("demo widget is valid"),
            );
        }
    }
    if !widgets_to_add.is_empty() {
        service
            .update_dashboard(
                dashboard.dashboard_id(),
                UpdateDashboard {
                    name: None,
                    widgets_to_add,
                    widget_ids_to_remove: Vec::new(),
                },
                &ctx,
            )
            .await?;
        created = true;
    }

    let staff = PgStaffRepository::new(pool.clone())
        .find_by_email(DEMO_STAFF_EMAIL)
        .await?
        .expect("demo staff user exists (seed_staff runs first)");
    if !service
        .list_reports(100, &ctx)
        .await?
        .iter()
        .any(|r| r.name() == "New contacts by lifecycle")
    {
        service
            .create_report(
                NewReport {
                    name: "New contacts by lifecycle".to_string(),
                    owner: staff.staff_id,
                    metric: MetricDefinition::new(
                        MetricEntity::Contact,
                        MetricAggregation::count(),
                        Some("lifecycle".to_string()),
                        DateRange::Last30Days,
                    )
                    .expect("demo report metric is valid"),
                    parameters: Vec::new(),
                },
                &ctx,
            )
            .await?;
        created = true;
    }

    // The demo dashboard was pre-existing only to make the anchor check
    // above meaningful; fall through to the return below.
    Ok(created)
}

/// Seeds demo admin data: agency settings v1 for the demo agency and
/// the maintenance singleton in the disabled state. Idempotent: skips
/// when a settings v1 already exists.
async fn seed_admin(pool: &sqlx::PgPool) -> Result<bool, DomainError> {
    use crm_core::admin::AdminServiceImpl;
    use crm_db::admin::{
        PgAgencySettingsRepository, PgBackupRepository, PgExportRepository,
        PgInstallationRepository, PgMaintenanceRepository, PgRoleAssignmentRepository,
        PgStaffInvitationRepository, PgUpgradeRepository,
    };
    use crm_domain::repositories::{AgencySettingsRepository, MaintenanceRepository};
    use crm_domain::services::{AdminService, UpdateAgencySettings};

    let staff_repo = PgStaffRepository::new(pool.clone());
    let staff_user = staff_repo
        .find_by_email(DEMO_STAFF_EMAIL)
        .await?
        .expect("demo staff user exists before admin seeding");
    let settings = PgAgencySettingsRepository::new(pool.clone());

    // Idempotency anchors, checked independently so a partial
    // first run (settings written, maintenance lost) is repaired
    // on re-run instead of skipped. Transient read errors fail
    // the seed loudly rather than being mistaken for "missing".
    let settings_missing = match settings.get_current(staff_user.agency_id).await {
        Ok(_) => false,
        Err(DomainError::NotFound { .. }) => true,
        Err(error) => return Err(error),
    };
    let maintenance = PgMaintenanceRepository::new(pool.clone());
    let maintenance_missing = match maintenance.get().await {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(error) => return Err(error),
    };
    if !settings_missing && !maintenance_missing {
        return Ok(false);
    }

    let service = AdminServiceImpl::new(
        PgStaffInvitationRepository::new(pool.clone()),
        PgRoleAssignmentRepository::new(pool.clone()),
        settings,
        maintenance,
        staff_repo,
        PgBackupRepository::new(pool.clone()),
        PgExportRepository::new(pool.clone()),
        PgUpgradeRepository::new(pool.clone()),
        PgInstallationRepository::new(pool.clone()),
        PgAuditService::new(PgAuditRepository::new(pool.clone())),
        vec![crm_db::admin::DbHealthReporter::new(pool.clone())],
    );
    let ctx = ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::from(staff_user.staff_id),
        client_profile_id: None,
        correlation_id: Uuid::new_v4(),
    };

    if settings_missing {
        service
            .update_settings(
                UpdateAgencySettings {
                    display_name: Some("Demo Agency".to_string()),
                    support_email: Some(Some("support@example.com".to_string())),
                    session_ttl_secs: Some(3600),
                },
                &ctx,
            )
            .await?;
    }
    // Ensures the maintenance singleton row exists in the disabled
    // state (the service defaults a missing row to disabled and
    // persists it here).
    if maintenance_missing {
        service.disable_maintenance(&ctx).await?;
    }
    Ok(true)
}

/// Env var gating writes: `CRM_SEED_DEV=1` is the explicit opt-in that
/// lets this example write to the database.
const SEED_DEV_ENV: &str = "CRM_SEED_DEV";

/// Seeds the demo staff user with a dev-only password credential and
/// grants for the demo profile. Idempotent: an existing email is skipped
/// for user creation, but missing grants are still added (FIX-05).
async fn seed_staff(pool: &sqlx::PgPool, agency_id: AgencyId) -> Result<bool, DomainError> {
    let staff_repo = PgStaffRepository::new(pool.clone());
    let grant_repo = PgGrantRepository::new(pool.clone());
    if let Some(user) = staff_repo.find_by_email(DEMO_STAFF_EMAIL).await? {
        // Existing demo user (e.g. from an earlier seed): ensure the
        // demo grants are present without duplicating anything already
        // assigned.
        ensure_demo_staff_grants(&grant_repo, user.staff_id, agency_id).await?;
        return Ok(false);
    }
    let now = chrono::Utc::now();
    let phc_hash = hash_password(DEMO_STAFF_PASSWORD.as_bytes()).await?;
    let user = crm_domain::security::StaffUser {
        staff_id: StaffId::new(),
        agency_id,
        email: DEMO_STAFF_EMAIL.to_owned(),
        display_name: "Demo User".to_owned(),
        status: crm_domain::security::StaffStatus::Active,
        credential: Some(PasswordCredential::new(phc_hash, now)?),
        created_at: now,
        updated_at: now,
    };
    let user = staff_repo.create(user).await?;

    // Grants for the demo profile: full contact lifecycle for the demo.
    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: profile,
    };
    for operation in [Operation::Read, Operation::Create, Operation::Update] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::Contact,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Grants for the email slice (task 07-MAIL-02): send mail and read
    // threads on the demo profile, so the demo user can try the email
    // endpoints after logging in.
    for (resource, operation) in [
        (Resource::Email, Operation::Create),
        (Resource::EmailThread, Operation::Read),
    ] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Grants for the Jev agents slice (task 09-JEV-02): run lifecycle
    // plus approval on the demo profile, so the demo user can try the
    // agent endpoints after logging in.
    for operation in [
        Operation::Create,
        Operation::Read,
        Operation::Update,
        Operation::Approve,
    ] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::AgentRun,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Grants for the form builder slice (task 11-FORM-02): full
    // form lifecycle on the demo profile, so the demo user can
    // try the form endpoints after logging in.
    for operation in [
        Operation::Create,
        Operation::Read,
        Operation::Update,
        Operation::Delete,
    ] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::Form,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Grants for the feed slice (task 12-FEED-02): list the feed
    // and annotate items (read/pin/hide) on the demo profile, so
    // the demo user can try the feed endpoints after logging in.
    for (resource, operation) in [
        (Resource::FeedItem, Operation::Read),
        (Resource::FeedItem, Operation::Update),
    ] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Grants for the billing slice (task 12-BILL-02): invoices and
    // payments on the demo profile, so the demo user can try the
    // billing endpoints after logging in.
    for (resource, operation) in [
        (Resource::Invoice, Operation::Read),
        (Resource::Invoice, Operation::Create),
        (Resource::Invoice, Operation::Update),
    ] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Grants for the contract signing slice (task 13-SIGN-02):
    // contracts on the demo profile, so the demo user can try the
    // signing endpoints after logging in.
    for (resource, operation) in [
        (Resource::Contract, Operation::Read),
        (Resource::Contract, Operation::Create),
        (Resource::Contract, Operation::Update),
    ] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Templates are agency-scoped, so the demo user also gets
    // agency-level contract grants to manage signing templates.
    let agency_scope = Scope::Agency { agency_id };
    for operation in [Operation::Read, Operation::Create] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource: Resource::Contract,
                        operation,
                    },
                    scope: agency_scope,
                },
            )
            .await?;
    }
    // Grants for the calling slice (task 14-VOICE-02): calls on the
    // demo profile, so the demo user can try the call endpoints
    // after logging in.
    for (resource, operation) in [
        (Resource::Call, Operation::Read),
        (Resource::Call, Operation::Create),
        (Resource::Call, Operation::Update),
    ] {
        grant_repo
            .assign(
                user.staff_id,
                Grant {
                    grant_id: GrantId::new(),
                    action: Action {
                        resource,
                        operation,
                    },
                    scope,
                },
            )
            .await?;
    }
    // Grants for profiles, assets, the audit log (FIX-05), and agency
    // settings (AUDIT-02): the demo user needs these to open the
    // Profiles, Assets, and Audit pages and to save branding out of
    // the box (they 403 without grants).
    ensure_demo_staff_grants(&grant_repo, user.staff_id, agency_id).await?;
    Ok(true)
}

/// Ensures the demo user holds the demo grants (FIX-05, AUDIT-02).
/// Idempotent: grants the user already holds are skipped, so re-running
/// the seed never duplicates them.
async fn ensure_demo_staff_grants(
    grant_repo: &PgGrantRepository,
    staff_id: StaffId,
    agency_id: AgencyId,
) -> Result<(), DomainError> {
    let existing = grant_repo.list_for_staff(staff_id).await?;
    let profile =
        ClientProfileId::from(Uuid::parse_str(DEMO_PROFILE_ID).expect("demo profile id parses"));
    let profile_scope = Scope::ClientProfile {
        agency_id,
        client_profile_id: profile,
    };
    let agency_scope = Scope::Agency { agency_id };
    // Profiles, the audit log, and agency settings are agency-scoped;
    // assets are profile-scoped (matching the API authorization).
    // AgencySettings:Manage (AUDIT-02) is what `PUT /api/v1/branding`
    // requires — without it the Branding Studio save 403s for the demo
    // user, the only login.
    for (resource, operation, scope) in [
        (Resource::ClientProfile, Operation::Read, agency_scope),
        (Resource::ClientProfile, Operation::Create, agency_scope),
        (Resource::Asset, Operation::Read, profile_scope),
        (Resource::Asset, Operation::Create, profile_scope),
        (Resource::AuditLog, Operation::Read, agency_scope),
        (Resource::AgencySettings, Operation::Manage, agency_scope),
    ] {
        let has = existing.iter().any(|grant| {
            grant.action.resource == resource
                && grant.action.operation == operation
                && grant.scope == scope
        });
        if !has {
            grant_repo
                .assign(
                    staff_id,
                    Grant {
                        grant_id: GrantId::new(),
                        action: Action {
                            resource,
                            operation,
                        },
                        scope,
                    },
                )
                .await?;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("seed failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let url = std::env::var(DATABASE_URL_ENV)
        .map_err(|_| format!("{DATABASE_URL_ENV} must be set, e.g. {DATABASE_URL_EXAMPLE}"))?;

    if std::env::var(SEED_DEV_ENV).as_deref() != Ok("1") {
        println!("Dry run (set {SEED_DEV_ENV}=1 to write). Plan:");
        println!("  database: {url}");
        println!("  1. apply pending migrations");
        println!("  2. ensure demo client profile {DEMO_PROFILE_ID}");
        println!(
            "  3. create {} sample contacts (idempotent)",
            SEED_CONTACTS.len()
        );
        println!("  4. claim demo profile with agency + name, register demo asset (idempotent)");
        println!("  5. create demo staff user {DEMO_STAFF_EMAIL} (idempotent)");
        println!(
            "  6. seed demo email thread (1 thread, 3 messages) on the demo profile (idempotent)"
        );
        println!(
            "  7. seed demo campaign (draft, 1 email step) + prospect pool + opt-out (idempotent)"
        );
        println!("  8. seed demo agent run (completed, chain [stub, local]) (idempotent)");
        println!("  9. seed demo workflow (draft, trigger → action → delay) (idempotent)");
        println!("  10. seed demo form (published, 3 fields, 1 reviewed submission) (idempotent)");
        println!("  11. seed demo feed items (pinned, normal, read, hidden) (idempotent)");
        println!("  12. seed demo invoice (issued, 2 lines, partial payment) (idempotent)");
        println!("  13. seed demo signing template + draft contract (idempotent)");
        println!("  14. seed demo call (outbound, ended) (idempotent)");
        println!(
            "  15. seed demo dashboard (agency overview, 2 widgets) + demo report (idempotent)"
        );
        println!("  16. seed demo agency settings v1 + maintenance disabled (idempotent)");
        return Ok(());
    }

    let pool = crm_db::connect(&url)
        .await
        .map_err(|error| format!("connect to database: {error}"))?;
    run_migrations(&pool)
        .await
        .map_err(|error| format!("apply migrations: {error}"))?;
    let (created, skipped) = seed(&pool)
        .await
        .map_err(|error| format!("seed contacts: {error}"))?;
    println!("seed complete: {created} contacts created, {skipped} already present");
    let agency_id = AgencyId::from(Uuid::parse_str(DEMO_AGENCY_ID).expect("demo agency id parses"));
    seed_profile(&pool, agency_id)
        .await
        .map_err(|error| format!("seed profile: {error}"))?;
    println!("seed complete: demo client profile {DEMO_PROFILE_ID} claimed");
    if seed_staff(&pool, agency_id)
        .await
        .map_err(|error| format!("seed staff user: {error}"))?
    {
        println!("seed complete: demo staff user {DEMO_STAFF_EMAIL} created");
    } else {
        println!("seed complete: demo staff user {DEMO_STAFF_EMAIL} already present");
    }
    if seed_email(&pool)
        .await
        .map_err(|error| format!("seed email: {error}"))?
    {
        println!("seed complete: demo email thread (3 messages) created");
    } else {
        println!("seed complete: demo email thread already present");
    }
    if seed_campaign(&pool)
        .await
        .map_err(|error| format!("seed campaign: {error}"))?
    {
        println!("seed complete: demo campaign + pool + opt-out created");
    } else {
        println!("seed complete: demo campaign already present");
    }
    if seed_agent_run(&pool)
        .await
        .map_err(|error| format!("seed agent run: {error}"))?
    {
        println!("seed complete: demo agent run (completed) created");
    } else {
        println!("seed complete: demo agent run already present");
    }
    if seed_workflow(&pool)
        .await
        .map_err(|error| format!("seed workflow: {error}"))?
    {
        println!("seed complete: demo workflow (draft, trigger → action → delay) created");
    } else {
        println!("seed complete: demo workflow already present");
    }
    if seed_form(&pool)
        .await
        .map_err(|error| format!("seed form: {error}"))?
    {
        println!("seed complete: demo form (published, 3 fields, 1 reviewed submission) created");
    } else {
        println!("seed complete: demo form already present");
    }
    if seed_feed(&pool)
        .await
        .map_err(|error| format!("seed feed: {error}"))?
    {
        println!("seed complete: demo feed items (pinned, normal, read, hidden) created");
    } else {
        println!("seed complete: demo feed items already present");
    }
    if seed_billing(&pool)
        .await
        .map_err(|error| format!("seed billing: {error}"))?
    {
        println!("seed complete: demo invoice (issued, 2 lines, partial payment) created");
    } else {
        println!("seed complete: demo invoice already present");
    }
    if seed_signing(&pool)
        .await
        .map_err(|error| format!("seed signing: {error}"))?
    {
        println!("seed complete: demo signing template + draft contract created");
    } else {
        println!("seed complete: demo contract already present");
    }
    if seed_voice(&pool)
        .await
        .map_err(|error| format!("seed voice: {error}"))?
    {
        println!("seed complete: demo call (outbound, ended) created");
    } else {
        println!("seed complete: demo call already present");
    }
    if seed_dashboard(&pool)
        .await
        .map_err(|error| format!("seed dashboard: {error}"))?
    {
        println!(
            "seed complete: demo dashboard (agency overview, 2 widgets) + demo report created"
        );
    } else {
        println!("seed complete: demo dashboard already present");
    }
    if seed_admin(&pool)
        .await
        .map_err(|error| format!("seed admin: {error}"))?
    {
        println!("seed complete: demo agency settings v1 + maintenance disabled created");
    } else {
        println!("seed complete: demo agency settings already present");
    }
    println!("dev login: email {DEMO_STAFF_EMAIL}, password {DEMO_STAFF_PASSWORD} (local only)");
    Ok(())
}
