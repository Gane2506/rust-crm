// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Feedback primitives: [`Progress`], [`Skeleton`], [`Avatar`],
//! [`Tooltip`], and the [`Toast`] / [`Toaster`] notification stack.
//!
//! All token-driven. Toasts live on a [`Toasts`] signal owned by the
//! app shell: each toast auto-dismisses after a per-variant duration,
//! the stack is capped at three visible toasts, and the × button on a
//! toast dismisses it manually at any time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use leptos::prelude::*;
use wasm_bindgen::{closure::Closure, JsCast};

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

/// Maximum number of toasts visible at once.
///
/// Three stacked toasts fit the bottom-right corner without covering
/// page content. Six (the R4-01 report: six successive Campaigns
/// actions) covered the Steps table and the "Back to campaigns"
/// button. When a new toast arrives past the cap, the oldest drops.
const MAX_VISIBLE_TOASTS: usize = 3;

/// How long a toast stays on screen before auto-dismissing.
///
/// Success and info toasts are confirmations: a one-line message
/// reads in ~2–3s, so 4s leaves margin without lingering. Warnings
/// and errors describe something that needs the user's attention —
/// and, for errors, a decision (retry, fix the input) — so they get
/// 8s, twice as long. These are judgment calls for this UI, not a
/// standard; the split is "glanceable confirmation" versus "needs a
/// read". Warnings sit with errors: both flag something wrong, and a
/// warning the user never reads is a warning wasted.
fn toast_duration(variant: ToastVariant) -> Duration {
    match variant {
        ToastVariant::Info | ToastVariant::Success => Duration::from_secs(4),
        ToastVariant::Warning | ToastVariant::Error => Duration::from_secs(8),
    }
}

/// Pushes a toast onto `toasts`.
///
/// Identical toasts (same title, description, and variant) already on
/// screen are coalesced: a duplicate push is dropped instead of
/// stacking — and re-announcing — the same message. Rapid identical
/// updates (e.g. a retry loop reporting the same failure) therefore
/// announce once, not once per attempt.
///
/// A new toast past the cap drops the oldest one, so the stack never
/// grows past the corner of the screen and covers page content.
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
            // Cap: the oldest toast drops so the stack stays within
            // MAX_VISIBLE_TOASTS. The list is append-ordered, so the
            // oldest entries are at the front.
            if list.len() > MAX_VISIBLE_TOASTS {
                list.drain(..list.len() - MAX_VISIBLE_TOASTS);
            }
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

/// Owns one toast's auto-dismiss timer.
///
/// Created by `schedule_toast_timer` and stored in the toast's
/// component arena (`StoredValue::new_local`): when the toast
/// unmounts — manual dismiss, cap, or auto-dismiss — the arena drops
/// this guard and the `Drop` impl cancels the pending `setTimeout`,
/// so no timer is left behind. Dropping also releases the JS
/// callback, so the closure itself does not leak either. A timer
/// that already fired has a stale handle; clearing it is a harmless
/// no-op per the `clearTimeout` contract.
struct ToastTimer {
    handle: i32,
    _callback: Closure<dyn FnMut()>,
}

impl Drop for ToastTimer {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            window.clear_timeout_with_handle(self.handle);
        }
    }
}

/// Schedules the auto-dismiss timer for one toast.
///
/// Returns the guard that owns the timer, or `None` when no timer
/// could be scheduled (no window, or `setTimeout` failed). The caller
/// must keep the guard alive (arena-stored) for the toast's lifetime:
/// dropping it cancels the pending timeout. The callback dismisses by
/// the toast's unique `id`, so a timer that fires after the toast is
/// already gone removes nothing — it can never remove a different
/// toast, because ids are unique per push.
fn schedule_toast_timer(toasts: Toasts, id: u64, duration: Duration) -> Option<ToastTimer> {
    let window = web_sys::window()?;
    let callback = Closure::<dyn FnMut()>::new(move || dismiss_toast(toasts, id));
    let handle = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            duration.as_millis() as i32,
        )
        .ok()?;
    Some(ToastTimer {
        handle,
        _callback: callback,
    })
}

/// One toast in the stack, with its own auto-dismiss timer.
///
/// The `For` in [`Toaster`] is keyed by toast id, so each toast gets
/// its own component instance. The effect below runs once, when this
/// toast mounts, and schedules a `setTimeout` that dismisses the
/// toast after the per-variant duration. The effect reads
/// no signal, so it never re-runs — a re-render of the stack cannot
/// restart the timer.
///
/// The live-region contract from HARD-03 is unchanged: this is the
/// same single live region per toast (`role="status"`, errors
/// `role="alert"`, `aria-atomic="true"`), and the [`Toaster`]
/// container is still not a live region. Auto-dismiss only removes
/// the node; it never re-announces it. Manual dismiss (the × button)
/// still calls the optional `on_dismiss` handler; auto-dismiss does
/// not — the handler reports a user dismissal, not a timeout.
#[component]
fn ToastItem(
    toast: Toast,
    toasts: Toasts,
    #[prop(optional)] on_dismiss: Option<UiValueHandler<u64>>,
) -> impl IntoView {
    let id = toast.id;
    let duration = toast_duration(toast.variant);
    // Schedule once, on mount. The effect reads no signal, so it
    // never re-runs. The guard is arena-stored: it lives until this
    // toast's owner disposes (unmount), when its Drop cancels the
    // pending timeout. Dropping the StoredValue handle here does not
    // drop the guard — the arena owns the value.
    Effect::new(move |_| {
        let _timer: StoredValue<Option<ToastTimer>, LocalStorage> =
            StoredValue::new_local(schedule_toast_timer(toasts, id, duration));
    });
    let (accent, icon) = toast_accent(toast.variant);
    // Errors are assertive (`alert`); everything else is polite
    // (`status`) so it never interrupts the user.
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
                    view! {
                        <ToastItem toast=toast toasts=toasts on_dismiss=on_dismiss />
                    }
                }
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toast list under a throwaway owner: signals need a current
    /// owner, and the timer code is never touched here (no DOM).
    fn test_toasts() -> (Owner, Toasts) {
        let owner = Owner::new();
        owner.set();
        (owner, RwSignal::new(Vec::new()))
    }

    fn titles(toasts: Toasts) -> Vec<String> {
        toasts
            .get_untracked()
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }

    #[test]
    fn push_toast_caps_at_three_oldest_drops() {
        let (_owner, toasts) = test_toasts();
        push_toast(toasts, "one", None, ToastVariant::Info);
        push_toast(toasts, "two", None, ToastVariant::Info);
        push_toast(toasts, "three", None, ToastVariant::Info);
        push_toast(toasts, "four", None, ToastVariant::Info);
        assert_eq!(titles(toasts), ["two", "three", "four"]);
    }

    #[test]
    fn push_toast_coalescing_survives_the_cap() {
        let (_owner, toasts) = test_toasts();
        push_toast(toasts, "one", None, ToastVariant::Info);
        push_toast(toasts, "two", None, ToastVariant::Info);
        push_toast(toasts, "three", None, ToastVariant::Info);
        // A duplicate push is dropped, not stacked — and it must not
        // knock the oldest toast off the stack either.
        push_toast(toasts, "one", None, ToastVariant::Info);
        assert_eq!(titles(toasts), ["one", "two", "three"]);
    }

    #[test]
    fn dismiss_toast_removes_only_its_id() {
        let (_owner, toasts) = test_toasts();
        push_toast(toasts, "one", None, ToastVariant::Info);
        push_toast(toasts, "two", None, ToastVariant::Info);
        let first = toasts.get_untracked()[0].id;
        dismiss_toast(toasts, first);
        assert_eq!(titles(toasts), ["two"]);
    }

    #[test]
    fn toast_durations_split_confirmations_from_attention() {
        assert_eq!(toast_duration(ToastVariant::Info), Duration::from_secs(4));
        assert_eq!(
            toast_duration(ToastVariant::Success),
            Duration::from_secs(4)
        );
        assert_eq!(
            toast_duration(ToastVariant::Warning),
            Duration::from_secs(8)
        );
        assert_eq!(toast_duration(ToastVariant::Error), Duration::from_secs(8));
    }
}
