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
use crate::components::{EmptyState, ErrorAlert, Loading, OkAlert};
use crate::styles;
use crate::ui::components::{Badge, Btn, BtnVariant, Card, FieldRow, HoverTr, PageHeader, Tone};

/// Maps a contact lifecycle to its badge tone.
fn lifecycle_tone(lifecycle: &crm_domain::contact::Lifecycle) -> Tone {
    use crm_domain::contact::Lifecycle;
    match lifecycle {
        Lifecycle::Prospect => Tone::Amber,
        Lifecycle::Qualified => Tone::Blue,
        Lifecycle::Customer => Tone::Green,
        Lifecycle::Archived => Tone::Gray,
    }
}

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
        <section aria-label="Contacts">
            <PageHeader
                title="Contacts"
                description="People and companies the agency works with."
            >
                <A href="/contacts/merge" attr:style=styles::BTN>"Merge duplicates"</A>
            </PageHeader>
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
                        <div style=styles::TABLE_WRAPPER>
                            <table style=styles::TABLE>
                                <thead>
                                    <tr>
                                        <th style=styles::TH>"Contact"</th>
                                        <th style=styles::TH>"Lifecycle"</th>
                                        <th style=styles::TH>"Emails"</th>
                                        <th style=styles::TH>"Phones"</th>
                                        <th style=styles::TH>"Source"</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    <For each=move || page.contacts.clone() key=|contact| contact.contact_id let:contact>
                                        <ContactRow contact=contact />
                                    </For>
                                </tbody>
                            </table>
                        </div>
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
    /// The server rejected the create as a duplicate (`409`); carries a
    /// user-facing message (no PII) plus a link to the merge workflow.
    Duplicate,
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
fn ContactForm(token: String, on_created: impl Fn() + Clone + Send + 'static) -> impl IntoView {
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
                // Dedupe: the server found an existing contact with the same
                // email/phone. The 409 body never echoes the address; neither
                // do we — point at the merge workflow instead.
                Err(ApiError::Api { status: 409, .. }) => {
                    status.set(Some(FormStatus::Duplicate));
                }
                Err(error) => status.set(Some(FormStatus::Error(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <Card>
            <h3 style=styles::FORM_TITLE>"New contact"</h3>
            <form on:submit=on_submit>
            <div style=styles::FORM>
                <div>
                    <label for="contact-emails" style=styles::LABEL>"Emails (comma-separated)"</label>
                    <input
                        id="contact-emails"
                        type="text"
                        node_ref=emails_ref
                        placeholder="a@example.com, b@example.com"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <label for="contact-phones" style=styles::LABEL>"Phones (comma-separated, optional)"</label>
                    <input
                        id="contact-phones"
                        type="text"
                        node_ref=phones_ref
                        placeholder="+14165550100"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <label for="contact-source" style=styles::LABEL>"Source"</label>
                    <input
                        id="contact-source"
                        type="text"
                        node_ref=source_ref
                        placeholder="manual-entry"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <Btn
                        variant=BtnVariant::Primary
                        btn_type="submit"
                        disabled=Signal::derive(move || submitting.get())
                    >
                        {move || if submitting.get() { "Creating…" } else { "Create contact" }}
                    </Btn>
                </div>
                {move || {
                    status
                        .get()
                        .map(|state| match state {
                            FormStatus::Created(id) => {
                                view! {
                                    <p role="status" style=styles::ALERT_OK>
                                        {format!("Contact created: {id}")}
                                    </p>
                                }
                                    .into_any()
                            }
                            FormStatus::Duplicate => {
                                view! {
                                    <div role="alert" style=styles::ALERT_ERROR>
                                        <p style="margin: 0 0 0.5rem;">
                                            "A contact with these details already exists. "
                                            <A href="/contacts/merge" attr:style=styles::LINK>"Merge duplicates →"</A>
                                        </p>
                                    </div>
                                }
                                    .into_any()
                            }
                            FormStatus::Error(message) => {
                                view! {
                                    <p role="alert" style=styles::ALERT_ERROR>{message}</p>
                                }
                                    .into_any()
                            }
                        })
                }}
                </div>
            </form>
        </Card>
    }
}

/// One contact as a table row: id link, lifecycle badge, emails, phones,
/// source.
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
    let tone = lifecycle_tone(&lifecycle);
    let lifecycle_label = format!("{lifecycle:?}");

    view! {
        <HoverTr>
            <td style=styles::TD>
                <A href=format!("/contacts/{id}") attr:style=styles::LINK>
                    <span style=styles::MONO>{format!("{short_id}…")}</span>
                </A>
            </td>
            <td style=styles::TD>
                <Badge tone=tone>{lifecycle_label}</Badge>
            </td>
            <td style=styles::TD>{emails}</td>
            <td style=styles::TD>{phones}</td>
            <td style=styles::TD>
                <span style=styles::MUTED>{latest_source}</span>
            </td>
        </HoverTr>
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
        <section aria-label="Contact detail">
            <PageHeader title="Contact" description="Full record for one contact.">
                <A href="/contacts" attr:style=styles::BTN>"← Back to contacts"</A>
            </PageHeader>
            {move || match contact.get() {
                None => view! { <Loading label="contact" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() /> }.into_any()
                }
                Some(Ok(c)) => {
                    let tone = lifecycle_tone(&c.lifecycle);
                    let lifecycle_label = format!("{:?}", c.lifecycle);
                    let touches = c.source_touches.clone();
                    view! {
                        <div style=styles::RECORD_GRID>
                            <Card>
                                <div style="display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1rem;">
                                    <h3 style="margin: 0; font-size: 1rem; font-weight: 600;">
                                        {format!("Contact {}", c.contact_id.to_string().chars().take(8).collect::<String>())}
                                    </h3>
                                    <Badge tone=tone>{lifecycle_label}</Badge>
                                </div>
                                <dl style=styles::DL>
                                    <FieldRow label="Contact id">
                                        <span style=styles::MONO>{c.contact_id.to_string()}</span>
                                    </FieldRow>
                                    <FieldRow label="Profile id">
                                        <span style=styles::MONO>{c.client_profile_id.to_string()}</span>
                                    </FieldRow>
                                    <FieldRow label="Emails">{c.normalized_emails.join(", ")}</FieldRow>
                                    <FieldRow label="Phones">{c.normalized_phones.join(", ")}</FieldRow>
                                    <FieldRow label="First source">{c.first_source.clone()}</FieldRow>
                                    <FieldRow label="Latest source">{c.latest_source.clone()}</FieldRow>
                                    <FieldRow label="Merged into">
                                        {c
                                            .merged_into
                                            .map(|id| id.to_string())
                                            .unwrap_or_else(|| "—".to_string())}
                                    </FieldRow>
                                </dl>
                            </Card>
                            <Card>
                                <h3 style=styles::FORM_TITLE>"Activity"</h3>
                                {if touches.is_empty() {
                                    view! { <p style=styles::MUTED>"No source touches recorded."</p> }.into_any()
                                } else {
                                    view! {
                                        <div>
                                            {touches
                                                .into_iter()
                                                .map(|touch| {
                                                    view! {
                                                        <div style=styles::TIMELINE_ITEM>
                                                            <span style=styles::TIMELINE_DOT aria-hidden="true"></span>
                                                            <span>{touch}</span>
                                                        </div>
                                                    }
                                                })
                                                .collect::<Vec<_>>()}
                                        </div>
                                    }
                                        .into_any()
                                }}
                            </Card>
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
        <section aria-label="Merge duplicates">
            <PageHeader
                title="Merge duplicates"
                description="Fold a duplicate contact into its survivor. The survivor keeps the union of identity, sources and consents; the duplicate is archived (never deleted)."
            />
            <Card>
            <form on:submit=on_submit>
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
                        <Btn
                            variant=BtnVariant::Primary
                            btn_type="submit"
                            disabled=Signal::derive(move || submitting.get())
                        >
                            {move || if submitting.get() { "Merging…" } else { "Merge contacts" }}
                        </Btn>
                    </div>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => view! { <ErrorAlert message=message /> }.into_any(),
                    }}
                </div>
            </form>
            </Card>
        </section>
    }
}
