// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Campaign list + detail pages (R3-04).
//!
//! Campaigns had no UI at all before this task: `/campaigns` was a
//! `ComingSoon` placeholder and `/campaigns/:id` was a 404, while the
//! seeded demo campaign is `running` and reachable via the API. The
//! backend is complete and unchanged — every action below goes through
//! the existing endpoints in `crates/crm-api/src/campaigns.rs`.
//!
//! Two API gaps shape the page (flagged, not papered over):
//! - The list response carries no step count, so the list fetches each
//!   campaign's detail (`GET /campaigns/{id}` → `steps`) to count its
//!   steps. That is one extra request per row, bounded by the page size
//!   (20); a `step_count` field on the list response would remove it.
//! - There is no `GET /prospect-pools/{id}`, so a campaign's pool name
//!   is resolved from the pools list of its owning profile when that
//!   list is loaded, and falls back to the short pool id otherwise.
//!
//! Campaign creation needs a prospect pool, and pools are
//! profile-scoped, so the create form only renders under a profile
//! scope; the Agency scope is read-only with a note saying so.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::campaign::{
    Campaign, CampaignStep, DeliveryState, OwnerContext, ProspectPool, StepKind,
};

use crate::api::{
    add_campaign_step, create_campaign, create_pool, fetch_campaign, fetch_campaigns, fetch_pools,
    fetch_profiles, schedule_campaign, transition_campaign, AddCampaignStepRequest, ApiError,
    CampaignListResponse, CreateCampaignRequest, CreatePoolRequest, PoolListResponse,
    ScheduleCampaignRequest, DEMO_PROFILE_ID_STR,
};
use crate::components::{
    CountLine, EmptyState, ErrorAlert, FieldRow, Loading, OkAlert, PageHeader,
};
use crate::ui::{
    push_toast, Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardHeader,
    CardTitle, Input as TextInput, Label, Select, Table, TableBody, TableCell, TableHead,
    TableHeader, TableRow, ToastVariant, Toasts, UiHandler, UiValueHandler,
};

/// Maps a campaign lifecycle state to its badge variant.
fn campaign_status_badge(state: &DeliveryState) -> BadgeVariant {
    match state {
        DeliveryState::Draft => BadgeVariant::Secondary,
        DeliveryState::Scheduled => BadgeVariant::Info,
        DeliveryState::Running => BadgeVariant::Success,
        DeliveryState::Paused => BadgeVariant::Warning,
        DeliveryState::Completed => BadgeVariant::Default,
        DeliveryState::Cancelled => BadgeVariant::Destructive,
    }
}

/// Maps a step kind to its badge variant.
fn step_kind_badge(kind: &StepKind) -> BadgeVariant {
    match kind {
        StepKind::Email => BadgeVariant::Info,
        StepKind::Call => BadgeVariant::Success,
        StepKind::Wait => BadgeVariant::Warning,
    }
}

/// Shortens a UUID string to its first 8 characters for table display.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Resolves a campaign's pool to a display name from the loaded pools;
/// falls back to the short pool id when the pool isn't among them (an
/// agency-scoped list has no pool list to resolve against).
fn pool_label(pool_id: &str, pools: &[ProspectPool]) -> String {
    match pools.iter().find(|p| p.pool_id.to_string() == pool_id) {
        Some(pool) => pool.name.clone(),
        None => short_id(pool_id),
    }
}

/// Campaigns page: owner selector, campaign list, create form.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn CampaignsPage(token: String) -> impl IntoView {
    // Scope selector value: `"agency"` for agency-owned campaigns, or a
    // profile UUID. Defaults to the demo profile so the seeded
    // demo-seed-campaign is visible on first load.
    let selected = RwSignal::new(DEMO_PROFILE_ID_STR.to_string());
    let scope_options = RwSignal::new(None::<Result<Vec<(String, String)>, ApiError>>);
    {
        let token = token.clone();
        Effect::new(move |_| {
            let token = token.clone();
            spawn_local(async move {
                match fetch_profiles(&token).await {
                    Ok(page) => scope_options.set(Some(Ok(page
                        .profiles
                        .into_iter()
                        .map(|p| (p.profile_id.to_string(), p.name))
                        .collect()))),
                    Err(error) => scope_options.set(Some(Err(error))),
                }
            });
        });
    }

    view! {
        <section aria-label="Campaigns">
            <PageHeader
                title="Campaigns"
                description="Outreach campaigns: prospect pools, steps, schedule, and lifecycle."
            />
            {move || match scope_options.get() {
                None => view! { <Loading label="profiles" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.friendly_message() /> }.into_any()
                }
                Some(Ok(mut options)) => {
                    options.insert(0, ("agency".to_string(), "Agency".to_string()));
                    view! {
                        <Card>
                            <CardContent>
                                <div style="max-width: 28rem; padding-top: 0.5rem;">
                                    <Label for_id="campaign-scope">"Owner"</Label>
                                    <Select
                                        id="campaign-scope"
                                        options=options
                                        selected=selected
                                    />
                                </div>
                            </CardContent>
                        </Card>
                        <CampaignBrowser token={token.clone()} selected=selected />
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Campaign list + create forms for the selected owner.
///
/// Extracted so the token prop is owned here instead of being threaded
/// through nested reactive closures (which would force `FnOnce`).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `selected`: the selected owner — `"agency"`, or a profile UUID.
#[component]
fn CampaignBrowser(token: String, selected: RwSignal<String>) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    let campaigns = RwSignal::new(None::<Result<CampaignListResponse, ApiError>>);
    // Pools for the selected profile (the create form + pool-name
    // resolution). `None` under the Agency scope: pools are
    // profile-scoped, so there is nothing to list.
    let pools = RwSignal::new(None::<Result<PoolListResponse, ApiError>>);
    // Step counts keyed by campaign id. The list response carries no
    // step count, so each row's detail is fetched once (bounded by the
    // page size, 20) and the `steps` length is cached here.
    let step_counts = RwSignal::new(HashMap::<String, usize>::new());
    // Generation counter: each effect run bumps it; a response is applied
    // only if its generation is still current, so a slow earlier request
    // can never overwrite a newer one.
    let generation = RwSignal::new(0u64);
    {
        let token = token.clone();
        Effect::new(move |_| {
            reload.get();
            let scope = selected.get();
            // Clear the list when the owner changes (or a reload starts)
            // so stale rows never linger under the new selection.
            campaigns.set(None);
            pools.set(None);
            step_counts.set(HashMap::new());
            let my_generation = generation.get_untracked() + 1;
            generation.set(my_generation);
            let token = token.clone();
            spawn_local(async move {
                let is_agency = scope == "agency";
                let profile_filter = if is_agency {
                    None
                } else {
                    Some(scope.as_str())
                };
                let result = fetch_campaigns(&token, profile_filter).await;
                if generation.get_untracked() != my_generation {
                    return;
                }
                if let Ok(page) = &result {
                    // Count steps per campaign from its detail.
                    for campaign in &page.campaigns {
                        let campaign_id = campaign.campaign_id.to_string();
                        let token = token.clone();
                        spawn_local(async move {
                            if let Ok(detail) = fetch_campaign(&campaign_id, &token).await {
                                if generation.get_untracked() == my_generation {
                                    step_counts.update(|counts| {
                                        counts.insert(campaign_id, detail.steps.len());
                                    });
                                }
                            }
                        });
                    }
                }
                campaigns.set(Some(result));
                if !is_agency {
                    let result = fetch_pools(&scope, &token).await;
                    if generation.get_untracked() == my_generation {
                        pools.set(Some(result));
                    }
                }
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    view! {
        {move || {
            let is_agency = selected.get() == "agency";
            // Clone here: the nested `move ||` blocks below capture `token`
            // by move, so the outer closure must only ever clone it.
            let token = token.clone();
            view! {
                {if is_agency {
                    view! {
                        <p style="margin: 0 0 1rem; font-size: 0.875rem; color: var(--muted-foreground);">
                            "Agency campaigns. Creating a campaign needs a prospect pool, and pools are scoped to a client profile — select a profile above to create one."
                        </p>
                    }
                        .into_any()
                } else {
                    view! {
                        {move || match pools.get() {
                            None => view! { <Loading label="pools" /> }.into_any(),
                            Some(Err(error)) => {
                                view! { <ErrorAlert message=error.friendly_message() on_retry=retry /> }
                                    .into_any()
                            }
                            Some(Ok(page)) => {
                                view! {
                                    <CampaignCreateForm
                                        token={token.clone()}
                                        profile_id=selected.get()
                                        pools=page.pools.clone()
                                        on_created=retry
                                    />
                                    <PoolCreateForm
                                        token={token.clone()}
                                        profile_id=selected.get()
                                        on_created=retry
                                    />
                                }
                                    .into_any()
                            }
                        }}
                    }
                        .into_any()
                }}
                {move || match campaigns.get() {
                    None => view! { <Loading label="campaigns" /> }.into_any(),
                    Some(Err(error)) => {
                        view! { <ErrorAlert message=error.friendly_message() on_retry=retry /> }
                            .into_any()
                    }
                    Some(Ok(page)) if page.campaigns.is_empty() => {
                        view! { <EmptyState message="No campaigns yet." /> }.into_any()
                    }
                    Some(Ok(page)) => {
                        let loaded_pools = match pools.get() {
                            Some(Ok(p)) => p.pools,
                            _ => Vec::new(),
                        };
                        view! {
                            <CountLine count=page.total singular="campaign" plural="campaigns" />
                            <Card>
                                <CardContent>
                                    <Table>
                                        <TableHeader>
                                            <TableRow>
                                                <TableHead>"Name"</TableHead>
                                                <TableHead>"Status"</TableHead>
                                                <TableHead>"Prospect pool"</TableHead>
                                                <TableHead>"Steps"</TableHead>
                                                <TableHead>"Created"</TableHead>
                                            </TableRow>
                                        </TableHeader>
                                        <TableBody>
                                            <For
                                                each=move || page.campaigns.clone()
                                                key=|c| c.campaign_id.to_string()
                                                children=move |c| {
                                                    let id = c.campaign_id.to_string();
                                                    // Compute the per-row strings up front: the
                                                    // `view!` below captures its environment by
                                                    // move, so `loaded_pools` and `id` cannot be
                                                    // borrowed inside it on every row.
                                                    let pool =
                                                        pool_label(&c.prospect_pool_id, &loaded_pools);
                                                    let href = format!("/campaigns/{id}");
                                                    let count = {
                                                        let id = id.clone();
                                                        move || {
                                                            step_counts
                                                                .get()
                                                                .get(id.as_str())
                                                                .copied()
                                                        }
                                                    };
                                                    view! {
                                                        <TableRow>
                                                            <TableCell>
                                                                <A
                                                                    href=href
                                                                    attr:style="font-weight: 600; font-size: 0.875rem; \
                                                                                color: var(--foreground); text-decoration: none;"
                                                                >
                                                                    {c.name.clone()}
                                                                </A>
                                                            </TableCell>
                                                            <TableCell>
                                                                <Badge variant=campaign_status_badge(&c.delivery_state)>
                                                                    {format!("{:?}", c.delivery_state)}
                                                                </Badge>
                                                            </TableCell>
                                                            <TableCell>
                                                                {pool}
                                                            </TableCell>
                                                            <TableCell>
                                                                {move || match count() {
                                                                    Some(n) => n.to_string(),
                                                                    None => "…".to_string(),
                                                                }}
                                                            </TableCell>
                                                            <TableCell>
                                                                {c.created_at.format("%Y-%m-%d %H:%M").to_string()}
                                                            </TableCell>
                                                        </TableRow>
                                                    }
                                                }
                                            />
                                        </TableBody>
                                    </Table>
                                </CardContent>
                            </Card>
                        }
                            .into_any()
                    }
                }}
            }
        }}
    }
}

/// Create-campaign form for one client profile.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the owning profile (UUID string).
/// - `pools`: the profile's prospect pools, for the pool selector.
/// - `on_created`: invoked after a successful create so the list reloads.
#[component]
fn CampaignCreateForm(
    token: String,
    profile_id: String,
    pools: Vec<ProspectPool>,
    on_created: UiHandler,
) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let channel = RwSignal::new("email".to_string());
    let schedule = RwSignal::new("0 9 * * *".to_string());
    let pool = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let profile_id = profile_id.clone();
        move || {
            if working.get() {
                return;
            }
            let name_value = name.get().trim().to_string();
            let pool_value = pool.get();
            let schedule_value = schedule.get().trim().to_string();
            if name_value.is_empty() {
                status.set(Some(Err("Enter a campaign name.".to_string())));
                return;
            }
            if pool_value.is_empty() {
                status.set(Some(Err("Select a prospect pool.".to_string())));
                return;
            }
            if schedule_value.is_empty() {
                status.set(Some(Err("Enter a cron schedule.".to_string())));
                return;
            }
            let request = CreateCampaignRequest {
                name: name_value,
                owner_kind: "client_profile".to_string(),
                client_profile_id: Some(profile_id.clone()),
                channel: channel.get(),
                prospect_pool_id: pool_value,
                schedule: schedule_value,
                suppression_policy: None,
                conversion_rules: None,
                attribution_window_days: None,
            };
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match create_campaign(&request, &token).await {
                    Ok(created) => {
                        status.set(Some(Ok(format!("Campaign \"{}\" created.", created.name))));
                        name.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Campaign created",
                                Some(created.name.clone()),
                                ToastVariant::Success,
                            );
                        }
                        on_created.call();
                    }
                    Err(e) => status.set(Some(Err(e.friendly_message()))),
                }
                working.set(false);
            });
        }
    };

    let mut pool_options = vec![(String::new(), "— select a pool —".to_string())];
    pool_options.extend(
        pools
            .iter()
            .map(|p| (p.pool_id.to_string(), p.name.clone())),
    );
    let channel_options = vec![
        ("email".to_string(), "Email".to_string()),
        ("call".to_string(), "Call".to_string()),
        ("mixed".to_string(), "Mixed".to_string()),
    ];

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"New campaign"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="campaign-name">"Campaign name"</Label>
                        <TextInput
                            id="campaign-name"
                            placeholder="Spring outreach"
                            value=name
                            on_input=UiValueHandler::new(move |v| name.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="campaign-channel">"Channel"</Label>
                        <Select
                            id="campaign-channel"
                            options=channel_options
                            selected=channel
                        />
                    </div>
                    <div>
                        <Label for_id="campaign-schedule">"Schedule (cron)"</Label>
                        <TextInput
                            id="campaign-schedule"
                            placeholder="0 9 * * *"
                            value=schedule
                            on_input=UiValueHandler::new(move |v| schedule.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="campaign-pool">"Prospect pool"</Label>
                        <Select
                            id="campaign-pool"
                            options=pool_options
                            selected=pool
                        />
                    </div>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => {
                            view! { <ErrorAlert message=message /> }.into_any()
                        }
                    }}
                    <div>
                        <Button
                            on_click=UiHandler::new(submit)
                            disabled=Signal::derive(move || working.get())
                        >
                            "Create campaign"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Create-prospect-pool form for one client profile. Pools have no
/// members UI yet — membership is seeded/managed server-side — so this
/// only creates an (initially empty) named pool the campaign form can
/// then select.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the owning profile (UUID string).
/// - `on_created`: invoked after a successful create so the pools reload.
#[component]
fn PoolCreateForm(token: String, profile_id: String, on_created: UiHandler) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let profile_id = profile_id.clone();
        move || {
            if working.get() {
                return;
            }
            let name_value = name.get().trim().to_string();
            if name_value.is_empty() {
                status.set(Some(Err("Enter a pool name.".to_string())));
                return;
            }
            let request = CreatePoolRequest {
                client_profile_id: profile_id.clone(),
                name: name_value,
            };
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match create_pool(&request, &token).await {
                    Ok(created) => {
                        status.set(Some(Ok(format!("Pool \"{}\" created.", created.name))));
                        name.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Prospect pool created",
                                Some(created.name.clone()),
                                ToastVariant::Success,
                            );
                        }
                        on_created.call();
                    }
                    Err(e) => status.set(Some(Err(e.friendly_message()))),
                }
                working.set(false);
            });
        }
    };

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"New prospect pool"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="pool-name">"Pool name"</Label>
                        <TextInput
                            id="pool-name"
                            placeholder="Spring prospects"
                            value=name
                            on_input=UiValueHandler::new(move |v| name.set(v))
                        />
                    </div>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => {
                            view! { <ErrorAlert message=message /> }.into_any()
                        }
                    }}
                    <div>
                        <Button
                            variant=ButtonVariant::Secondary
                            on_click=UiHandler::new(submit)
                            disabled=Signal::derive(move || working.get())
                        >
                            "Create pool"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Campaign detail page: full record, steps, schedule, and lifecycle
/// actions.
///
/// A fetch or transition failure never wipes the record: the error is
/// shown as an alert above the content, and the loaded record stays
/// visible underneath (the AUDIT-08 `ProfileDetail` pattern — a failed
/// transition used to replace the whole page with the error alert).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `campaign_id`: the campaign UUID from the route.
#[component]
pub fn CampaignDetail(token: String, campaign_id: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    // The loaded record, kept across fetch/transition failures.
    let campaign = RwSignal::new(None::<Campaign>);
    let steps = RwSignal::new(Vec::<CampaignStep>::new());
    // True while a fetch is in flight; only the initial load shows a
    // spinner — a reload keeps the previous record on screen.
    let loading = RwSignal::new(true);
    // The last page-level failure (fetch or transition), if any.
    let error = RwSignal::new(Option::<String>::None);
    // The owning profile's pools, for resolving the pool name; empty
    // for agency-owned campaigns (pools are profile-scoped).
    let pools = RwSignal::new(Vec::<ProspectPool>::new());
    let working = RwSignal::new(false);
    {
        let token = token.clone();
        let campaign_id = campaign_id.clone();
        Effect::new(move |_| {
            reload.get();
            loading.set(true);
            let token = token.clone();
            let campaign_id = campaign_id.clone();
            spawn_local(async move {
                match fetch_campaign(&campaign_id, &token).await {
                    Ok(detail) => {
                        // Resolve the pool name from the owning
                        // profile's pools when the campaign is
                        // profile-owned.
                        if let OwnerContext::ClientProfile(profile_id) =
                            &detail.campaign.owner_context
                        {
                            if let Ok(page) = fetch_pools(&profile_id.to_string(), &token).await {
                                pools.set(page.pools);
                            }
                        }
                        campaign.set(Some(detail.campaign));
                        steps.set(detail.steps);
                        error.set(None);
                    }
                    // Friendly message, not the raw server text: the
                    // user needs "You don't have access to do that.",
                    // not "server error 403: Forbidden — …".
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                loading.set(false);
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));
    let toasts = use_context::<Toasts>();

    let transition = {
        let token = token.clone();
        let campaign_id = campaign_id.clone();
        move |name: &'static str| {
            // Guard against double-clicks racing the async work: the buttons
            // are disabled via `working`, but check synchronously here too so
            // a second invocation can never start a duplicate transition.
            if working.get() {
                return;
            }
            let token = token.clone();
            let campaign_id = campaign_id.clone();
            working.set(true);
            spawn_local(async move {
                match transition_campaign(&campaign_id, name, &token).await {
                    Ok(updated) => {
                        let verb = match name {
                            "launch" => "launched",
                            "pause" => "paused",
                            "resume" => "resumed",
                            "complete" => "completed",
                            "cancel" => "cancelled",
                            _ => "updated",
                        };
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                format!("Campaign {verb}"),
                                Some(updated.name.clone()),
                                ToastVariant::Success,
                            );
                        }
                        campaign.set(Some(updated));
                        error.set(None);
                    }
                    // The record stays: only the error is recorded, so the
                    // alert renders above the still-visible record.
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                working.set(false);
            });
        }
    };

    view! {
        <section aria-label="Campaign detail">
            <PageHeader title="Campaign" description="Steps, schedule, and lifecycle.">
                <A
                    href="/campaigns"
                    attr:style="display: inline-flex; align-items: center; height: 2.5rem; \
                                padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                border: 1px solid var(--input); background: transparent; \
                                color: var(--foreground);"
                >
                    "← Back to campaigns"
                </A>
            </PageHeader>
            {move || error.get().map(|message| view! {
                <ErrorAlert message=message on_retry=retry />
            })}
            {move || match campaign.get() {
                Some(c) => {
                    let launch = transition.clone();
                    let pause = transition.clone();
                    let resume = transition.clone();
                    let complete = transition.clone();
                    let cancel = transition.clone();
                    let state = c.delivery_state;
                    let is_draft = state == DeliveryState::Draft;
                    let is_scheduled = state == DeliveryState::Scheduled;
                    let is_running = state == DeliveryState::Running;
                    let is_paused = state == DeliveryState::Paused;
                    let is_terminal = matches!(
                        state,
                        DeliveryState::Completed | DeliveryState::Cancelled
                    );
                    let can_act = !working.get() && !is_terminal;
                    let steps_list = steps.get();
                    let step_count = steps_list.len();
                    let pool_name = pool_label(&c.prospect_pool_id, &pools.get());
                    let owner_label = match &c.owner_context {
                        OwnerContext::Agency => "Agency".to_string(),
                        OwnerContext::ClientProfile(id) => {
                            format!("Profile {}", short_id(&id.to_string()))
                        }
                    };
                    view! {
                        <Card>
                            <CardHeader>
                                <div style="display: flex; align-items: center; gap: 0.625rem;">
                                    <CardTitle>{c.name.clone()}</CardTitle>
                                    <Badge variant=campaign_status_badge(&state)>
                                        {format!("{state:?}")}
                                    </Badge>
                                </div>
                            </CardHeader>
                            <CardContent>
                                <dl style="margin: 0 0 1rem;">
                                    <FieldRow label="Campaign id">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {c.campaign_id.to_string()}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Owner">{owner_label}</FieldRow>
                                    <FieldRow label="Channel">
                                        {format!("{:?}", c.channel)}
                                    </FieldRow>
                                    <FieldRow label="Prospect pool">{pool_name}</FieldRow>
                                    <FieldRow label="Schedule (cron)">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {c.schedule.clone()}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Scheduled launch">
                                        {match c.schedule_for {
                                            Some(at) => {
                                                at.format("%Y-%m-%d %H:%M UTC").to_string()
                                            }
                                            None => "—".to_string(),
                                        }}
                                    </FieldRow>
                                    <FieldRow label="Suppression">
                                        {if c.suppression_policy.is_empty() {
                                            "—".to_string()
                                        } else {
                                            c.suppression_policy.clone()
                                        }}
                                    </FieldRow>
                                    <FieldRow label="Conversion rules">
                                        {if c.conversion_rules.is_empty() {
                                            "—".to_string()
                                        } else {
                                            c.conversion_rules.clone()
                                        }}
                                    </FieldRow>
                                    <FieldRow label="Attribution window">
                                        {format!("{} days", c.attribution_window_days)}
                                    </FieldRow>
                                    <FieldRow label="Created">
                                        {c.created_at.format("%Y-%m-%d %H:%M").to_string()}
                                    </FieldRow>
                                </dl>
                                <div style="display: flex; gap: 0.5rem; flex-wrap: wrap;">
                                    <Button
                                        btn_type="button"
                                        disabled=Signal::derive(move || {
                                            !can_act || !(is_draft || is_scheduled)
                                        })
                                        on_click={UiHandler::new({
                                            let launch = launch.clone();
                                            move || launch("launch")
                                        })}
                                    >
                                        "Launch"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Secondary
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_running)
                                        on_click={UiHandler::new({
                                            let pause = pause.clone();
                                            move || pause("pause")
                                        })}
                                    >
                                        "Pause"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Secondary
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_paused)
                                        on_click={UiHandler::new({
                                            let resume = resume.clone();
                                            move || resume("resume")
                                        })}
                                    >
                                        "Resume"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Secondary
                                        btn_type="button"
                                        disabled=Signal::derive(move || {
                                            !can_act || !(is_running || is_paused)
                                        })
                                        on_click={UiHandler::new({
                                            let complete = complete.clone();
                                            move || complete("complete")
                                        })}
                                    >
                                        "Complete"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Destructive
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act)
                                        on_click={UiHandler::new({
                                            let cancel = cancel.clone();
                                            move || cancel("cancel")
                                        })}
                                    >
                                        "Cancel"
                                    </Button>
                                </div>
                                <p style="margin: 0.75rem 0 0; font-size: 0.75rem; color: var(--muted-foreground);">
                                    "Completed and Cancelled are terminal. Re-launching a running campaign is an idempotent resume on the server; the UI offers Resume only from Paused."
                                </p>
                            </CardContent>
                        </Card>
                        <Card>
                            <CardHeader>
                                <CardTitle>"Steps"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                {if steps_list.is_empty() {
                                    view! {
                                        <EmptyState message="No steps yet — add the first step below." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <Table>
                                            <TableHeader>
                                                <TableRow>
                                                    <TableHead>"Position"</TableHead>
                                                    <TableHead>"Kind"</TableHead>
                                                    <TableHead>"Template"</TableHead>
                                                    <TableHead>"Wait"</TableHead>
                                                </TableRow>
                                            </TableHeader>
                                            <TableBody>
                                                <For
                                                    each=move || steps_list.clone()
                                                    key=|s| s.step_id.to_string()
                                                    children=move |s| {
                                                        view! {
                                                            <TableRow>
                                                                <TableCell>
                                                                    {s.position.to_string()}
                                                                </TableCell>
                                                                <TableCell>
                                                                    <Badge variant=step_kind_badge(&s.kind)>
                                                                        {format!("{:?}", s.kind)}
                                                                    </Badge>
                                                                </TableCell>
                                                                <TableCell>
                                                                    {s
                                                                        .template_id
                                                                        .clone()
                                                                        .unwrap_or_else(|| {
                                                                            "—".to_string()
                                                                        })}
                                                                </TableCell>
                                                                <TableCell>
                                                                    {s
                                                                        .wait_duration_secs
                                                                        .map(|secs| format!("{secs}s"))
                                                                        .unwrap_or_else(|| {
                                                                            "—".to_string()
                                                                        })}
                                                                </TableCell>
                                                            </TableRow>
                                                        }
                                                    }
                                                />
                                            </TableBody>
                                        </Table>
                                    }
                                        .into_any()
                                }}
                            </CardContent>
                        </Card>
                        {if is_draft {
                            view! {
                                <CampaignScheduleForm
                                    token={token.clone()}
                                    campaign_id={c.campaign_id.to_string()}
                                    campaign=campaign
                                />
                                <CampaignStepForm
                                    token={token.clone()}
                                    campaign_id={c.campaign_id.to_string()}
                                    step_count=step_count
                                    on_added=retry
                                />
                            }
                                .into_any()
                        } else {
                            ().into_any()
                        }}
                    }
                        .into_any()
                }
                None => {
                    if loading.get() {
                        view! { <Loading label="campaign" /> }.into_any()
                    } else {
                        view! {
                            <EmptyState message="Campaign not found — it may have been deleted, or the link may be wrong." />
                        }
                            .into_any()
                    }
                }
            }}
        </section>
    }
}

/// One-shot launch scheduler for a draft campaign.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `campaign_id`: the campaign UUID.
/// - `campaign`: the detail page's record signal, updated on success so
///   the new `schedule_for` shows without a reload.
#[component]
fn CampaignScheduleForm(
    token: String,
    campaign_id: String,
    campaign: RwSignal<Option<Campaign>>,
) -> impl IntoView {
    let launch_at = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let campaign_id = campaign_id.clone();
        move || {
            if working.get() {
                return;
            }
            let value = launch_at.get();
            if value.trim().is_empty() {
                status.set(Some(Err("Pick a launch time.".to_string())));
                return;
            }
            // `datetime-local` yields `YYYY-MM-DDTHH:MM` (or with
            // seconds); the API wants RFC 3339 in UTC.
            let schedule_for = if value.matches(':').count() == 2 {
                format!("{value}Z")
            } else {
                format!("{value}:00Z")
            };
            let request = ScheduleCampaignRequest { schedule_for };
            let token = token.clone();
            let campaign_id = campaign_id.clone();
            working.set(true);
            spawn_local(async move {
                match schedule_campaign(&campaign_id, &request, &token).await {
                    Ok(updated) => {
                        status.set(Some(Ok("Launch scheduled.".to_string())));
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Launch scheduled",
                                Some(updated.name.clone()),
                                ToastVariant::Success,
                            );
                        }
                        campaign.set(Some(updated));
                        launch_at.set(String::new());
                    }
                    Err(e) => status.set(Some(Err(e.friendly_message()))),
                }
                working.set(false);
            });
        }
    };

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Schedule launch"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="campaign-launch-at">"Launch time (UTC)"</Label>
                        <TextInput
                            id="campaign-launch-at"
                            input_type="datetime-local"
                            value=launch_at
                            on_input=UiValueHandler::new(move |v| launch_at.set(v))
                        />
                    </div>
                    <p style="margin: 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        "Times are UTC. The campaign must still be a draft, and the time must be in the future."
                    </p>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => {
                            view! { <ErrorAlert message=message /> }.into_any()
                        }
                    }}
                    <div>
                        <Button
                            variant=ButtonVariant::Secondary
                            on_click=UiHandler::new(submit)
                            disabled=Signal::derive(move || working.get())
                        >
                            "Schedule launch"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Add-step form for a draft campaign. The server rejects steps once the
/// campaign leaves `Draft`, so this form only renders on drafts.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `campaign_id`: the campaign UUID.
/// - `step_count`: current number of steps; the default position.
/// - `on_added`: invoked after a successful add so the detail reloads.
#[component]
fn CampaignStepForm(
    token: String,
    campaign_id: String,
    step_count: usize,
    on_added: UiHandler,
) -> impl IntoView {
    let kind = RwSignal::new("email".to_string());
    let position = RwSignal::new(step_count.to_string());
    let template = RwSignal::new(String::new());
    let wait = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let campaign_id = campaign_id.clone();
        move || {
            if working.get() {
                return;
            }
            let kind_value = kind.get();
            let position_raw = position.get();
            let position_value = if position_raw.trim().is_empty() {
                step_count as u32
            } else {
                match position_raw.trim().parse::<u32>() {
                    Ok(n) => n,
                    Err(_) => {
                        status.set(Some(Err("Position must be a whole number.".to_string())));
                        return;
                    }
                }
            };
            let template_value = template.get().trim().to_string();
            let wait_raw = wait.get();
            let (template_id, wait_duration_secs) = if kind_value == "wait" {
                match wait_raw.trim().parse::<u64>() {
                    Ok(secs) if secs > 0 => (None, Some(secs)),
                    _ => {
                        status.set(Some(Err("Enter a wait of at least 1 second.".to_string())));
                        return;
                    }
                }
            } else if template_value.is_empty() {
                status.set(Some(Err(
                    "Enter a template id for email and call steps.".to_string()
                )));
                return;
            } else {
                (Some(template_value), None)
            };
            let request = AddCampaignStepRequest {
                kind: kind_value,
                position: position_value,
                template_id,
                wait_duration_secs,
            };
            let token = token.clone();
            let campaign_id = campaign_id.clone();
            working.set(true);
            spawn_local(async move {
                match add_campaign_step(&campaign_id, &request, &token).await {
                    Ok(_) => {
                        status.set(Some(Ok("Step added.".to_string())));
                        template.set(String::new());
                        wait.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Step added",
                                Some(format!("Position {position_value}")),
                                ToastVariant::Success,
                            );
                        }
                        on_added.call();
                    }
                    Err(e) => status.set(Some(Err(e.friendly_message()))),
                }
                working.set(false);
            });
        }
    };

    let kind_options = vec![
        ("email".to_string(), "Email".to_string()),
        ("call".to_string(), "Call".to_string()),
        ("wait".to_string(), "Wait".to_string()),
    ];

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Add step"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="step-kind">"Kind"</Label>
                        <Select id="step-kind" options=kind_options selected=kind />
                    </div>
                    <div>
                        <Label for_id="step-position">"Position"</Label>
                        <TextInput
                            id="step-position"
                            input_type="number"
                            value=position
                            on_input=UiValueHandler::new(move |v| position.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="step-template">"Template id"</Label>
                        <TextInput
                            id="step-template"
                            placeholder="welcome-template"
                            value=template
                            on_input=UiValueHandler::new(move |v| template.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="step-wait">"Wait (seconds)"</Label>
                        <TextInput
                            id="step-wait"
                            input_type="number"
                            placeholder="3600"
                            value=wait
                            on_input=UiValueHandler::new(move |v| wait.set(v))
                        />
                    </div>
                    <p style="margin: 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        "Template id applies to email and call steps; wait seconds applies to wait steps."
                    </p>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => {
                            view! { <ErrorAlert message=message /> }.into_any()
                        }
                    }}
                    <div>
                        <Button
                            on_click=UiHandler::new(submit)
                            disabled=Signal::derive(move || working.get())
                        >
                            "Add step"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}
