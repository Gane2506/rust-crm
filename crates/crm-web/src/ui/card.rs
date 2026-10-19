// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! shadcn-style card suite: [`Card`], [`CardHeader`], [`CardTitle`],
//! [`CardDescription`], [`CardContent`], [`CardFooter`].
//!
//! Token-driven surfaces (`var(--card)`, `var(--border)`); compose freely
//! — [`BentoTile`](crate::ui::BentoTile) builds on these visuals.

use leptos::prelude::*;

/// Card surface: bordered, radius-md, subtle shadow.
#[component]
pub fn Card(children: Children) -> impl IntoView {
    view! {
        <div style="background: var(--card); color: var(--card-foreground); border: 1px solid var(--border); border-radius: var(--radius-lg); box-shadow: 0 1px 2px oklch(0 0 0 / 0.04);">
            {children()}
        </div>
    }
}

/// Card header: title + description block with shadcn spacing.
#[component]
pub fn CardHeader(children: Children) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: 0.375rem; padding: 1.5rem 1.5rem 0;">
            {children()}
        </div>
    }
}

/// Card title: semibold, tight tracking.
#[component]
pub fn CardTitle(children: Children) -> impl IntoView {
    view! {
        <h3 style="margin: 0; font-size: 1rem; font-weight: 600; letter-spacing: -0.01em; line-height: 1.25;">
            {children()}
        </h3>
    }
}

/// Card description: muted small text under the title.
#[component]
pub fn CardDescription(children: Children) -> impl IntoView {
    view! {
        <p style="margin: 0; font-size: 0.875rem; color: var(--muted-foreground); line-height: 1.5;">
            {children()}
        </p>
    }
}

/// Card body content with standard padding.
#[component]
pub fn CardContent(children: Children) -> impl IntoView {
    view! {
        <div style="padding: 1.5rem;">{children()}</div>
    }
}

/// Card footer: actions row, top-bordered when content precedes it.
#[component]
pub fn CardFooter(children: Children) -> impl IntoView {
    view! {
        <div style="display: flex; align-items: center; gap: 0.5rem; padding: 1rem 1.5rem; border-top: 1px solid var(--border);">
            {children()}
        </div>
    }
}
