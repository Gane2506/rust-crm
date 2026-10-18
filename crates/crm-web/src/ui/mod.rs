// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Twenty-style component library for the CRM frontend (reimplemented
//! from scratch — no copied Twenty assets).
//!
//! Presentational building blocks composed from the design tokens in
//! [`crate::styles`]: the application shell (sidebar + utility bar),
//! buttons, badges, tables, cards, alerts, page headers, and a small
//! command menu. All behavior (routing, data fetching, auth) stays in
//! the page modules; these components are styling + structure only.

pub mod components;
