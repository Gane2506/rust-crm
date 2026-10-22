// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Selectable UI styles (task UI3-01): sixteen design languages, 100% Rust.
//!
//! Each [`UiStyle`] renders to a CSS override block via [`style_css`],
//! injected as `<style id="crm-ui-style">` layered after the base
//! [`theme_css`](crate::theme::theme_css) and the branding overrides.
//! The v2 components style themselves from design tokens
//! (`var(--primary)` et al.) through inline styles, so overriding the
//! tokens re-themes the whole app; a handful of element rules
//! (`button`, `input`, `aside`, …) add the per-style surface treatment
//! that tokens alone cannot express.
//!
//! [`UiStyle::as_str`] indexes
//! [`UI_STYLE_IDS`](crm_domain::branding::UI_STYLE_IDS) — the domain's
//! single source of truth — so the kebab-case ids can never drift from
//! the API validation. The domain rejects unknown ids at the API
//! boundary; [`apply_ui_style`] falls back to [`UiStyle::BentoGrid`] for
//! anything unrecognized, so rendering never panics.
//!
//! All font stacks are offline-safe system stacks (no `@import`, no
//! webfonts). The blocks target only real hooks: design tokens, the
//! `body`/`button`/`input`/`select`/`textarea`/`a`/`h1`–`h3`/`aside`
//! elements, and the `.crm-bento-tile` class.

/// One of the sixteen selectable UI styles.
///
/// Variant order matches
/// [`UI_STYLE_IDS`](crm_domain::branding::UI_STYLE_IDS); keep them in
/// sync — [`UiStyle::as_str`] indexes the domain table by discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiStyle {
    /// Quiet monochrome, thin rules, generous whitespace.
    Minimalism,
    /// Bold flat colour, crisp edges, subtle depth.
    FlatDesign2,
    /// Rich gradients and inset shadows, like real materials.
    Skeuomorphism,
    /// Soft extruded shapes on a calm monochrome surface.
    Neumorphism,
    /// Frosted translucent panels over a vivid gradient.
    Glassmorphism,
    /// Chunky pastel clay with thick soft shadows.
    Claymorphism,
    /// The default — tidy bento tiles, dense but airy.
    BentoGrid,
    /// Everything lives on elevated cards over a muted canvas.
    CardsBased,
    /// Bold two-tone shell — dark rail, light canvas.
    SplitScreen,
    /// Raw and honest — system type, hard rules, no decoration.
    Brutalism,
    /// Thick black borders, hard offset shadows, loud colour.
    NeoBrutalism,
    /// Primary red, yellow and blue geometry on black rules.
    Bauhaus,
    /// Wobbly ink borders and a handwritten feel.
    HandDrawn,
    /// Neon glow on near-black — cyan and magenta.
    Cyberpunk,
    /// Bubbly pastel gradients with a chrome shine.
    Y2K,
    /// Chunky pixels, hard steps, terminal monospace.
    PixelArt,
}

/// All sixteen styles, in [`UiStyle`] discriminant order.
pub const ALL_STYLES: [UiStyle; 16] = [
    UiStyle::Minimalism,
    UiStyle::FlatDesign2,
    UiStyle::Skeuomorphism,
    UiStyle::Neumorphism,
    UiStyle::Glassmorphism,
    UiStyle::Claymorphism,
    UiStyle::BentoGrid,
    UiStyle::CardsBased,
    UiStyle::SplitScreen,
    UiStyle::Brutalism,
    UiStyle::NeoBrutalism,
    UiStyle::Bauhaus,
    UiStyle::HandDrawn,
    UiStyle::Cyberpunk,
    UiStyle::Y2K,
    UiStyle::PixelArt,
];

impl UiStyle {
    /// Kebab-case id, e.g. `"neo-brutalism"`.
    ///
    /// Indexes the domain's [`UI_STYLE_IDS`](crm_domain::branding::UI_STYLE_IDS)
    /// by discriminant, so the id can never drift from API validation.
    pub fn as_str(&self) -> &'static str {
        crm_domain::branding::UI_STYLE_IDS[*self as usize]
    }

    /// Parses a kebab-case id; `None` for anything unknown.
    ///
    /// Trims surrounding whitespace, matching the domain validator.
    pub fn from_str(id: &str) -> Option<UiStyle> {
        match id.trim() {
            "minimalism" => Some(UiStyle::Minimalism),
            "flat-design-2" => Some(UiStyle::FlatDesign2),
            "skeuomorphism" => Some(UiStyle::Skeuomorphism),
            "neumorphism" => Some(UiStyle::Neumorphism),
            "glassmorphism" => Some(UiStyle::Glassmorphism),
            "claymorphism" => Some(UiStyle::Claymorphism),
            "bento-grid" => Some(UiStyle::BentoGrid),
            "cards-based" => Some(UiStyle::CardsBased),
            "split-screen" => Some(UiStyle::SplitScreen),
            "brutalism" => Some(UiStyle::Brutalism),
            "neo-brutalism" => Some(UiStyle::NeoBrutalism),
            "bauhaus" => Some(UiStyle::Bauhaus),
            "hand-drawn" => Some(UiStyle::HandDrawn),
            "cyberpunk" => Some(UiStyle::Cyberpunk),
            "y2k" => Some(UiStyle::Y2K),
            "pixel-art" => Some(UiStyle::PixelArt),
            _ => None,
        }
    }

    /// Display name, e.g. `"Neo-Brutalism"`.
    pub fn label(&self) -> &'static str {
        match self {
            UiStyle::Minimalism => "Minimalism",
            UiStyle::FlatDesign2 => "Flat Design 2.0",
            UiStyle::Skeuomorphism => "Skeuomorphism",
            UiStyle::Neumorphism => "Neumorphism",
            UiStyle::Glassmorphism => "Glassmorphism",
            UiStyle::Claymorphism => "Claymorphism",
            UiStyle::BentoGrid => "Bento Grid",
            UiStyle::CardsBased => "Cards-Based UI",
            UiStyle::SplitScreen => "Split-Screen Design",
            UiStyle::Brutalism => "Brutalism",
            UiStyle::NeoBrutalism => "Neo-Brutalism",
            UiStyle::Bauhaus => "Bauhaus Style",
            UiStyle::HandDrawn => "Hand-Drawn",
            UiStyle::Cyberpunk => "Cyberpunk",
            UiStyle::Y2K => "Y2K",
            UiStyle::PixelArt => "Pixel Art UI",
        }
    }

    /// One-line description shown in the style picker.
    pub fn description(&self) -> &'static str {
        match self {
            UiStyle::Minimalism => "Quiet monochrome, thin rules, generous whitespace.",
            UiStyle::FlatDesign2 => "Bold flat colour, crisp edges, subtle depth.",
            UiStyle::Skeuomorphism => "Rich gradients and inset shadows, like real materials.",
            UiStyle::Neumorphism => "Soft extruded shapes on a calm monochrome surface.",
            UiStyle::Glassmorphism => "Frosted translucent panels over a vivid gradient.",
            UiStyle::Claymorphism => "Chunky pastel clay with thick soft shadows.",
            UiStyle::BentoGrid => "The default — tidy bento tiles, dense but airy.",
            UiStyle::CardsBased => "Everything lives on elevated cards over a muted canvas.",
            UiStyle::SplitScreen => "Bold two-tone shell — dark rail, light canvas.",
            UiStyle::Brutalism => "Raw and honest — system type, hard rules, no decoration.",
            UiStyle::NeoBrutalism => "Thick black borders, hard offset shadows, loud colour.",
            UiStyle::Bauhaus => "Primary red, yellow and blue geometry on black rules.",
            UiStyle::HandDrawn => "Wobbly ink borders and a handwritten feel.",
            UiStyle::Cyberpunk => "Neon glow on near-black — cyan and magenta.",
            UiStyle::Y2K => "Bubbly pastel gradients with a chrome shine.",
            UiStyle::PixelArt => "Chunky pixels, hard steps, terminal monospace.",
        }
    }

    /// Three representative swatch colors for the style picker card:
    /// (page background, primary accent, surface).
    pub fn swatch(&self) -> [&'static str; 3] {
        match self {
            UiStyle::Minimalism => ["#fcfcfc", "#4d4d4d", "#ffffff"],
            UiStyle::FlatDesign2 => ["#ffffff", "#2563eb", "#f1f5f9"],
            UiStyle::Skeuomorphism => ["#ececec", "#6b7280", "#f7f7f7"],
            UiStyle::Neumorphism => ["#e6e9f0", "#8a93a6", "#e6e9f0"],
            UiStyle::Glassmorphism => ["#7c5cff", "#a855f7", "#ffffff88"],
            UiStyle::Claymorphism => ["#f3e9d7", "#c084fc", "#fdf6ec"],
            UiStyle::BentoGrid => ["#ffffff", "#18181b", "#f4f4f5"],
            UiStyle::CardsBased => ["#f1f1f1", "#18181b", "#ffffff"],
            UiStyle::SplitScreen => ["#ffffff", "#1e1b4b", "#312e81"],
            UiStyle::Brutalism => ["#ffffff", "#000000", "#f5f5f5"],
            UiStyle::NeoBrutalism => ["#fefce8", "#facc15", "#f472b6"],
            UiStyle::Bauhaus => ["#ffffff", "#e30613", "#ffed00"],
            UiStyle::HandDrawn => ["#fffdf5", "#4f46e5", "#ffffff"],
            UiStyle::Cyberpunk => ["#0a0a14", "#00e5ff", "#ff2fb3"],
            UiStyle::Y2K => ["#f0abfc", "#e879f9", "#ffffff"],
            UiStyle::PixelArt => ["#142114", "#4ade80", "#1e2b1e"],
        }
    }
}

/// Returns the CSS override block for `style`.
///
/// Token overrides re-theme every v2 component (they read
/// `var(--token)` through inline styles); the element rules add the
/// surface treatment tokens cannot express. [`UiStyle::BentoGrid`]
/// returns an empty string — the base theme already *is* bento.
///
/// The block is layered after the base theme and branding styles, so
/// `:root` rules win ties on source order; each style also carries a
/// `.dark` companion so the dark-mode toggle keeps working.
pub fn style_css(style: &UiStyle) -> String {
    match style {
        UiStyle::Minimalism => r#"
/* ---- Minimalism: quiet monochrome, thin rules, generous whitespace ---- */
:root {
  --background: oklch(0.995 0 0);
  --foreground: oklch(0.30 0 0);
  --card: oklch(1 0 0);
  --card-foreground: oklch(0.30 0 0);
  --popover: oklch(1 0 0);
  --popover-foreground: oklch(0.30 0 0);
  --primary: oklch(0.30 0 0);
  --primary-foreground: oklch(0.99 0 0);
  --secondary: oklch(0.965 0 0);
  --secondary-foreground: oklch(0.38 0 0);
  --muted: oklch(0.97 0 0);
  --muted-foreground: oklch(0.60 0 0);
  --accent: oklch(0.97 0 0);
  --accent-foreground: oklch(0.30 0 0);
  --border: oklch(0.93 0 0);
  --input: oklch(0.93 0 0);
  --ring: oklch(0.72 0 0);
  --radius: 0.25rem;
}
.dark {
  --background: oklch(0.16 0 0);
  --foreground: oklch(0.84 0 0);
  --card: oklch(0.18 0 0);
  --card-foreground: oklch(0.84 0 0);
  --popover: oklch(0.18 0 0);
  --popover-foreground: oklch(0.84 0 0);
  --primary: oklch(0.84 0 0);
  --primary-foreground: oklch(0.16 0 0);
  --secondary: oklch(0.22 0 0);
  --secondary-foreground: oklch(0.80 0 0);
  --muted: oklch(0.20 0 0);
  --muted-foreground: oklch(0.62 0 0);
  --accent: oklch(0.20 0 0);
  --accent-foreground: oklch(0.84 0 0);
  --border: oklch(1 0 0 / 8%);
  --input: oklch(1 0 0 / 12%);
  --ring: oklch(0.55 0 0);
  --radius: 0.25rem;
}
h1, h2, h3 { font-weight: 500; letter-spacing: -0.01em; }
"#
        .to_string(),

        UiStyle::FlatDesign2 => r#"
/* ---- Flat Design 2.0: bold flat colour, crisp edges, subtle depth ---- */
:root {
  --primary: oklch(0.55 0.22 255);
  --primary-foreground: oklch(1 0 0);
  --ring: oklch(0.55 0.22 255);
  --accent: oklch(0.94 0.04 255);
  --accent-foreground: oklch(0.35 0.12 255);
  --secondary: oklch(0.94 0.03 255);
  --secondary-foreground: oklch(0.35 0.10 255);
  --chart-1: oklch(0.55 0.22 255);
  --border: oklch(0.90 0.01 255);
  --input: oklch(0.90 0.01 255);
  --radius: 0.5rem;
}
.dark {
  --primary: oklch(0.70 0.17 255);
  --primary-foreground: oklch(0.14 0.04 255);
  --ring: oklch(0.70 0.17 255);
  --accent: oklch(0.28 0.08 255);
  --accent-foreground: oklch(0.92 0.03 255);
  --secondary: oklch(0.26 0.05 255);
  --secondary-foreground: oklch(0.92 0.03 255);
  --chart-1: oklch(0.70 0.17 255);
  --radius: 0.5rem;
}
button { box-shadow: 0 2px 8px oklch(0 0 0 / 0.14); }
"#
        .to_string(),

        UiStyle::Skeuomorphism => r#"
/* ---- Skeuomorphism: rich gradients and inset shadows, like real materials ---- */
:root {
  --background: oklch(0.93 0 0);
  --card: oklch(0.97 0 0);
  --border: oklch(0.78 0 0);
  --input: oklch(0.80 0 0);
  --primary: oklch(0.45 0.03 250);
  --primary-foreground: oklch(0.99 0 0);
  --radius: 0.375rem;
}
.dark {
  --background: oklch(0.22 0 0);
  --card: oklch(0.26 0 0);
  --border: oklch(0.36 0 0);
  --input: oklch(0.36 0 0);
  --radius: 0.375rem;
}
body { background: linear-gradient(to bottom, oklch(0.97 0 0), oklch(0.90 0 0)); }
.dark body { background: linear-gradient(to bottom, oklch(0.27 0 0), oklch(0.18 0 0)); }
button {
  box-shadow: inset 0 1px 0 oklch(1 0 0 / 0.55), inset 0 -2px 3px oklch(0 0 0 / 0.10), 0 1px 3px oklch(0 0 0 / 0.28);
}
input, select, textarea {
  box-shadow: inset 0 2px 5px oklch(0 0 0 / 0.12);
}
"#
        .to_string(),

        UiStyle::Neumorphism => r#"
/* ---- Neumorphism: soft extruded shapes on a calm monochrome surface ---- */
:root {
  --background: oklch(0.905 0.012 255);
  --foreground: oklch(0.38 0.02 255);
  --card: oklch(0.905 0.012 255);
  --card-foreground: oklch(0.38 0.02 255);
  --popover: oklch(0.905 0.012 255);
  --popover-foreground: oklch(0.38 0.02 255);
  --primary: oklch(0.55 0.03 255);
  --primary-foreground: oklch(0.99 0 0);
  --secondary: oklch(0.905 0.012 255);
  --secondary-foreground: oklch(0.38 0.02 255);
  --muted: oklch(0.88 0.012 255);
  --muted-foreground: oklch(0.55 0.02 255);
  --accent: oklch(0.88 0.012 255);
  --accent-foreground: oklch(0.38 0.02 255);
  --border: oklch(0.905 0.012 255);
  --input: oklch(0.905 0.012 255);
  --ring: oklch(0.60 0.03 255);
  --radius: 0.9rem;
}
.dark {
  --background: oklch(0.24 0.015 255);
  --foreground: oklch(0.82 0.02 255);
  --card: oklch(0.24 0.015 255);
  --card-foreground: oklch(0.82 0.02 255);
  --popover: oklch(0.24 0.015 255);
  --primary: oklch(0.75 0.03 255);
  --primary-foreground: oklch(0.16 0.02 255);
  --secondary: oklch(0.24 0.015 255);
  --secondary-foreground: oklch(0.82 0.02 255);
  --muted: oklch(0.27 0.015 255);
  --muted-foreground: oklch(0.62 0.02 255);
  --accent: oklch(0.27 0.015 255);
  --accent-foreground: oklch(0.82 0.02 255);
  --border: oklch(0.24 0.015 255);
  --input: oklch(0.24 0.015 255);
  --radius: 0.9rem;
}
button {
  box-shadow: 6px 6px 14px oklch(0.62 0.03 255 / 0.55), -6px -6px 14px oklch(1 0 0 / 0.85);
}
.dark button {
  box-shadow: 6px 6px 14px oklch(0 0 0 / 0.55), -6px -6px 14px oklch(0.45 0.03 255 / 0.35);
}
input, select, textarea {
  box-shadow: inset 4px 4px 10px oklch(0.62 0.03 255 / 0.45), inset -4px -4px 10px oklch(1 0 0 / 0.8);
}
.dark input, .dark select, .dark textarea {
  box-shadow: inset 4px 4px 10px oklch(0 0 0 / 0.5), inset -4px -4px 10px oklch(0.45 0.03 255 / 0.3);
}
"#
        .to_string(),

        UiStyle::Glassmorphism => r#"
/* ---- Glassmorphism: frosted translucent panels over a vivid gradient ---- */
:root {
  --background: transparent;
  --card: color-mix(in oklch, oklch(1 0 0) 52%, transparent);
  --card-foreground: oklch(0.22 0.02 280);
  --popover: color-mix(in oklch, oklch(1 0 0) 62%, transparent);
  --popover-foreground: oklch(0.22 0.02 280);
  --muted: color-mix(in oklch, oklch(1 0 0) 38%, transparent);
  --muted-foreground: oklch(0.38 0.03 280);
  --primary: oklch(0.55 0.22 290);
  --primary-foreground: oklch(1 0 0);
  --accent: color-mix(in oklch, oklch(1 0 0) 45%, transparent);
  --accent-foreground: oklch(0.30 0.05 290);
  --border: oklch(1 0 0 / 45%);
  --input: oklch(1 0 0 / 55%);
  --ring: oklch(0.75 0.15 300);
  --radius: 1rem;
}
.dark {
  --card: color-mix(in oklch, oklch(0.16 0.02 280) 58%, transparent);
  --card-foreground: oklch(0.93 0.02 280);
  --popover: color-mix(in oklch, oklch(0.16 0.02 280) 68%, transparent);
  --muted: color-mix(in oklch, oklch(1 0 0) 12%, transparent);
  --muted-foreground: oklch(0.72 0.03 280);
  --primary: oklch(0.72 0.18 300);
  --border: oklch(1 0 0 / 22%);
  --input: oklch(1 0 0 / 18%);
  --radius: 1rem;
}
body {
  background: linear-gradient(135deg, oklch(0.64 0.24 305), oklch(0.60 0.22 215) 48%, oklch(0.68 0.20 165));
  background-attachment: fixed;
}
.dark body {
  background: linear-gradient(135deg, oklch(0.30 0.16 305), oklch(0.26 0.14 230) 48%, oklch(0.28 0.12 170));
  background-attachment: fixed;
}
.crm-bento-tile, aside {
  backdrop-filter: blur(18px) saturate(1.5);
  -webkit-backdrop-filter: blur(18px) saturate(1.5);
}
"#
        .to_string(),

        UiStyle::Claymorphism => r#"
/* ---- Claymorphism: chunky pastel clay with thick soft shadows ---- */
:root {
  --background: oklch(0.94 0.035 85);
  --foreground: oklch(0.35 0.03 60);
  --card: oklch(0.97 0.045 90);
  --card-foreground: oklch(0.35 0.03 60);
  --popover: oklch(0.97 0.045 90);
  --primary: oklch(0.70 0.14 300);
  --primary-foreground: oklch(1 0 0);
  --secondary: oklch(0.90 0.07 210);
  --secondary-foreground: oklch(0.35 0.05 230);
  --muted: oklch(0.92 0.04 85);
  --muted-foreground: oklch(0.55 0.04 70);
  --accent: oklch(0.90 0.09 20);
  --accent-foreground: oklch(0.40 0.08 30);
  --border: oklch(0.90 0.045 85);
  --input: oklch(0.96 0.04 90);
  --ring: oklch(0.70 0.14 300);
  --radius: 1.5rem;
}
.dark {
  --background: oklch(0.30 0.03 85);
  --foreground: oklch(0.90 0.03 85);
  --card: oklch(0.34 0.04 85);
  --card-foreground: oklch(0.90 0.03 85);
  --primary: oklch(0.75 0.14 300);
  --secondary: oklch(0.38 0.06 210);
  --secondary-foreground: oklch(0.90 0.03 85);
  --muted: oklch(0.33 0.035 85);
  --muted-foreground: oklch(0.68 0.04 80);
  --accent: oklch(0.42 0.09 20);
  --border: oklch(0.36 0.035 85);
  --input: oklch(0.36 0.035 85);
  --radius: 1.5rem;
}
button {
  box-shadow: 5px 5px 14px oklch(0.55 0.05 80 / 0.35), inset 0 2px 3px oklch(1 0 0 / 0.55), inset 0 -2px 4px oklch(0.55 0.05 80 / 0.12);
}
.crm-bento-tile {
  box-shadow: 9px 9px 22px oklch(0.55 0.05 80 / 0.30), inset 0 2px 4px oklch(1 0 0 / 0.45) !important;
}
"#
        .to_string(),

        // Bento Grid is the base theme: no overrides.
        UiStyle::BentoGrid => String::new(),

        UiStyle::CardsBased => r#"
/* ---- Cards-Based UI: everything lives on elevated cards over a muted canvas ---- */
:root {
  --background: oklch(0.945 0 0);
  --card: oklch(1 0 0);
  --border: oklch(0.89 0 0);
  --muted: oklch(0.96 0 0);
  --radius: 0.75rem;
}
.dark {
  --background: oklch(0.15 0 0);
  --card: oklch(0.21 0 0);
  --border: oklch(1 0 0 / 9%);
  --radius: 0.75rem;
}
.crm-bento-tile { box-shadow: 0 10px 28px oklch(0 0 0 / 0.10) !important; }
button { box-shadow: 0 2px 5px oklch(0 0 0 / 0.10); }
"#
        .to_string(),

        UiStyle::SplitScreen => r#"
/* ---- Split-Screen Design: bold two-tone shell — dark rail, light canvas ---- */
:root {
  --background: oklch(0.99 0 0);
  --primary: oklch(0.45 0.18 250);
  --primary-foreground: oklch(1 0 0);
  --accent: oklch(0.32 0.06 250);
  --accent-foreground: oklch(0.96 0 0);
  --ring: oklch(0.55 0.18 250);
  --radius: 0.375rem;
}
.dark {
  --primary: oklch(0.70 0.15 250);
  --accent: oklch(0.38 0.08 250);
  --accent-foreground: oklch(0.96 0 0);
  --radius: 0.375rem;
}
aside {
  background: oklch(0.17 0.02 250) !important;
  border-right: 3px solid oklch(0.50 0.20 250) !important;
}
.dark aside { background: oklch(0.12 0.015 250) !important; }
aside p { color: oklch(0.62 0.03 250) !important; }
aside a { color: oklch(0.78 0.03 250) !important; }
"#
        .to_string(),

        UiStyle::Brutalism => r#"
/* ---- Brutalism: raw and honest — system type, hard rules, no decoration ---- */
:root {
  --radius: 0;
  --background: oklch(1 0 0);
  --foreground: oklch(0 0 0);
  --card: oklch(1 0 0);
  --card-foreground: oklch(0 0 0);
  --popover: oklch(1 0 0);
  --popover-foreground: oklch(0 0 0);
  --primary: oklch(0 0 0);
  --primary-foreground: oklch(1 0 0);
  --secondary: oklch(0.94 0 0);
  --secondary-foreground: oklch(0 0 0);
  --muted: oklch(0.96 0 0);
  --muted-foreground: oklch(0.35 0 0);
  --accent: oklch(0.94 0 0);
  --accent-foreground: oklch(0 0 0);
  --border: oklch(0 0 0);
  --input: oklch(0 0 0);
  --ring: oklch(0 0 0);
}
.dark {
  --radius: 0;
  --background: oklch(0.10 0 0);
  --foreground: oklch(0.95 0 0);
  --card: oklch(0.10 0 0);
  --card-foreground: oklch(0.95 0 0);
  --popover: oklch(0.10 0 0);
  --primary: oklch(0.95 0 0);
  --primary-foreground: oklch(0.10 0 0);
  --secondary: oklch(0.18 0 0);
  --muted: oklch(0.14 0 0);
  --muted-foreground: oklch(0.65 0 0);
  --accent: oklch(0.16 0 0);
  --accent-foreground: oklch(0.95 0 0);
  --border: oklch(0.95 0 0);
  --input: oklch(0.95 0 0);
  --ring: oklch(0.95 0 0);
}
h1, h2, h3 { font-family: Georgia, 'Times New Roman', serif; }
a { text-decoration: underline; text-underline-offset: 2px; }
button { box-shadow: none; }
.crm-bento-tile { box-shadow: none !important; }
"#
        .to_string(),

        UiStyle::NeoBrutalism => r#"
/* ---- Neo-Brutalism: thick black borders, hard offset shadows, loud colour ---- */
:root {
  --radius: 0;
  --background: oklch(0.97 0.02 95);
  --foreground: oklch(0.12 0 0);
  --card: oklch(1 0 0);
  --card-foreground: oklch(0.12 0 0);
  --primary: oklch(0.86 0.19 95);
  --primary-foreground: oklch(0.12 0 0);
  --secondary: oklch(0.75 0.20 350);
  --secondary-foreground: oklch(0.12 0 0);
  --accent: oklch(0.72 0.19 200);
  --accent-foreground: oklch(0.12 0 0);
  --border: oklch(0.12 0 0);
  --input: oklch(0.12 0 0);
  --ring: oklch(0.12 0 0);
  --chart-1: oklch(0.72 0.19 200);
  --destructive: oklch(0.55 0.24 25);
}
.dark {
  --radius: 0;
  --background: oklch(0.16 0.01 95);
  --foreground: oklch(0.95 0 0);
  --card: oklch(0.20 0.01 95);
  --card-foreground: oklch(0.95 0 0);
  --primary: oklch(0.86 0.19 95);
  --primary-foreground: oklch(0.12 0 0);
  --border: oklch(0.95 0 0);
  --input: oklch(0.95 0 0);
  --ring: oklch(0.95 0 0);
}
button {
  border: 2px solid #141414 !important;
  box-shadow: 4px 4px 0 #141414;
}
.dark button {
  border-color: oklch(0.95 0 0) !important;
  box-shadow: 4px 4px 0 oklch(0.95 0 0);
}
button:active { transform: translate(3px, 3px); box-shadow: 1px 1px 0 #141414; }
input, select, textarea { border: 2px solid #141414 !important; }
.dark input, .dark select, .dark textarea { border-color: oklch(0.95 0 0) !important; }
.crm-bento-tile { border: 2px solid #141414 !important; box-shadow: 6px 6px 0 #141414 !important; }
.dark .crm-bento-tile {
  border-color: oklch(0.95 0 0) !important;
  box-shadow: 6px 6px 0 oklch(0.95 0 0) !important;
}
"#
        .to_string(),

        UiStyle::Bauhaus => r#"
/* ---- Bauhaus Style: primary red, yellow and blue geometry on black rules ---- */
:root {
  --radius: 0;
  --background: oklch(0.99 0 0);
  --foreground: oklch(0.12 0 0);
  --primary: #e30613;
  --primary-foreground: #ffffff;
  --secondary: #ffed00;
  --secondary-foreground: #141414;
  --accent: #0066b3;
  --accent-foreground: #ffffff;
  --muted: oklch(0.95 0 0);
  --muted-foreground: oklch(0.40 0 0);
  --border: #141414;
  --input: #141414;
  --ring: #e30613;
  --chart-1: #0066b3;
  --chart-2: #e30613;
  --chart-3: #d9a800;
}
.dark {
  --radius: 0;
  --background: oklch(0.13 0 0);
  --foreground: oklch(0.95 0 0);
  --card: oklch(0.16 0 0);
  --card-foreground: oklch(0.95 0 0);
  --muted: oklch(0.20 0 0);
  --muted-foreground: oklch(0.65 0 0);
  --border: oklch(0.95 0 0);
  --input: oklch(0.95 0 0);
}
button { text-transform: uppercase; letter-spacing: 0.10em; font-weight: 700; }
h1, h2, h3 { text-transform: uppercase; letter-spacing: 0.06em; font-weight: 800; }
aside { border-right: 4px solid #141414 !important; }
.dark aside { border-right-color: oklch(0.95 0 0) !important; }
"#
        .to_string(),

        UiStyle::HandDrawn => r#"
/* ---- Hand-Drawn: wobbly ink borders and a handwritten feel ---- */
:root {
  --radius: 1rem;
  --font-sans: "Segoe Print", "Bradley Hand", "Chalkboard SE", "Comic Sans MS", cursive;
  --foreground: oklch(0.25 0 0);
  --border: oklch(0.32 0 0);
  --input: oklch(0.32 0 0);
  --primary: oklch(0.45 0.12 250);
  --muted-foreground: oklch(0.45 0 0);
}
.dark {
  --radius: 1rem;
  --font-sans: "Segoe Print", "Bradley Hand", "Chalkboard SE", "Comic Sans MS", cursive;
  --border: oklch(0.75 0 0);
  --input: oklch(0.75 0 0);
}
button {
  border-radius: 255px 15px 225px 15px / 15px 225px 15px 255px !important;
  border: 2px solid var(--foreground) !important;
}
input, select, textarea {
  border-radius: 225px 18px 255px 12px / 12px 225px 18px 255px !important;
  border: 2px solid var(--foreground) !important;
}
.crm-bento-tile {
  border-radius: 225px 15px 255px 15px / 15px 255px 15px 225px !important;
  border: 2px solid var(--foreground) !important;
}
"#
        .to_string(),

        UiStyle::Cyberpunk => r#"
/* ---- Cyberpunk: neon glow on near-black — cyan and magenta ---- */
:root {
  --background: #0a0a14;
  --foreground: oklch(0.88 0.06 195);
  --card: oklch(0.17 0.04 280);
  --card-foreground: oklch(0.90 0.06 195);
  --popover: oklch(0.17 0.04 280);
  --popover-foreground: oklch(0.90 0.06 195);
  --primary: #00e5ff;
  --primary-foreground: #06060c;
  --secondary: oklch(0.25 0.08 300);
  --secondary-foreground: #ff2fb3;
  --accent: #ff2fb3;
  --accent-foreground: #06060c;
  --muted: oklch(0.20 0.04 280);
  --muted-foreground: oklch(0.62 0.08 280);
  --border: oklch(0.38 0.14 295);
  --input: oklch(0.38 0.14 295);
  --ring: #00e5ff;
  --radius: 0.375rem;
  --chart-1: #00e5ff;
  --chart-2: #ff2fb3;
  --destructive: #ff2f5f;
  --destructive-foreground: #06060c;
}
.dark {
  --background: #06060e;
  --card: oklch(0.13 0.03 280);
  --muted: oklch(0.15 0.03 280);
  --radius: 0.375rem;
}
button {
  box-shadow: 0 0 14px oklch(0.85 0.16 195 / 0.45), inset 0 0 8px oklch(0.85 0.16 195 / 0.15);
  text-transform: uppercase;
  letter-spacing: 0.08em;
  font-weight: 600;
}
h1, h2, h3 { text-shadow: 0 0 16px oklch(0.85 0.16 195 / 0.35); }
input, select, textarea { box-shadow: inset 0 0 10px oklch(0.85 0.16 195 / 0.08); }
"#
        .to_string(),

        UiStyle::Y2K => r#"
/* ---- Y2K: bubbly pastel gradients with a chrome shine ---- */
:root {
  --background: transparent;
  --card: color-mix(in oklch, oklch(1 0 0) 70%, transparent);
  --card-foreground: oklch(0.30 0.05 320);
  --popover: color-mix(in oklch, oklch(1 0 0) 78%, transparent);
  --popover-foreground: oklch(0.30 0.05 320);
  --primary: oklch(0.68 0.20 330);
  --primary-foreground: oklch(1 0 0);
  --secondary: color-mix(in oklch, oklch(1 0 0) 60%, transparent);
  --secondary-foreground: oklch(0.35 0.08 320);
  --accent: oklch(0.82 0.14 200);
  --accent-foreground: oklch(0.25 0.06 220);
  --muted: color-mix(in oklch, oklch(1 0 0) 45%, transparent);
  --muted-foreground: oklch(0.45 0.06 320);
  --border: oklch(1 0 0 / 65%);
  --input: color-mix(in oklch, oklch(1 0 0) 65%, transparent);
  --ring: oklch(0.75 0.16 320);
  --radius: 1.25rem;
}
.dark {
  --card: color-mix(in oklch, oklch(0.20 0.03 320) 66%, transparent);
  --card-foreground: oklch(0.92 0.04 320);
  --primary: oklch(0.74 0.18 330);
  --border: oklch(1 0 0 / 30%);
  --radius: 1.25rem;
}
body {
  background: linear-gradient(165deg, oklch(0.88 0.10 330), oklch(0.86 0.10 210) 52%, oklch(0.90 0.08 140));
  background-attachment: fixed;
}
.dark body {
  background: linear-gradient(165deg, oklch(0.32 0.10 330), oklch(0.28 0.10 220) 52%, oklch(0.30 0.08 160));
  background-attachment: fixed;
}
h1 {
  background: linear-gradient(to bottom, #ffffff 15%, oklch(0.72 0.16 260) 55%, oklch(0.62 0.20 330));
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
}
button {
  background-image: linear-gradient(to bottom, oklch(1 0 0 / 0.55), oklch(1 0 0 / 0) 55%) !important;
  box-shadow: 0 4px 12px oklch(0.68 0.20 330 / 0.35);
}
.crm-bento-tile { backdrop-filter: blur(6px); -webkit-backdrop-filter: blur(6px); }
"#
        .to_string(),

        UiStyle::PixelArt => r#"
/* ---- Pixel Art UI: chunky pixels, hard steps, terminal monospace ---- */
:root {
  --radius: 0;
  --font-sans: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
  --background: oklch(0.13 0.02 140);
  --foreground: oklch(0.82 0.14 140);
  --card: oklch(0.17 0.02 140);
  --card-foreground: oklch(0.82 0.14 140);
  --popover: oklch(0.17 0.02 140);
  --popover-foreground: oklch(0.82 0.14 140);
  --primary: oklch(0.72 0.20 140);
  --primary-foreground: oklch(0.10 0.01 140);
  --secondary: oklch(0.25 0.04 280);
  --secondary-foreground: oklch(0.82 0.14 140);
  --accent: oklch(0.62 0.20 300);
  --accent-foreground: oklch(0.10 0.01 140);
  --muted: oklch(0.20 0.02 140);
  --muted-foreground: oklch(0.60 0.08 140);
  --border: oklch(0.45 0.12 140);
  --input: oklch(0.45 0.12 140);
  --ring: oklch(0.72 0.20 140);
}
.dark {
  --radius: 0;
  --font-sans: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
  --background: oklch(0.09 0.015 140);
  --card: oklch(0.12 0.015 140);
}
img { image-rendering: pixelated; }
button, input, select, textarea {
  border-radius: 0 !important;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
button { box-shadow: 4px 4px 0 oklch(0 0 0 / 0.65); }
button:active { transform: translate(2px, 2px); box-shadow: 2px 2px 0 oklch(0 0 0 / 0.65); }
"#
        .to_string(),
    }
}

/// Injects (or removes) the `<style id="crm-ui-style">` tag, ordered after
/// the base theme and before the branding style (see
/// [`crate::theme::enforce_style_order`]).
///
/// `None`/empty/unknown `id` falls back to [`UiStyle::BentoGrid`], whose
/// block is empty — the tag is removed so the base theme shows through.
/// Never panics; DOM operations are best-effort no-ops when the document
/// is unavailable.
pub fn apply_ui_style(id: Option<&str>) {
    use leptos::leptos_dom::helpers::document;

    let style = id.and_then(UiStyle::from_str).unwrap_or(UiStyle::BentoGrid);
    let css = style_css(&style);
    if let Some(body) = document().body() {
        if let Some(old) = document().get_element_by_id("crm-ui-style") {
            old.remove();
        }
        if !css.is_empty() {
            if let Ok(el) = document().create_element("style") {
                let _ = el.set_attribute("id", "crm-ui-style");
                el.set_text_content(Some(&css));
                let _ = body.append_child(&el);
            }
        }
    }
    crate::theme::enforce_style_order();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_styles_in_domain_order() {
        assert_eq!(ALL_STYLES.len(), 16);
        assert_eq!(
            crm_domain::branding::UI_STYLE_IDS.len(),
            16,
            "domain table and ALL_STYLES must stay the same length"
        );
        for (i, style) in ALL_STYLES.iter().enumerate() {
            assert_eq!(
                style.as_str(),
                crm_domain::branding::UI_STYLE_IDS[i],
                "UiStyle discriminant {i} must index the domain id table"
            );
        }
    }

    #[test]
    fn from_str_round_trips() {
        for style in ALL_STYLES {
            assert_eq!(UiStyle::from_str(style.as_str()), Some(style));
        }
        assert_eq!(UiStyle::from_str("  cyberpunk  "), Some(UiStyle::Cyberpunk));
        assert_eq!(UiStyle::from_str("neon-dreams"), None);
        assert_eq!(UiStyle::from_str(""), None);
        assert_eq!(UiStyle::from_str("BENTO-GRID"), None);
    }

    #[test]
    fn labels_and_descriptions_present() {
        for style in ALL_STYLES {
            assert!(!style.label().is_empty());
            assert!(!style.description().is_empty());
            assert_eq!(style.swatch().len(), 3);
        }
        assert_eq!(UiStyle::BentoGrid.label(), "Bento Grid");
        assert_eq!(
            UiStyle::Cyberpunk.description(),
            "Neon glow on near-black — cyan and magenta."
        );
    }

    #[test]
    fn bento_grid_emits_no_css() {
        assert!(style_css(&UiStyle::BentoGrid).is_empty());
    }

    #[test]
    fn every_other_style_emits_css() {
        for style in ALL_STYLES {
            if style == UiStyle::BentoGrid {
                continue;
            }
            let css = style_css(&style);
            assert!(!css.is_empty(), "{:?} must emit CSS", style);
            assert!(
                css.contains(":root"),
                "{:?} must override tokens via :root",
                style
            );
        }
    }

    #[test]
    fn css_blocks_carry_dark_companions() {
        // Every non-default style must keep the dark-mode toggle working.
        for style in ALL_STYLES {
            if style == UiStyle::BentoGrid {
                continue;
            }
            let css = style_css(&style);
            assert!(
                css.contains(".dark"),
                "{:?} must define a .dark companion",
                style
            );
        }
    }
}
