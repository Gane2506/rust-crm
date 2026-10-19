// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! shadcn-style form primitives: [`Label`], [`Input`], [`Textarea`],
//! [`Select`], [`Switch`], and [`Separator`].
//!
//! Token-driven (`var(--input)`, `var(--ring)` …). Inputs report through
//! `on_input`/`on_change` callbacks; pass a `value`/`checked` signal for
//! controlled usage.

use super::callback::UiValueHandler;

use leptos::leptos_dom::helpers::event_target_value;
use leptos::prelude::*;

const FIELD_BASE: &str = "display: flex; width: 100%; border-radius: var(--radius-md); \
    border: 1px solid var(--input); background: var(--card); color: var(--foreground); \
    font-size: 0.875rem; font-family: inherit; box-sizing: border-box;";

/// Form label.
///
/// # Props
/// - `for_id`: the `for` attribute targeting the field's id (optional).
/// - `children`: label text.
#[component]
pub fn Label(#[prop(optional)] for_id: Option<&'static str>, children: Children) -> impl IntoView {
    view! {
        <label
            for={for_id.unwrap_or("")}
            style="display: block; font-size: 0.875rem; font-weight: 500; color: var(--foreground); margin-bottom: 0.375rem;"
        >
            {children()}
        </label>
    }
}

/// Text input.
///
/// # Props
/// - `input_type`: e.g. `"text"` (default), `"email"`, `"password"`, `"search"`.
/// - `placeholder`: optional placeholder text.
/// - `value`: optional controlled signal. **Note:** with a controlled
///   signal, every keystroke should write back through `on_input`;
///   otherwise the field resets to the signal on re-render.
/// - `on_input`: called with the current value on each input event.
/// - `disabled`: disables the field.
/// - `id`: optional element id (pairs with [`Label`]).
#[component]
pub fn Input(
    #[prop(optional)] input_type: Option<&'static str>,
    #[prop(optional, into)] placeholder: Option<String>,
    #[prop(optional)] value: Option<RwSignal<String>>,
    #[prop(optional)] on_input: Option<UiValueHandler<String>>,
    #[prop(optional)] disabled: bool,
    #[prop(optional)] id: Option<&'static str>,
) -> impl IntoView {
    let current = value.map(|v| move || v.get());
    view! {
        <input
            type={input_type.unwrap_or("text")}
            id={id.unwrap_or("")}
            placeholder={placeholder.unwrap_or_default()}
            disabled=disabled
            prop:value=move || current.as_ref().map(|f| f()).unwrap_or_default()
            on:input=move |ev| {
                if let Some(handler) = on_input.as_ref().copied() {
                    handler.call(event_target_value(&ev));
                }
            }
            style=format!(
                "{FIELD_BASE} height: 2.5rem; padding: 0 0.75rem; {}",
                if disabled { "opacity: 0.5; cursor: not-allowed;" } else { "" }
            )
        />
    }
}

/// Multi-line text input.
///
/// Props mirror [`Input`], plus `rows` (default 3).
#[component]
pub fn Textarea(
    #[prop(optional)] rows: Option<u8>,
    #[prop(optional, into)] placeholder: Option<String>,
    #[prop(optional)] value: Option<RwSignal<String>>,
    #[prop(optional)] on_input: Option<UiValueHandler<String>>,
    #[prop(optional)] disabled: bool,
    #[prop(optional)] id: Option<&'static str>,
) -> impl IntoView {
    let current = value.map(|v| move || v.get());
    view! {
        <textarea
            rows={rows.unwrap_or(3).to_string()}
            id={id.unwrap_or("")}
            placeholder={placeholder.unwrap_or_default()}
            disabled=disabled
            prop:value=move || current.as_ref().map(|f| f()).unwrap_or_default()
            on:input=move |ev| {
                if let Some(handler) = on_input.as_ref().copied() {
                    handler.call(event_target_value(&ev));
                }
            }
            style=format!(
                "{FIELD_BASE} min-height: 4rem; padding: 0.5rem 0.75rem; resize: vertical; {}",
                if disabled { "opacity: 0.5; cursor: not-allowed;" } else { "" }
            )
        />
    }
}

/// One option in a [`Select`]: `(value, label)`.
pub type SelectOption = (String, String);

/// Native-styled select dropdown.
///
/// # Props
/// - `options`: `(value, label)` pairs.
/// - `selected`: signal holding the current value; updated on change.
/// - `on_change`: optional extra callback with the new value.
/// - `disabled`: disables the field.
/// - `id`: optional element id.
#[component]
pub fn Select(
    options: Vec<SelectOption>,
    selected: RwSignal<String>,
    #[prop(optional)] on_change: Option<UiValueHandler<String>>,
    #[prop(optional)] disabled: bool,
    #[prop(optional)] id: Option<&'static str>,
) -> impl IntoView {
    view! {
        <select
            id={id.unwrap_or("")}
            disabled=disabled
            prop:value=move || selected.get()
            on:change=move |ev| {
                let v = event_target_value(&ev);
                selected.set(v.clone());
                if let Some(handler) = on_change.as_ref().copied() {
                    handler.call(v);
                }
            }
            style=format!(
                "{FIELD_BASE} height: 2.5rem; padding: 0 0.75rem; appearance: auto; {}",
                if disabled { "opacity: 0.5; cursor: not-allowed;" } else { "" }
            )
        >
            {options
                .into_iter()
                .map(|(value, label)| {
                    view! {
                        <option value=value>{label}</option>
                    }
                })
                .collect::<Vec<_>>()}
        </select>
    }
}

/// Toggle switch (button-based, `role="switch"`).
///
/// # Props
/// - `checked`: the toggle state signal; flipped on click.
/// - `on_change`: optional callback with the new state.
/// - `disabled`: disables the switch.
/// - `label`: accessible name for the switch.
#[component]
pub fn Switch(
    checked: RwSignal<bool>,
    #[prop(optional)] on_change: Option<UiValueHandler<bool>>,
    #[prop(optional)] disabled: bool,
    #[prop(optional)] label: Option<&'static str>,
) -> impl IntoView {
    let toggle = move || {
        if disabled {
            return;
        }
        let next = !checked.get_untracked();
        checked.set(next);
        if let Some(handler) = on_change.as_ref().copied() {
            handler.call(next);
        }
    };
    view! {
        <button
            type="button"
            role="switch"
            aria-checked=move || checked.get().to_string()
            aria-label={label.unwrap_or("Toggle")}
            disabled=disabled
            on:click=move |_| toggle()
            style=move || {
                format!(
                    "position: relative; width: 2.25rem; height: 1.25rem; flex-shrink: 0; \
                     border-radius: 9999px; border: 1px solid var(--border); padding: 0; \
                     background: {}; transition: background 0.15s ease; {}",
                    if checked.get() { "var(--primary)" } else { "var(--input)" },
                    if disabled {
                        "opacity: 0.5; cursor: not-allowed;"
                    } else {
                        "cursor: pointer;"
                    },
                )
            }
        >
            <span
                aria-hidden="true"
                style=move || {
                    format!(
                        "position: absolute; top: 2px; left: 2px; width: 1rem; height: 1rem; \
                         border-radius: 9999px; background: #ffffff; \
                         box-shadow: 0 1px 2px oklch(0 0 0 / 0.2); transition: transform 0.15s ease; \
                         transform: translateX({});",
                        if checked.get() { "1rem" } else { "0" }
                    )
                }
            />
        </button>
    }
}

/// Visual divider.
///
/// # Props
/// - `vertical`: renders a vertical divider (default horizontal).
#[component]
pub fn Separator(#[prop(optional)] vertical: bool) -> impl IntoView {
    if vertical {
        view! {
            <div
                role="separator"
                aria-orientation="vertical"
                style="width: 1px; align-self: stretch; background: var(--border); margin: 0 0.5rem;"
            />
        }
        .into_any()
    } else {
        view! {
            <div role="separator" style="height: 1px; background: var(--border); margin: 0.75rem 0;" />
        }
        .into_any()
    }
}
