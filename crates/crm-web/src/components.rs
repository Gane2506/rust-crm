// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Shared state components for the CRM frontend: loading, empty, and
//! error states, now in the Twenty-style design language.
//!
//! Every page renders the same loading → error-with-retry → empty →
//! content ladder so the UI states stay predictable. Structural pieces
//! (shell, buttons, tables, headers) live in [`crate::ui::components`].

use leptos::prelude::*;

use crate::styles;
use crate::ui::components::{Btn, BtnVariant, Icon};

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

/// Empty-list state: dashed panel with an icon and message.
///
/// # Props
/// - `message`: e.g. `"No contacts yet."`.
#[component]
pub fn EmptyState(message: &'static str) -> impl IntoView {
    view! {
        <div style=styles::EMPTY_STATE>
            <div style=styles::EMPTY_ICON aria-hidden="true">
                <Icon glyph="◌" />
            </div>
            <p style="margin: 0.5rem 0 0; color: #64748b; font-size: 0.8125rem;">{message}</p>
        </div>
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
                        <Btn
                            variant=BtnVariant::Secondary
                            btn_type="button"
                            on_click={
                                let retry = retry.clone();
                                std::rc::Rc::new(move || retry()) as std::rc::Rc<dyn Fn()>
                            }
                        >
                            "Retry"
                        </Btn>
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
