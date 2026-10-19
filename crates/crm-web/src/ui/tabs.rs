// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! shadcn-style tabs: [`Tabs`], [`TabsList`], [`TabsTrigger`],
//! [`TabsContent`].
//!
//! [`Tabs`] owns the active-value signal and shares it through context;
//! triggers and content panels consume it. Token-driven styling.

use leptos::prelude::*;

use std::sync::atomic::{AtomicU64, Ordering};

/// Context shared by [`Tabs`] with its triggers and content panels.
#[derive(Debug, Clone, Copy)]
struct TabsContext {
    active: RwSignal<String>,
    /// Unique per-instance prefix so tab/panel ids stay unique when
    /// several [`Tabs`] share the same values on one page.
    id_prefix: u64,
}

static NEXT_TABS_ID: AtomicU64 = AtomicU64::new(1);

/// Tab container: owns the active tab signal.
///
/// # Props
/// - `default_value`: initially active tab value (default `""`).
/// - `children`: [`TabsList`] plus [`TabsContent`] panels.
#[component]
pub fn Tabs(
    #[prop(optional)] default_value: Option<&'static str>,
    children: Children,
) -> impl IntoView {
    let ctx = TabsContext {
        active: RwSignal::new(default_value.unwrap_or("").to_string()),
        id_prefix: NEXT_TABS_ID.fetch_add(1, Ordering::Relaxed),
    };
    provide_context(ctx);
    view! { <div>{children()}</div> }
}

fn tabs_context() -> TabsContext {
    use_context::<TabsContext>().expect("TabsTrigger/TabsContent must be inside <Tabs>")
}

/// Sanitizes a tab value into an id-safe fragment.
fn id_fragment(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Tab list: muted pill container for triggers.
#[component]
pub fn TabsList(children: Children) -> impl IntoView {
    view! {
        <div
            role="tablist"
            style="display: inline-flex; align-items: center; gap: 0.25rem; padding: 0.25rem; border-radius: var(--radius-lg); background: var(--muted); color: var(--muted-foreground);"
        >
            {children()}
        </div>
    }
}

/// One tab trigger.
///
/// # Props
/// - `value`: tab identifier, matched against [`TabsContent`] values.
/// - `children`: trigger label.
#[component]
pub fn TabsTrigger(value: &'static str, children: Children) -> impl IntoView {
    let ctx = tabs_context();
    let is_active = move || ctx.active.get() == value;
    let tab_id = format!("crm-tab-{}-{}", ctx.id_prefix, id_fragment(value));
    let panel_id = format!("crm-panel-{}-{}", ctx.id_prefix, id_fragment(value));
    view! {
        <button
            type="button"
            role="tab"
            id=tab_id.clone()
            aria-selected=move || is_active().to_string()
            aria-controls=panel_id
            on:click=move |_| ctx.active.set(value.to_string())
            style=move || {
                format!(
                    "display: inline-flex; align-items: center; justify-content: center; \
                     white-space: nowrap; border-radius: var(--radius-md); border: none; \
                     padding: 0.375rem 0.75rem; font-size: 0.875rem; font-weight: 500; \
                     transition: background 0.15s ease, color 0.15s ease; {}",
                    if is_active() {
                        "background: var(--card); color: var(--foreground); box-shadow: 0 1px 2px oklch(0 0 0 / 0.08);"
                    } else {
                        "background: transparent; color: var(--muted-foreground); cursor: pointer;"
                    },
                )
            }
        >
            {children()}
        </button>
    }
}

/// Tab content panel, shown when its `value` is active.
///
/// # Props
/// - `value`: tab identifier.
/// - `children`: panel content.
#[component]
pub fn TabsContent(value: &'static str, children: ChildrenFn) -> impl IntoView {
    let ctx = tabs_context();
    let tab_id = format!("crm-tab-{}-{}", ctx.id_prefix, id_fragment(value));
    let panel_id = format!("crm-panel-{}-{}", ctx.id_prefix, id_fragment(value));
    view! {
        <Show when=move || ctx.active.get() == value>
            <div
                role="tabpanel"
                id=panel_id.clone()
                aria-labelledby=tab_id.clone()
                style="margin-top: 0.75rem;"
            >
                {children()}
            </div>
        </Show>
    }
}
