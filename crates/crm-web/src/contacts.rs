// SPDX-License-Identifier: GPL-3.0-only
//! Contacts page for the 01-FND-02 vertical slice.
//!
//! [`ContactsPage`] lists the demo tenant's contacts (fetched on mount via
//! [`fetch_contacts`](crate::api::fetch_contacts)) and hosts [`ContactForm`]
//! to create new ones. UI states — loading, error with retry, empty, list —
//! use native form controls so everything stays keyboard-accessible.

use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crm_domain::contact::Contact;

use crate::api::{
    create_contact, demo_profile_id, fetch_contacts, ApiError, ContactListResponse,
    CreateContactRequest,
};

/// Contacts list page: fetch-on-mount list plus the create form.
#[component]
pub fn ContactsPage() -> impl IntoView {
    // Bumped to refetch the list (after a create, or on manual retry).
    let reload = RwSignal::new(0u32);
    // `None` = loading, `Some(Ok(_))` = loaded, `Some(Err(_))` = failed.
    //
    // Driven by an `Effect` + `spawn_local` instead of `Resource`: gloo-net
    // futures are `!Send`, and Leptos `Resource` requires `Send` futures.
    let contacts = RwSignal::new(None::<Result<ContactListResponse, ApiError>>);
    Effect::new(move |_| {
        reload.get();
        contacts.set(None);
        spawn_local(async move {
            contacts.set(Some(fetch_contacts().await));
        });
    });
    let retry = move |_| reload.update(|n| *n = n.wrapping_add(1));

    view! {
        <section aria-label="Contacts">
            <h2>"Contacts"</h2>
            <ContactForm on_created=move || reload.update(|n| *n = n.wrapping_add(1)) />
            <h3>"Contact list"</h3>
            {move || match contacts.get() {
                None => {
                    view! { <p aria-busy="true">"Loading contacts…"</p> }.into_any()
                }
                Some(Err(error)) => {
                    view! {
                        <div role="alert" style="border: 1px solid #fca5a5; background: #fef2f2; border-radius: 0.5rem; padding: 0.75rem; max-width: 32rem;">
                            <p style="margin: 0 0 0.5rem;">{format!("Could not load contacts: {error}")}</p>
                            <button on:click=retry>"Retry"</button>
                        </div>
                    }
                        .into_any()
                }
                Some(Ok(page)) if page.contacts.is_empty() => {
                    view! { <p>"No contacts yet."</p> }.into_any()
                }
                Some(Ok(page)) => {
                    view! {
                        <p style="font-size: 0.875rem; color: #6b7280;">
                            {format!("{} contact{} total", page.total, if page.total == 1 { "" } else { "s" })}
                        </p>
                        <ul style="list-style: none; margin: 0; padding: 0; display: grid; gap: 0.75rem;">
                            <For each=move || page.contacts.clone() key=|contact| contact.contact_id let:contact>
                                <ContactRow contact=contact />
                            </For>
                        </ul>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Outcome message shown under the create form.
#[derive(Debug, Clone)]
enum FormStatus {
    /// A contact was created; carries the new contact id.
    Created(String),
    /// Validation or API failure; carries the message to display.
    Error(String),
}

/// Splits a comma-separated input into trimmed, non-empty values.
fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Clears a text input if it is currently mounted.
fn clear_field(input: &NodeRef<Input>) {
    if let Some(element) = input.get() {
        element.set_value("");
    }
}

/// Create-contact form: comma-separated emails/phones plus a source label.
///
/// Validates client-side (at least one email, non-empty source), POSTs with
/// a fresh `Idempotency-Key`, then clears itself, shows a confirmation and
/// asks the parent list to refresh through `on_created`.
#[component]
fn ContactForm(on_created: impl Fn() + Clone + 'static) -> impl IntoView {
    let emails_ref = NodeRef::<Input>::new();
    let phones_ref = NodeRef::<Input>::new();
    let source_ref = NodeRef::<Input>::new();
    let status = RwSignal::new(None::<FormStatus>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }

        let emails = split_list(
            &emails_ref
                .get()
                .expect("emails input is mounted when its form is submitted")
                .value(),
        );
        let phones = split_list(
            &phones_ref
                .get()
                .expect("phones input is mounted when its form is submitted")
                .value(),
        );
        let source = source_ref
            .get()
            .expect("source input is mounted when its form is submitted")
            .value()
            .trim()
            .to_string();

        if emails.is_empty() {
            status.set(Some(FormStatus::Error(
                "Enter at least one email address.".to_string(),
            )));
            return;
        }
        if source.is_empty() {
            status.set(Some(FormStatus::Error("Enter a source label.".to_string())));
            return;
        }

        submitting.set(true);
        status.set(None);
        let request = CreateContactRequest {
            client_profile_id: demo_profile_id(),
            emails,
            phones,
            source,
        };
        let on_created = on_created.clone();
        spawn_local(async move {
            match create_contact(&request).await {
                Ok(contact) => {
                    clear_field(&emails_ref);
                    clear_field(&phones_ref);
                    clear_field(&source_ref);
                    status.set(Some(FormStatus::Created(contact.contact_id.to_string())));
                    on_created();
                }
                Err(error) => status.set(Some(FormStatus::Error(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <form
            on:submit=on_submit
            style="display: grid; gap: 0.5rem; max-width: 32rem; margin-bottom: 1.5rem; border: 1px solid #e5e7eb; border-radius: 0.5rem; padding: 1rem;"
        >
            <h3 style="margin: 0;">"New contact"</h3>
            <label for="contact-emails">"Emails (comma-separated)"</label>
            <input
                id="contact-emails"
                type="text"
                node_ref=emails_ref
                placeholder="a@example.com, b@example.com"
                autocomplete="off"
            />
            <label for="contact-phones">"Phones (comma-separated, optional)"</label>
            <input
                id="contact-phones"
                type="text"
                node_ref=phones_ref
                placeholder="+14165550100"
                autocomplete="off"
            />
            <label for="contact-source">"Source"</label>
            <input
                id="contact-source"
                type="text"
                node_ref=source_ref
                placeholder="manual-entry"
                autocomplete="off"
            />
            <div>
                <button type="submit" disabled=move || submitting.get()>
                    {move || if submitting.get() { "Creating…" } else { "Create contact" }}
                </button>
            </div>
            {move || {
                status
                    .get()
                    .map(|state| match state {
                        FormStatus::Created(id) => {
                            view! {
                                <p role="status" style="margin: 0; color: #166534;">
                                    {format!("Contact created: {id}")}
                                </p>
                            }
                                .into_any()
                        }
                        FormStatus::Error(message) => {
                            view! {
                                <p role="alert" style="margin: 0; color: #b91c1c;">{message}</p>
                            }
                                .into_any()
                        }
                    })
            }}
        </form>
    }
}

/// One contact in the list: short id, lifecycle, emails, phones and source.
///
/// Renders what the domain model carries — contacts have no display name in
/// this slice.
#[component]
fn ContactRow(contact: Contact) -> impl IntoView {
    let Contact {
        contact_id,
        lifecycle,
        normalized_emails,
        normalized_phones,
        latest_source,
        ..
    } = contact;
    let short_id: String = contact_id.to_string().chars().take(8).collect();
    let emails = normalized_emails.join(", ");
    let phones = normalized_phones.join(", ");
    let emails = if emails.is_empty() {
        "—".to_string()
    } else {
        emails
    };
    let phones = if phones.is_empty() {
        "—".to_string()
    } else {
        phones
    };

    view! {
        <li style="border: 1px solid #e5e7eb; border-radius: 0.5rem; padding: 0.75rem;">
            <div style="display: flex; justify-content: space-between; align-items: baseline; gap: 1rem;">
                <strong>{format!("Contact {short_id}…")}</strong>
                <span style="font-size: 0.875rem; color: #4b5563;">
                    {format!("{lifecycle:?}")}
                </span>
            </div>
            <dl style="margin: 0.5rem 0 0; display: grid; grid-template-columns: auto 1fr; gap: 0.25rem 0.75rem; font-size: 0.875rem;">
                <dt style="color: #6b7280;">"Emails"</dt>
                <dd style="margin: 0;">{emails}</dd>
                <dt style="color: #6b7280;">"Phones"</dt>
                <dd style="margin: 0;">{phones}</dd>
                <dt style="color: #6b7280;">"Source"</dt>
                <dd style="margin: 0;">{latest_source}</dd>
            </dl>
        </li>
    }
}

#[cfg(test)]
mod tests {
    use super::split_list;

    #[test]
    fn split_list_trims_and_drops_empties() {
        assert_eq!(
            split_list("a@example.com, b@example.com ,,  "),
            vec!["a@example.com".to_string(), "b@example.com".to_string()]
        );
        assert!(split_list("").is_empty());
        assert!(split_list("   , ").is_empty());
        assert_eq!(split_list("one"), vec!["one".to_string()]);
    }
}
