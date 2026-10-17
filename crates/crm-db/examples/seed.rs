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

use crm_core::campaign::CampaignServiceImpl;
use crm_core::email::{EmailServiceImpl, StubEmailProvider};
use crm_core::memory::ContactServiceImpl;
use crm_db::auth::hash_password;
use crm_db::campaign::{
    PgCampaignRepository, PgConversionRepository, PgOptOutRepository, PgProspectPoolRepository,
};
use crm_db::core_crm::{PgAssetRepository, PgClientProfileRepository};
use crm_db::email::{
    PgEmailAttachmentRepository, PgEmailMessageRepository, PgEmailThreadRepository,
};
use crm_db::postgres::{
    PgAuditRepository, PgAuditService, PgContactRepository, PgGrantRepository, PgStaffRepository,
};
use crm_db::{run_migrations, DATABASE_URL_ENV, DATABASE_URL_EXAMPLE};
use crm_domain::asset::Asset;
use crm_domain::audit::ActorKind;
use crm_domain::campaign::{Channel as CampaignChannel, OptOutChannel, OwnerContext, StepKind};
use crm_domain::email::InboundThreadHints;
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AgencyId, ClientProfileId, ContactId, GrantId, StaffId};
use crm_domain::profile::{ClientProfile, ProfileStatus};
use crm_domain::repositories::{
    AssetRepository, ClientProfileRepository, ContactRepository, EmailMessageRepository,
    EmailThreadRepository, GrantRepository, ProspectPoolRepository, StaffRepository,
};
use crm_domain::security::{Action, Grant, Operation, PasswordCredential, Resource, Scope};
use crm_domain::services::ActionContext;
use crm_domain::services::{
    CampaignService, ContactService, EmailService, InboundEmail, NewCampaign, NewCampaignStep,
    NewContact, NewEmailMessage,
};

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

/// Env var gating writes: `CRM_SEED_DEV=1` is the explicit opt-in that
/// lets this example write to the database.
const SEED_DEV_ENV: &str = "CRM_SEED_DEV";

/// Seeds the demo staff user with a dev-only password credential and
/// grants for the demo profile. Idempotent: an existing email is skipped.
async fn seed_staff(pool: &sqlx::PgPool, agency_id: AgencyId) -> Result<bool, DomainError> {
    let staff_repo = PgStaffRepository::new(pool.clone());
    if staff_repo.find_by_email(DEMO_STAFF_EMAIL).await?.is_some() {
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
    let grant_repo = PgGrantRepository::new(pool.clone());
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
    Ok(true)
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
    println!("dev login: email {DEMO_STAFF_EMAIL}, password {DEMO_STAFF_PASSWORD} (local only)");
    Ok(())
}
