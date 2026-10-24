// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Feedback primitives: [`Progress`], [`Skeleton`], [`Avatar`],
//! [`Tooltip`], and the [`Toast`] / [`Toaster`] notification stack.
//!
//! All token-driven. Toasts are dismissed manually (no timers), driven
//! by a [`Toasts`] signal owned by the app shell.

use std::sync::atomic::{AtomicU64, Ordering};

use leptos::prelude::*;

use super::callback::UiValueHandler;
use super::icon::{Icon, IconName};

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// Determinate progress bar.
///
/// # Props
/// - `value`: 0–100 (clamped).
#[component]
pub fn Progress(value: f64) -> impl IntoView {
    let pct = value.clamp(0.0, 100.0);
    view! {
        <div
            role="progressbar"
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow={pct.to_string()}
            style="height: 0.5rem; width: 100%; overflow: hidden; border-radius: 9999px; background: var(--secondary);"
        >
            <div style={format!(
                "height: 100%; width: {pct}%; border-radius: 9999px; background: var(--primary); transition: width 0.2s ease;"
            )} />
        </div>
    }
}

// ---------------------------------------------------------------------------
// Skeleton
// ---------------------------------------------------------------------------

/// Loading placeholder with a pulse animation.
///
/// # Props
/// - `width` / `height`: CSS sizes (default `"100%"` / `"1rem"`).
/// - `circle`: renders a round placeholder (avatars).
#[component]
pub fn Skeleton(
    #[prop(optional)] width: Option<&'static str>,
    #[prop(optional)] height: Option<&'static str>,
    #[prop(optional)] circle: bool,
) -> impl IntoView {
    view! {
        <div
            aria-hidden="true"
            class="crm-pulse"
            style={format!(
                "width: {}; height: {}; border-radius: {}; background: var(--muted);",
                width.unwrap_or("100%"),
                height.unwrap_or("1rem"),
                if circle { "9999px" } else { "var(--radius-md)" },
            )}
        />
    }
}

// ---------------------------------------------------------------------------
// Avatar
// ---------------------------------------------------------------------------

/// Initials of the first two whitespace-separated words, uppercased.
fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .collect::<String>()
        .to_uppercase()
}

/// Circular avatar with initials fallback.
///
/// # Props
/// - `name`: full name used for initials and the accessible label.
/// - `size`: pixel diameter (default 32).
#[component]
pub fn Avatar(name: String, #[prop(optional)] size: Option<u16>) -> impl IntoView {
    let size = size.unwrap_or(32);
    let label = format!("Avatar for {name}");
    let glyphs = initials(&name);
    view! {
        <span
            title=name
            aria-label=label
            role="img"
            style={format!(
                "display: inline-flex; align-items: center; justify-content: center; \
                 width: {size}px; height: {size}px; border-radius: 9999px; flex-shrink: 0; \
                 background: var(--secondary); color: var(--secondary-foreground); \
                 font-size: {}px; font-weight: 600;",
                size / 2 + 2,
            )}
        >
            {glyphs}
        </span>
    }
}

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

/// Minimal tooltip via the native `title` attribute (no JS positioning).
///
/// # Props
/// - `tip`: the tooltip text.
/// - `children`: the hovered content.
#[component]
pub fn Tooltip(tip: &'static str, children: Children) -> impl IntoView {
    view! {
        <span title=tip style="display: inline-flex; align-items: center;">
            {children()}
        </span>
    }
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

/// Toast severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastVariant {
    /// Neutral (default).
    #[default]
    Info,
    /// Green accent.
    Success,
    /// Amber accent.
    Warning,
    /// Red accent.
    Error,
}

/// One toast notification.
#[derive(Debug, Clone)]
pub struct Toast {
    /// Unique id (for dismissal).
    pub id: u64,
    /// Bold headline.
    pub title: String,
    /// Optional body text.
    pub description: Option<String>,
    /// Severity styling.
    pub variant: ToastVariant,
}

/// The toast list signal, owned by the app shell and passed to [`Toaster`].
pub type Toasts = RwSignal<Vec<Toast>>;

static NEXT_TOAST_ID: AtomicU64 = AtomicU64::new(1);

/// Pushes a toast onto `toasts`.
///
/// Identical toasts (same title, description, and variant) already on
/// screen are coalesced: a duplicate push is dropped instead of
/// stacking — and re-announcing — the same message. Rapid identical
/// updates (e.g. a retry loop reporting the same failure) therefore
/// announce once, not once per attempt.
///
/// # Props (arguments)
/// - `toasts`: the [`Toasts`] signal.
/// - `title` / `description`: content.
/// - `variant`: severity.
pub fn push_toast(
    toasts: Toasts,
    title: impl Into<String>,
    description: Option<String>,
    variant: ToastVariant,
) {
    let title = title.into();
    let id = NEXT_TOAST_ID.fetch_add(1, Ordering::Relaxed);
    toasts.update(|list| {
        // Coalesce: an identical toast already visible means the screen
        // reader has announced this message; stacking another copy
        // would spam the announcement without adding information.
        let duplicate = list
            .iter()
            .any(|t| t.title == title && t.description == description && t.variant == variant);
        if !duplicate {
            list.push(Toast {
                id,
                title,
                description,
                variant,
            });
        }
    });
}

/// Dismisses the toast with `id`.
pub fn dismiss_toast(toasts: Toasts, id: u64) {
    toasts.update(|list| list.retain(|t| t.id != id));
}

fn toast_accent(variant: ToastVariant) -> (&'static str, IconName) {
    match variant {
        ToastVariant::Info => ("var(--info)", IconName::Info),
        ToastVariant::Success => ("var(--success)", IconName::CheckCircle2),
        ToastVariant::Warning => ("var(--warning)", IconName::AlertTriangle),
        ToastVariant::Error => ("var(--destructive)", IconName::XCircle),
    }
}

/// Fixed bottom-right toast stack.
///
/// The container itself is deliberately NOT a live region: each toast
/// carries its own `role` (`status`, or `alert` for errors), so a new
/// toast announces itself exactly once. A live container wrapping live
/// toasts is a nested live region — screen readers announce the
/// addition twice and may re-read the whole stack on every change.
/// `aria-atomic` on each toast makes the reader speak the title and
/// description together.
#[component]
pub fn Toaster(
    toasts: Toasts,
    #[prop(optional)] on_dismiss: Option<UiValueHandler<u64>>,
) -> impl IntoView {
    view! {
        <div
            style="position: fixed; bottom: 1rem; right: 1rem; z-index: 100; display: flex; flex-direction: column; gap: 0.5rem; width: min(22rem, calc(100vw - 2rem));"
        >
            <For
                each=move || toasts.get()
                key=|toast| toast.id
                children=move |toast| {
                    let id = toast.id;
                    let (accent, icon) = toast_accent(toast.variant);
                    // Errors are assertive (`alert`); everything else is
                    // polite (`status`) so it never interrupts the user.
                    let role = if toast.variant == ToastVariant::Error {
                        "alert"
                    } else {
                        "status"
                    };
                    let dismiss = move |_| {
                        dismiss_toast(toasts, id);
                        if let Some(handler) = on_dismiss {
                            handler.call(id);
                        }
                    };
                    view! {
                        <div
                            role=role
                            aria-atomic="true"
                            class="crm-toast-in"
                            style={format!(
                                "display: flex; gap: 0.625rem; align-items: flex-start; padding: 0.875rem 1rem; \
                                 background: var(--popover); color: var(--popover-foreground); \
                                 border: 1px solid var(--border); border-left: 3px solid {accent}; \
                                 border-radius: var(--radius-md); box-shadow: 0 8px 24px oklch(0 0 0 / 0.12);"
                            )}
                        >
                            <span style={format!("color: {accent}; margin-top: 0.125rem;")}>
                                <Icon name=icon size=16 />
                            </span>
                            <div style="flex: 1; min-width: 0;">
                                <p style="margin: 0; font-size: 0.875rem; font-weight: 600;">{toast.title}</p>
                                {toast
                                    .description
                                    .map(|d| {
                                        view! {
                                            <p style="margin: 0.25rem 0 0; font-size: 0.8125rem; color: var(--muted-foreground);">
                                                {d}
                                            </p>
                                        }
                                    })}
                            </div>
                            <button
                                type="button"
                                aria-label="Dismiss notification"
                                on:click=dismiss
                                style="background: transparent; border: none; color: var(--muted-foreground); padding: 0.125rem; border-radius: var(--radius-sm);"
                            >
                                <Icon name=IconName::X size=14 />
                            </button>
                        </div>
                    }
                }
            />
        </div>
    }
}
