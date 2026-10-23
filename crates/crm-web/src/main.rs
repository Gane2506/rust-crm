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

use api::logout;
use assets::AssetsPage;
use audit::AuditPage;
use branding::BrandingPage;
use contacts::{ContactDetail, ContactsPage, MergePage};
use login::LoginForm;
use profiles::{ProfileDetail, ProfilesPage};
use theme::{provide_theme_mode, ThemeToggle};
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
    });

    view! {
        <>
            <style id="crm-base-style">{crate::theme::theme_css()}</style>
            {move || match token.get() {
                None => view! { <LoginForm on_logged_in=on_logged_in /> }.into_any(),
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

/// Left navigation rail: brand mark, nav items, log-out footer.
///
/// Reads the current route itself so the active item highlights.
///
/// # Props
/// - `on_logout`: revokes the session server-side, then clears the token.
#[component]
fn Sidebar(on_logout: UiHandler) -> impl IntoView {
    let location = use_location();
    view! {
        <aside
            aria-label="Primary"
            style="display: flex; flex-direction: column; width: 15rem; flex-shrink: 0; \
                   height: 100vh; position: sticky; top: 0; \
                   background: var(--card); border-right: 1px solid var(--border);"
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

/// Top bar: command-palette trigger, theme toggle, user chip.
///
/// # Props
/// - `on_search`: opens the command palette.
#[component]
fn TopBar(on_search: UiHandler) -> impl IntoView {
    view! {
        <div style="display: flex; align-items: center; gap: 0.5rem; \
                   height: 3.75rem; padding: 0 1.5rem; flex-shrink: 0; \
                   background: var(--background); border-bottom: 1px solid var(--border); \
                   position: sticky; top: 0; z-index: 10;">
            <button
                type="button"
                on:click=move |_| on_search.call()
                style="display: inline-flex; align-items: center; gap: 0.5rem; \
                       height: 2.25rem; padding: 0 0.75rem; min-width: 14rem; \
                       border-radius: var(--radius-md); border: 1px solid var(--input); \
                       background: var(--muted); color: var(--muted-foreground); \
                       font-size: 0.8125rem;"
            >
                <Icon name=IconName::Search size=15 />
                <span>"Search or jump to…"</span>
                <kbd style="margin-left: auto; font-family: var(--font-mono); font-size: 0.6875rem; \
                            padding: 0.125rem 0.375rem; border-radius: var(--radius-sm); \
                            border: 1px solid var(--border); background: var(--card);">
                    "⌘K"
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
                <span style="font-size: 0.8125rem; color: var(--muted-foreground);">"Staff"</span>
            </span>
        </div>
    }
}

/// Command palette: filter-as-you-type jump list over the nav entries.
///
/// Rendered in the v2 [`Dialog`]; closes on selection or Escape.
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

    view! {
        <Dialog open=open title="Jump to…">
            <div style="margin-bottom: 0.75rem;">
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
    // Toast stack, owned here and shared via context.
    let toasts: Toasts = RwSignal::new(Vec::new());
    provide_context(toasts);

    view! {
        <Router>
            <div style="display: flex; min-height: 100vh; background: var(--background); color: var(--foreground);">
                <Sidebar on_logout=on_logout />
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column;">
                    <TopBar on_search={UiHandler::new(open_palette)} />
                    <CommandPalette open=palette_open />
                    <main style="flex: 1; padding: 1.5rem; max-width: 90rem; width: 100%; margin: 0 auto;">
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
