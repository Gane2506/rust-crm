// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! shadcn-style table suite: [`Table`], [`TableHeader`], [`TableBody`],
//! [`TableRow`], [`TableHead`], [`TableCell`].
//!
//! Token-driven borders and muted header text; rows get a hover wash via
//! the `crm-row-hover` class defined in [`crate::theme::theme_css`].

use leptos::prelude::*;

/// Table wrapper: full width, card surface, outer border + radius.
#[component]
pub fn Table(children: Children) -> impl IntoView {
    view! {
        <div style="width: 100%; overflow-x: auto; background: var(--card); border: 1px solid var(--border); border-radius: var(--radius-lg);">
            <table style="width: 100%; border-collapse: collapse; font-size: 0.875rem;">
                {children()}
            </table>
        </div>
    }
}

/// Table head section.
#[component]
pub fn TableHeader(children: Children) -> impl IntoView {
    view! { <thead>{children()}</thead> }
}

/// Table body section.
#[component]
pub fn TableBody(children: Children) -> impl IntoView {
    view! { <tbody>{children()}</tbody> }
}

/// Table row with hover wash and bottom border.
#[component]
pub fn TableRow(children: Children) -> impl IntoView {
    view! {
        <tr class="crm-row-hover" style="border-bottom: 1px solid var(--border);">
            {children()}
        </tr>
    }
}

/// Header cell: muted, semibold, uppercase micro label.
#[component]
pub fn TableHead(children: Children) -> impl IntoView {
    view! {
        <th
            scope="col"
            style="padding: 0.75rem 1rem; text-align: left; font-size: 0.75rem; font-weight: 600; letter-spacing: 0.04em; text-transform: uppercase; color: var(--muted-foreground); white-space: nowrap;"
        >
            {children()}
        </th>
    }
}

/// Body cell with standard padding.
#[component]
pub fn TableCell(children: Children) -> impl IntoView {
    view! { <td style="padding: 0.75rem 1rem; vertical-align: middle;">{children()}</td> }
}
