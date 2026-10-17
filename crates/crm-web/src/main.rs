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
use leptos_router::components::{Route, Router, Routes, A};
use leptos_router::path;

mod api;
mod contacts;

/// Root application shell: nav header plus routed content.
#[component]
fn App() -> impl IntoView {
    view! {
        <Router>
            <div style="font-family: system-ui, sans-serif; max-width: 960px; margin: 0 auto; padding: 1rem;">
                <header style="display: flex; align-items: center; gap: 1.5rem; border-bottom: 1px solid #e5e7eb; padding-bottom: 0.75rem; margin-bottom: 1rem;">
                    <h1 style="font-size: 1.25rem; margin: 0;">"Rust CRM"</h1>
                    <nav style="display: flex; gap: 1rem;">
                        <A href="/">"Dashboard"</A>
                        <A href="/contacts">"Contacts"</A>
                    </nav>
                </header>
                <main>
                    <Routes fallback=|| view! { <p>"Page not found."</p> }>
                        <Route path=path!("/") view=Dashboard />
                        <Route path=path!("/contacts") view=ContactsPage />
                    </Routes>
                </main>
            </div>
        </Router>
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
