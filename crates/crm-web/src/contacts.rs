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
use leptos_router::components::A;

use crm_domain::contact::Contact;

use crate::api::{
    create_contact, demo_profile_id, fetch_contact, fetch_contacts, merge_contacts, ApiError,
    ContactListResponse, CreateContactRequest, MergeContactsRequest,
};
use crate::components::{EmptyState, ErrorAlert, Loading, OkAlert, PageHeader};
use crate::styles;

/// Contacts list page: fetch-on-mount list plus the create form.
///
/// `token` is the in-memory bearer token from the login form; every API
/// call sends it as `Authorization: Bearer`.
#[component]
pub fn ContactsPage(token: String) -> impl IntoView {
    // Bumped to refetch the list (after a create, or on manual retry).
    let reload = RwSignal::new(0u32);
    // `None` = loading, `Some(Ok(_))` = loaded, `Some(Err(_))` = failed.
    //
    // Driven by an `Effect` + `spawn_local` instead of `Resource`: gloo-net
    // futures are `!Send`, and Leptos `Resource` requires `Send` futures.
    let contacts = RwSignal::new(None::<Result<ContactListResponse, ApiError>>);
    let token_for_fetch = token.clone();
    Effect::new(move |_| {
        reload.get();
        contacts.set(None);
        let token = token_for_fetch.clone();
        spawn_local(async move {
            contacts.set(Some(fetch_contacts(&token).await));
        });
    });
    let retry = move || reload.update(|n| *n = n.wrapping_add(1));

    view! {
        <section aria-label="Contacts" style=styles::SECTION>
            <PageHeader title="Contacts" />
            <p>
                <A href="/contacts/merge">"Merge duplicate contacts"</A>
            </p>
            <ContactForm token={token.clone()} on_created=move || reload.update(|n| *n = n.wrapping_add(1)) />
            <h3 style=styles::H3>"Contact list"</h3>
            {move || match contacts.get() {
                None => {
                    view! { <Loading label="contacts" /> }.into_any()
                }
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() on_retry={std::rc::Rc::new(retry) as std::rc::Rc<dyn Fn()>} /> }.into_any()
                }
                Some(Ok(page)) if page.contacts.is_empty() => {
                    view! { <EmptyState message="No contacts yet." /> }.into_any()
                }
                Some(Ok(page)) => {
                    view! {
                        <p style=styles::MUTED>
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
fn ContactForm(token: String, on_created: impl Fn() + Clone + 'static) -> impl IntoView {
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
        let token = token.clone();
        spawn_local(async move {
            match create_contact(&request, &token).await {
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
/// this slice. The id links to the detail page.
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
    let id = contact_id.to_string();
    let short_id: String = id.chars().take(8).collect();
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
        <li style=styles::CARD>
            <div style="display: flex; justify-content: space-between; align-items: baseline; gap: 1rem;">
                <A href=format!("/contacts/{id}")>
                    <strong>{format!("Contact {short_id}…")}</strong>
                </A>
                <span style=styles::MUTED>{format!("{lifecycle:?}")}</span>
            </div>
            <dl style=styles::DL>
                <dt style=styles::DT>"Emails"</dt>
                <dd style="margin: 0;">{emails}</dd>
                <dt style=styles::DT>"Phones"</dt>
                <dd style="margin: 0;">{phones}</dd>
                <dt style=styles::DT>"Source"</dt>
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

// ---------------------------------------------------------------------------
// Detail + merge (appended by 06-UI-01)
// ---------------------------------------------------------------------------

/// Contact detail page: full record fetched by id.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `id`: the contact UUID from the route.
#[component]
pub fn ContactDetail(token: String, id: String) -> impl IntoView {
    let contact = RwSignal::new(None::<Result<Contact, ApiError>>);
    {
        let token = token.clone();
        Effect::new(move |_| {
            if id.is_empty() {
                return;
            }
            contact.set(None);
            let token = token.clone();
            let id = id.clone();
            spawn_local(async move {
                contact.set(Some(fetch_contact(&id, &token).await));
            });
        });
    }

    view! {
        <section aria-label="Contact detail" style=styles::SECTION>
            <PageHeader title="Contact" />
            <p>
                <A href="/contacts">"← Back to contacts"</A>
            </p>
            {move || match contact.get() {
                None => view! { <Loading label="contact" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() /> }.into_any()
                }
                Some(Ok(c)) => {
                    view! {
                        <div style=styles::CARD>
                            <dl style=styles::DL>
                                <dt style=styles::DT>"Contact id"</dt>
                                <dd style=styles::MONO>{c.contact_id.to_string()}</dd>
                                <dt style=styles::DT>"Profile id"</dt>
                                <dd style=styles::MONO>{c.client_profile_id.to_string()}</dd>
                                <dt style=styles::DT>"Lifecycle"</dt>
                                <dd style="margin: 0;">{format!("{:?}", c.lifecycle)}</dd>
                                <dt style=styles::DT>"Emails"</dt>
                                <dd style="margin: 0;">{c.normalized_emails.join(", ")}</dd>
                                <dt style=styles::DT>"Phones"</dt>
                                <dd style="margin: 0;">{c.normalized_phones.join(", ")}</dd>
                                <dt style=styles::DT>"First source"</dt>
                                <dd style="margin: 0;">{c.first_source.clone()}</dd>
                                <dt style=styles::DT>"Latest source"</dt>
                                <dd style="margin: 0;">{c.latest_source.clone()}</dd>
                                <dt style=styles::DT>"Source touches"</dt>
                                <dd style="margin: 0;">{c.source_touches.join(", ")}</dd>
                                <dt style=styles::DT>"Merged into"</dt>
                                <dd style="margin: 0;">
                                    {c
                                        .merged_into
                                        .map(|id| id.to_string())
                                        .unwrap_or_else(|| "—".to_string())}
                                </dd>
                            </dl>
                        </div>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Merge-duplicates page: folds a duplicate contact into its survivor.
///
/// Posts `POST /api/v1/contacts/merge`. The survivor absorbs identity,
/// sources and consents; the duplicate is archived with `merged_into`
/// set (tombstone — never deleted). A `409` means the pair cannot merge
/// (different profiles, already merged, or self-merge).
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn MergePage(token: String) -> impl IntoView {
    let survivor_ref = NodeRef::<Input>::new();
    let duplicate_ref = NodeRef::<Input>::new();
    let status = RwSignal::new(None::<Result<String, String>>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let survivor_id = survivor_ref
            .get()
            .map(|i| i.value().trim().to_string())
            .unwrap_or_default();
        let duplicate_id = duplicate_ref
            .get()
            .map(|i| i.value().trim().to_string())
            .unwrap_or_default();
        if survivor_id.is_empty() || duplicate_id.is_empty() {
            status.set(Some(Err("Enter both contact ids.".to_string())));
            return;
        }
        if survivor_id == duplicate_id {
            status.set(Some(Err(
                "Survivor and duplicate must be different contacts.".to_string(),
            )));
            return;
        }
        submitting.set(true);
        status.set(None);
        let request = MergeContactsRequest {
            survivor_id,
            duplicate_id,
        };
        let token = token.clone();
        spawn_local(async move {
            match merge_contacts(&request, &token).await {
                Ok(archived) => {
                    if let Some(input) = duplicate_ref.get() {
                        input.set_value("");
                    }
                    status.set(Some(Ok(format!(
                        "Merged — duplicate {} archived.",
                        archived.contact_id
                    ))));
                }
                Err(ApiError::Api { status: 409, .. }) => {
                    status.set(Some(Err(
                        "Cannot merge this pair (different profiles, already merged, or invalid ids)."
                            .to_string(),
                    )));
                }
                Err(error) => status.set(Some(Err(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <section aria-label="Merge duplicates" style=styles::SECTION>
            <PageHeader title="Merge duplicates" />
            <p style=styles::MUTED>
                "Fold a duplicate contact into its survivor. The survivor keeps the union of identity, sources and consents; the duplicate is archived (never deleted)."
            </p>
            <form on:submit=on_submit style=styles::CARD>
                <div style=styles::FORM>
                    <div>
                        <label for="merge-survivor" style=styles::LABEL>"Survivor contact id"</label>
                        <input
                            id="merge-survivor"
                            type="text"
                            node_ref=survivor_ref
                            placeholder="UUID of the surviving contact"
                            autocomplete="off"
                            style=styles::INPUT
                        />
                    </div>
                    <div>
                        <label for="merge-duplicate" style=styles::LABEL>"Duplicate contact id"</label>
                        <input
                            id="merge-duplicate"
                            type="text"
                            node_ref=duplicate_ref
                            placeholder="UUID of the duplicate"
                            autocomplete="off"
                            style=styles::INPUT
                        />
                    </div>
                    <div>
                        <button type="submit" disabled=move || submitting.get() style=styles::BTN_PRIMARY>
                            {move || if submitting.get() { "Merging…" } else { "Merge contacts" }}
                        </button>
                    </div>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => view! { <ErrorAlert message=message /> }.into_any(),
                    }}
                </div>
            </form>
        </section>
    }
}
