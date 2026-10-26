// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Mail list + thread detail pages (MAIL-01).
//!
//! `/mail` was a `ComingSoon` placeholder while the backend already
//! existed; this adds the frontend only. Every action below goes
//! through the existing endpoints in `crates/crm-api/src/emails.rs`
//! (`GET /api/v1/threads`, `GET /api/v1/threads/{id}`,
//! `POST /api/v1/emails/send`). The ingest webhook is not
//! bearer-authenticated and is not part of this UI.
//!
//! One API gap shapes the page (flagged, not papered over): the email
//! domain has no read/unread tracking — neither `EmailThread` nor
//! `EmailMessage` carries an unread flag — so the list shows message
//! count, last message, and status, and never an unread marker. An
//! unread column would need a backend read-tracking feature first.
//!
//! Threads are profile-scoped (the list query requires
//! `client_profile_id`), so the page has a profile selector defaulting
//! to the demo profile, where the seed thread lives.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::email::{
    is_valid_email, EmailDirection, EmailMessage, EmailStatus, EmailThread, ThreadStatus,
};

use crate::api::{
    fetch_profiles, fetch_thread, fetch_threads, send_email, ApiError, SendEmailRequest,
    ThreadListResponse, DEMO_PROFILE_ID_STR,
};
use crate::components::{
    CountLine, EmptyState, ErrorAlert, FieldRow, Loading, OkAlert, PageHeader,
};
use crate::ui::{
    push_toast, Badge, BadgeVariant, Button, Card, CardContent, CardHeader,
    CardTitle, Input as TextInput, Label, Select, Table, TableBody, TableCell, TableHead,
    TableHeader, TableRow, Textarea, ToastVariant, Toasts, UiHandler, UiValueHandler,
};

/// Maps a thread lifecycle state to its badge variant.
fn thread_status_badge(status: &ThreadStatus) -> BadgeVariant {
    match status {
        ThreadStatus::Open => BadgeVariant::Success,
        ThreadStatus::Closed => BadgeVariant::Secondary,
        ThreadStatus::Archived => BadgeVariant::Default,
    }
}

/// Maps a message lifecycle state to its badge variant.
fn message_status_badge(status: &EmailStatus) -> BadgeVariant {
    match status {
        EmailStatus::Draft => BadgeVariant::Secondary,
        EmailStatus::Queued => BadgeVariant::Info,
        EmailStatus::Sending => BadgeVariant::Info,
        EmailStatus::Sent => BadgeVariant::Success,
        EmailStatus::Delivered => BadgeVariant::Default,
        EmailStatus::Failed => BadgeVariant::Destructive,
        EmailStatus::Bounced => BadgeVariant::Warning,
    }
}

/// Maps a message direction to its badge variant.
fn direction_badge(direction: &EmailDirection) -> BadgeVariant {
    match direction {
        EmailDirection::Outbound => BadgeVariant::Info,
        EmailDirection::Inbound => BadgeVariant::Success,
    }
}

/// Splits a comma-separated recipient field into addresses: trims and
/// drops blanks. Validation happens at send time.
fn split_addresses(field: &str) -> Vec<String> {
    field
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Mail page: profile selector, thread list, compose form.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn MailPage(token: String) -> impl IntoView {
    // Scope selector value: a profile UUID. Defaults to the demo
    // profile so the seeded demo thread is visible on first load.
    let selected = RwSignal::new(DEMO_PROFILE_ID_STR.to_string());
    let scope_options = RwSignal::new(None::<Result<Vec<(String, String)>, ApiError>>);
    {
        let token = token.clone();
        Effect::new(move |_| {
            let token = token.clone();
            spawn_local(async move {
                match fetch_profiles(&token).await {
                    Ok(page) => scope_options.set(Some(Ok(page
                        .profiles
                        .into_iter()
                        .map(|p| (p.profile_id.to_string(), p.name))
                        .collect()))),
                    Err(error) => scope_options.set(Some(Err(error))),
                }
            });
        });
    }

    view! {
        <section aria-label="Mail">
            <PageHeader
                title="Mail"
                description="Email threads for a client profile: read, reply, and compose."
            />
            {move || match scope_options.get() {
                None => view! { <Loading label="profiles" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.friendly_message() /> }.into_any()
                }
                Some(Ok(options)) => {
                    view! {
                        <Card>
                            <CardContent>
                                <div style="max-width: 28rem; padding-top: 0.5rem;">
                                    <Label for_id="mail-profile">"Client profile"</Label>
                                    <Select
                                        id="mail-profile"
                                        options=options
                                        selected=selected
                                    />
                                </div>
                            </CardContent>
                        </Card>
                        <ThreadBrowser token={token.clone()} selected=selected />
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Thread list + compose form for the selected profile.
///
/// Extracted so the token prop is owned here instead of being threaded
/// through nested reactive closures (which would force `FnOnce`).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `selected`: the selected profile UUID.
#[component]
fn ThreadBrowser(token: String, selected: RwSignal<String>) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    let threads = RwSignal::new(None::<Result<ThreadListResponse, ApiError>>);
    // Generation counter: each effect run bumps it; a response is applied
    // only if its generation is still current, so a slow earlier request
    // can never overwrite a newer one.
    let generation = RwSignal::new(0u64);
    {
        let token = token.clone();
        Effect::new(move |_| {
            reload.get();
            let scope = selected.get();
            // Clear the list when the profile changes (or a reload
            // starts) so stale rows never linger under the new selection.
            threads.set(None);
            let my_generation = generation.get_untracked() + 1;
            generation.set(my_generation);
            let token = token.clone();
            spawn_local(async move {
                let result = fetch_threads(&token, &scope).await;
                if generation.get_untracked() == my_generation {
                    threads.set(Some(result));
                }
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    view! {
        {move || {
            // Clone here: the nested `move ||` blocks below capture `token`
            // by move, so the outer closure must only ever clone it.
            let token = token.clone();
            let profile_id = selected.get();
            view! {
                <ComposeForm
                    token={token.clone()}
                    profile_id=profile_id.clone()
                    on_sent=retry
                />
                {move || match threads.get() {
                    None => view! { <Loading label="threads" /> }.into_any(),
                    Some(Err(error)) => {
                        view! { <ErrorAlert message=error.friendly_message() on_retry=retry /> }
                            .into_any()
                    }
                    Some(Ok(page)) if page.threads.is_empty() => {
                        view! { <EmptyState message="No threads yet." /> }.into_any()
                    }
                    Some(Ok(page)) => {
                        view! {
                            <CountLine count=page.total singular="thread" plural="threads" />
                            <Card>
                                <CardContent>
                                    <Table>
                                        <TableHeader>
                                            <TableRow>
                                                <TableHead>"Subject"</TableHead>
                                                <TableHead>"Participants"</TableHead>
                                                <TableHead>"Messages"</TableHead>
                                                <TableHead>"Last message"</TableHead>
                                                <TableHead>"Status"</TableHead>
                                            </TableRow>
                                        </TableHeader>
                                        <TableBody>
                                            <For
                                                each=move || page.threads.clone()
                                                key=|t| t.thread_id.to_string()
                                                children=move |t| {
                                                    // Compute the per-row strings up front: the
                                                    // `view!` below captures its environment by
                                                    // move, so borrowed values cannot be used
                                                    // inside it on every row.
                                                    let id = t.thread_id.to_string();
                                                    let href = format!("/mail/{id}");
                                                    let participants = t.participants.join(", ");
                                                    let last = t
                                                        .last_message_at
                                                        .format("%Y-%m-%d %H:%M")
                                                        .to_string();
                                                    let status = t.status;
                                                    let count = t.message_count;
                                                    view! {
                                                        <TableRow>
                                                            <TableCell>
                                                                <A
                                                                    href=href
                                                                    attr:style="font-weight: 600; font-size: 0.875rem; \
                                                                                color: var(--foreground); text-decoration: none;"
                                                                >
                                                                    {t.subject.clone()}
                                                                </A>
                                                            </TableCell>
                                                            <TableCell>{participants}</TableCell>
                                                            <TableCell>{count.to_string()}</TableCell>
                                                            <TableCell>{last}</TableCell>
                                                            <TableCell>
                                                                <Badge variant=thread_status_badge(&status)>
                                                                    {format!("{status:?}")}
                                                                </Badge>
                                                            </TableCell>
                                                        </TableRow>
                                                    }
                                                }
                                            />
                                        </TableBody>
                                    </Table>
                                </CardContent>
                            </Card>
                        }
                            .into_any()
                    }
                }}
            }
        }}
    }
}

/// Compose form: new email on the selected profile.
///
/// Client-side validation mirrors the server's structural rules
/// (at least one recipient, structurally valid addresses, subject and
/// body not both blank) so a bad draft fails fast; the server still
/// validates authoritatively. A failed send keeps the draft visible
/// with the error above the button — the form itself is the retry.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the owning profile UUID string.
/// - `on_sent`: called after a successful send so the list reloads.
#[component]
fn ComposeForm(token: String, profile_id: String, on_sent: UiHandler) -> impl IntoView {
    let to = RwSignal::new(String::new());
    let subject = RwSignal::new(String::new());
    let body = RwSignal::new(String::new());
    let status = RwSignal::new(Option::<Result<String, String>>::None);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let profile_id = profile_id.clone();
        move || {
            if working.get() {
                return;
            }
            let recipients = split_addresses(&to.get());
            if recipients.is_empty() {
                status.set(Some(Err("Enter at least one recipient.".to_string())));
                return;
            }
            if let Some(bad) = recipients.iter().find(|a| !is_valid_email(a)) {
                status.set(Some(Err(format!("{bad} is not a valid email address."))));
                return;
            }
            let subject_value = subject.get();
            let body_value = body.get();
            if subject_value.trim().is_empty() && body_value.trim().is_empty() {
                status.set(Some(Err(
                    "Enter a subject or a message body.".to_string(),
                )));
                return;
            }
            let request = SendEmailRequest {
                client_profile_id: profile_id.clone(),
                thread_id: None,
                from: String::new(),
                to: recipients,
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject_value.clone(),
                body_text: body_value,
                body_html: None,
            };
            // Clone before the async block: the block moves these, and
            // this closure must stay `Fn` for the button handler.
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match send_email(&request, &token).await {
                    Ok(sent) => {
                        status.set(Some(Ok("Email sent.".to_string())));
                        to.set(String::new());
                        subject.set(String::new());
                        body.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Email sent",
                                Some(sent.subject.clone()),
                                ToastVariant::Success,
                            );
                        }
                        on_sent.call();
                    }
                    Err(e) => status.set(Some(Err(e.friendly_message()))),
                }
                working.set(false);
            });
        }
    };

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Compose"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="compose-to">"To"</Label>
                        <TextInput
                            id="compose-to"
                            placeholder="client@example.com, other@example.com"
                            value=to
                            on_input=UiValueHandler::new(move |v| to.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="compose-subject">"Subject"</Label>
                        <TextInput
                            id="compose-subject"
                            placeholder="Subject"
                            value=subject
                            on_input=UiValueHandler::new(move |v| subject.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="compose-body">"Message"</Label>
                        <Textarea
                            id="compose-body"
                            rows=5
                            placeholder="Write your message…"
                            value=body
                            on_input=UiValueHandler::new(move |v| body.set(v))
                        />
                    </div>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => {
                            view! { <ErrorAlert message=message /> }.into_any()
                        }
                    }}
                    <div>
                        <Button
                            on_click=UiHandler::new(submit)
                            disabled=Signal::derive(move || working.get())
                        >
                            "Send email"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Recipients for a reply: the thread's participants except addresses
/// the agency itself sent from (the `from` on outbound messages), so
/// the reply goes to the other side of the conversation instead of
/// back to the agency. Falls back to all participants when that
/// leaves nobody (e.g. a thread with no outbound message yet).
fn reply_recipients(thread: &EmailThread, messages: &[EmailMessage]) -> String {
    let agency: Vec<&str> = messages
        .iter()
        .filter(|m| m.direction == EmailDirection::Outbound)
        .map(|m| m.from.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let to: Vec<&str> = thread
        .participants
        .iter()
        .map(String::as_str)
        .filter(|p| !agency.contains(p))
        .collect();
    let to = if to.is_empty() {
        thread
            .participants
            .iter()
            .map(String::as_str)
            .collect::<Vec<&str>>()
    } else {
        to
    };
    to.join(", ")
}

/// Subject for a reply: the thread subject, prefixed with `Re: `
///
/// unless it already carries a reply prefix.
fn reply_subject(subject: &str) -> String {
    let lower = subject.trim().to_lowercase();
    if lower.starts_with("re:") {
        subject.trim().to_string()
    } else {
        format!("Re: {}", subject.trim())
    }
}

/// Thread detail page: the thread record, its messages in
/// chronological order, and a reply box.
///
/// A fetch failure never wipes the record: the error is shown as an
/// alert above the content, and the loaded thread stays visible
/// underneath (the AUDIT-08 `ProfileDetail` pattern).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `thread_id`: the thread UUID from the route.
#[component]
pub fn ThreadDetail(token: String, thread_id: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    // The loaded record, kept across fetch failures.
    let thread = RwSignal::new(None::<EmailThread>);
    let messages = RwSignal::new(Vec::<EmailMessage>::new());
    // True while a fetch is in flight; only the initial load shows a
    // spinner — a reload keeps the previous record on screen.
    let loading = RwSignal::new(true);
    // The last page-level failure (fetch), if any.
    let error = RwSignal::new(Option::<String>::None);
    {
        let token = token.clone();
        let thread_id = thread_id.clone();
        Effect::new(move |_| {
            reload.get();
            loading.set(true);
            let token = token.clone();
            let thread_id = thread_id.clone();
            spawn_local(async move {
                match fetch_thread(&thread_id, &token).await {
                    Ok(detail) => {
                        thread.set(Some(detail.thread));
                        messages.set(detail.messages);
                        error.set(None);
                    }
                    // Friendly message, not the raw server text.
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                loading.set(false);
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    view! {
        <section aria-label="Thread detail">
            <PageHeader title="Thread" description="Messages and reply.">
                <A
                    href="/mail"
                    attr:style="display: inline-flex; align-items: center; height: 2.5rem; \
                                padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                border: 1px solid var(--input); background: transparent; \
                                color: var(--foreground);"
                >
                    "← Back to mail"
                </A>
            </PageHeader>
            {move || error.get().map(|message| view! {
                <ErrorAlert message=message on_retry=retry />
            })}
            {move || match thread.get() {
                None => {
                    if loading.get() {
                        view! { <Loading label="thread" /> }.into_any()
                    } else {
                        ().into_any()
                    }
                }
                Some(t) => {
                    let profile_id = t.client_profile_id.to_string();
                    let participants = t.participants.join(", ");
                    let last = t.last_message_at.format("%Y-%m-%d %H:%M").to_string();
                    let created = t.created_at.format("%Y-%m-%d %H:%M").to_string();
                    let status = t.status;
                    let count = t.message_count;
                    let thread_messages = messages.get();
                    view! {
                        <Card>
                            <CardHeader>
                                <div style="display: flex; align-items: center; gap: 0.625rem;">
                                    <CardTitle>{t.subject.clone()}</CardTitle>
                                    <Badge variant=thread_status_badge(&status)>
                                        {format!("{status:?}")}
                                    </Badge>
                                </div>
                            </CardHeader>
                            <CardContent>
                                <dl style="margin: 0 0 1rem;">
                                    <FieldRow label="Thread id">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {t.thread_id.to_string()}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Client profile">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {profile_id}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Participants">{participants}</FieldRow>
                                    <FieldRow label="Messages">{count.to_string()}</FieldRow>
                                    <FieldRow label="Last message">{last}</FieldRow>
                                    <FieldRow label="Created">{created}</FieldRow>
                                </dl>
                            </CardContent>
                        </Card>
                        <Card>
                            <CardHeader>
                                <CardTitle>"Messages"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                {if thread_messages.is_empty() {
                                    view! {
                                        <EmptyState message="No messages in this thread yet." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <For
                                            each=move || thread_messages.clone()
                                            key=|m| m.message_id.to_string()
                                            children=move |m| {
                                                // Compute the per-row strings up front:
                                                // the `view!` below captures its
                                                // environment by move.
                                                let from = m.from.clone();
                                                let to = m.to.join(", ");
                                                let subject = m.subject.clone();
                                                let body = m.body_text.clone();
                                                let sent = match m.sent_at {
                                                    Some(at) => {
                                                        at.format("%Y-%m-%d %H:%M").to_string()
                                                    }
                                                    None => {
                                                        m.created_at
                                                            .format("%Y-%m-%d %H:%M")
                                                            .to_string()
                                                    }
                                                };
                                                let direction = m.direction;
                                                let status = m.status;
                                                view! {
                                                    <div style="margin: 0 0 1rem; padding: 0 0 1rem; border-bottom: 1px solid var(--border);">
                                                        <div style="display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; margin: 0 0 0.5rem;">
                                                            <Badge variant=direction_badge(&direction)>
                                                                {format!("{direction:?}")}
                                                            </Badge>
                                                            <Badge variant=message_status_badge(&status)>
                                                                {format!("{status:?}")}
                                                            </Badge>
                                                            <span style="font-size: 0.875rem; font-weight: 600;">
                                                                {from}
                                                            </span>
                                                            <span style="font-size: 0.875rem; color: var(--muted-foreground);">
                                                                {format!("→ {to} · {sent}")}
                                                            </span>
                                                        </div>
                                                        <p style="margin: 0 0 0.5rem; font-size: 0.875rem; font-weight: 600;">
                                                            {subject}
                                                        </p>
                                                        <p style="margin: 0; font-size: 0.875rem; white-space: pre-wrap;">
                                                            {body}
                                                        </p>
                                                    </div>
                                                }
                                            }
                                        />
                                    }
                                        .into_any()
                                }}
                            </CardContent>
                        </Card>
                        <ReplyForm
                            token={token.clone()}
                            thread=t.clone()
                            messages=messages.get()
                            on_sent=retry
                        />
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Reply box on a thread: prefilled To (other participants) and
/// subject (`Re: …`), body left blank. A failed send keeps the draft
/// visible with the error above the button; Retry re-sends the same
/// draft. On success the body clears and the thread reloads.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `thread`: the thread being replied to.
/// - `messages`: the thread's messages (used to work out who the agency
///   is, for the To prefill).
/// - `on_sent`: called after a successful send so the thread reloads.
#[component]
fn ReplyForm(
    token: String,
    thread: EmailThread,
    messages: Vec<EmailMessage>,
    on_sent: UiHandler,
) -> impl IntoView {
    let to = RwSignal::new(reply_recipients(&thread, &messages));
    let subject = RwSignal::new(reply_subject(&thread.subject));
    let body = RwSignal::new(String::new());
    let status = RwSignal::new(Option::<Result<String, String>>::None);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    // Built once and cloned for the button and for Retry: every capture
    // is `Clone`/`Copy`, so the closure stays `Fn` and can be used
    // twice. The async block moves per-call clones, never the captures.
    let submit = {
        let token = token.clone();
        let thread_id = thread.thread_id.to_string();
        let profile_id = thread.client_profile_id.to_string();
        move || {
            if working.get() {
                return;
            }
            let recipients = split_addresses(&to.get());
            if recipients.is_empty() {
                status.set(Some(Err("Enter at least one recipient.".to_string())));
                return;
            }
            if let Some(bad) = recipients.iter().find(|a| !is_valid_email(a)) {
                status.set(Some(Err(format!("{bad} is not a valid email address."))));
                return;
            }
            let subject_value = subject.get();
            let body_value = body.get();
            if subject_value.trim().is_empty() && body_value.trim().is_empty() {
                status.set(Some(Err(
                    "Enter a subject or a message body.".to_string(),
                )));
                return;
            }
            let request = SendEmailRequest {
                client_profile_id: profile_id.clone(),
                thread_id: Some(thread_id.clone()),
                from: String::new(),
                to: recipients,
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject_value.clone(),
                body_text: body_value,
                body_html: None,
            };
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match send_email(&request, &token).await {
                    Ok(sent) => {
                        status.set(Some(Ok("Reply sent.".to_string())));
                        body.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Reply sent",
                                Some(sent.subject.clone()),
                                ToastVariant::Success,
                            );
                        }
                        on_sent.call();
                    }
                    Err(e) => status.set(Some(Err(e.friendly_message()))),
                }
                working.set(false);
            });
        }
    };

    // The error alert's Retry and the button each take a handler, so
    // clone once up front: the view below moves one copy into the
    // status closure and the other into the button.
    let submit_for_retry = submit.clone();

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Reply"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="reply-to">"To"</Label>
                        <TextInput
                            id="reply-to"
                            placeholder="client@example.com"
                            value=to
                            on_input=UiValueHandler::new(move |v| to.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="reply-subject">"Subject"</Label>
                        <TextInput
                            id="reply-subject"
                            placeholder="Re: Subject"
                            value=subject
                            on_input=UiValueHandler::new(move |v| subject.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="reply-body">"Message"</Label>
                        <Textarea
                            id="reply-body"
                            rows=5
                            placeholder="Write your reply…"
                            value=body
                            on_input=UiValueHandler::new(move |v| body.set(v))
                        />
                    </div>
                    {move || match status.get() {
                        None => ().into_any(),
                        Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                        Some(Err(message)) => {
                            // Clone per render: moving the handler out
                            // here would make this render closure `FnOnce`.
                            view! {
                                <ErrorAlert
                                    message=message
                                    on_retry=UiHandler::new(submit_for_retry.clone())
                                />
                            }
                                .into_any()
                        }
                    }}
                    <div>
                        <Button
                            on_click=UiHandler::new(submit)
                            disabled=Signal::derive(move || working.get())
                        >
                            "Send reply"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}
