// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Design tokens for the CRM frontend, in Twenty CRM's design language
//! (reimplemented from scratch — no copied assets).
//!
//! Styling is declarative CSS via inline `style` attributes only — no
//! handwritten JavaScript, no CSS framework, per the handoff's Rust-only
//! rule. These constants are the single source of truth for the visual
//! language: a Notion-like light theme with an Inter type stack, a left
//! navigation rail, a utility top bar, clean data tables, and record
//! pages. The component library in [`crate::ui`] composes these tokens.

/// App root: counteracts the browser's default body margin and sets the
/// base font. The sidebar layout needs full-bleed, so this (not a
/// centered max-width column) is the top-level wrapper.
pub const APP_ROOT: &str =
    "margin: -8px; min-height: 100vh; display: flex; background: #f8fafc; color: #0f172a; font-family: Inter, -apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif; font-size: 13px; line-height: 1.5;";

/// Left navigation rail.
pub const SIDEBAR: &str =
    "width: 220px; flex-shrink: 0; min-height: 100vh; background: #ffffff; border-right: 1px solid #e5e7eb; display: flex; flex-direction: column; position: sticky; top: 0; height: 100vh;";
/// Workspace mark row at the top of the sidebar.
pub const SIDEBAR_BRAND: &str =
    "display: flex; align-items: center; gap: 0.625rem; padding: 1rem 1rem 0.75rem;";
/// Workspace monogram tile.
pub const SIDEBAR_MONOGRAM: &str =
    "width: 2rem; height: 2rem; border-radius: 0.5rem; background: #2563eb; color: #ffffff; display: flex; align-items: center; justify-content: center; font-weight: 700; font-size: 0.875rem;";
/// Workspace name next to the monogram.
pub const SIDEBAR_BRAND_NAME: &str = "font-weight: 600; font-size: 0.875rem; color: #0f172a;";
/// Nav section label ("Workspace", "System").
pub const SIDEBAR_SECTION: &str =
    "font-size: 0.6875rem; font-weight: 600; letter-spacing: 0.04em; text-transform: uppercase; color: #94a3b8; padding: 0.75rem 1rem 0.25rem;";
/// Nav item (inactive).
pub const SIDEBAR_ITEM: &str =
    "display: flex; align-items: center; gap: 0.625rem; padding: 0.4375rem 0.75rem; margin: 0 0.5rem; border-radius: 0.375rem; color: #475569; text-decoration: none; font-size: 0.8125rem; font-weight: 500;";
/// Nav item (active): soft blue wash + accent text.
pub const SIDEBAR_ITEM_ACTIVE: &str =
    "display: flex; align-items: center; gap: 0.625rem; padding: 0.4375rem 0.75rem; margin: 0 0.5rem; border-radius: 0.375rem; background: #eff6ff; color: #2563eb; text-decoration: none; font-size: 0.8125rem; font-weight: 600;";
/// Sidebar footer (log out).
pub const SIDEBAR_FOOTER: &str =
    "margin-top: auto; padding: 0.75rem 1rem; border-top: 1px solid #f1f5f9;";

/// Main column next to the sidebar.
pub const MAIN_COL: &str = "flex: 1; min-width: 0; display: flex; flex-direction: column;";
/// Utility top bar: search trigger + user chip.
pub const TOPBAR: &str =
    "height: 3.5rem; background: #ffffff; border-bottom: 1px solid #e5e7eb; display: flex; align-items: center; gap: 0.75rem; padding: 0 1.5rem; position: sticky; top: 0; z-index: 10;";
/// Search trigger button in the top bar.
pub const SEARCH_TRIGGER: &str =
    "display: flex; align-items: center; gap: 0.5rem; border: 1px solid #e5e7eb; border-radius: 0.375rem; background: #f8fafc; color: #64748b; font-size: 0.8125rem; padding: 0.375rem 0.625rem; cursor: pointer; min-width: 16rem;";
/// User avatar chip.
pub const AVATAR: &str =
    "width: 1.75rem; height: 1.75rem; border-radius: 9999px; background: #e0e7ff; color: #3730a3; display: inline-flex; align-items: center; justify-content: center; font-size: 0.75rem; font-weight: 600;";
/// Content wrapper inside the main column.
pub const CONTENT: &str =
    "padding: 1.5rem 2rem 3rem; max-width: 76rem; width: 100%; box-sizing: border-box;";

/// Page title row (title + actions).
pub const PAGE_HEAD: &str =
    "display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; margin-bottom: 1.25rem;";
/// Page title (`h2`).
pub const H2: &str =
    "font-size: 1.25rem; font-weight: 600; margin: 0; color: #0f172a; letter-spacing: -0.01em;";
/// Page description under the title.
pub const PAGE_DESC: &str = "margin: 0.25rem 0 0; font-size: 0.8125rem; color: #64748b;";
/// Section subtitle (`h3`).
pub const H3: &str =
    "font-size: 0.875rem; font-weight: 600; margin: 1.5rem 0 0.75rem; color: #0f172a;";
/// Card container (forms, list rows, detail panels).
pub const CARD: &str =
    "border: 1px solid #e5e7eb; border-radius: 0.5rem; padding: 1.25rem; margin-bottom: 1rem; background: #ffffff; box-shadow: 0 1px 2px rgba(15, 23, 42, 0.04);";
/// Muted secondary text.
pub const MUTED: &str = "font-size: 0.8125rem; color: #64748b;";
/// Monospace identifier (short ids, hashes).
pub const MONO: &str =
    "font-family: ui-monospace, SFMono-Regular, monospace; font-size: 0.8125rem;";
/// Content link.
pub const LINK: &str = "color: #2563eb; text-decoration: none; font-weight: 500;";

/// Base button (secondary).
pub const BTN: &str =
    "padding: 0.4375rem 0.875rem; border: 1px solid #e5e7eb; border-radius: 0.375rem; background: #ffffff; color: #0f172a; font-size: 0.8125rem; font-weight: 500; cursor: pointer; font-family: inherit; text-decoration: none;";
/// Primary action button.
pub const BTN_PRIMARY: &str =
    "padding: 0.4375rem 0.875rem; border: 1px solid #2563eb; border-radius: 0.375rem; background: #2563eb; color: #ffffff; font-size: 0.8125rem; font-weight: 500; cursor: pointer; font-family: inherit; text-decoration: none;";
/// Destructive action button.
pub const BTN_DANGER: &str =
    "padding: 0.4375rem 0.875rem; border: 1px solid #dc2626; border-radius: 0.375rem; background: #dc2626; color: #ffffff; font-size: 0.8125rem; font-weight: 500; cursor: pointer; font-family: inherit; text-decoration: none;";
/// Ghost button (sidebar footer, subtle actions).
pub const BTN_GHOST: &str =
    "padding: 0.4375rem 0.75rem; border: none; border-radius: 0.375rem; background: transparent; color: #475569; font-size: 0.8125rem; font-weight: 500; cursor: pointer; font-family: inherit; display: inline-flex; align-items: center; gap: 0.5rem; text-decoration: none;";
/// Disabled button overlay (appended when disabled).
pub const BTN_DISABLED: &str = "opacity: 0.55; cursor: not-allowed;";

/// Text input.
pub const INPUT: &str =
    "width: 100%; padding: 0.5rem 0.625rem; border: 1px solid #e5e7eb; border-radius: 0.375rem; box-sizing: border-box; font-size: 0.8125rem; font-family: inherit; color: #0f172a; background: #ffffff;";
/// Form label.
pub const LABEL: &str =
    "display: block; margin-bottom: 0.375rem; font-size: 0.8125rem; font-weight: 500; color: #334155;";
/// Form grid.
pub const FORM: &str = "display: grid; gap: 0.875rem; max-width: 36rem;";
/// Form card title.
pub const FORM_TITLE: &str =
    "margin: 0 0 1rem; font-size: 0.9375rem; font-weight: 600; color: #0f172a;";

/// Error alert box.
pub const ALERT_ERROR: &str =
    "border: 1px solid #fecaca; background: #fef2f2; border-radius: 0.5rem; padding: 0.75rem 1rem; max-width: 36rem; font-size: 0.8125rem; color: #991b1b;";
/// Success/status box.
pub const ALERT_OK: &str =
    "border: 1px solid #bbf7d0; background: #f0fdf4; border-radius: 0.5rem; padding: 0.75rem 1rem; max-width: 36rem; font-size: 0.8125rem; color: #166534;";

/// Definition list for detail views.
pub const DL: &str =
    "margin: 0; display: grid; grid-template-columns: 11rem 1fr; gap: 0.625rem 1rem; font-size: 0.8125rem;";
/// Definition term.
pub const DT: &str = "color: #64748b; font-weight: 500;";
/// Definition description.
pub const DD: &str = "margin: 0; color: #0f172a;";

/// Status pill (generic gray).
pub const PILL: &str =
    "display: inline-block; padding: 0.125rem 0.625rem; border-radius: 9999px; font-size: 0.75rem; font-weight: 500; background: #f1f5f9; color: #475569; border: 1px solid #e5e7eb;";

/// Table wrapper: bordered, rounded, horizontal scroll on narrow screens.
pub const TABLE_WRAPPER: &str =
    "border: 1px solid #e5e7eb; border-radius: 0.5rem; overflow-x: auto; background: #ffffff; box-shadow: 0 1px 2px rgba(15, 23, 42, 0.04);";
/// Data table.
pub const TABLE: &str = "width: 100%; border-collapse: collapse; font-size: 0.8125rem;";
/// Table header cell.
pub const TH: &str =
    "text-align: left; padding: 0.625rem 0.875rem; border-bottom: 1px solid #e5e7eb; color: #64748b; font-weight: 600; font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.03em; white-space: nowrap; background: #f8fafc;";
/// Table data cell.
pub const TD: &str =
    "padding: 0.75rem 0.875rem; border-bottom: 1px solid #f1f5f9; vertical-align: middle;";
/// Table row hover background (applied by the `HoverTr` component).
pub const TR_HOVER_BG: &str = "#f8fafc";

/// Record page two-column grid (fields left, timeline right).
pub const RECORD_GRID: &str =
    "display: grid; grid-template-columns: minmax(0, 1fr) 22rem; gap: 1rem; align-items: start;";
/// Timeline item.
pub const TIMELINE_ITEM: &str =
    "display: flex; gap: 0.75rem; padding: 0.625rem 0; border-bottom: 1px solid #f1f5f9; font-size: 0.8125rem;";
/// Timeline dot.
pub const TIMELINE_DOT: &str =
    "width: 0.5rem; height: 0.5rem; border-radius: 9999px; background: #cbd5e1; margin-top: 0.375rem; flex-shrink: 0;";

/// Empty state container.
pub const EMPTY_STATE: &str =
    "border: 1px dashed #e5e7eb; border-radius: 0.5rem; padding: 2.5rem 1.5rem; text-align: center; background: #ffffff;";
/// Empty state icon.
pub const EMPTY_ICON: &str = "font-size: 1.75rem; color: #cbd5e1;";

/// Command-menu overlay.
pub const OVERLAY: &str =
    "position: fixed; inset: 0; background: rgba(15, 23, 42, 0.4); z-index: 50; display: flex; justify-content: center; padding-top: 12vh;";
/// Command-menu panel.
pub const CMD_PANEL: &str =
    "width: 36rem; max-width: calc(100vw - 2rem); height: fit-content; background: #ffffff; border-radius: 0.75rem; box-shadow: 0 20px 50px rgba(15, 23, 42, 0.25); overflow: hidden;";
/// Command-menu input.
pub const CMD_INPUT: &str =
    "width: 100%; box-sizing: border-box; border: none; border-bottom: 1px solid #e5e7eb; padding: 0.875rem 1rem; font-size: 0.875rem; font-family: inherit; outline: none;";
/// Command-menu item row.
pub const CMD_ITEM: &str =
    "display: flex; align-items: center; gap: 0.625rem; width: 100%; box-sizing: border-box; padding: 0.625rem 1rem; font-size: 0.8125rem; color: #0f172a; text-decoration: none;";

/// Login page backdrop (rendered outside the app shell, so it counteracts
/// the body margin itself).
pub const LOGIN_PAGE: &str =
    "margin: -8px; min-height: 100vh; display: flex; align-items: center; justify-content: center; background: #f8fafc; padding: 1rem; box-sizing: border-box; font-family: Inter, -apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif;";
/// Login card.
pub const LOGIN_CARD: &str =
    "width: 24rem; max-width: 100%; background: #ffffff; border: 1px solid #e5e7eb; border-radius: 0.75rem; padding: 2rem; box-shadow: 0 10px 30px rgba(15, 23, 42, 0.08);";

/// Dashboard quick-link tile grid.
pub const TILE_GRID: &str =
    "display: grid; grid-template-columns: repeat(auto-fill, minmax(14rem, 1fr)); gap: 1rem;";
/// Dashboard quick-link tile.
pub const TILE: &str =
    "display: block; border: 1px solid #e5e7eb; border-radius: 0.5rem; padding: 1.25rem; background: #ffffff; text-decoration: none; color: inherit; box-shadow: 0 1px 2px rgba(15, 23, 42, 0.04);";
/// Tile icon chip.
pub const TILE_ICON: &str =
    "width: 2.25rem; height: 2.25rem; border-radius: 0.5rem; background: #eff6ff; color: #2563eb; display: flex; align-items: center; justify-content: center; font-size: 1.125rem; margin-bottom: 0.75rem;";
