// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! [`UiHandler`]: a `Send` type-erased event callback for v2 components.
//!
//! Leptos `#[component]` view trees must be `Send`. A plain
//! `Rc<dyn Fn()>` is not `Send`, and `Arc<dyn Fn()>` demands `Sync`,
//! which closures capturing reactive state (`RwSignal`, …) do not have.
//! `UiHandler` wraps `Box<dyn Fn() + Send>` in a [`StoredValue`] (which is
//! `Copy` + `Send`), giving components a handler prop that satisfies the
//! view-tree bound without `Sync`.

use leptos::prelude::*;

/// Type-erased `Send` event callback.
///
/// Construct with [`UiHandler::new`]; invoke with [`UiHandler::call`].
/// `Copy`, so it moves freely through `view!` closures.
#[derive(Clone, Copy)]
pub struct UiHandler(StoredValue<Box<dyn Fn() + Send>, LocalStorage>);

impl UiHandler {
    /// Wraps a `Send` closure as a UI event handler.
    pub fn new(handler: impl Fn() + Send + 'static) -> Self {
        Self(StoredValue::new_local(Box::new(handler)))
    }

    /// Invokes the wrapped closure.
    pub fn call(self) {
        self.0.with_value(|f| f());
    }
}

impl std::fmt::Debug for UiHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("UiHandler").finish()
    }
}

/// Type-erased `Send` value callback, for form inputs.
///
/// Like [`UiHandler`] but carries a value (`String` for text inputs,
/// `bool` for switches).
pub struct UiValueHandler<T>(StoredValue<Box<dyn Fn(T) + Send>, LocalStorage>);

// Manual `Clone`/`Copy`: the inner `StoredValue` is `Copy` regardless of
// `T`, so we must not require `T: Copy` (which `#[derive]` would add).
impl<T> Clone for UiValueHandler<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for UiValueHandler<T> {}

impl<T: 'static> UiValueHandler<T> {
    /// Wraps a `Send` closure as a value event handler.
    pub fn new(handler: impl Fn(T) + Send + 'static) -> Self {
        Self(StoredValue::new_local(Box::new(handler)))
    }

    /// Invokes the wrapped closure with `value`.
    pub fn call(self, value: T) {
        self.0.with_value(|f| f(value));
    }
}

impl<T> std::fmt::Debug for UiValueHandler<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("UiValueHandler").finish()
    }
}
