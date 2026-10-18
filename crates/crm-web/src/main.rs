// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! `crm-web`: Leptos client-side-rendered single-page application.
//!
//! Compiles to `wasm32-unknown-unknown` (`cargo build -p crm-web --target
//! wasm32-unknown-unknown --release`). Styling is declarative CSS via inline
//! `style` attributes only — no handwritten JavaScript, per the handoff's
//! Rust-only rule. Component contracts (props, signals, API calls) are
//! documented per component; visual polish lands in task `06-UI-02`.

use leptos::mount::mount_to_body;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;

use api::logout;
use assets::AssetsPage;
use audit::AuditPage;
use contacts::{ContactDetail, ContactsPage, MergePage};
use login::LoginForm;
use profiles::{ProfileDetail, ProfilesPage};
use ui::components::{AppShell, PageHeader, Tile};

mod api;
mod assets;
mod audit;
mod components;
mod contacts;
mod login;
mod profiles;
mod styles;
mod ui;

/// Root application shell: login gate plus nav header and routed content.
///
/// The bearer token lives in a Leptos signal (memory only — it is never
/// persisted). Until login succeeds, the login form is the whole app; the
/// router (and every route in it) only renders for authenticated sessions.
#[component]
fn App() -> impl IntoView {
    // `None` = not logged in; `Some(token)` = bearer token in memory.
    let token = RwSignal::new(None::<String>);
    let on_logged_in = move |new_token: String| token.set(Some(new_token));
    // Server-side revocation first, then drop the local token either way.
    let on_logout = move || {
        if let Some(current) = token.get_untracked() {
            spawn_local(async move {
                logout(&current).await.ok();
            });
        }
        token.set(None);
    };

    view! {
        {move || match token.get() {
            None => view! { <LoginForm on_logged_in=on_logged_in /> }.into_any(),
            Some(token) => {
                view! { <Shell token=token on_logout=on_logout /> }.into_any()
            }
        }}
    }
}

/// Authenticated shell: Twenty-style sidebar + utility bar + route table.
///
/// # Props
/// - `token`: the in-memory bearer token, cloned per route view.
/// - `on_logout`: revokes server-side, then clears the token signal.
#[component]
fn Shell(token: String, on_logout: impl Fn() + Clone + Send + 'static) -> impl IntoView {
    view! {
        <Router>
            <AppShell on_logout=on_logout>
                <Routes fallback=|| view! { <p>"Page not found."</p> }>
                    <Route path=path!("/") view=Dashboard />
                    <Route
                        path=path!("/contacts")
                        view={
                            let token = token.clone();
                            move || view! { <ContactsPage token=token.clone() /> }
                        }
                    />
                    <Route
                        path=path!("/contacts/merge")
                        view={
                            let token = token.clone();
                            move || view! { <MergePage token=token.clone() /> }
                        }
                    />
                    <Route
                        path=path!("/contacts/:id")
                        view={
                            let token = token.clone();
                            move || {
                                let params = leptos_router::hooks::use_params_map();
                                let id = params.get().get("id").unwrap_or_default();
                                let token = token.clone();
                                view! { <ContactDetail token=token id=id /> }
                            }
                        }
                    />
                    <Route
                        path=path!("/profiles")
                        view={
                            let token = token.clone();
                            move || view! { <ProfilesPage token=token.clone() /> }
                        }
                    />
                    <Route
                        path=path!("/profiles/:id")
                        view={
                            let token = token.clone();
                            move || {
                                let params = leptos_router::hooks::use_params_map();
                                let id = params.get().get("id").unwrap_or_default();
                                let token = token.clone();
                                view! { <ProfileDetail token=token profile_id=id /> }
                            }
                        }
                    />
                    <Route
                        path=path!("/assets")
                        view={
                            let token = token.clone();
                            move || view! { <AssetsPage token=token.clone() /> }
                        }
                    />
                    <Route
                        path=path!("/audit")
                        view={
                            let token = token.clone();
                            move || view! { <AuditPage token=token.clone() /> }
                        }
                    />
                </Routes>
            </AppShell>
        </Router>
    }
}

/// Agency overview: quick-link tiles into each workspace area.
///
/// Full metric dashboards land in the 15-DASH workstream; until then this
/// page is an intentional launchpad rather than a bare placeholder.
#[component]
fn Dashboard() -> impl IntoView {
    view! {
        <section aria-label="Dashboard">
            <PageHeader
                title="Dashboard"
                description="Agency overview — jump into a workspace area."
            />
            <div style=styles::TILE_GRID>
                <Tile
                    href="/contacts"
                    glyph="◉"
                    title="Contacts"
                    description="People and companies the agency works with."
                />
                <Tile
                    href="/profiles"
                    glyph="◈"
                    title="Client profiles"
                    description="Agency-operated client workspaces."
                />
                <Tile
                    href="/assets"
                    glyph="▤"
                    title="Assets"
                    description="Content-addressed files and documents."
                />
                <Tile
                    href="/audit"
                    glyph="⌗"
                    title="Audit log"
                    description="Tamper-evident security event history."
                />
            </div>
        </section>
    }
}

fn main() {
    mount_to_body(|| view! { <App /> });
}
