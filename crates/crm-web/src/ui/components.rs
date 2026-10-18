// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Twenty-style presentational components (reimplemented from scratch).
//!
//! Every component here is pure structure + [`crate::styles`] tokens:
//! no data fetching, no auth logic. Icons are plain unicode glyphs —
//! no image assets, nothing copied.

use std::rc::Rc;

use leptos::html::Input;
use leptos::prelude::*;
use leptos_router::components::A;

use crate::styles;

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

/// Small inline icon rendered as a unicode glyph (no image assets).
///
/// # Props
/// - `glyph`: the character to render, e.g. `"▦"`.
#[component]
pub fn Icon(glyph: &'static str) -> impl IntoView {
    view! {
        <span aria-hidden="true" style="display: inline-flex; width: 1.25rem; justify-content: center; flex-shrink: 0;">
            {glyph}
        </span>
    }
}

// ---------------------------------------------------------------------------
// Application shell: sidebar + utility bar
// ---------------------------------------------------------------------------

/// One sidebar navigation entry.
#[derive(Debug, Clone, Copy)]
pub struct NavItem {
    /// Display label.
    pub label: &'static str,
    /// Route href.
    pub href: &'static str,
    /// Icon glyph.
    pub glyph: &'static str,
}

/// Navigation entries, in sidebar order.
pub const NAV_ITEMS: &[NavItem] = &[
    NavItem {
        label: "Dashboard",
        href: "/",
        glyph: "▦",
    },
    NavItem {
        label: "Contacts",
        href: "/contacts",
        glyph: "◉",
    },
    NavItem {
        label: "Profiles",
        href: "/profiles",
        glyph: "◈",
    },
    NavItem {
        label: "Assets",
        href: "/assets",
        glyph: "▤",
    },
    NavItem {
        label: "Audit",
        href: "/audit",
        glyph: "⌗",
    },
];

/// Whether `href` is the active nav entry for `pathname`.
///
/// The root path matches exactly; every other entry also matches its
/// sub-routes (e.g. `/contacts/merge` highlights Contacts).
fn is_active(pathname: &str, href: &str) -> bool {
    if href == "/" {
        pathname == "/"
    } else {
        pathname == href || pathname.starts_with(&format!("{href}/"))
    }
}

/// Left navigation rail: workspace mark, nav items, log-out footer.
///
/// Reads the current route itself so the active item highlights.
///
/// # Props
/// - `on_logout`: revokes the session server-side, then clears the token.
#[component]
pub fn Sidebar(on_logout: impl Fn() + Clone + 'static) -> impl IntoView {
    let location = leptos_router::hooks::use_location();
    view! {
        <aside style=styles::SIDEBAR aria-label="Primary">
            <div style=styles::SIDEBAR_BRAND>
                <span style=styles::SIDEBAR_MONOGRAM>{"R"}</span>
                <span style=styles::SIDEBAR_BRAND_NAME>"Rust CRM"</span>
            </div>
            <nav>
                <p style=styles::SIDEBAR_SECTION>"Workspace"</p>
                {NAV_ITEMS
                    .iter()
                    .map(|item| {
                        let href = item.href;
                        let active = move || is_active(&location.pathname.get(), href);
                        view! {
                            <A
                                href=href
                                attr:style=move || if active() {
                                    styles::SIDEBAR_ITEM_ACTIVE
                                } else {
                                    styles::SIDEBAR_ITEM
                                }
                            >
                                <Icon glyph=item.glyph />
                                {item.label}
                            </A>
                        }
                    })
                    .collect::<Vec<_>>()}
            </nav>
            <div style=styles::SIDEBAR_FOOTER>
                <Btn
                    variant=BtnVariant::Ghost
                    btn_type="button"
                    on_click={
                        std::rc::Rc::new(on_logout.clone()) as std::rc::Rc<dyn Fn()>
                    }
                >
                    <Icon glyph="↩" />
                    "Log out"
                </Btn>
            </div>
        </aside>
    }
}

/// Utility top bar: search trigger (opens the command menu) + user chip.
///
/// # Props
/// - `on_search`: opens the command menu.
#[component]
pub fn UtilityBar(on_search: Rc<dyn Fn()>) -> impl IntoView {
    view! {
        <div style=styles::TOPBAR>
            <button type="button" on:click=move |_| on_search() style=styles::SEARCH_TRIGGER>
                <Icon glyph="⌕" />
                <span>"Search or jump to…"</span>
            </button>
            <span style="margin-left: auto; display: inline-flex; align-items: center; gap: 0.5rem;">
                <span style=styles::AVATAR aria-hidden="true">"S"</span>
                <span style="font-size: 0.8125rem; color: #475569;">"Staff"</span>
            </span>
        </div>
    }
}

/// Small command menu: filter-as-you-type jump list over the nav entries.
///
/// Pure client-side navigation aid; closes on selection or on overlay
/// click. This is the Twenty-style ⌘K surface, minus the global hotkey
/// (a follow-on can wire `⌘K`/`Ctrl+K` at the window level).
///
/// # Props
/// - `open`: controls visibility.
#[component]
pub fn CommandMenu(open: RwSignal<bool>) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let input_ref = NodeRef::<Input>::new();

    // Clear the query each time the menu opens so stale filters never linger.
    Effect::new(move |_| {
        if open.get() {
            query.set(String::new());
        }
    });

    let on_input = move |_| {
        let value = input_ref.get().map(|el| el.value()).unwrap_or_default();
        query.set(value);
    };
    let on_keydown = move |ev: leptos::ev::KeyboardEvent| {
        if ev.key() == "Escape" {
            open.set(false);
        }
    };
    let close = move |_| open.set(false);

    view! {
        <Show when=move || open.get()>
            <div style=styles::OVERLAY on:click=close role="dialog" aria-label="Command menu">
                <div style=styles::CMD_PANEL on:click=|ev| ev.stop_propagation()>
                    <input
                        node_ref=input_ref
                        type="text"
                        placeholder="Type a command or search…"
                        on:input=on_input
                        on:keydown=on_keydown
                        style=styles::CMD_INPUT
                    />
                    <div>
                        {move || {
                            let needle = query.get().to_lowercase();
                            NAV_ITEMS
                                .iter()
                                .filter(|item| {
                                    needle.is_empty()
                                        || item.label.to_lowercase().contains(&needle)
                                })
                                .map(|item| {
                                    view! {
                                        <A
                                            href=item.href
                                            attr:style=styles::CMD_ITEM
                                            on:click=move |_| open.set(false)
                                        >
                                            <Icon glyph=item.glyph />
                                            {item.label}
                                        </A>
                                    }
                                })
                                .collect::<Vec<_>>()
                        }}
                    </div>
                </div>
            </div>
        </Show>
    }
}

/// Application shell: sidebar + utility bar + command menu + content.
///
/// # Props
/// - `on_logout`: passed to the sidebar.
/// - `children`: the routed page content.
#[component]
pub fn AppShell(
    on_logout: impl Fn() + Clone + Send + 'static,
    children: Children,
) -> impl IntoView {
    let menu_open = RwSignal::new(false);
    let open_menu = Rc::new(move || menu_open.set(true)) as Rc<dyn Fn()>;
    view! {
        <div style=styles::APP_ROOT>
            <Sidebar on_logout=on_logout />
            <div style=styles::MAIN_COL>
                <UtilityBar on_search=open_menu />
                <CommandMenu open=menu_open />
                <main style=styles::CONTENT>{children()}</main>
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// Button visual variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BtnVariant {
    /// Blue filled (primary actions).
    Primary,
    /// White with border (default).
    #[default]
    Secondary,
    /// Red filled (destructive actions).
    Danger,
    /// Borderless subtle button.
    Ghost,
}

fn btn_style(variant: BtnVariant, disabled: bool) -> String {
    let base = match variant {
        BtnVariant::Primary => styles::BTN_PRIMARY,
        BtnVariant::Secondary => styles::BTN,
        BtnVariant::Danger => styles::BTN_DANGER,
        BtnVariant::Ghost => styles::BTN_GHOST,
    };
    if disabled {
        format!("{base} {}", styles::BTN_DISABLED)
    } else {
        base.to_string()
    }
}

/// Styled button.
///
/// # Props
/// - `variant`: visual style (default [`BtnVariant::Secondary`]).
/// - `btn_type`: `"button"` (default) or `"submit"`.
/// - `disabled`: optional reactive disabled flag.
/// - `on_click`: optional click handler (not needed for submit buttons).
/// - `children`: label content.
#[component]
pub fn Btn(
    #[prop(optional)] variant: BtnVariant,
    #[prop(optional)] btn_type: Option<&'static str>,
    #[prop(optional)] disabled: Option<Signal<bool>>,
    #[prop(optional)] on_click: Option<Rc<dyn Fn()>>,
    children: Children,
) -> impl IntoView {
    let is_disabled = move || disabled.map(|signal| signal.get()).unwrap_or(false);
    view! {
        <button
            type=btn_type.unwrap_or("button")
            disabled=is_disabled
            on:click=move |_| {
                if let Some(handler) = on_click.as_ref() {
                    handler();
                }
            }
            style=move || btn_style(variant, is_disabled())
        >
            {children()}
        </button>
    }
}

// ---------------------------------------------------------------------------
// Badges, cards, alerts, headers
// ---------------------------------------------------------------------------

/// Badge color tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// Blue — informational / in-progress.
    Blue,
    /// Green — success / active.
    Green,
    /// Amber — warning / pending.
    Amber,
    /// Gray (default) — neutral.
    #[default]
    Gray,
}

fn tone_style(tone: Tone) -> &'static str {
    match tone {
        Tone::Blue => "display: inline-block; padding: 0.125rem 0.625rem; border-radius: 9999px; font-size: 0.75rem; font-weight: 500; background: #eff6ff; color: #1d4ed8; border: 1px solid #bfdbfe; white-space: nowrap;",
        Tone::Green => "display: inline-block; padding: 0.125rem 0.625rem; border-radius: 9999px; font-size: 0.75rem; font-weight: 500; background: #f0fdf4; color: #15803d; border: 1px solid #bbf7d0; white-space: nowrap;",
        Tone::Amber => "display: inline-block; padding: 0.125rem 0.625rem; border-radius: 9999px; font-size: 0.75rem; font-weight: 500; background: #fffbeb; color: #b45309; border: 1px solid #fde68a; white-space: nowrap;",
        Tone::Gray => styles::PILL,
    }
}

/// Status badge.
///
/// # Props
/// - `tone`: color (default [`Tone::Gray`]).
/// - `children`: badge text.
#[component]
pub fn Badge(#[prop(optional)] tone: Tone, children: Children) -> impl IntoView {
    view! {
        <span style=tone_style(tone)>{children()}</span>
    }
}

/// White surface card.
///
/// # Props
/// - `children`: card content.
#[component]
pub fn Card(children: Children) -> impl IntoView {
    view! {
        <div style=styles::CARD>{children()}</div>
    }
}

/// Page title row: title, optional description, and an actions slot.
///
/// # Props
/// - `title`: the page heading.
/// - `description`: optional one-line summary under the title.
/// - `children`: action buttons rendered at the row's right end (optional).
#[component]
pub fn PageHeader(
    title: &'static str,
    #[prop(optional)] description: Option<&'static str>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div style=styles::PAGE_HEAD>
            <div>
                <h2 style=styles::H2>{title}</h2>
                {description.map(|text| view! { <p style=styles::PAGE_DESC>{text}</p> })}
            </div>
            <div style="display: flex; gap: 0.5rem; align-items: center; flex-shrink: 0;">
                {children.map(|actions| actions())}
            </div>
        </div>
    }
}

/// Labeled value row for record pages.
///
/// # Props
/// - `label`: the field label.
/// - `children`: the field value.
#[component]
pub fn FieldRow(label: &'static str, children: Children) -> impl IntoView {
    view! {
        <>
            <dt style=styles::DT>{label}</dt>
            <dd style=styles::DD>{children()}</dd>
        </>
    }
}

/// Table row with a hover wash (inline styles cannot do `:hover`, so the
/// component tracks pointer presence in a signal — still pure Rust/WASM).
///
/// # Props
/// - `children`: the row's cells.
#[component]
pub fn HoverTr(children: Children) -> impl IntoView {
    let hover = RwSignal::new(false);
    view! {
        <tr
            on:mouseenter=move |_| hover.set(true)
            on:mouseleave=move |_| hover.set(false)
            style=move || {
                if hover.get() {
                    format!("background: {};", styles::TR_HOVER_BG)
                } else {
                    "background: transparent;".to_string()
                }
            }
        >
            {children()}
        </tr>
    }
}

/// Dashboard quick-link tile.
///
/// # Props
/// - `href`: destination route.
/// - `glyph`: tile icon.
/// - `title`: tile heading.
/// - `description`: one-line summary.
#[component]
pub fn Tile(
    href: &'static str,
    glyph: &'static str,
    title: &'static str,
    description: &'static str,
) -> impl IntoView {
    view! {
        <A href=href attr:style=styles::TILE>
            <div style=styles::TILE_ICON aria-hidden="true">{glyph}</div>
            <div style="font-weight: 600; font-size: 0.875rem; margin-bottom: 0.25rem;">
                {title}
            </div>
            <div style=styles::MUTED>{description}</div>
        </A>
    }
}
