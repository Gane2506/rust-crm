// SPDX-License-Identifier: GPL-3.0-only
//! Login form for the 03-SEC-02 auth vertical slice.
//!
//! [`LoginForm`] collects email + password and calls
//! [`login`](crate::api::login); on success the bearer token is handed to
//! the parent, which keeps it in a Leptos signal (in memory only — never
//! persisted to local storage). No design polish: the 06-UI workstream
//! owns the visual design.

use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{login, ApiError};

/// Outcome of a login attempt, shown under the form.
#[derive(Debug, Clone)]
enum LoginStatus {
    /// Wrong credentials or transport failure; carries the message.
    Error(String),
}

/// Login form: email + password, submitting to `POST /api/v1/auth/login`.
///
/// Calls `on_logged_in` with the bearer token on success. The password is
/// only ever held in the input element and the request body — it is never
/// stored.
#[component]
pub fn LoginForm(on_logged_in: impl Fn(String) + Clone + 'static) -> impl IntoView {
    let email_ref = NodeRef::<Input>::new();
    let password_ref = NodeRef::<Input>::new();
    let status = RwSignal::new(None::<LoginStatus>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let email = email_ref
            .get()
            .map(|input| input.value().trim().to_string())
            .unwrap_or_default();
        let password = password_ref
            .get()
            .map(|input| input.value())
            .unwrap_or_default();
        if email.is_empty() || password.is_empty() {
            status.set(Some(LoginStatus::Error(
                "Enter your email and password.".to_string(),
            )));
            return;
        }

        submitting.set(true);
        status.set(None);
        let on_logged_in = on_logged_in.clone();
        spawn_local(async move {
            match login(&email, &password).await {
                Ok(token) => {
                    // Clear the password field immediately; the token (not
                    // the password) is what the session keeps.
                    if let Some(input) = password_ref.get() {
                        input.set_value("");
                    }
                    on_logged_in(token);
                }
                Err(ApiError::Api { status: 401, .. }) => {
                    status.set(Some(LoginStatus::Error(
                        "Invalid email or password.".to_string(),
                    )));
                }
                Err(error) => status.set(Some(LoginStatus::Error(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <section aria-label="Log in" style="max-width: 24rem; margin: 4rem auto;">
            <h2>"Rust CRM"</h2>
            <p style="color: #6b7280;">"Log in to continue."</p>
            <form on:submit=on_submit>
                <div style="margin-bottom: 0.75rem;">
                    <label for="login-email" style="display: block; margin-bottom: 0.25rem;">"Email"</label>
                    <input
                        id="login-email"
                        node_ref=email_ref
                        type="email"
                        autocomplete="username"
                        style="width: 100%; padding: 0.5rem; border: 1px solid #d1d5db; border-radius: 0.375rem;"
                    />
                </div>
                <div style="margin-bottom: 0.75rem;">
                    <label for="login-password" style="display: block; margin-bottom: 0.25rem;">"Password"</label>
                    <input
                        id="login-password"
                        node_ref=password_ref
                        type="password"
                        autocomplete="current-password"
                        style="width: 100%; padding: 0.5rem; border: 1px solid #d1d5db; border-radius: 0.375rem;"
                    />
                </div>
                <button
                    type="submit"
                    disabled=move || submitting.get()
                    style="padding: 0.5rem 1rem; background: #2563eb; color: white; border: none; border-radius: 0.375rem; cursor: pointer;"
                >
                    {move || if submitting.get() { "Logging in…" } else { "Log in" }}
                </button>
            </form>
            {move || match status.get() {
                None => ().into_any(),
                Some(LoginStatus::Error(message)) => {
                    view! {
                        <p role="alert" style="color: #b91c1c; margin-top: 0.75rem;">{message}</p>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}
