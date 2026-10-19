// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Component library v2: shadcn-style primitives ported to Leptos,
//! token-driven from [`crate::theme`].
//!
//! SVG [`Icon`] set, [`Button`], [`Card`] suite, [`Badge`], form
//! primitives, feedback primitives, overlays, tables, tabs, and the
//! [`BentoGrid`] / [`BentoTile`] bento system.
//!
//! Every v2 component styles itself from CSS custom properties
//! (`var(--card)`, `var(--primary)` …) emitted by
//! [`crate::theme::theme_css`], so dark mode and branding overrides
//! restyle the whole UI without touching component code.
//
// Some v2 primitives (DropdownMenu, Toast, Tabs, …) are not yet wired
// into views; UI2-02 lands the library, follow-on tasks mount them.
// Allowed at the module level to keep the library's public surface
// stable while views migrate.
#![allow(dead_code)]
#![allow(unused_imports)]

pub mod badge;
pub mod bento;
pub mod button;
pub mod callback;
pub mod card;
pub mod feedback;
pub mod form;
pub mod icon;
pub mod overlay;
pub mod table;
pub mod tabs;

pub use badge::{Badge, BadgeVariant};
pub use bento::{BentoGrid, BentoTile};
pub use button::{Button, ButtonSize, ButtonVariant};
pub use callback::{UiHandler, UiValueHandler};
pub use card::{Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle};
pub use feedback::{
    dismiss_toast, push_toast, Avatar, Progress, Skeleton, Toast, ToastVariant, Toaster, Toasts,
    Tooltip,
};
pub use form::{Input, Label, Select, SelectOption, Separator, Switch, Textarea};
pub use icon::{Icon, IconName};
pub use overlay::{Dialog, DropdownMenu, MenuAlign, MenuItem};
pub use table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};
pub use tabs::{Tabs, TabsContent, TabsList, TabsTrigger};
