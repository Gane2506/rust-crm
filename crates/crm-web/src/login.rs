// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Login form for the 03-SEC-02 auth vertical slice.
//!
//! Reskinned on the v2 component library (UI2-02). [`LoginForm`] collects
//! email + password and calls [`login`](crate::api::login); on success
//! the bearer token is handed to the parent, which keeps it in a Leptos
//! signal (in memory only — never persisted to local storage). The
//! password is only ever held in the input element and the request
//! body — it is never stored.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{login, ApiError};
use crate::components::ErrorAlert;
use crate::ui::{
    Button, ButtonVariant, Card, CardContent, Icon, IconName, Input as TextInput, Label,
    UiValueHandler,
};

/// Outcome of a login attempt, shown under the form.
#[derive(Debug, Clone)]
enum LoginStatus {
    /// Wrong credentials or transport failure; carries the message.
    Error(String),
}

/// Login form: email + password, submitting to `POST /api/v1/auth/login`.
///
/// Calls `on_logged_in` with the bearer token on success.
#[component]
pub fn LoginForm(on_logged_in: UiValueHandler<String>) -> impl IntoView {
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let status = RwSignal::new(None::<LoginStatus>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let email_value = email.get_untracked().trim().to_string();
        let password_value = password.get_untracked();
        if email_value.is_empty() || password_value.is_empty() {
            status.set(Some(LoginStatus::Error(
                "Enter your email and password.".to_string(),
            )));
            return;
        }

        submitting.set(true);
        status.set(None);
        spawn_local(async move {
            match login(&email_value, &password_value).await {
                Ok(token) => {
                    // Clear the password field immediately; the token (not
                    // the password) is what the session keeps.
                    password.set(String::new());
                    on_logged_in.call(token);
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
        <div style="min-height: 100vh; display: flex; align-items: center; justify-content: center; \
                   padding: 1rem; background: var(--background);">
            <div style="width: 100%; max-width: 24rem;">
                <Card>
                    <CardContent>
                        <div style="display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1.5rem; padding-top: 0.5rem;">
                            <span style="display: inline-flex; align-items: center; justify-content: center; \
                                         width: 2.5rem; height: 2.5rem; border-radius: var(--radius-md); \
                                         background: var(--primary); color: var(--primary-foreground); \
                                         font-weight: 700; font-size: 1rem;">
                                "R"
                            </span>
                            <div>
                                <h2 style="margin: 0; font-size: 1.125rem; font-weight: 700; letter-spacing: -0.01em;">
                                    "Rust CRM"
                                </h2>
                                <p style="margin: 0; font-size: 0.8125rem; color: var(--muted-foreground);">
                                    "Log in to continue."
                                </p>
                            </div>
                        </div>
                        <form on:submit=on_submit>
                            <div style="display: grid; gap: 0.875rem;">
                                <div>
                                    <Label for_id="login-email">"Email"</Label>
                                    <TextInput
                                        id="login-email"
                                        input_type="email"
                                        placeholder="you@agency.com"
                                        value=email
                                        on_input={UiValueHandler::new(move |v: String| email.set(v))}
                                    />
                                </div>
                                <div>
                                    <Label for_id="login-password">"Password"</Label>
                                    <TextInput
                                        id="login-password"
                                        input_type="password"
                                        placeholder="••••••••"
                                        value=password
                                        on_input={UiValueHandler::new(move |v: String| password.set(v))}
                                    />
                                </div>
                                <Button
                                    variant=ButtonVariant::Default
                                    btn_type="submit"
                                    disabled=Signal::derive(move || submitting.get())
                                >
                                    {move || if submitting.get() { "Logging in…" } else { "Log in" }}
                                </Button>
                            </div>
                        </form>
                        <div style="margin-top: 1rem;">
                            {move || match status.get() {
                                None => ().into_any(),
                                Some(LoginStatus::Error(message)) => {
                                    view! { <ErrorAlert message=message /> }.into_any()
                                }
                            }}
                        </div>
                        <p style="margin: 1.25rem 0 0; font-size: 0.75rem; color: var(--muted-foreground); \
                                  text-align: center; display: flex; align-items: center; \
                                  justify-content: center; gap: 0.375rem;">
                            <Icon name=IconName::Shield size=13 />
                            "Staff access only — there is no client login."
                        </p>
                    </CardContent>
                </Card>
            </div>
        </div>
    }
}
