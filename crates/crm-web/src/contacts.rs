// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Contacts pages: list + create, detail, and merge-duplicates.
//!
//! Reskinned on the v2 component library (UI2-02): shadcn-style table,
//! cards, badges, and buttons over the design-token system. Behavior is
//! unchanged from the 01-FND-02 vertical slice.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::contact::Contact;

use crate::api::{
    create_contact, demo_profile_id, fetch_contact, fetch_contacts, merge_contacts, ApiError,
    ContactListResponse, CreateContactRequest, MergeContactsRequest,
};
use crate::components::{
    CountLine, EmptyState, ErrorAlert, FieldRow, Loading, OkAlert, PageHeader,
};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle, Icon,
    IconName, Input as TextInput, Label, Table, TableBody, TableCell, TableHead, TableHeader,
    TableRow, UiHandler, UiValueHandler,
};

/// Maps a contact lifecycle to its badge variant.
fn lifecycle_badge(lifecycle: &crm_domain::contact::Lifecycle) -> BadgeVariant {
    use crm_domain::contact::Lifecycle;
    match lifecycle {
        Lifecycle::Prospect => BadgeVariant::Warning,
        Lifecycle::Qualified => BadgeVariant::Info,
        Lifecycle::Customer => BadgeVariant::Success,
        Lifecycle::Archived => BadgeVariant::Secondary,
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
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    view! {
        <section aria-label="Contacts">
            <PageHeader
                title="Contacts"
                description="People and companies the agency works with."
            >
                <A
                    href="/contacts/merge"
                    attr:style="display: inline-flex; align-items: center; gap: 0.375rem; \
                                height: 2.5rem; padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                background: var(--secondary); color: var(--secondary-foreground);"
                >
                    <Icon name=IconName::GitBranch size=15 />
                    "Merge duplicates"
                </A>
            </PageHeader>
            <ContactForm token={token.clone()} on_created=retry />
            {move || match contacts.get() {
                None => {
                    view! { <Loading label="contacts" /> }.into_any()
                }
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() on_retry={retry} /> }.into_any()
                }
                Some(Ok(page)) if page.contacts.is_empty() => {
                    view! { <EmptyState message="No contacts yet." /> }.into_any()
                }
                Some(Ok(page)) => {
                    view! {
                        <CountLine count=page.total singular="contact" plural="contacts" />
                        <Card>
                            <Table>
                                <TableHeader>
                                    <TableRow>
                                        <TableHead>"Contact"</TableHead>
                                        <TableHead>"Lifecycle"</TableHead>
                                        <TableHead>"Emails"</TableHead>
                                        <TableHead>"Phones"</TableHead>
                                        <TableHead>"Source"</TableHead>
                                    </TableRow>
                                </TableHeader>
                                <TableBody>
                                    <For each=move || page.contacts.clone() key=|contact| contact.contact_id let:contact>
                                        <ContactRow contact=contact />
                                    </For>
                                </TableBody>
                            </Table>
                        </Card>
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

/// Create-contact form: comma-separated emails/phones plus a source label.
///
/// Validates client-side (at least one email, non-empty source), POSTs with
/// a fresh `Idempotency-Key`, then clears itself, shows a confirmation and
/// asks the parent list to refresh through `on_created`.
#[component]
fn ContactForm(token: String, on_created: UiHandler) -> impl IntoView {
    let emails = RwSignal::new(String::new());
    let phones = RwSignal::new(String::new());
    let source = RwSignal::new(String::new());
    let status = RwSignal::new(None::<FormStatus>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }

        let email_list = split_list(&emails.get_untracked());
        let phone_list = split_list(&phones.get_untracked());
        let source_label = source.get_untracked().trim().to_string();

        if email_list.is_empty() {
            status.set(Some(FormStatus::Error(
                "Enter at least one email address.".to_string(),
            )));
            return;
        }
        if source_label.is_empty() {
            status.set(Some(FormStatus::Error("Enter a source label.".to_string())));
            return;
        }

        submitting.set(true);
        status.set(None);
        let request = CreateContactRequest {
            client_profile_id: demo_profile_id(),
            emails: email_list,
            phones: phone_list,
            source: source_label,
        };
        let token = token.clone();
        spawn_local(async move {
            match create_contact(&request, &token).await {
                Ok(contact) => {
                    emails.set(String::new());
                    phones.set(String::new());
                    source.set(String::new());
                    status.set(Some(FormStatus::Created(contact.contact_id.to_string())));
                    on_created.call();
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
            <CardHeader>
                <CardTitle>"New contact"</CardTitle>
            </CardHeader>
            <CardContent>
                <form on:submit=on_submit>
                    <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                        <div>
                            <Label for_id="contact-emails">"Emails (comma-separated)"</Label>
                            <TextInput
                                id="contact-emails"
                                placeholder="a@example.com, b@example.com"
                                value=emails
                                on_input={UiValueHandler::new(move |v: String| emails.set(v))}
                            />
                        </div>
                        <div>
                            <Label for_id="contact-phones">"Phones (comma-separated, optional)"</Label>
                            <TextInput
                                id="contact-phones"
                                placeholder="+14165550100"
                                value=phones
                                on_input={UiValueHandler::new(move |v: String| phones.set(v))}
                            />
                        </div>
                        <div>
                            <Label for_id="contact-source">"Source"</Label>
                            <TextInput
                                id="contact-source"
                                placeholder="manual-entry"
                                value=source
                                on_input={UiValueHandler::new(move |v: String| source.set(v))}
                            />
                        </div>
                        <div>
                            <Button
                                variant=ButtonVariant::Default
                                btn_type="submit"
                                disabled=Signal::derive(move || submitting.get())
                            >
                                {move || if submitting.get() { "Creating…" } else { "Create contact" }}
                            </Button>
                        </div>
                        {move || {
                            status
                                .get()
                                .map(|state| match state {
                                    FormStatus::Created(id) => {
                                        view! {
                                            <OkAlert message=format!("Contact created: {id}") />
                                        }
                                            .into_any()
                                    }
                                    FormStatus::Duplicate => {
                                        view! {
                                            <div
                                                role="alert"
                                                style="padding: 0.875rem 1rem; border-radius: var(--radius-md); \
                                                       border: 1px solid var(--warning); \
                                                       background: var(--warning-light); \
                                                       color: var(--warning-dark); font-size: 0.875rem;"
                                            >
                                                <p style="margin: 0;">
                                                    "A contact with these details already exists. "
                                                    <A
                                                        href="/contacts/merge"
                                                        attr:style="font-weight: 600; color: inherit;"
                                                    >
                                                        "Merge duplicates →"
                                                    </A>
                                                </p>
                                            </div>
                                        }
                                            .into_any()
                                    }
                                    FormStatus::Error(message) => {
                                        view! { <ErrorAlert message=message /> }.into_any()
                                    }
                                })
                        }}
                    </div>
                </form>
            </CardContent>
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
    let variant = lifecycle_badge(&lifecycle);
    let lifecycle_label = format!("{lifecycle:?}");

    view! {
        <TableRow>
            <TableCell>
                <A
                    href=format!("/contacts/{id}")
                    attr:style="font-family: var(--font-mono); font-size: 0.8125rem; \
                                color: var(--primary); text-decoration: none;"
                >
                    {format!("{short_id}…")}
                </A>
            </TableCell>
            <TableCell>
                <Badge variant=variant>{lifecycle_label}</Badge>
            </TableCell>
            <TableCell>{emails}</TableCell>
            <TableCell>{phones}</TableCell>
            <TableCell>
                <span style="color: var(--muted-foreground);">{latest_source}</span>
            </TableCell>
        </TableRow>
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
// Detail + merge
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
                <A
                    href="/contacts"
                    attr:style="display: inline-flex; align-items: center; height: 2.5rem; \
                                padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                border: 1px solid var(--input); background: transparent; \
                                color: var(--foreground);"
                >
                    "← Back to contacts"
                </A>
            </PageHeader>
            {move || match contact.get() {
                None => view! { <Loading label="contact" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() /> }.into_any()
                }
                Some(Ok(c)) => {
                    let variant = lifecycle_badge(&c.lifecycle);
                    let lifecycle_label = format!("{:?}", c.lifecycle);
                    let touches = c.source_touches.clone();
                    view! {
                        <div style="display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));">
                            <Card>
                                <CardHeader>
                                    <div style="display: flex; align-items: center; gap: 0.625rem;">
                                        <CardTitle>
                                            {format!(
                                                "Contact {}",
                                                c.contact_id.to_string().chars().take(8).collect::<String>()
                                            )}
                                        </CardTitle>
                                        <Badge variant=variant>{lifecycle_label}</Badge>
                                    </div>
                                </CardHeader>
                                <CardContent>
                                    <dl style="margin: 0;">
                                        <FieldRow label="Contact id">
                                            <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                                {c.contact_id.to_string()}
                                            </span>
                                        </FieldRow>
                                        <FieldRow label="Profile id">
                                            <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                                {c.client_profile_id.to_string()}
                                            </span>
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
                                </CardContent>
                            </Card>
                            <Card>
                                <CardHeader>
                                    <CardTitle>"Activity"</CardTitle>
                                </CardHeader>
                                <CardContent>
                                    {if touches.is_empty() {
                                        view! {
                                            <p style="margin: 0; font-size: 0.875rem; color: var(--muted-foreground);">
                                                "No source touches recorded."
                                            </p>
                                        }
                                            .into_any()
                                    } else {
                                        view! {
                                            <ul style="margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 0.625rem;">
                                                {touches
                                                    .into_iter()
                                                    .map(|touch| {
                                                        view! {
                                                            <li style="display: flex; align-items: center; gap: 0.625rem; font-size: 0.875rem;">
                                                                <span
                                                                    aria-hidden="true"
                                                                    style="width: 0.5rem; height: 0.5rem; border-radius: 9999px; \
                                                                           background: var(--chart-1); flex-shrink: 0;"
                                                                ></span>
                                                                <span>{touch}</span>
                                                            </li>
                                                        }
                                                    })
                                                    .collect::<Vec<_>>()}
                                            </ul>
                                        }
                                            .into_any()
                                    }}
                                </CardContent>
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
    let survivor = RwSignal::new(String::new());
    let duplicate = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let survivor_id = survivor.get_untracked().trim().to_string();
        let duplicate_id = duplicate.get_untracked().trim().to_string();
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
                    duplicate.set(String::new());
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
                <CardContent>
                    <form on:submit=on_submit>
                        <div style="display: grid; gap: 0.875rem; max-width: 36rem; padding-top: 0.5rem;">
                            <div>
                                <Label for_id="merge-survivor">"Survivor contact id"</Label>
                                <TextInput
                                    id="merge-survivor"
                                    placeholder="UUID of the surviving contact"
                                    value=survivor
                                    on_input={UiValueHandler::new(move |v: String| survivor.set(v))}
                                />
                            </div>
                            <div>
                                <Label for_id="merge-duplicate">"Duplicate contact id"</Label>
                                <TextInput
                                    id="merge-duplicate"
                                    placeholder="UUID of the duplicate"
                                    value=duplicate
                                    on_input={UiValueHandler::new(move |v: String| duplicate.set(v))}
                                />
                            </div>
                            <div>
                                <Button
                                    variant=ButtonVariant::Default
                                    btn_type="submit"
                                    disabled=Signal::derive(move || submitting.get())
                                >
                                    {move || if submitting.get() { "Merging…" } else { "Merge contacts" }}
                                </Button>
                            </div>
                            {move || match status.get() {
                                None => ().into_any(),
                                Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                                Some(Err(message)) => view! { <ErrorAlert message=message /> }.into_any(),
                            }}
                        </div>
                    </form>
                </CardContent>
            </Card>
        </section>
    }
}
