// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Overlay primitives: [`Dialog`] (signal-driven modal) and
//! [`DropdownMenu`] (button + popover menu).
//!
//! Token-driven surfaces; no positioning JavaScript.

use std::sync::Arc;

use leptos::prelude::*;

use super::icon::{Icon, IconName};

// ---------------------------------------------------------------------------
// Dialog
// ---------------------------------------------------------------------------

/// Modal dialog with overlay.
///
/// # Props
/// - `open`: controls visibility.
/// - `on_close`: called after the dialog closes (optional).
/// - `close_on_escape`: Escape closes the dialog when it has focus
///   (default true).
/// - `title`: optional dialog heading.
/// - `children`: dialog body.
#[component]
pub fn Dialog(
    open: RwSignal<bool>,
    #[prop(optional)] on_close: Option<Arc<dyn Fn() + Send + Sync>>,
    #[prop(default = true)] close_on_escape: bool,
    #[prop(optional)] title: Option<&'static str>,
    children: ChildrenFn,
) -> impl IntoView {
    // `Arc` (not `Rc`): the close handler is captured inside the `<Show>`
    // children closure, which must be `Send`.
    let close: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        open.set(false);
        if let Some(handler) = on_close.as_ref() {
            handler();
        }
    });
    view! {
        <Show when=move || open.get()>
            <div
                role="dialog"
                aria-modal="true"
                tabindex="-1"
                // Cloned inline: the `<Show>` children closure is `Fn` and
                // cannot move a captured handler out on re-render.
                on:click={
                    let close = close.clone();
                    move |_| close()
                }
                on:keydown={
                    let close = close.clone();
                    move |ev: leptos::ev::KeyboardEvent| {
                        if close_on_escape && ev.key() == "Escape" {
                            close();
                        }
                    }
                }
                style="position: fixed; inset: 0; z-index: 50; display: flex; align-items: center; justify-content: center; padding: 1rem; background: oklch(0 0 0 / 0.5);"
            >
                <div
                    role="document"
                    class="crm-fade-in"
                    on:click=|ev| ev.stop_propagation()
                    style="width: 100%; max-width: 28rem; background: var(--card); color: var(--card-foreground); border: 1px solid var(--border); border-radius: var(--radius-lg); box-shadow: 0 24px 64px oklch(0 0 0 / 0.24);"
                >
                    <div style="display: flex; align-items: center; justify-content: space-between; padding: 1.25rem 1.5rem 0;">
                        {title
                            .map(|t| {
                                view! {
                                    <h2 style="margin: 0; font-size: 1.125rem; font-weight: 600; letter-spacing: -0.01em;">
                                        {t}
                                    </h2>
                                }
                            })}
                        <button
                            type="button"
                            aria-label="Close dialog"
                            on:click={
                                let close = close.clone();
                                move |_| close()
                            }
                            style="margin-left: auto; display: inline-flex; align-items: center; justify-content: center; width: 2rem; height: 2rem; border-radius: var(--radius-md); border: none; background: transparent; color: var(--muted-foreground);"
                        >
                            <Icon name=IconName::X size=16 />
                        </button>
                    </div>
                    <div style="padding: 1rem 1.5rem 1.5rem;">{children()}</div>
                </div>
            </div>
        </Show>
    }
}

// ---------------------------------------------------------------------------
// DropdownMenu
// ---------------------------------------------------------------------------

/// Horizontal alignment of the menu popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MenuAlign {
    /// Left edges aligned (default).
    #[default]
    Left,
    /// Right edges aligned.
    Right,
}

/// One dropdown menu entry.
#[derive(Debug, Clone)]
pub struct MenuItem {
    /// Entry label.
    pub label: String,
    /// Optional leading icon.
    pub icon: Option<IconName>,
    /// Rendered in destructive red.
    pub danger: bool,
}

impl MenuItem {
    /// Plain entry.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            danger: false,
        }
    }

    /// Entry with a leading icon.
    pub fn with_icon(label: impl Into<String>, icon: IconName) -> Self {
        Self {
            label: label.into(),
            icon: Some(icon),
            danger: false,
        }
    }

    /// Destructive entry (red).
    pub fn danger(label: impl Into<String>, icon: Option<IconName>) -> Self {
        Self {
            label: label.into(),
            icon,
            danger: true,
        }
    }
}

/// Button + popover menu. Closes on selection or outside click.
///
/// # Props
/// - `items`: menu entries.
/// - `on_select`: called with the entry index on selection.
/// - `align`: popover alignment (default [`MenuAlign::Left`]).
/// - `children`: trigger button content.
#[component]
pub fn DropdownMenu(
    items: Vec<MenuItem>,
    on_select: Arc<dyn Fn(usize) + Send + Sync>,
    #[prop(optional)] align: MenuAlign,
    children: Children,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let align_style = match align {
        MenuAlign::Left => "left: 0;",
        MenuAlign::Right => "right: 0;",
    };
    // `Arc` (not `Rc`): `on_select` is captured inside the `<Show>`
    // children closure, which must be `Send`. `items` and the handler
    // live in `StoredValue`s so the `Fn` render closure can reach them
    // on every re-render without moving anything out.
    let items = StoredValue::new(items);
    let on_select = StoredValue::new(on_select);
    view! {
        <div style="position: relative; display: inline-block;">
            <button
                type="button"
                aria-haspopup="menu"
                aria-expanded=move || open.get().to_string()
                on:click=move |_| open.set(!open.get_untracked())
                style="display: inline-flex; align-items: center; gap: 0.375rem; height: 2.5rem; padding: 0 0.75rem; border-radius: var(--radius-md); border: 1px solid var(--input); background: var(--card); color: var(--foreground); font-size: 0.875rem; font-weight: 500;"
            >
                {children()}
                <Icon name=IconName::ChevronDown size=14 />
            </button>
            <Show when=move || open.get()>
                // Transparent click-catcher: outside clicks close the menu.
                <div
                    on:click=move |_| open.set(false)
                    style="position: fixed; inset: 0; z-index: 40; background: transparent;"
                />
                <div
                    role="menu"
                    class="crm-fade-in"
                    style=format!(
                        "position: absolute; top: calc(100% + 4px); {align_style} z-index: 50; \
                         min-width: 12rem; padding: 0.25rem; background: var(--popover); \
                         color: var(--popover-foreground); border: 1px solid var(--border); \
                         border-radius: var(--radius-md); box-shadow: 0 12px 32px oklch(0 0 0 / 0.14);"
                    )
                >
                    {move || {
                        items.with_value(|items| {
                            items
                                .iter()
                                .enumerate()
                                .map(|(i, item)| {
                                    let select = {
                                        move |_| {
                                            open.set(false);
                                            on_select.with_value(|f| f(i));
                                        }
                                    };
                                    view! {
                                        <button
                                            type="button"
                                            role="menuitem"
                                            on:click=select
                                            style=format!(
                                                "display: flex; align-items: center; gap: 0.5rem; width: 100%; \
                                                 padding: 0.5rem 0.625rem; border: none; border-radius: var(--radius-sm); \
                                                 background: transparent; font-size: 0.875rem; text-align: left; \
                                                 color: {};",
                                                if item.danger {
                                                    "var(--destructive)"
                                                } else {
                                                    "var(--foreground)"
                                                },
                                            )
                                        >
                                            {item
                                                .icon
                                                .map(|icon| {
                                                    view! { <Icon name=icon size=15 /> }
                                                })}
                                            {item.label.clone()}
                                        </button>
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                    }}
                </div>
            </Show>
        </div>
    }
}
