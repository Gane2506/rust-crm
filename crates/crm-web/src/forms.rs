// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Forms list + detail pages (FORM-01).
//!
//! Forms had no UI at all before this task: `/forms` was a
//! `ComingSoon` placeholder and `/forms/:id` was a 404, while the
//! backend (create, list, get, publish, archive, fields, submissions,
//! review) is complete and unchanged — every action below goes
//! through the existing endpoints in `crates/crm-api/src/forms.rs`.
//!
//! One API-adjacent gap shapes the page (flagged, not papered over):
//! the demo user had no `Form` grants at all, so every form call
//! 403d even though the seed plants a published demo form. The seed
//! now grants `Form` Read/Create/Update/Delete at agency scope
//! (FORM-01, same class as AUDIT-07's Campaign grants) — seed data,
//! not an API change. Field removal needs `Form:Delete`.
//!
//! The list response carries no field count, so the list fetches each
//! form's detail (`GET /forms/{id}` → `fields`) to count its fields.
//! That is one extra request per row, bounded by the page size (20);
//! a `field_count` field on the list response would remove it
//! (same N+1 shape as R3-04's step counts and BILL-01's balances).
//!
//! Forms are profile-scoped — the list requires a `client_profile_id`
//! — so, unlike Campaigns, there is no Agency scope on this page:
//! the selector lists profiles only and defaults to the demo profile
//! so the seeded demo-seed-form is visible on first load.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::form::{
    Form, FormField, FormFieldType, FormStatus, FormSubmission, SubmissionStatus,
};

use crate::api::{
    add_field, create_form, fetch_form, fetch_forms, fetch_profiles, fetch_submissions,
    remove_field, review_submission, transition_form, AddFieldRequest, ApiError, CreateFormRequest,
    FormListResponse, DEMO_PROFILE_ID_STR,
};
use crate::components::{
    CountLine, EmptyState, ErrorAlert, FieldRow, Loading, OkAlert, PageHeader,
};
use crate::ui::{
    push_toast, Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardHeader,
    CardTitle, Input as TextInput, Label, Select, Table, TableBody, TableCell, TableHead,
    TableHeader, TableRow, ToastVariant, Toasts, UiHandler, UiValueHandler,
};

/// Maps a form lifecycle status to its badge variant.
fn form_status_badge(status: FormStatus) -> BadgeVariant {
    match status {
        FormStatus::Draft => BadgeVariant::Secondary,
        FormStatus::Published => BadgeVariant::Success,
        FormStatus::Archived => BadgeVariant::Secondary,
    }
}

/// Maps a submission lifecycle status to its badge variant.
fn submission_status_badge(status: SubmissionStatus) -> BadgeVariant {
    match status {
        SubmissionStatus::New => BadgeVariant::Info,
        SubmissionStatus::Reviewed => BadgeVariant::Success,
        SubmissionStatus::Archived => BadgeVariant::Secondary,
    }
}

/// Human-readable label for a field type.
fn field_type_label(field_type: &FormFieldType) -> &'static str {
    match field_type {
        FormFieldType::TextField => "Text",
        FormFieldType::EmailField => "Email",
        FormFieldType::PhoneField => "Phone",
        FormFieldType::NumberField => "Number",
        FormFieldType::SelectField => "Select",
        FormFieldType::CheckboxField => "Checkbox",
        FormFieldType::DateField => "Date",
    }
}

/// Shortens a UUID string to its first 8 characters for display.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Splits a comma-separated select-options input into non-empty
/// trimmed options. `"basic, pro,, "` → `["basic", "pro"]`.
fn parse_select_options(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|option| !option.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Resolves a submission's answers to `(key, label, value)` triples
/// using the form's loaded fields; an answer for an unknown field
/// falls back to the short field id so nothing is ever dropped
/// silently. The key is the field id (unique per answer in practice).
fn answer_rows(submission: &FormSubmission, fields: &[FormField]) -> Vec<(String, String, String)> {
    submission
        .answers
        .iter()
        .map(|answer| {
            let key = answer.field_id.to_string();
            let label = fields
                .iter()
                .find(|field| field.field_id == answer.field_id)
                .map(|field| field.label.clone())
                .unwrap_or_else(|| short_id(&key));
            (key, label, answer.value.clone())
        })
        .collect()
}

/// Forms page: profile selector, form list, create form.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn FormsPage(token: String) -> impl IntoView {
    // Scope selector value: a profile UUID. Defaults to the demo
    // profile so the seeded demo-seed-form is visible on first load.
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
        <section aria-label="Forms">
            <PageHeader
                title="Forms"
                description="Typed intake and lead forms: fields, submissions, and lifecycle."
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
                                    <Label for_id="forms-profile">"Client profile"</Label>
                                    <Select
                                        id="forms-profile"
                                        options=options
                                        selected=selected
                                    />
                                </div>
                            </CardContent>
                        </Card>
                        <FormBrowser token={token.clone()} selected=selected />
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Form list + create form for the selected profile.
///
/// Extracted so the token prop is owned here instead of being threaded
/// through nested reactive closures (which would force `FnOnce`).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `selected`: the selected profile UUID.
#[component]
fn FormBrowser(token: String, selected: RwSignal<String>) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    let forms = RwSignal::new(None::<Result<FormListResponse, ApiError>>);
    // Field counts keyed by form id. The list response carries no
    // field count, so each row's detail is fetched once (bounded by
    // the page size, 20) and the `fields` length is cached here.
    let field_counts = RwSignal::new(HashMap::<String, usize>::new());
    // Generation counter: each effect run bumps it; a response is applied
    // only if its generation is still current, so a slow earlier request
    // can never overwrite a newer one.
    let generation = RwSignal::new(0u64);
    {
        let token = token.clone();
        Effect::new(move |_| {
            reload.get();
            let scope = selected.get();
            // Clear the list when the profile changes (or a reload starts)
            // so stale rows never linger under the new selection.
            forms.set(None);
            field_counts.set(HashMap::new());
            let my_generation = generation.get_untracked() + 1;
            generation.set(my_generation);
            let token = token.clone();
            spawn_local(async move {
                let result = fetch_forms(&scope, &token).await;
                if generation.get_untracked() != my_generation {
                    return;
                }
                if let Ok(page) = &result {
                    // Count fields per form from its detail.
                    for form in &page.forms {
                        let form_id = form.form_id.to_string();
                        let token = token.clone();
                        spawn_local(async move {
                            if let Ok(detail) = fetch_form(&form_id, &token).await {
                                if generation.get_untracked() == my_generation {
                                    field_counts.update(|counts| {
                                        counts.insert(form_id, detail.fields.len());
                                    });
                                }
                            }
                        });
                    }
                }
                forms.set(Some(result));
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    view! {
        {move || {
            // Clone here: the nested `move ||` blocks below capture `token`
            // by move, so the outer closure must only ever clone it.
            let token = token.clone();
            view! {
                <FormCreateForm
                    token={token.clone()}
                    profile_id=selected.get()
                    on_created=retry
                />
                {move || match forms.get() {
                    None => view! { <Loading label="forms" /> }.into_any(),
                    Some(Err(error)) => {
                        view! { <ErrorAlert message=error.friendly_message() on_retry=retry /> }
                            .into_any()
                    }
                    Some(Ok(page)) if page.forms.is_empty() => {
                        view! { <EmptyState message="No forms yet." /> }.into_any()
                    }
                    Some(Ok(page)) => {
                        view! {
                            <CountLine count=page.total singular="form" plural="forms" />
                            <Card>
                                <CardContent>
                                    <Table>
                                        <TableHeader>
                                            <TableRow>
                                                <TableHead>"Name"</TableHead>
                                                <TableHead>"Status"</TableHead>
                                                <TableHead>"Fields"</TableHead>
                                                <TableHead>"Created"</TableHead>
                                            </TableRow>
                                        </TableHeader>
                                        <TableBody>
                                            <For
                                                each=move || page.forms.clone()
                                                key=|f| f.form_id.to_string()
                                                children=move |f| {
                                                    let id = f.form_id.to_string();
                                                    // Compute the per-row strings up front: the
                                                    // `view!` below captures its environment by
                                                    // move, so `id` cannot be borrowed inside
                                                    // it on every row.
                                                    let href = format!("/forms/{id}");
                                                    let status = f.status;
                                                    let count = {
                                                        let id = id.clone();
                                                        move || {
                                                            field_counts
                                                                .get()
                                                                .get(id.as_str())
                                                                .copied()
                                                        }
                                                    };
                                                    view! {
                                                        <TableRow>
                                                            <TableCell>
                                                                <A
                                                                    href=href
                                                                    attr:style="font-weight: 600; font-size: 0.875rem; \
                                                                                color: var(--foreground); text-decoration: none;"
                                                                >
                                                                    {f.name.clone()}
                                                                </A>
                                                            </TableCell>
                                                            <TableCell>
                                                                <Badge variant=form_status_badge(status)>
                                                                    {format!("{status:?}")}
                                                                </Badge>
                                                            </TableCell>
                                                            <TableCell>
                                                                {move || match count() {
                                                                    Some(n) => n.to_string(),
                                                                    None => "…".to_string(),
                                                                }}
                                                            </TableCell>
                                                            <TableCell>
                                                                {f.created_at.format("%Y-%m-%d %H:%M").to_string()}
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

/// Create-form form for one client profile.
///
/// A failed create keeps the draft: the name is cleared only on
/// success, so a validation error never eats what was typed.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the owning profile (UUID string).
/// - `on_created`: invoked after a successful create so the list reloads.
#[component]
fn FormCreateForm(token: String, profile_id: String, on_created: UiHandler) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let profile_id = profile_id.clone();
        move || {
            if working.get() {
                return;
            }
            let name_value = name.get().trim().to_string();
            if name_value.is_empty() {
                status.set(Some(Err("Enter a form name.".to_string())));
                return;
            }
            let request = CreateFormRequest {
                client_profile_id: profile_id.clone(),
                name: name_value,
            };
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match create_form(&request, &token).await {
                    Ok(created) => {
                        status.set(Some(Ok(format!("Form \"{}\" created.", created.name))));
                        name.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Form created",
                                Some(created.name.clone()),
                                ToastVariant::Success,
                            );
                        }
                        on_created.call();
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
                <CardTitle>"New form"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="form-name">"Form name"</Label>
                        <TextInput
                            id="form-name"
                            placeholder="Lead capture"
                            value=name
                            on_input=UiValueHandler::new(move |v| name.set(v))
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
                            "Create form"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Form detail page: full record, fields, submissions, and lifecycle
/// actions.
///
/// A fetch or transition failure never wipes the record: the error is
/// shown as an alert above the content, and the loaded record stays
/// visible underneath (the AUDIT-08 `ProfileDetail` pattern — a failed
/// transition used to replace the whole page with the error alert).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `form_id`: the form UUID from the route.
#[component]
pub fn FormDetail(token: String, form_id: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    // The loaded record, kept across fetch/transition failures.
    let form = RwSignal::new(None::<Form>);
    let fields = RwSignal::new(Vec::<FormField>::new());
    let submissions = RwSignal::new(Vec::<FormSubmission>::new());
    // True while a fetch is in flight; only the initial load shows a
    // spinner — a reload keeps the previous record on screen.
    let loading = RwSignal::new(true);
    // The last page-level failure (fetch or transition), if any.
    let error = RwSignal::new(Option::<String>::None);
    let working = RwSignal::new(false);
    {
        let token = token.clone();
        let form_id = form_id.clone();
        Effect::new(move |_| {
            reload.get();
            loading.set(true);
            let token = token.clone();
            let form_id = form_id.clone();
            spawn_local(async move {
                match fetch_form(&form_id, &token).await {
                    Ok(detail) => {
                        form.set(Some(detail.form));
                        fields.set(detail.fields);
                        error.set(None);
                    }
                    // Friendly message, not the raw server text: the
                    // user needs "You don't have access to do that.",
                    // not "server error 403: Forbidden — …".
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                match fetch_submissions(&form_id, &token).await {
                    Ok(page) => submissions.set(page.submissions),
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                loading.set(false);
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));
    let toasts = use_context::<Toasts>();

    let transition = {
        let token = token.clone();
        let form_id = form_id.clone();
        move |name: &'static str| {
            // Guard against double-clicks racing the async work: the buttons
            // are disabled via `working`, but check synchronously here too so
            // a second invocation can never start a duplicate transition.
            if working.get() {
                return;
            }
            let token = token.clone();
            let form_id = form_id.clone();
            working.set(true);
            spawn_local(async move {
                match transition_form(&form_id, name, &token).await {
                    Ok(updated) => {
                        let verb = match name {
                            "publish" => "published",
                            "archive" => "archived",
                            _ => "updated",
                        };
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                format!("Form {verb}"),
                                Some(updated.name.clone()),
                                ToastVariant::Success,
                            );
                        }
                        form.set(Some(updated));
                        error.set(None);
                    }
                    // The record stays: only the error is recorded, so the
                    // alert renders above the still-visible record.
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                working.set(false);
            });
        }
    };

    let remove = {
        let token = token.clone();
        let form_id = form_id.clone();
        move |field_id: String| {
            if working.get() {
                return;
            }
            let token = token.clone();
            let form_id = form_id.clone();
            working.set(true);
            spawn_local(async move {
                match remove_field(&form_id, &field_id, &token).await {
                    Ok(()) => {
                        if let Some(toasts) = toasts {
                            push_toast(toasts, "Field removed", None, ToastVariant::Success);
                        }
                        error.set(None);
                        reload.update(|n| *n = n.wrapping_add(1));
                    }
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                working.set(false);
            });
        }
    };

    let review = {
        let token = token.clone();
        let form_id = form_id.clone();
        move |submission_id: String| {
            if working.get() {
                return;
            }
            let token = token.clone();
            let form_id = form_id.clone();
            working.set(true);
            spawn_local(async move {
                match review_submission(&form_id, &submission_id, &token).await {
                    Ok(updated) => {
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Submission reviewed",
                                Some(updated.submitted_by_label.clone()),
                                ToastVariant::Success,
                            );
                        }
                        submissions.update(|list| {
                            if let Some(current) = list
                                .iter_mut()
                                .find(|s| s.submission_id == updated.submission_id)
                            {
                                *current = updated;
                            }
                        });
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                working.set(false);
            });
        }
    };

    view! {
        <section aria-label="Form detail">
            <PageHeader title="Form" description="Fields, submissions, and lifecycle.">
                <A
                    href="/forms"
                    attr:style="display: inline-flex; align-items: center; height: 2.5rem; \
                                padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                border: 1px solid var(--input); background: transparent; \
                                color: var(--foreground);"
                >
                    "← Back to forms"
                </A>
            </PageHeader>
            {move || error.get().map(|message| view! {
                <ErrorAlert message=message on_retry=retry />
            })}
            {move || match form.get() {
                Some(f) => {
                    let publish = transition.clone();
                    let archive = transition.clone();
                    let status = f.status;
                    let is_draft = status == FormStatus::Draft;
                    let is_archived = status == FormStatus::Archived;
                    let can_act = !working.get() && !is_archived;
                    let fields_list = fields.get();
                    let field_count = fields_list.len();
                    let submissions_list = submissions.get();
                    // Precompute each submission's answer rows before the
                    // `view!`: a `For` children closure cannot borrow
                    // per-row data computed inside it.
                    let submission_rows: Vec<(FormSubmission, Vec<(String, String, String)>)> =
                        submissions_list
                            .iter()
                            .map(|s| (s.clone(), answer_rows(s, &fields_list)))
                            .collect();
                    let form_id_value = f.form_id.to_string();
                    let profile_id_value = f.client_profile_id.to_string();
                    let form_name = f.name.clone();
                    view! {
                        <Card>
                            <CardHeader>
                                <div style="display: flex; align-items: center; gap: 0.625rem;">
                                    <CardTitle>{form_name}</CardTitle>
                                    <Badge variant=form_status_badge(status)>
                                        {format!("{status:?}")}
                                    </Badge>
                                </div>
                            </CardHeader>
                            <CardContent>
                                <dl style="margin: 0 0 1rem;">
                                    <FieldRow label="Form id">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {form_id_value}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Client profile">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {profile_id_value}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Fields">{field_count.to_string()}</FieldRow>
                                    <FieldRow label="Submissions">
                                        {submission_rows.len().to_string()}
                                    </FieldRow>
                                    <FieldRow label="Created">
                                        {f.created_at.format("%Y-%m-%d %H:%M").to_string()}
                                    </FieldRow>
                                    <FieldRow label="Updated">
                                        {f.updated_at.format("%Y-%m-%d %H:%M").to_string()}
                                    </FieldRow>
                                </dl>
                                <div style="display: flex; gap: 0.5rem; flex-wrap: wrap;">
                                    <Button
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_draft)
                                        on_click={UiHandler::new({
                                            let publish = publish.clone();
                                            move || publish("publish")
                                        })}
                                    >
                                        "Publish"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Destructive
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act)
                                        on_click={UiHandler::new({
                                            let archive = archive.clone();
                                            move || archive("archive")
                                        })}
                                    >
                                        "Archive"
                                    </Button>
                                </div>
                                <p style="margin: 0.75rem 0 0; font-size: 0.75rem; color: var(--muted-foreground);">
                                    "Only Draft forms can be published. Archived is terminal. Archiving is idempotent on the server: archiving an already-archived form succeeds without changing it."
                                </p>
                            </CardContent>
                        </Card>
                        <Card>
                            <CardHeader>
                                <CardTitle>"Fields"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                {if fields_list.is_empty() {
                                    view! {
                                        <EmptyState message="No fields yet — add the first field below." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <Table>
                                            <TableHeader>
                                                <TableRow>
                                                    <TableHead>"Position"</TableHead>
                                                    <TableHead>"Label"</TableHead>
                                                    <TableHead>"Type"</TableHead>
                                                    <TableHead>"Required"</TableHead>
                                                    <TableHead>"Options"</TableHead>
                                                    <TableHead>"Actions"</TableHead>
                                                </TableRow>
                                            </TableHeader>
                                            <TableBody>
                                                <For
                                                    each=move || fields_list.clone()
                                                    key=|field| field.field_id.to_string()
                                                    children=move |field| {
                                                        // Per-row strings up front: the `view!`
                                                        // captures its environment by move.
                                                        let field_id = field.field_id.to_string();
                                                        let field_id_for_remove =
                                                            field_id.clone();
                                                        let remove_one = remove.clone();
                                                        let options = field
                                                            .config
                                                            .get("options")
                                                            .and_then(|v| v.as_array())
                                                            .map(|options| {
                                                                options
                                                                    .iter()
                                                                    .filter_map(|o| o.as_str())
                                                                    .collect::<Vec<&str>>()
                                                                    .join(", ")
                                                            })
                                                            .unwrap_or_else(|| {
                                                                "—".to_string()
                                                            });
                                                        let type_label =
                                                            field_type_label(&field.field_type);
                                                        view! {
                                                            <TableRow>
                                                                <TableCell>
                                                                    {field.position.to_string()}
                                                                </TableCell>
                                                                <TableCell>
                                                                    {field.label.clone()}
                                                                </TableCell>
                                                                <TableCell>{type_label}</TableCell>
                                                                <TableCell>
                                                                    {if field.required {
                                                                        "Yes".to_string()
                                                                    } else {
                                                                        "—".to_string()
                                                                    }}
                                                                </TableCell>
                                                                <TableCell>{options}</TableCell>
                                                                <TableCell>
                                                                    <Button
                                                                        variant=ButtonVariant::Destructive
                                                                        btn_type="button"
                                                                        disabled=Signal::derive(move || {
                                                                            working.get()
                                                                        })
                                                                        on_click={UiHandler::new(move || {
                                                                            remove_one(
                                                                                field_id_for_remove.clone(),
                                                                            )
                                                                        })}
                                                                    >
                                                                        "Remove"
                                                                    </Button>
                                                                </TableCell>
                                                            </TableRow>
                                                        }
                                                    }
                                                />
                                            </TableBody>
                                        </Table>
                                    }
                                        .into_any()
                                }}
                            </CardContent>
                        </Card>
                        <FormFieldForm
                            token={token.clone()}
                            form_id={f.form_id.to_string()}
                            field_count=field_count
                            on_added=retry
                        />
                        <Card>
                            <CardHeader>
                                <CardTitle>"Submissions"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                <p style="margin: 0 0 1rem; font-size: 0.875rem; color: var(--muted-foreground);">
                                    "Newest first. Review is idempotent on the server: reviewing an already-reviewed submission succeeds without changing it. Archived submissions cannot be reviewed."
                                </p>
                                {if submission_rows.is_empty() {
                                    view! {
                                        <EmptyState message="No submissions yet." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <For
                                            each=move || submission_rows.clone()
                                            key=|(s, _)| s.submission_id.to_string()
                                            children=move |(s, answers)| {
                                                let submission_id = s.submission_id.to_string();
                                                let review_one = review.clone();
                                                let is_new =
                                                    s.status == SubmissionStatus::New;
                                                let status = s.status;
                                                view! {
                                                    <Card>
                                                        <CardHeader>
                                                            <div style="display: flex; align-items: center; gap: 0.625rem; flex-wrap: wrap;">
                                                                <CardTitle>
                                                                    {s.submitted_by_label.clone()}
                                                                </CardTitle>
                                                                <Badge variant=submission_status_badge(status)>
                                                                    {format!("{status:?}")}
                                                                </Badge>
                                                                <span style="font-size: 0.8125rem; color: var(--muted-foreground);">
                                                                    {s.submitted_at.format("%Y-%m-%d %H:%M").to_string()}
                                                                </span>
                                                            </div>
                                                        </CardHeader>
                                                        <CardContent>
                                                            {if answers.is_empty() {
                                                                view! {
                                                                    <p style="margin: 0; font-size: 0.875rem; color: var(--muted-foreground);">
                                                                        "No answers recorded."
                                                                    </p>
                                                                }
                                                                    .into_any()
                                                            } else {
                                                                view! {
                                                                    <dl style="margin: 0 0 1rem;">
                                                                        <For
                                                                            each=move || answers.clone()
                                                                            key=|(key, _, _)| key.clone()
                                                                            children=move |(_, label, value)| {
                                                                                view! {
                                                                                    <div style="display: grid; grid-template-columns: 10rem 1fr; gap: 0.75rem; padding: 0.625rem 0; border-bottom: 1px solid var(--border); font-size: 0.875rem;">
                                                                                        <dt style="margin: 0; color: var(--muted-foreground); font-weight: 500;">
                                                                                            {label}
                                                                                        </dt>
                                                                                        <dd style="margin: 0; min-width: 0; overflow-wrap: anywhere;">
                                                                                            {value}
                                                                                        </dd>
                                                                                    </div>
                                                                                }
                                                                            }
                                                                        />
                                                                    </dl>
                                                                }
                                                                    .into_any()
                                                            }}
                                                            {if is_new {
                                                                view! {
                                                                    <div style="display: flex; gap: 0.5rem; flex-wrap: wrap;">
                                                                        <Button
                                                                            btn_type="button"
                                                                            disabled=Signal::derive(move || {
                                                                                working.get()
                                                                            })
                                                                            on_click={UiHandler::new(move || {
                                                                                review_one(
                                                                                    submission_id.clone(),
                                                                                )
                                                                            })}
                                                                        >
                                                                            "Review"
                                                                        </Button>
                                                                    </div>
                                                                }
                                                                    .into_any()
                                                            } else {
                                                                ().into_any()
                                                            }}
                                                        </CardContent>
                                                    </Card>
                                                }
                                            }
                                        />
                                    }
                                        .into_any()
                                }}
                            </CardContent>
                        </Card>
                    }
                        .into_any()
                }
                None => {
                    if loading.get() {
                        view! { <Loading label="form" /> }.into_any()
                    } else {
                        view! {
                            <EmptyState message="Form not found — it may have been deleted, or the link may be wrong." />
                        }
                            .into_any()
                    }
                }
            }}
        </section>
    }
}

/// Add-field form for one form.
///
/// A failed add keeps the draft: the label and options are cleared
/// only on success. Client-side validation mirrors the server rules:
/// a non-blank label, and a non-empty options list for select fields
/// (the server rejects a select field without one). The options input
/// is only sent for select fields; other types send `{}`.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `form_id`: the owning form (UUID string).
/// - `field_count`: the current field count, used as the default
///   position for the new field (appended last).
/// - `on_added`: invoked after a successful add so the detail reloads.
#[component]
fn FormFieldForm(
    token: String,
    form_id: String,
    field_count: usize,
    on_added: UiHandler,
) -> impl IntoView {
    let label = RwSignal::new(String::new());
    let field_type = RwSignal::new("text".to_string());
    let required = RwSignal::new("false".to_string());
    let position = RwSignal::new(field_count.to_string());
    let options = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let form_id = form_id.clone();
        move || {
            if working.get() {
                return;
            }
            let label_value = label.get().trim().to_string();
            if label_value.is_empty() {
                status.set(Some(Err("Enter a field label.".to_string())));
                return;
            }
            let type_value = field_type.get();
            let parsed_options = parse_select_options(&options.get());
            if type_value == "select" && parsed_options.is_empty() {
                status.set(Some(Err(
                    "Select fields need at least one option (comma-separated).".to_string(),
                )));
                return;
            }
            let position_raw = position.get();
            let position_value = if position_raw.trim().is_empty() {
                field_count as u32
            } else {
                match position_raw.trim().parse::<u32>() {
                    Ok(value) => value,
                    Err(_) => {
                        status.set(Some(Err("Position must be a whole number.".to_string())));
                        return;
                    }
                }
            };
            let config = if type_value == "select" {
                serde_json::json!({ "options": parsed_options })
            } else {
                serde_json::json!({})
            };
            let request = AddFieldRequest {
                field_type: type_value,
                label: label_value,
                required: required.get() == "true",
                position: position_value,
                config,
            };
            let token = token.clone();
            let form_id = form_id.clone();
            working.set(true);
            spawn_local(async move {
                match add_field(&form_id, &request, &token).await {
                    Ok(added) => {
                        status.set(Some(Ok(format!("Field \"{}\" added.", added.label))));
                        label.set(String::new());
                        options.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Field added",
                                Some(added.label.clone()),
                                ToastVariant::Success,
                            );
                        }
                        on_added.call();
                    }
                    Err(e) => status.set(Some(Err(e.friendly_message()))),
                }
                working.set(false);
            });
        }
    };

    let type_options = vec![
        ("text".to_string(), "Text".to_string()),
        ("email".to_string(), "Email".to_string()),
        ("phone".to_string(), "Phone".to_string()),
        ("number".to_string(), "Number".to_string()),
        ("select".to_string(), "Select".to_string()),
        ("checkbox".to_string(), "Checkbox".to_string()),
        ("date".to_string(), "Date".to_string()),
    ];
    let required_options = vec![
        ("false".to_string(), "Optional".to_string()),
        ("true".to_string(), "Required".to_string()),
    ];

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Add field"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="field-label">"Label"</Label>
                        <TextInput
                            id="field-label"
                            placeholder="Full name"
                            value=label
                            on_input=UiValueHandler::new(move |v| label.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="field-type">"Type"</Label>
                        <Select
                            id="field-type"
                            options=type_options
                            selected=field_type
                        />
                    </div>
                    <div>
                        <Label for_id="field-required">"Required"</Label>
                        <Select
                            id="field-required"
                            options=required_options
                            selected=required
                        />
                    </div>
                    <div>
                        <Label for_id="field-position">"Position"</Label>
                        <TextInput
                            id="field-position"
                            input_type="number"
                            value=position
                            on_input=UiValueHandler::new(move |v| position.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="field-options">"Options (comma-separated)"</Label>
                        <TextInput
                            id="field-options"
                            placeholder="basic, pro"
                            value=options
                            on_input=UiValueHandler::new(move |v| options.set(v))
                        />
                    </div>
                    <p style="margin: 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        "Options are only used for Select fields; other types ignore them. A select field needs at least one option."
                    </p>
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
                            "Add field"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_select_options_splits_and_trims() {
        assert_eq!(
            parse_select_options("basic, pro"),
            vec!["basic".to_string(), "pro".to_string()]
        );
    }

    #[test]
    fn parse_select_options_drops_blanks() {
        assert_eq!(
            parse_select_options("basic,, pro, "),
            vec!["basic".to_string(), "pro".to_string()]
        );
    }

    #[test]
    fn parse_select_options_empty_is_empty() {
        assert!(parse_select_options("").is_empty());
        assert!(parse_select_options("  ,  ").is_empty());
    }

    #[test]
    fn answer_rows_resolve_labels_and_fall_back_to_short_id() {
        use crm_domain::form::{FormAnswer, FormFieldType};
        use crm_domain::ids::FormId;
        use serde_json::json;
        let form_id = FormId::new();
        let field = FormField::new(
            form_id,
            FormFieldType::TextField,
            "Full name".to_string(),
            true,
            0,
            json!({}),
        )
        .expect("valid field");
        let unknown = crm_domain::ids::FormFieldId::new();
        let submission = FormSubmission::new(
            form_id,
            crm_domain::ids::ClientProfileId::new(),
            None,
            "Web form".to_string(),
            vec![
                FormAnswer {
                    field_id: field.field_id,
                    value: "Ada".to_string(),
                },
                FormAnswer {
                    field_id: unknown,
                    value: "x".to_string(),
                },
            ],
        )
        .expect("valid submission");
        let rows = answer_rows(&submission, &[field]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "Full name");
        assert_eq!(rows[0].2, "Ada");
        assert_eq!(rows[1].1, short_id(&unknown.to_string()));
        assert_eq!(rows[1].2, "x");
    }
}
