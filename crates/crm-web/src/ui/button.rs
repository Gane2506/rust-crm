// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! shadcn-style [`Button`] for the component library v2.
//!
//! Token-driven (`var(--primary)` et al.) inline styles: variants and
//! sizes compose from the design tokens in [`crate::theme`], so dark
//! mode and branding overrides restyle buttons without code changes.

use std::rc::Rc;

use leptos::prelude::*;

/// Button visual variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    /// Solid primary (default).
    #[default]
    Default,
    /// Solid secondary.
    Secondary,
    /// Bordered, transparent background.
    Outline,
    /// Borderless, hover wash only.
    Ghost,
    /// Solid destructive red.
    Destructive,
}

/// Button size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonSize {
    /// Compact (form rows, table actions).
    Sm,
    /// Standard (default).
    #[default]
    Md,
    /// Large (page headers, dialogs).
    Lg,
    /// Square icon-only button.
    Icon,
}

fn variant_style(variant: ButtonVariant) -> &'static str {
    match variant {
        ButtonVariant::Default => {
            "background: var(--primary); color: var(--primary-foreground); border: 1px solid transparent;"
        }
        ButtonVariant::Secondary => {
            "background: var(--secondary); color: var(--secondary-foreground); border: 1px solid transparent;"
        }
        ButtonVariant::Outline => {
            "background: transparent; color: var(--foreground); border: 1px solid var(--input);"
        }
        ButtonVariant::Ghost => {
            "background: transparent; color: var(--foreground); border: 1px solid transparent;"
        }
        ButtonVariant::Destructive => {
            "background: var(--destructive); color: var(--destructive-foreground); border: 1px solid transparent;"
        }
    }
}

fn size_style(size: ButtonSize) -> &'static str {
    match size {
        ButtonSize::Sm => "height: 2rem; padding: 0 0.75rem; font-size: 0.75rem; gap: 0.375rem;",
        ButtonSize::Md => "height: 2.5rem; padding: 0 1rem; font-size: 0.875rem; gap: 0.5rem;",
        ButtonSize::Lg => "height: 2.75rem; padding: 0 1.5rem; font-size: 0.9375rem; gap: 0.5rem;",
        ButtonSize::Icon => "height: 2.5rem; width: 2.5rem; padding: 0; font-size: 0.875rem;",
    }
}

/// shadcn-style button.
///
/// # Props
/// - `variant`: visual style (default [`ButtonVariant::Default`]).
/// - `size`: sizing (default [`ButtonSize::Md`]).
/// - `btn_type`: `"button"` (default) or `"submit"`.
/// - `disabled`: optional reactive disabled flag.
/// - `on_click`: optional click handler; invoked as a 0-arg callback.
/// - `children`: label content (text, [`crate::ui::Icon`], or both).
#[component]
pub fn Button(
    #[prop(optional)] variant: ButtonVariant,
    #[prop(optional)] size: ButtonSize,
    #[prop(optional)] btn_type: Option<&'static str>,
    #[prop(optional)] disabled: Option<Signal<bool>>,
    #[prop(optional)] on_click: Option<Rc<dyn Fn()>>,
    children: Children,
) -> impl IntoView {
    let is_disabled = move || disabled.map(|s| s.get()).unwrap_or(false);
    let base = "display: inline-flex; align-items: center; justify-content: center; \
        white-space: nowrap; border-radius: var(--radius-md); font-weight: 500; \
        cursor: pointer; transition: background 0.15s ease, opacity 0.15s ease;";
    let style = move || {
        let mut s = format!("{base} {} {}", variant_style(variant), size_style(size));
        if is_disabled() {
            s.push_str(" opacity: 0.5; cursor: not-allowed;");
        }
        s
    };
    view! {
        <button
            type={btn_type.unwrap_or("button")}
            disabled=is_disabled
            on:click=move |_| {
                if let Some(handler) = on_click.as_ref() {
                    handler();
                }
            }
            style=style
        >
            {children()}
        </button>
    }
}
