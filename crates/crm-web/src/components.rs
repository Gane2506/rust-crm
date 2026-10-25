// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Shared state components for the CRM frontend: loading, empty, and
//! error states, in the shadcn-style design language.
//!
//! Every page renders the same loading → error-with-retry → empty →
//! content ladder so the UI states stay predictable.

use leptos::prelude::*;

use crate::ui::{Button, ButtonVariant, Icon, IconName, UiHandler};

/// Loading indicator with a spinning icon.
///
/// # Props
/// - `label`: what is being loaded, e.g. `"contacts"`.
#[component]
pub fn Loading(label: &'static str) -> impl IntoView {
    view! {
        <p
            aria-busy="true"
            style="display: inline-flex; align-items: center; gap: 0.5rem; \
                   color: var(--muted-foreground); font-size: 0.875rem;"
        >
            <Icon name=IconName::Loader2 size=16 spin=true />
            {format!("Loading {label}…")}
        </p>
    }
}

/// Empty-list state: dashed panel with an icon and message.
///
/// # Props
/// - `message`: e.g. `"No contacts yet."`.
#[component]
pub fn EmptyState(message: &'static str) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; \
                   padding: 2.5rem 1rem; border: 1px dashed var(--border); \
                   border-radius: var(--radius-lg); background: var(--muted);">
            <span style="display: inline-flex; align-items: center; justify-content: center; \
                         width: 2.5rem; height: 2.5rem; border-radius: 9999px; \
                         background: var(--card); color: var(--muted-foreground); \
                         border: 1px solid var(--border); margin-bottom: 0.75rem;" aria-hidden="true">
                <Icon name=IconName::Search size=18 />
            </span>
            <p style="margin: 0; color: var(--muted-foreground); font-size: 0.875rem;">{message}</p>
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
pub fn ErrorAlert(message: String, #[prop(optional)] on_retry: Option<UiHandler>) -> impl IntoView {
    view! {
        <div
            role="alert"
            style="display: flex; align-items: flex-start; gap: 0.625rem; \
                   padding: 0.875rem 1rem; border-radius: var(--radius-md); \
                   border: 1px solid var(--destructive); \
                   background: var(--destructive-light); color: var(--destructive-dark); \
                   font-size: 0.875rem;"
        >
            <Icon name=IconName::XCircle size=16 />
            <div style="flex: 1;">
                <p style="margin: 0 0 0.5rem;">{message}</p>
                {on_retry
                    .map(|retry| {
                        view! {
                            // `type="button"`: the alert can render inside a
                            // `<form>`; without this the button would submit it.
                            <Button
                                variant=ButtonVariant::Outline
                                size=crate::ui::ButtonSize::Sm
                                btn_type="button"
                                on_click=retry
                            >
                                "Retry"
                            </Button>
                        }
                    })}
            </div>
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
        <p
            role="status"
            style="display: flex; align-items: center; gap: 0.5rem; \
                   padding: 0.75rem 1rem; border-radius: var(--radius-md); \
                   border: 1px solid var(--success); background: var(--success-light); \
                   color: var(--success-dark); font-size: 0.875rem; margin: 0;"
        >
            <Icon name=IconName::CheckCircle2 size=16 />
            {message}
        </p>
    }
}

/// Small muted helper text under a section.
#[component]
pub fn Hint(text: String) -> impl IntoView {
    view! {
        <p style="margin: 0.5rem 0 0; font-size: 0.75rem; color: var(--muted-foreground);">
            {text}
        </p>
    }
}

/// Renders `count` with correct pluralization, e.g. "3 contacts".
#[component]
pub fn CountLine(count: u64, singular: &'static str, plural: &'static str) -> impl IntoView {
    view! {
        <p style="margin: 0 0 0.75rem; font-size: 0.8125rem; color: var(--muted-foreground);">
            {format!(
                "{count} {}",
                if count == 1 { singular } else { plural }
            )}
        </p>
    }
}

/// Page header: title + description + optional action slot.
#[component]
pub fn PageHeader(
    title: &'static str,
    #[prop(optional)] description: Option<&'static str>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        // `flex-wrap` is load-bearing on narrow viewports (AUDIT-10):
        // without it a long title plus action buttons overflow the
        // 390px content column instead of dropping the actions below.
        // Rows that fit stay on one line, so desktop is unchanged.
        <div style="display: flex; align-items: flex-start; justify-content: space-between; \
                   flex-wrap: wrap; gap: 1rem; margin-bottom: 1.25rem;">
            <div>
                <h2 style="margin: 0; font-size: 1.5rem; font-weight: 700; letter-spacing: -0.02em;">
                    {title}
                </h2>
                {description
                    .map(|d| {
                        view! {
                            <p style="margin: 0.25rem 0 0; font-size: 0.875rem; color: var(--muted-foreground);">
                                {d}
                            </p>
                        }
                    })}
            </div>
            <div style="display: flex; gap: 0.5rem; align-items: center; flex-shrink: 0;">
                {children.map(|c| c())}
            </div>
        </div>
    }
}

/// Labeled value row for record pages.
#[component]
pub fn FieldRow(label: &'static str, children: Children) -> impl IntoView {
    view! {
        <div style="display: grid; grid-template-columns: 10rem 1fr; gap: 0.75rem; \
                   padding: 0.625rem 0; border-bottom: 1px solid var(--border); \
                   font-size: 0.875rem;">
            <dt style="margin: 0; color: var(--muted-foreground); font-weight: 500;">{label}</dt>
            <dd style="margin: 0; min-width: 0; overflow-wrap: anywhere;">{children()}</dd>
        </div>
    }
}
