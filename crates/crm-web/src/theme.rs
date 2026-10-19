// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Design-token foundation for the CRM frontend, ported from the
//! rust-ui/ui shadcn-style token system (oklch `:root` / `.dark`
//! custom properties) and re-expressed as one injected `<style>` tag —
//! 100% Rust, no CSS framework, no handwritten JavaScript.
//!
//! [`theme_css`] emits the full token block plus base styles; inject it
//! once in the application shell. Every component in [`crate::ui`]
//! references `var(--token)` in its inline styles, so dark mode and the
//! branding overrides from [`branded_css`] apply without touching
//! component code.
//!
//! Dark mode is driven by [`ThemeMode`] (a `RwSignal<bool>` in context):
//! toggling it adds/removes `class="dark"` on `<html>`. The choice is
//! intentionally **not** persisted to `localStorage` (no storage
//! dependency is wired yet); see [`ThemeMode`] docs.

use leptos::leptos_dom::helpers::document;
use leptos::prelude::*;

/// Full design-token stylesheet: `:root` + `.dark` custom properties,
/// radius scale, base element styles, scrollbar, selection, and the
/// shared keyframes (`crm-spin`, `crm-pulse`, `crm-fade-in`).
///
/// Token values follow the rust-ui/ui shadcn-style system (oklch color
/// space). The neutral palette keeps the CRM chrome monochrome; brand
/// color arrives through [`branded_css`] (UI2-03) by overriding
/// `--primary` et al.
pub fn theme_css() -> &'static str {
    r#"
:root {
  --radius: 0.625rem;
  --radius-sm: calc(var(--radius) - 4px);
  --radius-md: calc(var(--radius) - 2px);
  --radius-lg: var(--radius);
  --radius-xl: calc(var(--radius) + 4px);
  --font-sans: 'Inter', -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  --font-mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;

  --background: oklch(1 0 0);
  --foreground: oklch(0.145 0 0);
  --card: oklch(1 0 0);
  --card-foreground: oklch(0.145 0 0);
  --popover: oklch(0.985 0 0);
  --popover-foreground: oklch(0.145 0 0);
  --primary: oklch(0.205 0 0);
  --primary-foreground: oklch(0.985 0 0);
  --primary-light: oklch(0.95 0 0);
  --primary-dark: oklch(0.145 0 0);
  --secondary: oklch(0.92 0 0);
  --secondary-foreground: oklch(0.205 0 0);
  --muted: oklch(0.985 0 0);
  --muted-foreground: oklch(0.556 0 0);
  --accent: oklch(0.97 0 0);
  --accent-foreground: oklch(0.205 0 0);
  --destructive: oklch(0.577 0.245 27.325);
  --destructive-foreground: oklch(0.985 0 0);
  --destructive-light: oklch(0.95 0.08 27.325);
  --destructive-dark: oklch(0.35 0.245 27.325);
  --border: oklch(0.922 0 0);
  --input: oklch(0.922 0 0);
  --ring: oklch(0.708 0 0);
  --chart-1: oklch(0.809 0.105 251.813);
  --chart-2: oklch(0.623 0.214 259.815);
  --chart-3: oklch(0.546 0.245 262.881);
  --chart-4: oklch(0.488 0.243 264.376);
  --chart-5: oklch(0.424 0.199 265.638);
  --chart-primary: oklch(0.809 0.105 251.813);
  --chart-muted: oklch(0.708 0 0);
  --chart-grid: oklch(0.9 0 0);
  --selection: oklch(0.65 0.16 55);
  --selection-foreground: oklch(1 0 0);
  --success: oklch(0.65 0.16 145);
  --success-foreground: oklch(0.985 0 0);
  --success-light: oklch(0.95 0.08 145);
  --success-dark: oklch(0.35 0.16 145);
  --warning: oklch(0.65 0.16 55);
  --warning-foreground: oklch(0.985 0 0);
  --warning-light: oklch(0.95 0.08 55);
  --warning-dark: oklch(0.35 0.16 55);
  --info: oklch(0.55 0.18 250);
  --info-foreground: oklch(0.985 0 0);
  --info-light: oklch(0.95 0.08 250);
  --info-dark: oklch(0.35 0.18 250);
  --surface: oklch(0.98 0 0);
  --surface-foreground: oklch(0.145 0 0);
  --scrollbar-thumb: #ccc;
  --scrollbar-track: transparent;
}

.dark {
  --background: oklch(0.18 0 0);
  --foreground: oklch(0.985 0 0);
  --card: oklch(0.205 0 0);
  --card-foreground: oklch(0.985 0 0);
  --popover: oklch(0.269 0 0);
  --popover-foreground: oklch(0.985 0 0);
  --primary: oklch(0.922 0 0);
  --primary-foreground: oklch(0.205 0 0);
  --primary-light: oklch(0.25 0 0);
  --primary-dark: oklch(0.985 0 0);
  --secondary: oklch(0.269 0 0);
  --secondary-foreground: oklch(0.985 0 0);
  --muted: oklch(0.269 0 0);
  --muted-foreground: oklch(0.708 0 0);
  --accent: oklch(0.371 0 0);
  --accent-foreground: oklch(0.985 0 0);
  --destructive: oklch(0.704 0.191 22.216);
  --destructive-foreground: oklch(0.985 0 0);
  --destructive-light: oklch(0.25 0.08 22.216);
  --destructive-dark: oklch(0.85 0.191 22.216);
  --border: oklch(1 0 0 / 10%);
  --input: oklch(1 0 0 / 15%);
  --ring: oklch(0.556 0 0);
  --chart-1: oklch(0.809 0.105 251.813);
  --chart-2: oklch(0.623 0.214 259.815);
  --chart-3: oklch(0.546 0.245 262.881);
  --chart-4: oklch(0.488 0.243 264.376);
  --chart-5: oklch(0.424 0.199 265.638);
  --chart-primary: oklch(0.809 0.105 251.813);
  --chart-muted: oklch(0.556 0 0);
  --chart-grid: oklch(0.2 0 0);
  --selection: oklch(0.55 0.18 55);
  --selection-foreground: oklch(0.205 0 0);
  --success: oklch(0.65 0.16 145);
  --success-foreground: oklch(0.205 0 0);
  --success-light: oklch(0.25 0.08 145);
  --success-dark: oklch(0.75 0.16 145);
  --warning: oklch(0.65 0.16 55);
  --warning-foreground: oklch(0.205 0 0);
  --warning-light: oklch(0.25 0.08 55);
  --warning-dark: oklch(0.75 0.16 55);
  --info: oklch(0.65 0.18 250);
  --info-foreground: oklch(0.985 0 0);
  --info-light: oklch(0.25 0.08 250);
  --info-dark: oklch(0.75 0.18 250);
  --surface: oklch(0.22 0 0);
  --surface-foreground: oklch(0.708 0 0);
  --scrollbar-thumb: #555;
  --scrollbar-track: transparent;
}

/* ---- base ---- */
*, *::before, *::after { border-color: var(--border); }
html { background: var(--background); }
body {
  margin: 0;
  background: var(--background);
  color: var(--foreground);
  font-family: var(--font-sans);
  font-synthesis-weight: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
}
::selection { background: var(--selection); color: var(--selection-foreground); }
button:not(:disabled), [role="button"]:not(:disabled) { cursor: pointer; }
a { color: inherit; }
:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; border-radius: var(--radius-sm); }
::-webkit-scrollbar { width: 8px; height: 8px; }
::-webkit-scrollbar-track { background: var(--scrollbar-track); }
::-webkit-scrollbar-thumb { background: var(--scrollbar-thumb); border-radius: 8px; }
* { scrollbar-width: thin; scrollbar-color: var(--scrollbar-thumb) transparent; }

/* ---- shared keyframes ---- */
@keyframes crm-spin { to { transform: rotate(360deg); } }
.crm-spin { animation: crm-spin 1s linear infinite; }
@keyframes crm-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.45; } }
.crm-pulse { animation: crm-pulse 1.6s ease-in-out infinite; }
@keyframes crm-fade-in { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: none; } }
.crm-fade-in { animation: crm-fade-in 0.16s ease-out; }
@keyframes crm-toast-in { from { opacity: 0; transform: translateY(8px) scale(0.98); } to { opacity: 1; transform: none; } }
.crm-toast-in { animation: crm-toast-in 0.18s ease-out; }

/* ---- table row hover (inline styles cannot do :hover) ---- */
.crm-row-hover { transition: background 0.12s ease; }
.crm-row-hover:hover { background: var(--muted); }

/* ---- bento grid: responsive columns via container classes ---- */
.crm-bento-grid { display: grid; grid-template-columns: repeat(1, minmax(0, 1fr)); }
@media (min-width: 640px) {
  .crm-bento-grid { grid-template-columns: repeat(var(--bento-sm, 2), minmax(0, 1fr)); }
}
@media (min-width: 1024px) {
  .crm-bento-grid { grid-template-columns: repeat(var(--bento-lg, 4), minmax(0, 1fr)); }
}
.crm-bento-tile { transition: transform 0.18s ease, box-shadow 0.18s ease; }
/* Column spans apply only where the grid is wide enough: single column
   below 640px always spans 1; wider breakpoints clamp the requested
   span to the available column count so tiles never overflow. */
.crm-bento-tile { grid-column: span 1; }
@media (min-width: 640px) {
  .crm-bento-tile { grid-column: span min(var(--bento-col-span, 1), var(--bento-sm, 2)); }
}
@media (min-width: 1024px) {
  .crm-bento-tile { grid-column: span min(var(--bento-col-span, 1), var(--bento-lg, 4)); }
}
.crm-bento-tile-interactive:hover { transform: translateY(-2px); box-shadow: 0 12px 32px oklch(0 0 0 / 0.10); }
"#
}

/// Per-business branding overrides (UI2-03 hook).
///
/// Each field is an optional oklch color string (e.g. `"oklch(0.55 0.2 250)"`)
/// or any CSS color. [`branded_css`] turns the set fields into a
/// `:root` override block layered after [`theme_css`]; unset fields
/// keep the default tokens.
//
// Wired up by the branding studio in UI2-03; allowed dead until then.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct BrandingTheme {
    /// Overrides `--primary` (buttons, active states, links).
    pub primary: Option<String>,
    /// Overrides `--primary-foreground`.
    pub primary_foreground: Option<String>,
    /// Overrides `--ring` (focus rings, selection accents).
    pub ring: Option<String>,
    /// Overrides `--chart-1` (primary dashboard accent).
    pub chart_1: Option<String>,
    /// Display name rendered next to the logo mark (UI2-03).
    pub business_name: Option<String>,
    /// Logo image URL rendered in the sidebar brand slot (UI2-03).
    pub logo_url: Option<String>,
}

/// Builds the branding override stylesheet for `branding`.
///
/// Emits a `:root` block containing only the tokens the branding sets,
/// so it composes with [`theme_css`] and dark mode (dark-mode-specific
/// overrides are a UI2-03 follow-on). Returns an empty string when no
/// color field is set.
///
/// # Security
///
/// Color values are validated against a bounded CSS color grammar before
/// being interpolated: branding input is admin-controlled today, but the
/// branding studio (UI2-03) will accept it through the UI, and raw
/// interpolation would allow CSS rule injection (`</style>`, `url(`, …).
/// Values that fail validation are silently dropped.
//
// Wired up by the branding studio in UI2-03; allowed dead until then.
#[allow(dead_code)]
pub fn branded_css(branding: &BrandingTheme) -> String {
    /// Accepts hex (`#rrggbb`), `rgb()/hsl()/oklch()` functional notation,
    /// and CSS color keywords — rejects anything that could break out of
    /// a declaration value (`;`, `{`, `}`, quotes, `url(`, …).
    fn is_safe_css_color(value: &str) -> bool {
        let v = value.trim();
        !v.is_empty()
            && v.len() <= 64
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || " \t.,%/#()".contains(c))
            && !v.to_ascii_lowercase().contains("url(")
            && !v.contains([';', '{', '}', '"', '\'', '<', '>', '\\'])
    }

    let mut vars = String::new();
    let mut push = |name: &str, value: &Option<String>| {
        if let Some(v) = value.as_deref().filter(|v| is_safe_css_color(v)) {
            vars.push_str(&format!("  {name}: {v};\n"));
        }
    };
    push("--primary", &branding.primary);
    push("--primary-foreground", &branding.primary_foreground);
    push("--ring", &branding.ring);
    push("--chart-1", &branding.chart_1);
    if vars.is_empty() {
        String::new()
    } else {
        format!(":root {{\n{vars}}}")
    }
}

/// Adds or removes `class="dark"` on the `<html>` element.
///
/// No-op when the document element is unavailable (never in CSR).
pub fn set_dark_mode(dark: bool) {
    if let Some(html) = document().document_element() {
        let _ = html.class_list().toggle_with_force("dark", dark);
    }
}

/// Dark-mode state, provided once at the app root via [`provide_theme_mode`].
///
/// The inner signal is `Copy`; toggle through [`ThemeMode::set`], which
/// updates both the signal and the `<html>` class. The mode is
/// intentionally not persisted (see module docs).
#[derive(Debug, Clone, Copy)]
pub struct ThemeMode {
    /// `true` when dark mode is active.
    pub dark: RwSignal<bool>,
}

impl ThemeMode {
    /// Sets the mode and applies the `<html>` class to match.
    pub fn set(self, dark: bool) {
        self.dark.set(dark);
        set_dark_mode(dark);
    }

    /// Toggles the current mode.
    pub fn toggle(self) {
        self.set(!self.dark.get_untracked());
    }
}

/// Creates the [`ThemeMode`] context at the app root.
///
/// Applies the initial (light) mode to `<html>` and installs an effect
/// that keeps the class in sync. The effect only *reads* the signal and
/// touches the DOM — it never writes a signal, so it cannot loop.
pub fn provide_theme_mode() -> ThemeMode {
    let mode = ThemeMode {
        dark: RwSignal::new(false),
    };
    set_dark_mode(false);
    provide_context(mode);
    Effect::new(move |_| {
        set_dark_mode(mode.dark.get());
    });
    mode
}

/// Returns the [`ThemeMode`] from context.
///
/// # Panics
///
/// Panics when called outside a subtree of [`provide_theme_mode`].
pub fn use_theme_mode() -> ThemeMode {
    use_context::<ThemeMode>()
        .expect("ThemeMode context: call provide_theme_mode() at the app root")
}

/// Sun/moon toggle button for the top bar.
///
/// Reads [`ThemeMode`] from context and flips it on click.
#[component]
pub fn ThemeToggle() -> impl IntoView {
    let mode = use_theme_mode();
    view! {
        <button
            type="button"
            title=move || if mode.dark.get() { "Switch to light mode" } else { "Switch to dark mode" }
            aria-label=move || if mode.dark.get() { "Switch to light mode" } else { "Switch to dark mode" }
            on:click=move |_| mode.toggle()
            style="display: inline-flex; align-items: center; justify-content: center; width: 2rem; height: 2rem; border-radius: var(--radius-md); border: 1px solid var(--border); background: var(--card); color: var(--muted-foreground);"
        >
            {move || {
                let name = if mode.dark.get() {
                    crate::ui::IconName::Sun
                } else {
                    crate::ui::IconName::Moon
                };
                view! { <crate::ui::Icon name=name size=16 /> }
            }}
        </button>
    }
}
