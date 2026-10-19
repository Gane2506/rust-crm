// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Lucide-style inline SVG icon set for the component library v2.
//!
//! Every icon is hand-ported path data (24x24, stroke-based, drawn with
//! `currentColor`) rendered through Leptos SVG elements — no image assets,
//! no icon font, no JavaScript. [`Icon`] renders [`IconName`] at a given
//! size; `Loader2` pairs with the `spin` prop for the `crm-spin`
//! animation defined in [`crate::theme::theme_css`].

use leptos::prelude::*;

/// Icon identifier: one variant per Lucide-style glyph.
///
/// Render with [`Icon`]; size and spin are [`Icon`] props.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    /// `layout-dashboard` glyph.
    LayoutDashboard,
    /// `layout-grid` glyph.
    LayoutGrid,
    /// `users` glyph.
    Users,
    /// `user` glyph.
    User,
    /// `briefcase` glyph.
    Briefcase,
    /// `image` glyph.
    Image,
    /// `mail` glyph.
    Mail,
    /// `megaphone` glyph.
    Megaphone,
    /// `bot` glyph.
    Bot,
    /// `git-branch` glyph.
    GitBranch,
    /// `clipboard-list` glyph.
    ClipboardList,
    /// `rss` glyph.
    Rss,
    /// `receipt` glyph.
    Receipt,
    /// `pen-line` glyph.
    PenLine,
    /// `phone` glyph.
    Phone,
    /// `bar-chart3` glyph.
    BarChart3,
    /// `settings` glyph.
    Settings,
    /// `shield` glyph.
    Shield,
    /// `shield-check` glyph.
    ShieldCheck,
    /// `search` glyph.
    Search,
    /// `plus` glyph.
    Plus,
    /// `x` glyph.
    X,
    /// `check` glyph.
    Check,
    /// `chevron-down` glyph.
    ChevronDown,
    /// `chevron-right` glyph.
    ChevronRight,
    /// `arrow-right` glyph.
    ArrowRight,
    /// `sun` glyph.
    Sun,
    /// `moon` glyph.
    Moon,
    /// `log-out` glyph.
    LogOut,
    /// `menu` glyph.
    Menu,
    /// `bell` glyph.
    Bell,
    /// `calendar` glyph.
    Calendar,
    /// `clock` glyph.
    Clock,
    /// `dollar-sign` glyph.
    DollarSign,
    /// `file-text` glyph.
    FileText,
    /// `filter` glyph.
    Filter,
    /// `download` glyph.
    Download,
    /// `upload` glyph.
    Upload,
    /// `trash2` glyph.
    Trash2,
    /// `pencil` glyph.
    Pencil,
    /// `eye` glyph.
    Eye,
    /// `alert-triangle` glyph.
    AlertTriangle,
    /// `info` glyph.
    Info,
    /// `check-circle2` glyph.
    CheckCircle2,
    /// `x-circle` glyph.
    XCircle,
    /// `loader2` glyph.
    Loader2,
}

/// SVG child elements for `name`, as an opaque view.
///
/// Kept private: callers use [`Icon`], which wraps these in the
/// `<svg>` shell with the shared stroke settings.
fn icon_children(name: IconName) -> AnyView {
    match name {
        IconName::LayoutDashboard => view! {
            <>
                <rect width="7" height="9" x="3" y="3" rx="1" />
                <rect width="7" height="5" x="14" y="3" rx="1" />
                <rect width="7" height="9" x="14" y="12" rx="1" />
                <rect width="7" height="5" x="3" y="16" rx="1" />
            </>
        }
        .into_any(),
        IconName::LayoutGrid => view! {
            <>
                <rect width="7" height="7" x="3" y="3" rx="1" />
                <rect width="7" height="7" x="14" y="3" rx="1" />
                <rect width="7" height="7" x="14" y="14" rx="1" />
                <rect width="7" height="7" x="3" y="14" rx="1" />
            </>
        }
        .into_any(),
        IconName::Users => view! {
            <>
                <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
                <circle cx="9" cy="7" r="4" />
                <path d="M22 21v-2a4 4 0 0 0-3-3.87" />
                <path d="M16 3.13a4 4 0 0 1 0 7.75" />
            </>
        }
        .into_any(),
        IconName::User => view! {
            <>
                <path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2" />
                <circle cx="12" cy="7" r="4" />
            </>
        }
        .into_any(),
        IconName::Briefcase => view! {
            <>
                <path d="M16 20V4a2 2 0 0 0-2-2h-4a2 2 0 0 0-2 2v16" />
                <rect width="20" height="14" x="2" y="7" rx="2" />
            </>
        }
        .into_any(),
        IconName::Image => view! {
            <>
                <rect width="18" height="18" x="3" y="3" rx="2" ry="2" />
                <circle cx="9" cy="9" r="2" />
                <path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" />
            </>
        }
        .into_any(),
        IconName::Mail => view! {
            <>
                <rect width="20" height="16" x="2" y="4" rx="2" />
                <path d="m22 7-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 7" />
            </>
        }
        .into_any(),
        IconName::Megaphone => view! {
            <>
                <path d="m3 11 18-5v12L3 14v-3z" />
                <path d="M11.6 16.8a3 3 0 1 1-5.8-1.6" />
            </>
        }
        .into_any(),
        IconName::Bot => view! {
            <>
                <path d="M12 8V4H8" />
                <rect width="16" height="12" x="4" y="8" rx="2" />
                <path d="M2 14h2" />
                <path d="M20 14h2" />
                <path d="M15 13v2" />
                <path d="M9 13v2" />
            </>
        }
        .into_any(),
        IconName::GitBranch => view! {
            <>
                <line x1="6" x2="6" y1="3" y2="15" />
                <circle cx="18" cy="6" r="3" />
                <circle cx="6" cy="18" r="3" />
                <path d="M18 9a9 9 0 0 1-9 9" />
            </>
        }
        .into_any(),
        IconName::ClipboardList => view! {
            <>
                <rect width="8" height="4" x="8" y="2" rx="1" ry="1" />
                <path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" />
                <path d="M12 11h4" />
                <path d="M12 16h4" />
                <path d="M8 11h.01" />
                <path d="M8 16h.01" />
            </>
        }
        .into_any(),
        IconName::Rss => view! {
            <>
                <path d="M4 11a9 9 0 0 1 9 9" />
                <path d="M4 4a16 16 0 0 1 16 16" />
                <circle cx="5" cy="19" r="1" />
            </>
        }
        .into_any(),
        IconName::Receipt => view! {
            <>
                <path d="M4 2v20l2-1 2 1 2-1 2 1 2-1 2 1 2-1 2 1V2l-2 1-2-1-2 1-2-1-2 1-2-1-2 1Z" />
                <path d="M14 8H8" />
                <path d="M16 12H8" />
                <path d="M13 16H8" />
            </>
        }
        .into_any(),
        IconName::PenLine => view! {
            <>
                <path d="M12 20h9" />
                <path d="M16.376 3.622a1 1 0 0 1 3.002 3.002L7.368 18.635a2 2 0 0 1-.855.506l-2.872.838a.5.5 0 0 1-.62-.62l.838-2.872a2 2 0 0 1 .506-.854z" />
            </>
        }
        .into_any(),
        IconName::Phone => view! {
            <>
                <path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92z" />
            </>
        }
        .into_any(),
        IconName::BarChart3 => view! {
            <>
                <path d="M3 3v16a2 2 0 0 0 2 2h16" />
                <path d="M18 17V9" />
                <path d="M13 17V5" />
                <path d="M8 17v-3" />
            </>
        }
        .into_any(),
        IconName::Settings => view! {
            <>
                <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
                <circle cx="12" cy="12" r="3" />
            </>
        }
        .into_any(),
        IconName::Shield => view! {
            <>
                <path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" />
            </>
        }
        .into_any(),
        IconName::ShieldCheck => view! {
            <>
                <path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" />
                <path d="m9 12 2 2 4-4" />
            </>
        }
        .into_any(),
        IconName::Search => view! {
            <>
                <circle cx="11" cy="11" r="8" />
                <path d="m21 21-4.3-4.3" />
            </>
        }
        .into_any(),
        IconName::Plus => view! {
            <>
                <path d="M5 12h14" />
                <path d="M12 5v14" />
            </>
        }
        .into_any(),
        IconName::X => view! {
            <>
                <path d="M18 6 6 18" />
                <path d="m6 6 12 12" />
            </>
        }
        .into_any(),
        IconName::Check => view! {
            <>
                <path d="M20 6 9 17l-5-5" />
            </>
        }
        .into_any(),
        IconName::ChevronDown => view! {
            <>
                <path d="m6 9 6 6 6-6" />
            </>
        }
        .into_any(),
        IconName::ChevronRight => view! {
            <>
                <path d="m9 18 6-6-6-6" />
            </>
        }
        .into_any(),
        IconName::ArrowRight => view! {
            <>
                <path d="M5 12h14" />
                <path d="m12 5 7 7-7 7" />
            </>
        }
        .into_any(),
        IconName::Sun => view! {
            <>
                <circle cx="12" cy="12" r="4" />
                <path d="M12 2v2" />
                <path d="M12 20v2" />
                <path d="m4.93 4.93 1.41 1.41" />
                <path d="m17.66 17.66 1.41 1.41" />
                <path d="M2 12h2" />
                <path d="M20 12h2" />
                <path d="m6.34 17.66-1.41 1.41" />
                <path d="m19.07 4.93-1.41 1.41" />
            </>
        }
        .into_any(),
        IconName::Moon => view! {
            <>
                <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
            </>
        }
        .into_any(),
        IconName::LogOut => view! {
            <>
                <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
                <polyline points="16 17 21 12 16 7" />
                <line x1="21" x2="9" y1="12" y2="12" />
            </>
        }
        .into_any(),
        IconName::Menu => view! {
            <>
                <line x1="4" x2="20" y1="12" y2="12" />
                <line x1="4" x2="20" y1="6" y2="6" />
                <line x1="4" x2="20" y1="18" y2="18" />
            </>
        }
        .into_any(),
        IconName::Bell => view! {
            <>
                <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
                <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
            </>
        }
        .into_any(),
        IconName::Calendar => view! {
            <>
                <path d="M8 2v4" />
                <path d="M16 2v4" />
                <rect width="18" height="18" x="3" y="4" rx="2" />
                <path d="M3 10h18" />
            </>
        }
        .into_any(),
        IconName::Clock => view! {
            <>
                <circle cx="12" cy="12" r="10" />
                <polyline points="12 6 12 12 16 14" />
            </>
        }
        .into_any(),
        IconName::DollarSign => view! {
            <>
                <line x1="12" x2="12" y1="2" y2="22" />
                <path d="M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" />
            </>
        }
        .into_any(),
        IconName::FileText => view! {
            <>
                <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
                <path d="M14 2v4a2 2 0 0 0 2 2h4" />
                <path d="M10 9H8" />
                <path d="M16 13H8" />
                <path d="M16 17H8" />
            </>
        }
        .into_any(),
        IconName::Filter => view! {
            <>
                <polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3" />
            </>
        }
        .into_any(),
        IconName::Download => view! {
            <>
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" x2="12" y1="15" y2="3" />
            </>
        }
        .into_any(),
        IconName::Upload => view! {
            <>
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="17 8 12 3 7 8" />
                <line x1="12" x2="12" y1="3" y2="15" />
            </>
        }
        .into_any(),
        IconName::Trash2 => view! {
            <>
                <path d="M3 6h18" />
                <path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" />
                <path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2" />
                <line x1="10" x2="10" y1="11" y2="17" />
                <line x1="14" x2="14" y1="11" y2="17" />
            </>
        }
        .into_any(),
        IconName::Pencil => view! {
            <>
                <path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z" />
                <path d="m15 5 4 4" />
            </>
        }
        .into_any(),
        IconName::Eye => view! {
            <>
                <path d="M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0" />
                <circle cx="12" cy="12" r="3" />
            </>
        }
        .into_any(),
        IconName::AlertTriangle => view! {
            <>
                <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3" />
                <path d="M12 9v4" />
                <path d="M12 17h.01" />
            </>
        }
        .into_any(),
        IconName::Info => view! {
            <>
                <circle cx="12" cy="12" r="10" />
                <path d="M12 16v-4" />
                <path d="M12 8h.01" />
            </>
        }
        .into_any(),
        IconName::CheckCircle2 => view! {
            <>
                <circle cx="12" cy="12" r="10" />
                <path d="m9 12 2 2 4-4" />
            </>
        }
        .into_any(),
        IconName::XCircle => view! {
            <>
                <circle cx="12" cy="12" r="10" />
                <path d="m15 9-6 6" />
                <path d="m9 9 6 6" />
            </>
        }
        .into_any(),
        IconName::Loader2 => view! {
            <>
                <path d="M21 12a9 9 0 1 1-6.219-8.56" />
            </>
        }
        .into_any(),
    }
}

/// Inline SVG icon (Lucide-style, `currentColor` stroke).
///
/// # Props
/// - `name`: the [`IconName`] glyph to render.
/// - `size`: pixel size (default 16).
/// - `spin`: adds the `crm-spin` rotation animation (for [`IconName::Loader2`]).
/// - `class`: optional extra CSS classes.
#[component]
pub fn Icon(
    name: IconName,
    #[prop(optional)] size: Option<u16>,
    #[prop(optional)] spin: bool,
    #[prop(optional)] class: Option<&'static str>,
) -> impl IntoView {
    let size = size.unwrap_or(16);
    let cls = if spin {
        match class {
            Some(c) => format!("crm-spin {c}"),
            None => "crm-spin".to_string(),
        }
    } else {
        class.unwrap_or_default().to_string()
    };
    view! {
        <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            width={size.to_string()}
            height={size.to_string()}
            class=cls
            aria-hidden="true"
            style="flex-shrink: 0; vertical-align: middle;"
        >
            {icon_children(name)}
        </svg>
    }
}
