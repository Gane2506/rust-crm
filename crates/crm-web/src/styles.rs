// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Shared inline-style constants for the CRM frontend.
//!
//! Styling is declarative CSS via inline `style` attributes only — no
//! handwritten JavaScript, no CSS framework, per the handoff's Rust-only
//! rule. These constants keep the look consistent across pages; visual
//! polish lands in task `06-UI-02`.

/// Page section container.
pub const SECTION: &str = "max-width: 56rem;";
/// Page title (`h2`).
pub const H2: &str = "font-size: 1.5rem; margin: 0 0 1rem;";
/// Section subtitle (`h3`).
pub const H3: &str = "font-size: 1.125rem; margin: 1.5rem 0 0.75rem;";
/// Card container (forms, list rows, detail panels).
pub const CARD: &str =
    "border: 1px solid #e5e7eb; border-radius: 0.5rem; padding: 1rem; margin-bottom: 1rem; background: white;";
/// Muted secondary text.
pub const MUTED: &str = "font-size: 0.875rem; color: #6b7280;";
/// Monospace identifier (short ids, hashes).
pub const MONO: &str = "font-family: ui-monospace, monospace; font-size: 0.875rem;";
/// Base button.
pub const BTN: &str =
    "padding: 0.375rem 0.75rem; border: 1px solid #d1d5db; border-radius: 0.375rem; background: white; cursor: pointer;";
/// Primary action button.
pub const BTN_PRIMARY: &str =
    "padding: 0.375rem 0.75rem; border: none; border-radius: 0.375rem; background: #2563eb; color: white; cursor: pointer;";
/// Destructive action button.
pub const BTN_DANGER: &str =
    "padding: 0.375rem 0.75rem; border: none; border-radius: 0.375rem; background: #b91c1c; color: white; cursor: pointer;";
/// Text input.
pub const INPUT: &str =
    "width: 100%; padding: 0.5rem; border: 1px solid #d1d5db; border-radius: 0.375rem; box-sizing: border-box;";
/// Form label.
pub const LABEL: &str =
    "display: block; margin-bottom: 0.25rem; font-size: 0.875rem; font-weight: 600;";
/// Form grid.
pub const FORM: &str = "display: grid; gap: 0.5rem; max-width: 32rem;";
/// Error alert box.
pub const ALERT_ERROR: &str =
    "border: 1px solid #fca5a5; background: #fef2f2; border-radius: 0.5rem; padding: 0.75rem; max-width: 32rem;";
/// Success/status box.
pub const ALERT_OK: &str =
    "border: 1px solid #86efac; background: #f0fdf4; border-radius: 0.5rem; padding: 0.75rem; max-width: 32rem;";
/// Definition list for detail views.
pub const DL: &str =
    "margin: 0.5rem 0 0; display: grid; grid-template-columns: auto 1fr; gap: 0.25rem 0.75rem; font-size: 0.875rem;";
/// Definition term.
pub const DT: &str = "color: #6b7280;";
/// Status pill.
pub const PILL: &str =
    "display: inline-block; padding: 0.125rem 0.5rem; border-radius: 9999px; font-size: 0.75rem; background: #f3f4f6; color: #374151;";
/// Data table.
pub const TABLE: &str = "width: 100%; border-collapse: collapse; font-size: 0.875rem;";
/// Table header cell.
pub const TH: &str =
    "text-align: left; padding: 0.5rem; border-bottom: 2px solid #e5e7eb; color: #6b7280; font-weight: 600;";
/// Table data cell.
pub const TD: &str = "padding: 0.5rem; border-bottom: 1px solid #f3f4f6; vertical-align: top;";
