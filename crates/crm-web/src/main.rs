// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! `crm-web`: Leptos client-side-rendered single-page application.
//!
//! Compiles to `wasm32-unknown-unknown` (`cargo build -p crm-web --target
//! wasm32-unknown-unknown --release`). Styling is the shadcn-style
//! design-token system from [`crate::theme`] (100% Rust, no CSS
//! framework, no handwritten JavaScript). UI2-02 migrates every view
//! onto the v2 component library in [`crate::ui`].

use leptos::mount::mount_to_body;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{Route, Router, Routes, A};
use leptos_router::hooks::use_location;
use leptos_router::path;
use wasm_bindgen::JsCast;

use api::logout;
use assets::AssetsPage;
use audit::AuditPage;
use branding::BrandingPage;
use contacts::{ContactDetail, ContactsPage, MergePage};
use login::LoginForm;
use profiles::{ProfileDetail, ProfilesPage};
use theme::{provide_theme_mode, ThemeToggle};
use ui::overlay::{document, focus_first_in, trap_tab};
use ui::{
    Badge, BadgeVariant, BentoGrid, BentoTile, Button, ButtonVariant, Card, CardContent,
    CardHeader, CardTitle, Dialog, Icon, IconName, Input, Skeleton, Toaster, Toasts, UiHandler,
    UiValueHandler,
};

mod api;
mod assets;
mod audit;
mod branding;
mod components;
mod contacts;
mod login;
mod profiles;
mod theme;
mod ui;
mod ui_styles;

/// `sessionStorage` key for the bearer token. Session scope (not local)
/// is deliberate: survives refresh, cleared on tab close.
///
/// Security note (CWE-922): storing the bearer token in `sessionStorage`
/// exposes it to XSS. This is a conscious tradeoff — the pre-fix behavior
/// (memory-only) logged users out on every refresh, which was reported as
/// a bug. `sessionStorage` over `localStorage` limits the exposure window
/// to the tab lifetime. The app has no `eval`/inline-script vectors; all
/// UI is Leptos-rendered (no `innerHTML` with user data).
const TOKEN_STORAGE_KEY: &str = "crm.auth.token";

/// Best-effort access to `sessionStorage`. Returns `None` when the window
/// is unavailable or storage access is denied (private mode, disabled
/// cookies) — every caller treats `None` as "no persisted session" and
/// falls back to the login form. Never panics.
fn session_storage() -> Option<web_sys::Storage> {
    let window = web_sys::window()?;
    window.session_storage().ok()?
}

/// Reads the persisted bearer token, if any. Returns `None` when storage
/// is unavailable, denied, or empty — the login form handles that case.
fn restore_token() -> Option<String> {
    let storage = session_storage()?;
    storage.get_item(TOKEN_STORAGE_KEY).ok()?
}

/// Persists the bearer token to `sessionStorage`. Best-effort: a storage
/// failure just means the next refresh will ask for login again.
fn persist_token(token: &str) {
    if let Some(storage) = session_storage() {
        let _ = storage.set_item(TOKEN_STORAGE_KEY, token);
    }
}

/// Removes the bearer token from `sessionStorage` on logout.
fn clear_token() {
    if let Some(storage) = session_storage() {
        let _ = storage.remove_item(TOKEN_STORAGE_KEY);
    }
}

/// Root application: login gate plus the authenticated shell.
///
/// The bearer token lives in a Leptos signal, persisted to `sessionStorage`
/// so a page refresh keeps the session. `sessionStorage` (not
/// `localStorage`) is the deliberate choice: the token survives reloads
/// but is cleared when the tab closes, limiting the XSS exposure window
/// vs. a persistent store. Until login succeeds, the login form is the
/// whole app; the router (and every route in it) only renders for
/// authenticated sessions.
#[component]
fn App() -> impl IntoView {
    // Design-token foundation (UI2-01): injected once; every v2 component
    // references var(--token). Also provides the dark-mode context.
    provide_theme_mode();

    // `None` = not logged in; `Some(token)` = bearer token. Restored from
    // `sessionStorage` on init so refreshes don't log the user out.
    let token = RwSignal::new(restore_token());

    // Session expiry (SESSION-02): set by the API client when any
    // authenticated call gets a 401. App watches it — the pages never
    // have to — and drops the dead token so the login form renders
    // instead of a shell whose every call 401s. The flag stays set
    // until the next login, so the form can say why the user is there.
    let session_expired = RwSignal::new(false);
    crate::api::register_session_expired(session_expired);
    Effect::new(move |_| {
        if session_expired.get() {
            clear_token();
            token.set(None);
        }
    });

    // Branding (UI2-03): fetch the public theme on mount (the login page
    // is themed too) and re-fetch after login; inject as a `<style>`
    // layered after the theme_css tag. The UI style (UI3-01) is applied
    // right after, as its own `<style id="crm-ui-style">` tag.
    let apply_branding = move || {
        spawn_local(async move {
            if let Ok(resp) = crate::api::fetch_branding().await {
                let theme = crate::theme::BrandingTheme {
                    primary: resp.theme.primary,
                    primary_foreground: resp.theme.primary_foreground,
                    ring: resp.theme.ring,
                    chart_1: resp.theme.chart_1,
                    business_name: None,
                    logo_url: resp.theme.logo_url,
                    ui_style: resp.theme.ui_style,
                };
                crate::branding::apply_branding_style(&theme);
                crate::ui_styles::apply_ui_style(theme.ui_style.as_deref());
            }
        });
    };
    apply_branding();
    let on_logged_in = UiValueHandler::new(move |new_token: String| {
        persist_token(&new_token);
        token.set(Some(new_token));
        // Reset so a later expiry is a fresh false → true transition
        // and the watcher above fires again.
        session_expired.set(false);
        apply_branding();
    });
    // Server-side revocation first, then drop the local token either way.
    let on_logout = UiHandler::new(move || {
        if let Some(current) = token.get_untracked() {
            spawn_local(async move {
                logout(&current).await.ok();
            });
        }
        clear_token();
        token.set(None);
        // A stale expiry flag must never show the "session expired"
        // banner after an intentional logout.
        session_expired.set(false);
    });

    view! {
        <>
            <style id="crm-base-style">{crate::theme::theme_css()}</style>
            {move || match token.get() {
                None => view! {
                    <LoginForm
                        on_logged_in=on_logged_in
                        session_expired=session_expired.get()
                    />
                }
                    .into_any(),
                Some(token) => {
                    view! { <Shell token=token on_logout=on_logout /> }.into_any()
                }
            }}
        </>
    }
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

/// One sidebar navigation entry.
struct NavItem {
    /// Display label.
    label: &'static str,
    /// Route href.
    href: &'static str,
    /// Sidebar icon.
    icon: IconName,
}

/// Navigation entries, in sidebar order. Entries without a dedicated view
/// yet route to a placeholder page so the IA stays complete.
const NAV_ITEMS: &[NavItem] = &[
    NavItem {
        label: "Dashboard",
        href: "/",
        icon: IconName::LayoutDashboard,
    },
    NavItem {
        label: "Contacts",
        href: "/contacts",
        icon: IconName::Users,
    },
    NavItem {
        label: "Profiles",
        href: "/profiles",
        icon: IconName::Briefcase,
    },
    NavItem {
        label: "Assets",
        href: "/assets",
        icon: IconName::Image,
    },
    NavItem {
        label: "Mail",
        href: "/mail",
        icon: IconName::Mail,
    },
    NavItem {
        label: "Campaigns",
        href: "/campaigns",
        icon: IconName::Megaphone,
    },
    NavItem {
        label: "Agents",
        href: "/agents",
        icon: IconName::Bot,
    },
    NavItem {
        label: "Workflows",
        href: "/workflows",
        icon: IconName::GitBranch,
    },
    NavItem {
        label: "Forms",
        href: "/forms",
        icon: IconName::ClipboardList,
    },
    NavItem {
        label: "Billing",
        href: "/billing",
        icon: IconName::Receipt,
    },
    NavItem {
        label: "Signing",
        href: "/signing",
        icon: IconName::PenLine,
    },
    NavItem {
        label: "Calls",
        href: "/calls",
        icon: IconName::Phone,
    },
    NavItem {
        label: "Analytics",
        href: "/analytics",
        icon: IconName::BarChart3,
    },
    NavItem {
        label: "Admin",
        href: "/admin",
        icon: IconName::Settings,
    },
    NavItem {
        label: "Branding",
        href: "/branding",
        icon: IconName::Image,
    },
    NavItem {
        label: "Audit",
        href: "/audit",
        icon: IconName::Shield,
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

/// Whether the viewport is narrow enough for the mobile shell
/// (AUDIT-10: the sidebar collapses into a drawer below 640px —
/// the same boundary as the bento grid's `sm` breakpoint).
///
/// Read live from the window so a resize across the breakpoint
/// changes behavior without a reload. `false` when the window is
/// unavailable — the desktop rail is the safe default.
fn is_mobile() -> bool {
    web_sys::window()
        .and_then(|w| w.inner_width().ok())
        .and_then(|v| v.as_f64())
        .map(|w| w <= 639.0)
        .unwrap_or(false)
}

/// Whether the browser reports a macOS platform (AUDIT-11).
///
/// Read from the user-agent string — `navigator.platform()` is
/// deprecated in the web platform, and the user agent still
/// reports `Macintosh` on macOS (including iPadOS in desktop mode,
/// where an external keyboard's ⌘K genuinely works). `false` when
/// the window or the user agent is unavailable — the Ctrl+K label
/// and handler are the safe default.
fn is_macos() -> bool {
    web_sys::window()
        .and_then(|w| w.navigator().user_agent().ok())
        .map(|ua| ua.contains("Macintosh"))
        .unwrap_or(false)
}

/// The palette-shortcut label shown on the TopBar search button
/// (AUDIT-11): `⌘K` on macOS, `Ctrl+K` elsewhere. The global
/// handler listens for the matching modifier on each platform, so
/// the badge always names the shortcut that actually works.
fn palette_shortcut_label() -> &'static str {
    if is_macos() {
        "⌘K"
    } else {
        "Ctrl+K"
    }
}

/// Whether a keydown's target is an editable control (AUDIT-11).
///
/// A palette shortcut must never hijack typing: while focus is in
/// an `<input>`, `<textarea>`, `<select>`, or a `contenteditable`
/// element, ⌘K/Ctrl+K stays an ordinary keystroke for that control
/// (e.g. a text field's own shortcuts). Tag names come back
/// upper-case from the DOM; a `contenteditable` attribute present
/// with an empty value means editable, `"false"` means not.
fn is_editable_target(ev: &web_sys::KeyboardEvent) -> bool {
    let Some(target) = ev.target() else {
        return false;
    };
    let Ok(el) = target.dyn_into::<web_sys::Element>() else {
        return false;
    };
    match el.tag_name().as_str() {
        "INPUT" | "TEXTAREA" | "SELECT" => true,
        _ => el
            .get_attribute("contenteditable")
            .is_some_and(|v| v.is_empty() || v.eq_ignore_ascii_case("true")),
    }
}

/// Left navigation rail: brand mark, nav items, log-out footer.
///
/// Reads the current route itself so the active item highlights.
/// Below 640px the rail collapses into a drawer (AUDIT-10): hidden
/// until `nav_open`, shown as a fixed overlay with a backdrop, with
/// the same focus management as [`Dialog`] (focus in on open, Tab
/// trapped, Escape closes, focus restored on close) — but only while
/// the viewport is narrow; on desktop the rail is always visible
/// and Tab must flow through it normally.
///
/// # Props
/// - `on_logout`: revokes the session server-side, then clears the token.
/// - `nav_open`: controls the mobile drawer.
#[component]
fn Sidebar(on_logout: UiHandler, nav_open: RwSignal<bool>) -> impl IntoView {
    let location = use_location();
    // Close the drawer on navigation: covers nav links, the command
    // palette, and programmatic navigation with one hook. Runs on
    // mount too (no-op: the drawer starts closed).
    Effect::new(move |_| {
        location.pathname.track();
        nav_open.set(false);
    });
    // Per-instance drawer reference + opener restore, mirroring
    // `Dialog` in `ui/overlay.rs`. The effect below only manages
    // focus while the viewport is narrow; on desktop the rail is
    // always visible and focus must not be moved or trapped.
    let panel_ref = NodeRef::<leptos::html::Aside>::new();
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);
    Effect::new(move |_| {
        // Read `nav_open` first: the effect must stay subscribed to
        // it even on desktop (where it returns early below), or a
        // desktop-to-mobile resize would leave it dormant and the
        // drawer would open without moving focus in.
        let open = nav_open.get();
        if !is_mobile() {
            return;
        }
        if open {
            if let Some(panel) = panel_ref.get() {
                if let Some(active) = document().and_then(|doc| doc.active_element()) {
                    if let Ok(el) = active.dyn_into::<web_sys::HtmlElement>() {
                        opener.update_value(|slot| *slot = Some(el));
                    }
                }
                focus_first_in(&panel);
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
        <aside
            id="crm-sidebar"
            aria-label="Primary"
            node_ref=panel_ref
            tabindex="-1"
            class="crm-sidebar"
            class:crm-sidebar-open=move || nav_open.get()
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                // The drawer is only modal on a narrow viewport; on
                // desktop the rail is always visible and Tab must
                // flow through it normally.
                if !is_mobile() {
                    return;
                }
                match ev.key().as_str() {
                    // Key events from nav children bubble to the
                    // aside, so one handler traps Tab and closes on
                    // Escape for the whole drawer.
                    "Tab" => {
                        if let Some(panel) = panel_ref.get() {
                            trap_tab(&panel, &ev);
                        }
                    }
                    "Escape" => nav_open.set(false),
                    _ => {}
                }
            }
        >
            <div style="display: flex; align-items: center; gap: 0.625rem; padding: 1.25rem 1rem 1rem;">
                <span style="display: inline-flex; align-items: center; justify-content: center; \
                             width: 2rem; height: 2rem; border-radius: var(--radius-md); \
                             background: var(--primary); color: var(--primary-foreground); \
                             font-weight: 700; font-size: 0.875rem;">
                    "R"
                </span>
                <span style="font-weight: 600; font-size: 0.9375rem; letter-spacing: -0.01em;">
                    "Rust CRM"
                </span>
            </div>
            <nav style="flex: 1; overflow-y: auto; padding: 0 0.5rem;">
                <p style="margin: 0.5rem 0.5rem 0.25rem; font-size: 0.6875rem; font-weight: 600; \
                          letter-spacing: 0.08em; text-transform: uppercase; \
                          color: var(--muted-foreground);">
                    "Workspace"
                </p>
                {NAV_ITEMS
                    .iter()
                    .map(|item| {
                        let href = item.href;
                        let icon = item.icon;
                        let label = item.label;
                        let active = move || is_active(&location.pathname.get(), href);
                        view! {
                            <A
                                href=href
                                attr:style=move || {
                                    if active() {
                                        "display: flex; align-items: center; gap: 0.625rem; \
                                         padding: 0.5rem 0.625rem; margin-bottom: 1px; \
                                         border-radius: var(--radius-md); font-size: 0.875rem; \
                                         font-weight: 500; text-decoration: none; \
                                         background: var(--accent); color: var(--accent-foreground);"
                                    } else {
                                        "display: flex; align-items: center; gap: 0.625rem; \
                                         padding: 0.5rem 0.625rem; margin-bottom: 1px; \
                                         border-radius: var(--radius-md); font-size: 0.875rem; \
                                         font-weight: 400; text-decoration: none; \
                                         color: var(--muted-foreground);"
                                    }
                                }
                            >
                                <Icon name=icon size=17 />
                                {label}
                            </A>
                        }
                    })
                    .collect::<Vec<_>>()}
            </nav>
            <div style="padding: 0.75rem 0.5rem; border-top: 1px solid var(--border);">
                <Button
                    variant=ButtonVariant::Ghost
                    on_click=on_logout
                >
                    <Icon name=IconName::LogOut size=16 />
                    "Log out"
                </Button>
            </div>
        </aside>
    }
}

/// Top bar: nav toggle (mobile only), command-palette trigger,
/// theme toggle, user chip.
///
/// Below 640px the sidebar collapses into a drawer (AUDIT-10), so
/// the bar gains a hamburger toggle and the search button shrinks
/// instead of forcing overflow; the shortcut hint and the "Staff" label
/// hide at that width (classes in [`crate::theme::theme_css`]).
/// The hint names the platform shortcut — `⌘K` on macOS, `Ctrl+K`
/// elsewhere (AUDIT-11) — and the matching global handler lives in
/// [`Shell`].
///
/// # Props
/// - `on_search`: opens the command palette.
/// - `nav_open`: controls the mobile drawer.
#[component]
fn TopBar(on_search: UiHandler, nav_open: RwSignal<bool>) -> impl IntoView {
    view! {
        <div class="crm-topbar">
            <button
                type="button"
                class="crm-nav-toggle"
                aria-label=move || if nav_open.get() { "Close navigation" } else { "Open navigation" }
                aria-expanded=move || nav_open.get().to_string()
                aria-controls="crm-sidebar"
                on:click=move |_| nav_open.set(!nav_open.get_untracked())
                style="align-items: center; justify-content: center; width: 2.25rem; height: 2.25rem; \
                       flex-shrink: 0; border-radius: var(--radius-md); border: 1px solid var(--input); \
                       background: var(--muted); color: var(--muted-foreground);"
            >
                {move || {
                    let name = if nav_open.get() {
                        IconName::X
                    } else {
                        IconName::Menu
                    };
                    view! { <Icon name=name size=17 /> }
                }}
            </button>
            <button
                type="button"
                class="crm-topbar-search"
                on:click=move |_| on_search.call()
                style="display: inline-flex; align-items: center; gap: 0.5rem; \
                       height: 2.25rem; padding: 0 0.75rem; \
                       border-radius: var(--radius-md); border: 1px solid var(--input); \
                       background: var(--muted); color: var(--muted-foreground); \
                       font-size: 0.8125rem;"
            >
                <Icon name=IconName::Search size=15 />
                <span>"Search or jump to…"</span>
                <kbd class="crm-topbar-search-kbd" style="margin-left: auto; font-family: var(--font-mono); font-size: 0.6875rem; \
                            padding: 0.125rem 0.375rem; border-radius: var(--radius-sm); \
                            border: 1px solid var(--border); background: var(--card);">
                    {palette_shortcut_label()}
                </kbd>
            </button>
            <span style="margin-left: auto; display: inline-flex; align-items: center; gap: 0.5rem;">
                <ThemeToggle />
                <span style="display: inline-flex; align-items: center; justify-content: center; \
                             width: 2rem; height: 2rem; border-radius: 9999px; \
                             background: var(--secondary); color: var(--secondary-foreground); \
                             font-size: 0.75rem; font-weight: 600;" aria-hidden="true">
                    "S"
                </span>
                <span class="crm-topbar-user-label" style="font-size: 0.8125rem; color: var(--muted-foreground);">"Staff"</span>
            </span>
        </div>
    }
}

/// Command palette: filter-as-you-type jump list over the nav entries.
///
/// Rendered in the v2 [`Dialog`]; opens from the TopBar Search button
/// or the global ⌘K/Ctrl+K shortcut (AUDIT-11, registered in [`Shell`]);
/// closes on selection or Escape.
///
/// Focus lands on the search input on open (via the dialog's
/// `autofocus` prop — the close button is the first focusable element
/// in DOM order, so the default would swallow typing) and Enter
/// selects the first filtered entry (R3-01).
///
/// # Props
/// - `open`: controls visibility.
#[component]
fn CommandPalette(open: RwSignal<bool>) -> impl IntoView {
    let query = RwSignal::new(String::new());

    // Clear the query each time the palette opens so stale filters linger.
    Effect::new(move |_| {
        if open.get() {
            query.set(String::new());
        }
    });

    // Leptos Router v0.8: navigate programmatically via `use_navigate`.
    // Wrapped in a `StoredValue` (local storage: `NavigateFn` is `Send`
    // but not `Sync`) so the `Dialog`'s `ChildrenFn` (`Send + Sync`)
    // can reach it.
    let navigate = StoredValue::new_local(leptos_router::hooks::use_navigate());

    // The first entry the list shows for the current query — Enter
    // selects it. Mirrors the filter in the list render below.
    let first_match = move || {
        let needle = query.get().to_lowercase();
        NAV_ITEMS
            .iter()
            .find(|item| needle.is_empty() || item.label.to_lowercase().contains(&needle))
            .map(|item| item.href)
    };

    view! {
        <Dialog open=open title="Jump to…" autofocus="#cmd-palette-input">
            <div
                style="margin-bottom: 0.75rem;"
                on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                    // Enter selects the first filtered entry — the same
                    // navigation the row buttons do on click. Keydown
                    // bubbles from the input to this wrapper, so the
                    // shared `Input` component needs no keydown prop;
                    // Tab and Escape keep bubbling to the dialog panel
                    // handler. With no match (empty filter) Enter does
                    // nothing.
                    if ev.key() == "Enter" {
                        if let Some(href) = first_match() {
                            open.set(false);
                            let href = href.to_string();
                            navigate.with_value(|n| n(&href, Default::default()));
                        }
                    }
                }
            >
                <Input
                    input_type="search"
                    placeholder="Type a workspace area…"
                    value=query
                    on_input={UiValueHandler::new(move |v: String| query.set(v))}
                    id="cmd-palette-input"
                />
            </div>
            <div style="max-height: 18rem; overflow-y: auto;">
                {move || {
                    let needle = query.get().to_lowercase();
                    NAV_ITEMS
                        .iter()
                        .filter(|item| {
                            needle.is_empty() || item.label.to_lowercase().contains(&needle)
                        })
                        .map(|item| {
                            let href = item.href.to_string();
                            let icon = item.icon;
                            let label = item.label;
                            view! {
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        open.set(false);
                                        navigate.with_value(|n| n(&href, Default::default()));
                                    }
                                    style="display: flex; align-items: center; gap: 0.625rem; \
                                           width: 100%; padding: 0.625rem 0.75rem; border: none; \
                                           border-radius: var(--radius-md); background: transparent; \
                                           color: var(--foreground); font-size: 0.875rem; text-align: left;"
                                    class="crm-row-hover"
                                >
                                    <Icon name=icon size=16 />
                                    {label}
                                </button>
                            }
                        })
                        .collect::<Vec<_>>()
                }}
            </div>
        </Dialog>
    }
}

/// Authenticated shell: v2 sidebar + top bar + command palette + routes.
///
/// # Props
/// - `token`: the in-memory bearer token, cloned per route view.
/// - `on_logout`: revokes server-side, then clears the token signal.
#[component]
fn Shell(token: String, on_logout: UiHandler) -> impl IntoView {
    let palette_open = RwSignal::new(false);
    let open_palette = move || palette_open.set(true);
    // Global palette shortcut (AUDIT-11): ⌘K on macOS, Ctrl+K
    // elsewhere. The TopBar badge advertised the shortcut but no
    // handler existed. One window-level listener, registered once —
    // the effect tracks no signals — so the palette opens from any
    // authenticated page, through the same `palette_open` signal the
    // Search button uses. Escape, the focus trap, and focus restore
    // are untouched: they live in the Dialog's own panel handler
    // (`ui/overlay.rs`), which this listener never intercepts.
    Effect::new(move |_| {
        let Some(window) = web_sys::window() else {
            return;
        };
        let handler = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
            move |ev: web_sys::KeyboardEvent| {
                // Never hijack typing: ignore auto-repeats and keys
                // pressed while focus is in an editable control.
                if ev.repeat() || is_editable_target(&ev) {
                    return;
                }
                // `key()` is layout-aware ("k" vs "K" with Shift), so
                // compare case-insensitively. The modifier matches
                // the badge: ⌘ on macOS, Ctrl elsewhere.
                let modifier = if is_macos() {
                    ev.meta_key()
                } else {
                    ev.ctrl_key()
                };
                if modifier && ev.key().eq_ignore_ascii_case("k") {
                    // Block the browser's own Ctrl+K (focus the
                    // address/search bar) so the palette wins.
                    ev.prevent_default();
                    palette_open.set(true);
                }
            },
        );
        // App-lifetime listener: the closure is intentionally leaked.
        // `Shell` lives for the whole authenticated session, and
        // setting an already-open palette open again is a no-op, so
        // there is no unmount path that needs the listener removed.
        let _ =
            window.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
        handler.forget();
    });
    // Mobile nav drawer (AUDIT-10): collapsed below 640px, toggled
    // by the hamburger in the TopBar.
    let nav_open = RwSignal::new(false);
    // Toast stack, owned here and shared via context.
    let toasts: Toasts = RwSignal::new(Vec::new());
    provide_context(toasts);

    view! {
        <Router>
            <div style="display: flex; min-height: 100vh; background: var(--background); color: var(--foreground);">
                <Sidebar on_logout=on_logout nav_open=nav_open />
                <Show when=move || nav_open.get()>
                    // Dimmed backdrop for the mobile drawer; hidden on
                    // desktop by the `.crm-sidebar-backdrop` class.
                    // Click closes the drawer.
                    <div
                        class="crm-sidebar-backdrop"
                        on:click=move |_| nav_open.set(false)
                    />
                </Show>
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column;">
                    <TopBar on_search={UiHandler::new(open_palette)} nav_open=nav_open />
                    <CommandPalette open=palette_open />
                    // `box-sizing: border-box` is load-bearing here: with
                    // `width: 100%` plus 1.5rem of padding under content-box
                    // sizing, the column rendered 48px wider than its
                    // flex parent and overflowed the viewport (AUDIT-03).
                    <main class="crm-main" style="flex: 1; padding: 1.5rem; max-width: 90rem; width: 100%; box-sizing: border-box; margin: 0 auto;">
                        <Routes fallback=|| {
                            view! { <NotFound /> }
                        }>
                            <Route
                                path=path!("/")
                                view={
                                    let token = token.clone();
                                    move || view! { <Dashboard token=token.clone() /> }
                                }
                            />
                            <Route
                                path=path!("/contacts")
                                view={
                                    let token = token.clone();
                                    move || view! { <ContactsPage token=token.clone() /> }
                                }
                            />
                            <Route
                                path=path!("/contacts/merge")
                                view={
                                    let token = token.clone();
                                    move || view! { <MergePage token=token.clone() /> }
                                }
                            />
                            <Route
                                path=path!("/contacts/:id")
                                view={
                                    let token = token.clone();
                                    move || {
                                        let params = leptos_router::hooks::use_params_map();
                                        let id = params.get().get("id").unwrap_or_default();
                                        let token = token.clone();
                                        view! { <ContactDetail token=token id=id /> }
                                    }
                                }
                            />
                            <Route
                                path=path!("/profiles")
                                view={
                                    let token = token.clone();
                                    move || view! { <ProfilesPage token=token.clone() /> }
                                }
                            />
                            <Route
                                path=path!("/profiles/:id")
                                view={
                                    let token = token.clone();
                                    move || {
                                        let params = leptos_router::hooks::use_params_map();
                                        let id = params.get().get("id").unwrap_or_default();
                                        let token = token.clone();
                                        view! { <ProfileDetail token=token profile_id=id /> }
                                    }
                                }
                            />
                            <Route
                                path=path!("/assets")
                                view={
                                    let token = token.clone();
                                    move || view! { <AssetsPage token=token.clone() /> }
                                }
                            />
                            <Route
                                path=path!("/audit")
                                view={
                                    let token = token.clone();
                                    move || view! { <AuditPage token=token.clone() /> }
                                }
                            />
                            <Route
                                path=path!("/branding")
                                view={
                                    let token = token.clone();
                                    move || view! { <BrandingPage token=token.clone() /> }
                                }
                            />
                            // Feature areas whose dedicated views land in later
                            // tasks route to a placeholder so the IA is complete.
                            <Route path=path!("/mail") view=|| view! { <ComingSoon feature="Mail" /> } />
                            <Route path=path!("/campaigns") view=|| view! { <ComingSoon feature="Campaigns" /> } />
                            <Route path=path!("/agents") view=|| view! { <ComingSoon feature="Jev agents" /> } />
                            <Route path=path!("/workflows") view=|| view! { <ComingSoon feature="Workflows" /> } />
                            <Route path=path!("/forms") view=|| view! { <ComingSoon feature="Forms" /> } />
                            <Route path=path!("/billing") view=|| view! { <ComingSoon feature="Billing" /> } />
                            <Route path=path!("/signing") view=|| view! { <ComingSoon feature="Signing" /> } />
                            <Route path=path!("/calls") view=|| view! { <ComingSoon feature="Calls" /> } />
                            <Route path=path!("/analytics") view=|| view! { <ComingSoon feature="Analytics" /> } />
                            <Route path=path!("/admin") view=|| view! { <ComingSoon feature="Admin" /> } />
                        </Routes>
                    </main>
                </div>
            </div>
            <Toaster toasts=toasts />
        </Router>
    }
}

/// 404 page.
#[component]
fn NotFound() -> impl IntoView {
    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Page not found"</CardTitle>
            </CardHeader>
            <CardContent>
                <p style="margin: 0; color: var(--muted-foreground); font-size: 0.875rem;">
                    "The page you asked for does not exist. "
                    <A href="/" attr:style="color: var(--primary);">"Back to the dashboard →"</A>
                </p>
            </CardContent>
        </Card>
    }
}

/// Placeholder for feature areas whose dedicated views land in later tasks.
///
/// Keeps the information architecture complete and gives every sidebar
/// entry a real destination.
#[component]
fn ComingSoon(feature: &'static str) -> impl IntoView {
    view! {
        <section aria-label=feature>
            <BentoGrid lg_cols=1>
                <BentoTile
                    eyebrow="Workspace"
                    title=feature
                    description="This workspace area is being rebuilt on the new design system. Check back soon."
                    icon=IconName::LayoutGrid
                >
                    <A
                        href="/"
                        attr:style="display: inline-flex; align-items: center; gap: 0.375rem; \
                                    margin-top: 1rem; font-size: 0.875rem; font-weight: 500; \
                                    color: var(--primary); text-decoration: none;"
                    >
                        "Back to the dashboard →"
                    </A>
                </BentoTile>
            </BentoGrid>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Dashboard (Bento)
// ---------------------------------------------------------------------------

/// Agency overview as a Bento grid: live stat tiles, recent activity,
/// quick actions, and audit-chain health.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
fn Dashboard(token: String) -> impl IntoView {
    use crate::api::{fetch_audit_events, fetch_contacts, fetch_profiles, verify_audit_chain};

    // `None` = loading; `Some(_)` = loaded (errors render as "—").
    let contact_total = RwSignal::new(None::<u64>);
    let profile_total = RwSignal::new(None::<u64>);
    let active_profiles = RwSignal::new(None::<u64>);
    let recent_events = RwSignal::new(None::<Vec<(String, String, String)>>);
    let chain_health = RwSignal::new(None::<Option<(bool, u64)>>);

    {
        let token = token.clone();
        Effect::new(move |_| {
            let token = token.clone();
            spawn_local(async move {
                if let Ok(page) = fetch_contacts(&token).await {
                    contact_total.set(Some(page.total));
                }
                if let Ok(page) = fetch_profiles(&token).await {
                    profile_total.set(Some(page.total));
                    let active = page
                        .profiles
                        .iter()
                        .filter(|p| p.status == crm_domain::profile::ProfileStatus::Active)
                        .count() as u64;
                    active_profiles.set(Some(active));
                }
                if let Ok(page) = fetch_audit_events(
                    &crate::api::AuditQuery {
                        action: None,
                        limit: Some(8),
                        newest_first: true,
                    },
                    &token,
                )
                .await
                {
                    recent_events.set(Some(
                        page.events
                            .into_iter()
                            .map(|item| {
                                (
                                    item.event.action.clone(),
                                    item.event.occurred_at.format("%H:%M").to_string(),
                                    format!("{:?}", item.event.result),
                                )
                            })
                            .collect(),
                    ));
                }
                match verify_audit_chain(&token).await {
                    Ok(v) => chain_health.set(Some(Some((v.valid, v.events_checked)))),
                    Err(_) => chain_health.set(Some(None)),
                }
            });
        });
    }

    /// Renders a stat metric or a skeleton while loading.
    fn metric(value: RwSignal<Option<u64>>) -> impl IntoView {
        move || match value.get() {
            Some(n) => view! { <span>{n.to_string()}</span> }.into_any(),
            None => view! { <Skeleton width="4rem" height="2rem" /> }.into_any(),
        }
    }

    view! {
        <section aria-label="Dashboard">
            <div style="margin-bottom: 1.25rem;">
                <h2 style="margin: 0; font-size: 1.5rem; font-weight: 700; letter-spacing: -0.02em;">
                    "Dashboard"
                </h2>
                <p style="margin: 0.25rem 0 0; font-size: 0.875rem; color: var(--muted-foreground);">
                    "Agency overview — live from the API."
                </p>
            </div>
            <BentoGrid lg_cols=4 sm_cols=2>
                <BentoTile
                    eyebrow="CRM"
                    title="Contacts"
                    icon=IconName::Users
                    footer_href="/contacts"
                    footer_label="View contacts"
                    interactive=true
                >
                    <p style="margin: 0.5rem 0 0; font-size: 2rem; font-weight: 700; letter-spacing: -0.02em;">
                        {metric(contact_total)}
                    </p>
                </BentoTile>
                <BentoTile
                    eyebrow="CRM"
                    title="Client profiles"
                    icon=IconName::Briefcase
                    footer_href="/profiles"
                    footer_label="View profiles"
                    interactive=true
                >
                    <p style="margin: 0.5rem 0 0; font-size: 2rem; font-weight: 700; letter-spacing: -0.02em;">
                        {metric(profile_total)}
                    </p>
                    <p style="margin: 0.25rem 0 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        {move || match active_profiles.get() {
                            Some(n) => format!("{n} active"),
                            None => "…".to_string(),
                        }}
                    </p>
                </BentoTile>
                <BentoTile
                    eyebrow="Security"
                    title="Audit chain"
                    icon=IconName::ShieldCheck
                    footer_href="/audit"
                    footer_label="Open audit log"
                    interactive=true
                >
                    <div style="margin-top: 0.5rem;">
                        {move || match chain_health.get() {
                            None => view! { <Skeleton width="5rem" height="1.75rem" /> }.into_any(),
                            Some(None) => {
                                view! {
                                    <Badge variant=BadgeVariant::Warning>"Unknown"</Badge>
                                }
                                    .into_any()
                            }
                            Some(Some((valid, checked))) => {
                                let (variant, label) = if valid {
                                    (BadgeVariant::Success, format!("Valid · {checked} checked"))
                                } else {
                                    (BadgeVariant::Destructive, "BROKEN".to_string())
                                };
                                view! { <Badge variant=variant>{label}</Badge> }.into_any()
                            }
                        }}
                    </div>
                </BentoTile>
                <BentoTile
                    eyebrow="Shortcuts"
                    title="Quick actions"
                    icon=IconName::Plus
                >
                    <div style="display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.75rem;">
                        <A
                            href="/contacts"
                            attr:style="font-size: 0.8125rem; font-weight: 500; color: var(--primary); text-decoration: none;"
                        >
                            "＋ New contact"
                        </A>
                        <A
                            href="/profiles"
                            attr:style="font-size: 0.8125rem; font-weight: 500; color: var(--primary); text-decoration: none;"
                        >
                            "＋ New client profile"
                        </A>
                        <A
                            href="/contacts/merge"
                            attr:style="font-size: 0.8125rem; font-weight: 500; color: var(--primary); text-decoration: none;"
                        >
                            "⤨ Merge duplicates"
                        </A>
                    </div>
                </BentoTile>
                <BentoTile
                    eyebrow="Activity"
                    title="Recent audit events"
                    icon=IconName::Clock
                    col_span=2
                    footer_href="/audit"
                    footer_label="View all events"
                >
                    {move || match recent_events.get() {
                        None => {
                            view! {
                                <div style="display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.75rem;">
                                    <Skeleton width="100%" height="1rem" />
                                    <Skeleton width="100%" height="1rem" />
                                    <Skeleton width="70%" height="1rem" />
                                </div>
                            }
                                .into_any()
                        }
                        Some(events) if events.is_empty() => {
                            view! {
                                <p style="margin: 0.75rem 0 0; font-size: 0.8125rem; color: var(--muted-foreground);">
                                    "No audit events yet."
                                </p>
                            }
                                .into_any()
                        }
                        Some(events) => {
                            view! {
                                <ul style="margin: 0.75rem 0 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 0.5rem;">
                                    {events
                                        .into_iter()
                                        .take(5)
                                        .map(|(action, time, result)| {
                                            view! {
                                                <li style="display: flex; align-items: center; gap: 0.625rem; font-size: 0.8125rem;">
                                                    <span style="font-family: var(--font-mono); font-size: 0.75rem; color: var(--muted-foreground);">
                                                        {time}
                                                    </span>
                                                    <span style="font-family: var(--font-mono); font-size: 0.75rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">
                                                        {action}
                                                    </span>
                                                    <span style="margin-left: auto;">
                                                        <Badge variant=BadgeVariant::Secondary>{result}</Badge>
                                                    </span>
                                                </li>
                                            }
                                        })
                                        .collect::<Vec<_>>()}
                                </ul>
                            }
                                .into_any()
                        }
                    }}
                </BentoTile>
                <BentoTile
                    eyebrow="Workspace"
                    title="Explore"
                    icon=IconName::LayoutGrid
                    description="Jump into any workspace area from the sidebar — mail, campaigns, agents, workflows, forms, billing, signing, calls, and analytics."
                    col_span=2
                />
            </BentoGrid>
        </section>
    }
}

fn main() {
    mount_to_body(|| view! { <App /> });
}
