// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Component library v2: shadcn-style primitives ported to Leptos,
//! token-driven from [`crate::theme`].
//!
//! - [`components`]: the original Twenty-style shell components
//!   (sidebar, utility bar, command menu, legacy buttons/badges/cards).
//!   Untouched by the v2 rebuild; views migrate onto the v2 library in
//!   UI2-02.
//! - New v2 modules below: SVG [`Icon`] set, [`Button`], [`Card`] suite,
//!   [`Badge`], form primitives, feedback primitives, overlays, tables,
//!   tabs, and the [`BentoGrid`] / [`BentoTile`] bento system.
//!
//! Every v2 component styles itself from CSS custom properties
//! (`var(--card)`, `var(--primary)` …) emitted by
//! [`crate::theme::theme_css`], so dark mode and branding overrides
//! restyle the whole UI without touching component code.

// The v2 modules are the component library's public API. Their items read
// as dead code until views migrate onto them in UI2-02; the allow is
// narrowly scoped to these modules and removed once the migration lands.
#[allow(dead_code)]
pub mod badge;
#[allow(dead_code)]
pub mod bento;
#[allow(dead_code)]
pub mod button;
#[allow(dead_code)]
pub mod card;
pub mod components;
#[allow(dead_code)]
pub mod feedback;
#[allow(dead_code)]
pub mod form;
#[allow(dead_code)]
pub mod icon;
#[allow(dead_code)]
pub mod overlay;
#[allow(dead_code)]
pub mod table;
#[allow(dead_code)]
pub mod tabs;

// The v2 re-exports are the library's public API. Views migrate onto them
// in UI2-02; until then nothing references them from the binary.
#[allow(unused_imports)]
pub use badge::{Badge, BadgeVariant};
#[allow(unused_imports)]
pub use bento::{BentoGrid, BentoTile};
#[allow(unused_imports)]
pub use button::{Button, ButtonSize, ButtonVariant};
#[allow(unused_imports)]
pub use card::{Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle};
#[allow(unused_imports)]
pub use feedback::{
    dismiss_toast, push_toast, Avatar, Progress, Skeleton, Toast, ToastVariant, Toaster, Toasts,
    Tooltip,
};
#[allow(unused_imports)]
pub use form::{Input, Label, Select, SelectOption, Separator, Switch, Textarea};
#[allow(unused_imports)]
pub use icon::{Icon, IconName};
#[allow(unused_imports)]
pub use overlay::{Dialog, DropdownMenu, MenuAlign, MenuItem};
#[allow(unused_imports)]
pub use table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};
#[allow(unused_imports)]
pub use tabs::{Tabs, TabsContent, TabsList, TabsTrigger};
