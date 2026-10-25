// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Client-profile pages: list + create, and detail with lifecycle actions.
//!
//! Reskinned on the v2 component library (UI2-02). Profiles are
//! agency-level resources (`ClientProfile` grant at agency scope). The
//! caller's agency comes from the session — the UI never sends an
//! agency id.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::profile::{ClientProfile, ProfileStatus};

use crate::api::{create_profile, fetch_profile, fetch_profiles, transition_profile, ApiError};
use crate::components::{
    CountLine, EmptyState, ErrorAlert, FieldRow, Loading, OkAlert, PageHeader,
};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle, Icon,
    IconName, Input as TextInput, Label, Table, TableBody, TableCell, TableHead, TableHeader,
    TableRow, UiHandler, UiValueHandler,
};

/// Maps a profile status to its badge variant.
fn profile_badge(status: &ProfileStatus) -> BadgeVariant {
    match status {
        ProfileStatus::Active => BadgeVariant::Success,
        ProfileStatus::Suspended => BadgeVariant::Warning,
        ProfileStatus::Archived => BadgeVariant::Secondary,
    }
}

/// Profiles list page: fetch-on-mount list plus the create form.
///
/// # Props
/// - `token`: the in-memory bearer token; every API call sends it as
///   `Authorization: Bearer`.
#[component]
pub fn ProfilesPage(token: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    // `None` = loading, `Some(Ok(_))` = loaded, `Some(Err(_))` = failed.
    let profiles = RwSignal::new(None::<Result<crate::api::ProfileListResponse, ApiError>>);
    let token_for_fetch = token.clone();
    Effect::new(move |_| {
        reload.get();
        profiles.set(None);
        let token = token_for_fetch.clone();
        spawn_local(async move {
            profiles.set(Some(fetch_profiles(&token).await));
        });
    });
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    view! {
        <section aria-label="Client profiles">
            <PageHeader
                title="Client profiles"
                description="Agency-operated client workspaces."
            />
            <ProfileForm token={token.clone()} on_created=retry />
            {move || match profiles.get() {
                None => view! { <Loading label="profiles" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() on_retry={retry} /> }.into_any()
                }
                Some(Ok(page)) if page.profiles.is_empty() => {
                    view! { <EmptyState message="No profiles yet." /> }.into_any()
                }
                Some(Ok(page)) => {
                    view! {
                        <CountLine count=page.total singular="profile" plural="profiles" />
                        <Card>
                            <Table>
                                <TableHeader>
                                    <TableRow>
                                        <TableHead>"Profile"</TableHead>
                                        <TableHead>"Status"</TableHead>
                                        <TableHead>"Id"</TableHead>
                                    </TableRow>
                                </TableHeader>
                                <TableBody>
                                    <For
                                        each=move || page.profiles.clone()
                                        key=|profile| profile.profile_id
                                        let:profile
                                    >
                                        <ProfileRow profile=profile />
                                    </For>
                                </TableBody>
                            </Table>
                        </Card>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Create-profile form: just a name; the agency comes from the session.
///
/// POSTs with a fresh `Idempotency-Key`, then asks the parent list to
/// refresh through `on_created`.
#[component]
fn ProfileForm(token: String, on_created: UiHandler) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let profile_name = name.get_untracked().trim().to_string();
        if profile_name.is_empty() {
            status.set(Some(Err("Enter a profile name.".to_string())));
            return;
        }
        submitting.set(true);
        status.set(None);
        let token = token.clone();
        spawn_local(async move {
            match create_profile(&profile_name, &token).await {
                Ok(profile) => {
                    name.set(String::new());
                    status.set(Some(Ok(format!("Profile created: {}", profile.name))));
                    on_created.call();
                }
                Err(error) => status.set(Some(Err(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"New profile"</CardTitle>
            </CardHeader>
            <CardContent>
                <form on:submit=on_submit>
                    <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                        <div>
                            <Label for_id="profile-name">"Name"</Label>
                            <TextInput
                                id="profile-name"
                                placeholder="Acme Corp"
                                value=name
                                on_input={UiValueHandler::new(move |v: String| name.set(v))}
                            />
                        </div>
                        <div>
                            <Button
                                variant=ButtonVariant::Default
                                btn_type="submit"
                                disabled=Signal::derive(move || submitting.get())
                            >
                                {move || if submitting.get() { "Creating…" } else { "Create profile" }}
                            </Button>
                        </div>
                        {move || match status.get() {
                            None => ().into_any(),
                            Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                            Some(Err(message)) => view! { <ErrorAlert message=message /> }.into_any(),
                        }}
                    </div>
                </form>
            </CardContent>
        </Card>
    }
}

/// One profile as a table row: name link, status badge, short id.
#[component]
fn ProfileRow(profile: ClientProfile) -> impl IntoView {
    let id = profile.profile_id.to_string();
    let short_id: String = id.chars().take(8).collect();
    let variant = profile_badge(&profile.status);
    let status_label = format!("{:?}", profile.status);
    view! {
        <TableRow>
            <TableCell>
                <A
                    href=format!("/profiles/{id}")
                    attr:style="font-weight: 600; font-size: 0.875rem; \
                                color: var(--foreground); text-decoration: none;"
                >
                    {profile.name.clone()}
                </A>
            </TableCell>
            <TableCell>
                <Badge variant=variant>{status_label}</Badge>
            </TableCell>
            <TableCell>
                <span style="font-family: var(--font-mono); font-size: 0.8125rem; color: var(--muted-foreground);">
                    {format!("{short_id}…")}
                </span>
            </TableCell>
        </TableRow>
    }
}

/// Profile detail page: full record plus lifecycle actions.
///
/// A fetch or transition failure never wipes the record: the error is
/// shown as an alert above the content, and the loaded record stays
/// visible underneath (AUDIT-08 — a failed transition used to replace
/// the whole page with the error alert).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the profile UUID from the route.
#[component]
pub fn ProfileDetail(token: String, profile_id: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    // The loaded record, kept across fetch/transition failures.
    let profile = RwSignal::new(None::<ClientProfile>);
    // True while a fetch is in flight; only the initial load shows a
    // spinner — a reload keeps the previous record on screen.
    let loading = RwSignal::new(true);
    // The last page-level failure (fetch or transition), if any.
    let error = RwSignal::new(Option::<String>::None);
    let working = RwSignal::new(false);
    {
        let token = token.clone();
        let profile_id = profile_id.clone();
        Effect::new(move |_| {
            reload.get();
            loading.set(true);
            let token = token.clone();
            let profile_id = profile_id.clone();
            spawn_local(async move {
                match fetch_profile(&profile_id, &token).await {
                    Ok(p) => {
                        profile.set(Some(p));
                        error.set(None);
                    }
                    // Display (not Debug): the user needs the server's
                    // message, e.g. "server error 403: Forbidden — the
                    // authenticated caller lacks the grant for this action".
                    Err(e) => error.set(Some(e.to_string())),
                }
                loading.set(false);
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    let transition = {
        let token = token.clone();
        let profile_id = profile_id.clone();
        move |name: &'static str| {
            // Guard against double-clicks racing the async work: the buttons
            // are disabled via `working`, but check synchronously here too so
            // a second invocation can never start a duplicate transition.
            if working.get() {
                return;
            }
            let token = token.clone();
            let profile_id = profile_id.clone();
            working.set(true);
            spawn_local(async move {
                match transition_profile(&profile_id, name, &token).await {
                    Ok(updated) => {
                        profile.set(Some(updated));
                        error.set(None);
                    }
                    // The record stays: only the error is recorded, so the
                    // alert renders above the still-visible record.
                    Err(e) => error.set(Some(e.to_string())),
                }
                working.set(false);
            });
        }
    };

    view! {
        <section aria-label="Profile detail">
            <PageHeader title="Profile" description="Client workspace record and lifecycle.">
                <A
                    href="/profiles"
                    attr:style="display: inline-flex; align-items: center; height: 2.5rem; \
                                padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                border: 1px solid var(--input); background: transparent; \
                                color: var(--foreground);"
                >
                    "← Back to profiles"
                </A>
            </PageHeader>
            {move || error.get().map(|message| view! {
                <ErrorAlert message=message on_retry=retry />
            })}
            {move || match profile.get() {
                Some(p) => {
                    let suspend = transition.clone();
                    let reactivate = transition.clone();
                    let archive = transition.clone();
                    let status = p.status;
                    let can_act = !working.get() && status != ProfileStatus::Archived;
                    let is_active = status == ProfileStatus::Active;
                    let is_suspended = status == ProfileStatus::Suspended;
                    let variant = profile_badge(&status);
                    let status_label = format!("{status:?}");
                    view! {
                        <Card>
                            <CardHeader>
                                <div style="display: flex; align-items: center; gap: 0.625rem;">
                                    <CardTitle>{p.name.clone()}</CardTitle>
                                    <Badge variant=variant>{status_label}</Badge>
                                </div>
                            </CardHeader>
                            <CardContent>
                                <dl style="margin: 0 0 1rem;">
                                    <FieldRow label="Profile id">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {p.profile_id.to_string()}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Agency id">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {p.agency_id.to_string()}
                                        </span>
                                    </FieldRow>
                                </dl>
                                <div style="display: flex; gap: 0.5rem; flex-wrap: wrap;">
                                    <Button
                                        variant=ButtonVariant::Secondary
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_active)
                                        on_click={UiHandler::new({
                                            let suspend = suspend.clone();
                                            move || suspend("suspend")
                                        })}
                                    >
                                        <Icon name=IconName::XCircle size=15 />
                                        "Suspend"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Secondary
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_suspended)
                                        on_click={UiHandler::new({
                                            let reactivate = reactivate.clone();
                                            move || reactivate("reactivate")
                                        })}
                                    >
                                        <Icon name=IconName::CheckCircle2 size=15 />
                                        "Reactivate"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Destructive
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act)
                                        on_click={UiHandler::new({
                                            let archive = archive.clone();
                                            move || archive("archive")
                                        })}
                                    >
                                        <Icon name=IconName::Trash2 size=15 />
                                        "Archive"
                                    </Button>
                                </div>
                                <p style="margin: 0.75rem 0 0; font-size: 0.75rem; color: var(--muted-foreground);">
                                    "Archive is terminal."
                                </p>
                            </CardContent>
                        </Card>
                    }
                        .into_any()
                }
                // No record yet: a spinner while the first fetch is in
                // flight, nothing once it has failed (the alert above
                // already says what went wrong).
                None if loading.get() => view! { <Loading label="profile" /> }.into_any(),
                None => ().into_any(),
            }}
        </section>
    }
}
