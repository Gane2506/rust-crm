// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Client-profile pages: list + create, and detail with lifecycle actions.
//!
//! Profiles are agency-level resources (`ClientProfile` grant at agency
//! scope). The caller's agency comes from the session — the UI never
//! sends an agency id.

use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::profile::{ClientProfile, ProfileStatus};

use crate::api::{create_profile, fetch_profile, fetch_profiles, transition_profile, ApiError};
use crate::components::{EmptyState, ErrorAlert, Loading, OkAlert, PageHeader};
use crate::styles;

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
    let retry = move || reload.update(|n| *n = n.wrapping_add(1));

    view! {
        <section aria-label="Client profiles" style=styles::SECTION>
            <PageHeader title="Client profiles" />
            <ProfileForm token={token.clone()} on_created=retry />
            <h3 style=styles::H3>"Profiles"</h3>
            {move || match profiles.get() {
                None => view! { <Loading label="profiles" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() on_retry={std::rc::Rc::new(retry) as std::rc::Rc<dyn Fn()>} /> }.into_any()
                }
                Some(Ok(page)) if page.profiles.is_empty() => {
                    view! { <EmptyState message="No profiles yet." /> }.into_any()
                }
                Some(Ok(page)) => {
                    view! {
                        <p style=styles::MUTED>
                            {format!(
                                "{} profile{} total",
                                page.total,
                                if page.total == 1 { "" } else { "s" },
                            )}
                        </p>
                        <ul style="list-style: none; margin: 0; padding: 0; display: grid; gap: 0.75rem;">
                            <For
                                each=move || page.profiles.clone()
                                key=|profile| profile.profile_id
                                let:profile
                            >
                                <ProfileRow profile=profile />
                            </For>
                        </ul>
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
fn ProfileForm(token: String, on_created: impl Fn() + Clone + 'static) -> impl IntoView {
    let name_ref = NodeRef::<Input>::new();
    let status = RwSignal::new(None::<Result<String, String>>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let name = name_ref
            .get()
            .map(|input| input.value().trim().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            status.set(Some(Err("Enter a profile name.".to_string())));
            return;
        }
        submitting.set(true);
        status.set(None);
        let on_created = on_created.clone();
        let token = token.clone();
        spawn_local(async move {
            match create_profile(&name, &token).await {
                Ok(profile) => {
                    if let Some(input) = name_ref.get() {
                        input.set_value("");
                    }
                    status.set(Some(Ok(format!("Profile created: {}", profile.name))));
                    on_created();
                }
                Err(error) => status.set(Some(Err(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <form on:submit=on_submit style=styles::CARD>
            <h3 style="margin: 0 0 0.5rem;">"New profile"</h3>
            <div style=styles::FORM>
                <div>
                    <label for="profile-name" style=styles::LABEL>"Name"</label>
                    <input
                        id="profile-name"
                        type="text"
                        node_ref=name_ref
                        placeholder="Acme Corp"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <button type="submit" disabled=move || submitting.get() style=styles::BTN_PRIMARY>
                        {move || if submitting.get() { "Creating…" } else { "Create profile" }}
                    </button>
                </div>
                {move || match status.get() {
                    None => ().into_any(),
                    Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                    Some(Err(message)) => view! { <ErrorAlert message=message /> }.into_any(),
                }}
            </div>
        </form>
    }
}

/// One profile in the list: name, status pill, link to detail.
#[component]
fn ProfileRow(profile: ClientProfile) -> impl IntoView {
    let id = profile.profile_id.to_string();
    view! {
        <li style=styles::CARD>
            <div style="display: flex; justify-content: space-between; align-items: baseline; gap: 1rem;">
                <A href=format!("/profiles/{id}")>
                    <strong>{profile.name.clone()}</strong>
                </A>
                <span style=styles::PILL>{format!("{:?}", profile.status)}</span>
            </div>
            <p style=styles::MUTED>{format!("id {}", &id[..8])}</p>
        </li>
    }
}

/// Profile detail page: full record plus lifecycle actions.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the profile UUID from the route.
#[component]
pub fn ProfileDetail(token: String, profile_id: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    let profile = RwSignal::new(None::<Result<ClientProfile, ApiError>>);
    let working = RwSignal::new(false);
    {
        let token = token.clone();
        let profile_id = profile_id.clone();
        Effect::new(move |_| {
            reload.get();
            profile.set(None);
            let token = token.clone();
            let profile_id = profile_id.clone();
            spawn_local(async move {
                profile.set(Some(fetch_profile(&profile_id, &token).await));
            });
        });
    }
    let retry = move || reload.update(|n| *n = n.wrapping_add(1));

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
                    Ok(updated) => profile.set(Some(Ok(updated))),
                    Err(error) => profile.set(Some(Err(error))),
                }
                working.set(false);
            });
        }
    };

    view! {
        <section aria-label="Profile detail" style=styles::SECTION>
            <PageHeader title="Profile" />
            {move || match profile.get() {
                None => view! { <Loading label="profile" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() on_retry={std::rc::Rc::new(retry) as std::rc::Rc<dyn Fn()>} /> }.into_any()
                }
                Some(Ok(p)) => {
                    let suspend = transition.clone();
                    let reactivate = transition.clone();
                    let archive = transition.clone();
                    let status = p.status;
                    let can_act = !working.get() && status != ProfileStatus::Archived;
                    let is_active = status == ProfileStatus::Active;
                    let is_suspended = status == ProfileStatus::Suspended;
                    view! {
                        <div style=styles::CARD>
                            <h3 style="margin: 0;">{p.name.clone()}</h3>
                            <p>
                                <span style=styles::PILL>{format!("{:?}", p.status)}</span>
                            </p>
                            <dl style=styles::DL>
                                <dt style=styles::DT>"Profile id"</dt>
                                <dd style=styles::MONO>{p.profile_id.to_string()}</dd>
                                <dt style=styles::DT>"Agency id"</dt>
                                <dd style=styles::MONO>{p.agency_id.to_string()}</dd>
                            </dl>
                            <div style="display: flex; gap: 0.5rem; margin-top: 1rem;">
                                <button
                                    type="button"
                                    on:click=move |_| suspend("suspend")
                                    disabled=move || !can_act || !is_active
                                    style=styles::BTN
                                >
                                    "Suspend"
                                </button>
                                <button
                                    type="button"
                                    on:click=move |_| reactivate("reactivate")
                                    disabled=move || !can_act || !is_suspended
                                    style=styles::BTN
                                >
                                    "Reactivate"
                                </button>
                                <button
                                    type="button"
                                    on:click=move |_| archive("archive")
                                    disabled=move || !can_act
                                    style=styles::BTN_DANGER
                                >
                                    "Archive"
                                </button>
                            </div>
                            <p style=styles::MUTED>"Archive is terminal."</p>
                        </div>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}
