// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Overlay primitives: [`Dialog`] (signal-driven modal) and
//! [`DropdownMenu`] (button + popover menu).
//!
//! Token-driven surfaces; no positioning JavaScript. Dialog focus
//! management uses `web-sys` DOM APIs directly.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

use super::callback::{UiHandler, UiValueHandler};
use super::icon::{Icon, IconName};

// ---------------------------------------------------------------------------
// Dialog
// ---------------------------------------------------------------------------

/// CSS selector for keyboard-focusable descendants of a dialog panel.
///
/// `:not([disabled])` covers native controls; the `tabindex` branch
/// covers custom focusables, excluding explicitly unfocusable ones.
const FOCUSABLE_SELECTOR: &str = "a[href], button:not([disabled]), \
    input:not([disabled]), select:not([disabled]), textarea:not([disabled]), \
    [tabindex]:not([tabindex=\"-1\"])";

/// The app's `document`, or `None` when the DOM is unavailable.
///
/// Mirrors the `session_storage()` helper in `main.rs`: every DOM
/// access degrades to a no-op instead of panicking.
///
/// Shared with the mobile nav drawer (`crate::Sidebar`), which
/// reuses the dialog focus helpers below.
pub(crate) fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

/// Keyboard-focusable, visible descendants of `panel`, in tab order.
///
/// Hidden elements (`display: none` → null `offset_parent`) and
/// explicitly unfocusable ones (`tabindex="-1"`, including natively
/// focusable elements opted out via the attribute) are excluded.
/// `position: fixed` elements report a null `offset_parent` too, but
/// dialog content is statically positioned inside the fixed backdrop,
/// so the backdrop (a positioned ancestor) is their offset parent.
fn focusable_in(panel: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    let Ok(list) = panel.query_selector_all(FOCUSABLE_SELECTOR) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..list.length() {
        let Some(node) = list.item(i) else { continue };
        let Ok(el) = node.dyn_into::<web_sys::HtmlElement>() else {
            continue;
        };
        if el.has_attribute("disabled") || el.tab_index() < 0 {
            continue;
        }
        if el.offset_parent().is_none() {
            continue;
        }
        out.push(el);
    }
    out
}

/// Moves focus to the first focusable element in `panel`, or to the
/// panel itself (it carries `tabindex="-1"`) when there is none.
///
/// Shared with the mobile nav drawer (`crate::Sidebar`).
pub(crate) fn focus_first_in(panel: &web_sys::Element) {
    match focusable_in(panel).first() {
        Some(el) => {
            let _ = el.focus();
        }
        None => {
            if let Ok(panel) = panel.clone().dyn_into::<web_sys::HtmlElement>() {
                let _ = panel.focus();
            }
        }
    }
}

/// Moves focus to the first element in `panel` matching `selector`,
/// returning whether anything was focused.
///
/// Used by [`Dialog`]'s `autofocus` prop: a search palette wants its
/// input focused on open, not the dialog's close button (the first
/// focusable element in DOM order). Guards mirror [`focusable_in`]:
/// disabled, unfocusable, and hidden elements are skipped.
fn focus_selector_in(panel: &web_sys::Element, selector: &str) -> bool {
    let Ok(Some(node)) = panel.query_selector(selector) else {
        return false;
    };
    let Ok(el) = node.dyn_into::<web_sys::HtmlElement>() else {
        return false;
    };
    if el.has_attribute("disabled") || el.tab_index() < 0 {
        return false;
    }
    if el.offset_parent().is_none() {
        return false;
    }
    let _ = el.focus();
    true
}

/// Cycles Tab / Shift+Tab within `panel` (focus trap).
///
/// Focus wraps from last to first (and back) instead of escaping to
/// the page behind the dialog. When nothing inside the dialog has
/// focus yet, Tab enters at the first element. With no focusable
/// elements at all, Tab is swallowed so focus stays in the dialog.
///
/// Shared with the mobile nav drawer (`crate::Sidebar`).
pub(crate) fn trap_tab(panel: &web_sys::Element, ev: &leptos::ev::KeyboardEvent) {
    let focusable = focusable_in(panel);
    if focusable.is_empty() {
        ev.prevent_default();
        return;
    }
    let current = document()
        .and_then(|doc| doc.active_element())
        .and_then(|el| el.dyn_into::<HtmlElement>().ok());
    let idx = current
        .as_ref()
        .and_then(|el| focusable.iter().position(|f| f == el));
    let next = if ev.shift_key() {
        match idx {
            Some(0) | None => focusable.len() - 1,
            Some(i) => i - 1,
        }
    } else {
        match idx {
            Some(i) if i + 1 >= focusable.len() => 0,
            Some(i) => i + 1,
            None => 0,
        }
    };
    ev.prevent_default();
    let _ = focusable[next].focus();
}

/// Modal dialog with overlay.
///
/// Focus management (WAI-APG dialog pattern):
/// - On open, focus moves to the element matching `autofocus` when
///   one is given, else to the first focusable element inside the
///   dialog (or the panel itself when there is none).
/// - Tab / Shift+Tab cycles within the dialog; focus cannot escape
///   to the page behind it.
/// - Escape closes the dialog when `close_on_escape` (default true).
/// - On close, focus returns to the element that had focus when the
///   dialog opened (the trigger), whatever closed the dialog.
///
/// # Props
/// - `open`: controls visibility.
/// - `on_close`: called after the dialog closes (optional).
/// - `close_on_escape`: Escape closes the dialog when it has focus
///   (default true).
/// - `title`: optional dialog heading.
/// - `autofocus`: optional CSS selector for the element to focus on
///   open, instead of the first focusable element (e.g. a search
///   input in a command palette). When the selector matches nothing,
///   focus falls back to the first focusable element.
/// - `children`: dialog body.
#[component]
pub fn Dialog(
    open: RwSignal<bool>,
    #[prop(optional)] on_close: Option<UiHandler>,
    #[prop(default = true)] close_on_escape: bool,
    #[prop(optional)] title: Option<&'static str>,
    #[prop(optional)] autofocus: Option<&'static str>,
    children: ChildrenFn,
) -> impl IntoView {
    // `UiHandler`: the close handler is captured inside the `<Show>`
    // children closure, which must be `Send`.
    let close = UiHandler::new(move || {
        open.set(false);
        if let Some(handler) = on_close {
            handler.call();
        }
    });
    // The opener is a DOM element (`!Send`), so it lives in local
    // `StoredValue` storage — it only ever matters on this thread.
    // The effect below runs after each render: on open it records the
    // focused element and moves focus in; on close it restores focus
    // to the opener, covering every close path (backdrop, X button,
    // Escape, programmatic) with one hook.
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);
    // Per-instance panel reference: focus management reads this
    // dialog's own panel, never a document-wide query — two dialogs
    // open at once each manage their own focus. `NodeRef` is `Copy`,
    // so the effect and the keydown handler can both hold it. The
    // effect runs after each render, so the ref is populated whenever
    // `open` is true.
    let panel_ref = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if open.get() {
            if let Some(panel) = panel_ref.get() {
                if let Some(active) = document().and_then(|doc| doc.active_element()) {
                    if let Ok(el) = active.dyn_into::<web_sys::HtmlElement>() {
                        opener.update_value(|slot| *slot = Some(el));
                    }
                }
                // A caller-nominated focus target (e.g. the palette's
                // search input) wins over the first focusable element;
                // when the selector matches nothing, fall back to the
                // default so the dialog still takes focus on open.
                let focused = autofocus
                    .map(|selector| focus_selector_in(&panel, selector))
                    .unwrap_or(false);
                if !focused {
                    focus_first_in(&panel);
                }
            }
        } else {
            opener.with_value(|slot| {
                if let Some(el) = slot {
                    // Focusing a detached element is a harmless no-op,
                    // so no liveness check is needed here.
                    let _ = el.focus();
                }
            });
            opener.update_value(|slot| *slot = None);
        }
    });
    view! {
        <Show when=move || open.get()>
            // Backdrop: plain overlay; the dialog role lives on the panel
            // so assistive tech names it from the title.
            <div
                on:click={
                    let close = close;
                    move |_| close.call()
                }
                style="position: fixed; inset: 0; z-index: 50; display: flex; align-items: center; justify-content: center; padding: 1rem; background: oklch(0 0 0 / 0.5);"
            >
                <div
                    node_ref=panel_ref
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby=title.map(|_| "crm-dialog-title")
                    tabindex="-1"
                    class="crm-fade-in"
                    on:click=|ev| ev.stop_propagation()
                    on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                        match ev.key().as_str() {
                            // Key events from dialog children bubble to
                            // the panel, so one handler traps Tab and
                            // closes on Escape for the whole dialog.
                            "Tab" => {
                                if let Some(panel) = panel_ref.get() {
                                    trap_tab(&panel, &ev);
                                }
                            }
                            "Escape" if close_on_escape => close.call(),
                            _ => {}
                        }
                    }
                    style="width: 100%; max-width: 28rem; background: var(--card); color: var(--card-foreground); border: 1px solid var(--border); border-radius: var(--radius-lg); box-shadow: 0 24px 64px oklch(0 0 0 / 0.24);"
                >
                    <div style="display: flex; align-items: center; justify-content: space-between; padding: 1.25rem 1.5rem 0;">
                        {title
                            .map(|t| {
                                view! {
                                    <h2
                                        id="crm-dialog-title"
                                        style="margin: 0; font-size: 1.125rem; font-weight: 600; letter-spacing: -0.01em;"
                                    >
                                        {t}
                                    </h2>
                                }
                            })}
                        <button
                            type="button"
                            aria-label="Close dialog"
                            on:click={
                                let close = close;
                                move |_| close.call()
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
    on_select: UiValueHandler<usize>,
    #[prop(optional)] align: MenuAlign,
    children: Children,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let align_style = match align {
        MenuAlign::Left => "left: 0;",
        MenuAlign::Right => "right: 0;",
    };
    // `items` lives in a `StoredValue` so the `Fn` render closure can
    // reach it on every re-render without moving it out. `on_select` is
    // a `Copy` `UiValueHandler`.
    let items = StoredValue::new(items);
    view! {
        <div style="position: relative; display: inline-block;">
            <button
                type="button"
                aria-haspopup="menu"
                aria-expanded=move || open.get().to_string()
                on:click=move |_| open.set(!open.get_untracked())
                on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                    if ev.key() == "Escape" {
                        open.set(false);
                    }
                }
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
                                    let select = move |_| {
                                        open.set(false);
                                        on_select.call(i);
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
