// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Bento UI: [`BentoGrid`] (responsive tile grid) and [`BentoTile`]
//! (varied-size content tile).
//!
//! The bento-box aesthetic — dense but airy tiles with generous radius,
//! subtle borders, and soft shadows. Responsive columns come from the
//! `crm-bento-grid` classes in [`crate::theme::theme_css`] (media
//! queries can't live in inline styles); the tile visuals are
//! token-driven (`var(--card)`, `var(--border)` …), so dark mode and
//! branding overrides apply automatically.

use leptos::prelude::*;
use leptos_router::components::A;

use super::icon::{Icon, IconName};

/// Responsive bento grid container.
///
/// Columns are 1 below 640px, then `--bento-sm` / `--bento-lg`
/// (set from props); the breakpoints live in the `crm-bento-grid`
/// classes in the theme stylesheet.
///
/// # Props
/// - `sm_cols`: columns at ≥640px (default 2).
/// - `lg_cols`: columns at ≥1024px (default 4).
/// - `gap`: grid gap CSS size (default `"1rem"`).
/// - `children`: [`BentoTile`]s.
#[component]
pub fn BentoGrid(
    #[prop(optional)] sm_cols: Option<u8>,
    #[prop(optional)] lg_cols: Option<u8>,
    #[prop(optional)] gap: Option<&'static str>,
    children: Children,
) -> impl IntoView {
    view! {
        <div
            class="crm-bento-grid"
            style={format!(
                "gap: {}; --bento-sm: {}; --bento-lg: {};",
                gap.unwrap_or("1rem"),
                sm_cols.unwrap_or(2),
                lg_cols.unwrap_or(4),
            )}
        >
            {children()}
        </div>
    }
}

/// A bento tile: card surface with configurable span and rich slots.
///
/// Looks good as a stat tile (icon + big metric) or with rich content.
/// Set `interactive` for the hover-lift treatment on clickable tiles.
///
/// # Props
/// - `col_span` / `row_span`: grid span (default 1 each).
/// - `eyebrow`: uppercase micro label above the title.
/// - `title`: tile heading.
/// - `metric`: big value line (stat tiles), e.g. `"1,248"`.
/// - `metric_tone`: optional CSS color for the metric (default foreground).
/// - `description`: muted supporting text.
/// - `icon`: optional leading glyph.
/// - `footer_href` / `footer_label`: optional footer link row.
/// - `interactive`: hover lift (`translateY(-2px)` + shadow).
/// - `children`: arbitrary extra content (charts, lists, actions).
// Narrowly-scoped, deliberate API: a bento tile is a slot-based layout
// component and the brief requires each slot as an independent optional
// prop. Grouping them into a struct would only move the arity.
#[component]
#[allow(clippy::too_many_arguments)]
pub fn BentoTile(
    #[prop(optional)] col_span: Option<u8>,
    #[prop(optional)] row_span: Option<u8>,
    #[prop(optional)] eyebrow: Option<&'static str>,
    #[prop(optional)] title: Option<&'static str>,
    #[prop(optional)] metric: Option<String>,
    #[prop(optional)] metric_tone: Option<&'static str>,
    #[prop(optional)] description: Option<&'static str>,
    #[prop(optional)] icon: Option<IconName>,
    #[prop(optional)] footer_href: Option<&'static str>,
    #[prop(optional)] footer_label: Option<&'static str>,
    #[prop(optional)] interactive: bool,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let cls = if interactive {
        "crm-bento-tile crm-bento-tile-interactive"
    } else {
        "crm-bento-tile"
    };
    view! {
        <div
            class=cls
            style={format!(
                "grid-column: span {}; grid-row: span {}; display: flex; flex-direction: column; \
                 min-width: 0; padding: 1.25rem; background: var(--card); color: var(--card-foreground); \
                 border: 1px solid var(--border); border-radius: calc(var(--radius) + 4px); \
                 box-shadow: 0 1px 2px oklch(0 0 0 / 0.04);",
                col_span.unwrap_or(1),
                row_span.unwrap_or(1),
            )}
        >
            {eyebrow
                .map(|e| {
                    view! {
                        <p style="margin: 0 0 0.5rem; font-size: 0.6875rem; font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted-foreground);">
                            {e}
                        </p>
                    }
                })}
            <div style="display: flex; align-items: center; gap: 0.625rem; margin-bottom: 0.5rem;">
                {icon
                    .map(|name| {
                        view! {
                            <span style="display: inline-flex; align-items: center; justify-content: center; width: 2.25rem; height: 2.25rem; border-radius: var(--radius-md); background: var(--secondary); color: var(--secondary-foreground); flex-shrink: 0;">
                                <Icon name=name size=18 />
                            </span>
                        }
                    })}
                {title
                    .map(|t| {
                        view! {
                            <h3 style="margin: 0; font-size: 0.9375rem; font-weight: 600; letter-spacing: -0.01em;">
                                {t}
                            </h3>
                        }
                    })}
            </div>
            {metric
                .map(|m| {
                    view! {
                        <p style={format!(
                            "margin: 0.25rem 0; font-size: 2rem; font-weight: 700; letter-spacing: -0.02em; line-height: 1.1; color: {};",
                            metric_tone.unwrap_or("var(--foreground)"),
                        )}>{m}</p>
                    }
                })}
            {description
                .map(|d| {
                    view! {
                        <p style="margin: 0.25rem 0 0; font-size: 0.875rem; line-height: 1.5; color: var(--muted-foreground);">
                            {d}
                        </p>
                    }
                })}
            {children.map(|c| view! { <div style="margin-top: 0.75rem;">{c()}</div> })}
            {footer_href
                .map(|href| {
                    view! {
                        <div style="margin-top: auto; padding-top: 0.875rem;">
                            <A
                                href=href
                                attr:style="display: inline-flex; align-items: center; gap: 0.375rem; font-size: 0.8125rem; font-weight: 500; color: var(--foreground); text-decoration: none;"
                            >
                                {footer_label.unwrap_or("View details")}
                                <Icon name=IconName::ArrowRight size=14 />
                            </A>
                        </div>
                    }
                })}
        </div>
    }
}
