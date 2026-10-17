// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Shared presentational components for the CRM frontend.
//!
//! Small, prop-driven building blocks used across pages: page headers,
//! loading / empty / error states, and labeled form fields. Every page
//! renders the same loading → error-with-retry → empty → content ladder
//! so the UI states stay predictable.

use leptos::prelude::*;

use crate::styles;

/// Page title header.
///
/// # Props
/// - `title`: the page heading text.
#[component]
pub fn PageHeader(title: &'static str) -> impl IntoView {
    view! {
        <h2 style=styles::H2>{title}</h2>
    }
}

/// Loading indicator.
///
/// # Props
/// - `label`: what is being loaded, e.g. `"contacts"`.
#[component]
pub fn Loading(label: &'static str) -> impl IntoView {
    view! {
        <p aria-busy="true" style=styles::MUTED>{format!("Loading {label}…")}</p>
    }
}

/// Empty-list state.
///
/// # Props
/// - `message`: e.g. `"No contacts yet."`.
#[component]
pub fn EmptyState(message: &'static str) -> impl IntoView {
    view! {
        <p style=styles::MUTED>{message}</p>
    }
}

/// Error alert with an optional retry button.
///
/// # Props
/// - `message`: human-readable failure description (never includes PII —
///   callers pass [`ApiError`](crate::api::ApiError)'s display string,
///   which the client keeps secret-free).
/// - `on_retry`: when `Some`, renders a Retry button wired to it.
#[component]
pub fn ErrorAlert(
    message: String,
    #[prop(optional)] on_retry: Option<std::rc::Rc<dyn Fn()>>,
) -> impl IntoView {
    view! {
        <div role="alert" style=styles::ALERT_ERROR>
            <p style="margin: 0 0 0.5rem;">{message}</p>
            {on_retry
                .map(|retry| {
                    view! {
                        // `type="button"`: the alert can render inside a
                        // `<form>`; without this the button would submit it.
                        <button type="button" on:click=move |_| retry() style=styles::BTN>
                            "Retry"
                        </button>
                    }
                })}
        </div>
    }
}

/// Success/status message box.
///
/// # Props
/// - `message`: the confirmation text.
#[component]
pub fn OkAlert(message: String) -> impl IntoView {
    view! {
        <p role="status" style=styles::ALERT_OK>{message}</p>
    }
}
