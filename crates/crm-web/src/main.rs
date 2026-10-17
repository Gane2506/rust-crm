// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! `crm-web`: Leptos client-side-rendered single-page application.
//!
//! Compiles to `wasm32-unknown-unknown` (`cargo build -p crm-web --target
//! wasm32-unknown-unknown --release`). Styling is declarative CSS via inline
//! `style` attributes only — no handwritten JavaScript, per the handoff's
//! Rust-only rule. Full design-system views land in task `06-UI-01`.

use contacts::ContactsPage;
use leptos::mount::mount_to_body;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{Route, Router, Routes, A};
use leptos_router::path;
use login::LoginForm;

use api::logout;

mod api;
mod contacts;
mod login;

/// Root application shell: login gate plus nav header and routed content.
///
/// The bearer token lives in a Leptos signal (memory only — it is never
/// persisted). Until login succeeds, the login form is the whole app.
#[component]
fn App() -> impl IntoView {
    // `None` = not logged in; `Some(token)` = bearer token in memory.
    let token = RwSignal::new(None::<String>);
    let on_logged_in = move |new_token: String| token.set(Some(new_token));
    // Server-side revocation first, then drop the local token either way.
    let on_logout = move |_| {
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
                // Clone per render: the route view is `Fn`, not `FnOnce`.
                let token = token.clone();
                view! {
                    <Router>
                        <div style="font-family: system-ui, sans-serif; max-width: 960px; margin: 0 auto; padding: 1rem;">
                            <header style="display: flex; align-items: center; gap: 1.5rem; border-bottom: 1px solid #e5e7eb; padding-bottom: 0.75rem; margin-bottom: 1rem;">
                                <h1 style="font-size: 1.25rem; margin: 0;">"Rust CRM"</h1>
                                <nav style="display: flex; gap: 1rem;">
                                    <A href="/">"Dashboard"</A>
                                    <A href="/contacts">"Contacts"</A>
                                </nav>
                                <button
                                    on:click=on_logout
                                    style="margin-left: auto; padding: 0.375rem 0.75rem; border: 1px solid #d1d5db; border-radius: 0.375rem; background: white; cursor: pointer;"
                                >
                                    "Log out"
                                </button>
                            </header>
                            <main>
                                <Routes fallback=|| view! { <p>"Page not found."</p> }>
                                    <Route path=path!("/") view=Dashboard />
                                    <Route
                                        path=path!("/contacts")
                                        view=move || { let t = token.clone(); view! { <ContactsPage token=t /> } }
                                    />
                                </Routes>
                            </main>
                        </div>
                    </Router>
                }
                    .into_any()
            }
        }}
    }
}

/// Agency overview placeholder (full dashboards land in task 15-DASH-01).
#[component]
fn Dashboard() -> impl IntoView {
    view! {
        <section>
            <h2>"Dashboard"</h2>
            <p>"Agency overview placeholder."</p>
        </section>
    }
}

fn main() {
    mount_to_body(|| view! { <App /> });
}
