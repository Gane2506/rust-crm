// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! shadcn-style [`Badge`] with semantic variants.
//!
//! Token-driven (`var(--primary)`, `var(--success-light)` …): dark mode
//! and branding overrides restyle badges without code changes.

use leptos::prelude::*;

/// Badge visual variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BadgeVariant {
    /// Solid primary (default).
    #[default]
    Default,
    /// Solid secondary.
    Secondary,
    /// Transparent with border.
    Outline,
    /// Solid destructive red.
    Destructive,
    /// Soft green (success states).
    Success,
    /// Soft amber (warnings, pending).
    Warning,
    /// Soft blue (informational).
    Info,
}

fn variant_style(variant: BadgeVariant) -> &'static str {
    match variant {
        BadgeVariant::Default => {
            "background: var(--primary); color: var(--primary-foreground); border: 1px solid transparent;"
        }
        BadgeVariant::Secondary => {
            "background: var(--secondary); color: var(--secondary-foreground); border: 1px solid transparent;"
        }
        BadgeVariant::Outline => {
            "background: transparent; color: var(--foreground); border: 1px solid var(--border);"
        }
        BadgeVariant::Destructive => {
            "background: var(--destructive); color: var(--destructive-foreground); border: 1px solid transparent;"
        }
        BadgeVariant::Success => {
            "background: var(--success-light); color: var(--success-dark); border: 1px solid var(--success);"
        }
        BadgeVariant::Warning => {
            "background: var(--warning-light); color: var(--warning-dark); border: 1px solid var(--warning);"
        }
        BadgeVariant::Info => {
            "background: var(--info-light); color: var(--info-dark); border: 1px solid var(--info);"
        }
    }
}

/// Status badge.
///
/// # Props
/// - `variant`: color semantics (default [`BadgeVariant::Default`]).
/// - `children`: badge text.
#[component]
pub fn Badge(#[prop(optional)] variant: BadgeVariant, children: Children) -> impl IntoView {
    view! {
        <span style=format!(
            "display: inline-flex; align-items: center; gap: 0.25rem; padding: 0.125rem 0.625rem; \
             border-radius: 9999px; font-size: 0.75rem; font-weight: 500; line-height: 1.5; \
             white-space: nowrap; {}",
            variant_style(variant)
        )>{children()}</span>
    }
}
