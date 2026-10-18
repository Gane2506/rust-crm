// SPDX-License-Identifier: GPL-3.0-only
//! Integration tests for the Calling vertical slice (task 14-VOICE-02).
//!
//! These tests need a live PostgreSQL. Set `DATABASE_URL`, e.g.:
//! `postgres://postgres:postgres@localhost:5432/crm_test`
//!
//! ```sh
//! docker compose up -d            # ephemeral Postgres 16 on :5432
//! DATABASE_URL=<redacted>
//!     cargo test -p crm-db --all-features
//! ```
//!
//! Each test creates its own database, migrates a fresh schema, and
//! drops it afterwards. The service tests wire the real SQLx
//! repository with the stub provider and a no-op audit service.

mod common;

use std::future::Future;

use chrono::{SubsecRound, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::{drop_db, fresh_db_raw};
use crm_core::voice::{StubVoiceProvider, VoiceServiceImpl};
use crm_db::run_migrations;
use crm_db::voice::PgCallRepository;
use crm_domain::audit::{ActionResult, ActorKind, AuditEvent, PermissionDecision};
use crm_domain::error::DomainError;
use crm_domain::ids::{ActorId, AuditEventId, ClientProfileId};
use crm_domain::repositories::CallRepository;
use crm_domain::services::{ActionContext, AuditService, NewCall, NewRecording, VoiceService};
use crm_domain::voice::{CallDirection, CallStatus, PhoneNumber, RecordingConsent};

/// Creates a dedicated test database and migrates a fresh schema into it.
async fn fresh_db() -> (PgPool, String, String) {
    let (pool, admin_url, db_name) = fresh_db_raw().await;
    run_migrations(&pool)
        .await
        .expect("migrate the fresh test schema");
    (pool, admin_url, db_name)
}

/// Inserts a client-profile row (calls reference it).
async fn ensure_profile(pool: &PgPool, id: ClientProfileId) {
    sqlx::query(
        "INSERT INTO client_profiles (client_profile_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(id.as_uuid())
    .execute(pool)
    .await
    .expect("insert client profile");
}

/// No-op [`AuditService`] for service tests: audit must never fail the
/// operation under test.
#[derive(Debug, Clone, Default)]
struct NoopAudit;

impl AuditService for NoopAudit {
    // The trait declares `fn -> impl Future` (not `async fn`), so the
    // impl must keep that exact form.
    #[allow(clippy::manual_async_fn)]
    fn record_action<'a>(
        &'a self,
        ctx: &'a ActionContext,
        action: &'a str,
        target_kind: &'a str,
        target_id: &'a str,
        permission_decision: PermissionDecision,
        result: ActionResult,
        _before_ref: Option<&'a str>,
        _after_ref: Option<&'a str>,
    ) -> impl Future<Output = Result<AuditEvent, DomainError>> + Send + 'a {
        async move {
            Ok(AuditEvent {
                event_id: AuditEventId::new(),
                occurred_at: Utc::now().round_subsecs(6),
                actor_kind: ActorKind::System,
                actor_id: ctx.actor_id,
                client_profile_id: ctx.client_profile_id,
                action: action.to_owned(),
                target_kind: target_kind.to_owned(),
                target_id: target_id.to_owned(),
                correlation_id: ctx.correlation_id,
                permission_decision,
                result,
                before_ref: None,
                after_ref: None,
                chain_hash: [0u8; 32],
            })
        }
    }
}

/// Builds the real service over `pool` with the stub provider and a
/// no-op audit service.
fn service(pool: &PgPool) -> VoiceServiceImpl<PgCallRepository, StubVoiceProvider, NoopAudit> {
    VoiceServiceImpl::new(
        PgCallRepository::new(pool.clone()),
        StubVoiceProvider::new(),
        NoopAudit,
    )
}

/// Test action context scoped to `client_profile_id`.
fn ctx_for(profile_id: ClientProfileId) -> ActionContext {
    ActionContext {
        actor_kind: ActorKind::Human,
        actor_id: ActorId::new(),
        client_profile_id: Some(profile_id),
        correlation_id: Uuid::new_v4(),
    }
}

fn call_input(profile: ClientProfileId) -> NewCall {
    NewCall {
        client_profile_id: profile,
        contact_id: None,
        direction: CallDirection::Outbound,
        from: PhoneNumber::new("+14155550100".to_string()).expect("valid"),
        to: PhoneNumber::new("+14155550199".to_string()).expect("valid"),
    }
}

#[tokio::test]
async fn call_repo_crud_round_trip() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let calls = PgCallRepository::new(pool.clone());

    let call = crm_domain::voice::Call::initiate(
        profile,
        None,
        CallDirection::Outbound,
        PhoneNumber::new("+14155550100".to_string()).expect("valid"),
        PhoneNumber::new("+14155550199".to_string()).expect("valid"),
    )
    .expect("valid call");
    let created = calls.create(call).await.expect("create call");
    assert_eq!(created.status, CallStatus::Initiated);
    assert_eq!(created.client_profile_id, profile);

    let fetched = calls.get(created.call_id).await.expect("get call");
    assert_eq!(fetched.call_id, created.call_id);

    let listed = calls
        .list_by_profile(profile, 10)
        .await
        .expect("list by profile");
    assert_eq!(listed.len(), 1);

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_place_call_assigns_provider_reference() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);

    let call = svc
        .place_call(call_input(profile), &ctx_for(profile))
        .await
        .expect("place call");
    assert_eq!(call.status, CallStatus::Initiated);
    assert!(call.provider_call_id.is_some());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn service_end_call_moves_to_terminal_status() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let calls = PgCallRepository::new(pool.clone());

    let call = svc
        .place_call(call_input(profile), &ctx_for(profile))
        .await
        .expect("place call");
    // The stub provider never rings; the test advances the domain
    // lifecycle on the stored row directly, then ends via the service.
    let mut in_progress = call.clone();
    in_progress.ring().expect("ring");
    in_progress.answer().expect("answer");
    calls.update(in_progress).await.expect("update");

    let ended = svc
        .end_call(call.call_id, &ctx_for(profile))
        .await
        .expect("end call");
    assert_eq!(ended.status, CallStatus::Completed);
    assert!(ended.duration_secs.is_some());

    // Ending a finished call is rejected.
    let err = svc
        .end_call(call.call_id, &ctx_for(profile))
        .await
        .expect_err("double end must fail");
    assert!(matches!(err, DomainError::Validation(_)));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn recording_requires_granted_consent() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let calls = PgCallRepository::new(pool.clone());

    let call = svc
        .place_call(call_input(profile), &ctx_for(profile))
        .await
        .expect("place call");
    let mut in_progress = call.clone();
    in_progress.ring().expect("ring");
    in_progress.answer().expect("answer");
    calls.update(in_progress).await.expect("update");

    // Unknown consent is rejected by the domain gate.
    let err = svc
        .start_recording(
            NewRecording {
                call_id: call.call_id,
                consent: RecordingConsent::Unknown,
            },
            &ctx_for(profile),
        )
        .await
        .expect_err("unknown consent must be rejected");
    assert!(matches!(err, DomainError::Validation(_)));

    // Denied consent is rejected too.
    let err = svc
        .start_recording(
            NewRecording {
                call_id: call.call_id,
                consent: RecordingConsent::Denied,
            },
            &ctx_for(profile),
        )
        .await
        .expect_err("denied consent must be rejected");
    assert!(matches!(err, DomainError::Validation(_)));

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn recording_lifecycle_granted_consent() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile = ClientProfileId::new();
    ensure_profile(&pool, profile).await;
    let svc = service(&pool);
    let calls = PgCallRepository::new(pool.clone());

    let call = svc
        .place_call(call_input(profile), &ctx_for(profile))
        .await
        .expect("place call");
    let mut in_progress = call.clone();
    in_progress.ring().expect("ring");
    in_progress.answer().expect("answer");
    calls.update(in_progress).await.expect("update");

    let recording = svc
        .start_recording(
            NewRecording {
                call_id: call.call_id,
                consent: RecordingConsent::Granted,
            },
            &ctx_for(profile),
        )
        .await
        .expect("start recording");
    assert!(recording.recording.as_ref().expect("recording").active);

    let stopped = svc
        .stop_recording(
            call.call_id,
            crm_domain::ids::AssetId::new(),
            &ctx_for(profile),
        )
        .await
        .expect("stop recording");
    let recording = stopped.recording.as_ref().expect("recording");
    assert!(!recording.active);
    assert!(recording.asset_id.is_some());

    // The recording row round-trips through storage.
    let fetched = calls.get(call.call_id).await.expect("get call");
    let recording = fetched.recording.as_ref().expect("recording");
    assert!(!recording.active);
    assert!(recording.asset_id.is_some());

    drop_db(&admin_url, &db_name, pool).await;
}

#[tokio::test]
async fn calls_are_profile_scoped() {
    let (pool, admin_url, db_name) = fresh_db().await;
    let profile_a = ClientProfileId::new();
    let profile_b = ClientProfileId::new();
    ensure_profile(&pool, profile_a).await;
    ensure_profile(&pool, profile_b).await;
    let svc = service(&pool);

    let call = svc
        .place_call(call_input(profile_a), &ctx_for(profile_a))
        .await
        .expect("place call");

    // Cross-profile reads are forbidden.
    let err = svc
        .end_call(call.call_id, &ctx_for(profile_b))
        .await
        .expect_err("cross-profile end must be forbidden");
    assert!(matches!(err, DomainError::Forbidden(_)));

    // Listing is isolated per profile.
    let listed_a = svc.list_calls(10, &ctx_for(profile_a)).await.expect("list");
    assert_eq!(listed_a.len(), 1);
    let listed_b = svc.list_calls(10, &ctx_for(profile_b)).await.expect("list");
    assert!(listed_b.is_empty());

    drop_db(&admin_url, &db_name, pool).await;
}
