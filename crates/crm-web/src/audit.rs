// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Audit log viewer: query the tamper-evident security log and verify
//! the chain.
//!
//! Reskinned on the v2 component library (UI2-02). Both endpoints
//! require `AuditLog:Read` at agency scope — a caller without it gets
//! `403`. The server returns events in chain order (oldest first) with
//! no descending-order parameter; the "newest first" toggle therefore
//! only reorders the fetched page for display. It does NOT fetch the
//! newest N events — with more than one page of matches, the newest
//! events may be beyond the fetched page. True newest-first querying
//! needs a backend `order` parameter (a future backend task).

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{
    fetch_audit_events, verify_audit_chain, ApiError, AuditEventsResponse, AuditQuery,
    VerifyResponse,
};
use crate::components::{EmptyState, ErrorAlert, Hint, Loading, PageHeader};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, Icon, IconName,
    Input as TextInput, Label, Switch, Table, TableBody, TableCell, TableHead, TableHeader,
    TableRow, UiHandler, UiValueHandler,
};

/// Audit log page: filterable event table plus chain verification.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn AuditPage(token: String) -> impl IntoView {
    let action = RwSignal::new(String::new());
    let reload = RwSignal::new(0u32);
    let newest_first = RwSignal::new(true);
    let events = RwSignal::new(None::<Result<AuditEventsResponse, ApiError>>);
    let verification = RwSignal::new(None::<Result<VerifyResponse, ApiError>>);
    let verifying = RwSignal::new(false);
    // Generation counter: a response is applied only if its generation is
    // still current, so a slow earlier query can never overwrite a newer one.
    let generation = RwSignal::new(0u64);

    {
        let token = token.clone();
        Effect::new(move |_| {
            reload.get();
            events.set(None);
            let filter = action.get_untracked().trim().to_string();
            let query = AuditQuery {
                action: if filter.is_empty() {
                    None
                } else {
                    Some(filter)
                },
                limit: Some(50),
            };
            let my_generation = generation.get_untracked() + 1;
            generation.set(my_generation);
            let token = token.clone();
            spawn_local(async move {
                let result = fetch_audit_events(&query, &token).await;
                if generation.get_untracked() == my_generation {
                    events.set(Some(result));
                }
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    // 0-arg so it plugs straight into [`Button`]'s `on_click`.
    let on_verify = UiHandler::new(move || {
        if verifying.get() {
            return;
        }
        verifying.set(true);
        verification.set(None);
        let token = token.clone();
        spawn_local(async move {
            verification.set(Some(verify_audit_chain(&token).await));
            verifying.set(false);
        });
    });

    view! {
        <section aria-label="Audit log">
            <PageHeader
                title="Audit log"
                description="Tamper-evident security event history."
            />
            <Card>
                <CardContent>
                    <div style="display: flex; gap: 0.75rem; align-items: end; flex-wrap: wrap; padding-top: 0.5rem;">
                        <div style="flex: 1; min-width: 12rem;">
                            <Label for_id="audit-action">"Action filter"</Label>
                            <TextInput
                                id="audit-action"
                                placeholder="auth.login_failed"
                                value=action
                                on_input={UiValueHandler::new(move |v: String| action.set(v))}
                            />
                        </div>
                        <Button
                            variant=ButtonVariant::Default
                            btn_type="button"
                            on_click={retry}
                        >
                            <Icon name=IconName::Search size=15 />
                            "Query"
                        </Button>
                        <Button
                            variant=ButtonVariant::Secondary
                            btn_type="button"
                            disabled=Signal::derive(move || verifying.get())
                            on_click={on_verify}
                        >
                            <Icon name=IconName::ShieldCheck size=15 />
                            {move || if verifying.get() { "Verifying…" } else { "Verify chain" }}
                        </Button>
                        <div style="display: flex; align-items: center; gap: 0.5rem; padding-bottom: 0.625rem;">
                            <Switch
                                checked=newest_first
                                label="Newest first"
                            />
                            <span style="font-size: 0.8125rem; color: var(--muted-foreground);">
                                "Newest first"
                            </span>
                        </div>
                    </div>
                    <Hint text="Newest-first reorders the fetched page only — the API returns chain order (oldest first).".to_string() />
                </CardContent>
            </Card>
            {move || match verification.get() {
                None => ().into_any(),
                Some(Ok(v)) => {
                    let (variant, message) = if v.valid {
                        (
                            BadgeVariant::Success,
                            format!("Chain valid — {} events checked.", v.events_checked),
                        )
                    } else {
                        (
                            BadgeVariant::Destructive,
                            format!(
                                "CHAIN BROKEN at event {}. Treat as a security incident.",
                                v.first_bad_event.as_deref().unwrap_or("?"),
                            ),
                        )
                    };
                    view! {
                        <div style="margin: 1rem 0;">
                            <Badge variant=variant>{message}</Badge>
                            {v
                                .head
                                .as_ref()
                                .map(|head| {
                                    view! {
                                        <p style="margin: 0.5rem 0 0; font-family: var(--font-mono); font-size: 0.75rem; color: var(--muted-foreground);">
                                            {format!("head {head}")}
                                        </p>
                                    }
                                })}
                        </div>
                    }
                        .into_any()
                }
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() /> }.into_any()
                }
            }}
            {move || match events.get() {
                None => view! { <Loading label="audit events" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() on_retry={retry} /> }.into_any()
                }
                Some(Ok(page)) if page.events.is_empty() => {
                    view! { <EmptyState message="No audit events match." /> }.into_any()
                }
                Some(Ok(page)) => {
                    let mut rows = page.events.clone();
                    if newest_first.get() {
                        rows.reverse();
                    }
                    view! {
                        <Card>
                            <Table>
                                <TableHeader>
                                    <TableRow>
                                        <TableHead>"Time"</TableHead>
                                        <TableHead>"Action"</TableHead>
                                        <TableHead>"Actor"</TableHead>
                                        <TableHead>"Target"</TableHead>
                                        <TableHead>"Decision"</TableHead>
                                        <TableHead>"Payload"</TableHead>
                                    </TableRow>
                                </TableHeader>
                                <TableBody>
                                    <For each=move || rows.clone() key=|item| item.event.event_id.to_string() let:item>
                                        <AuditRow item=item />
                                    </For>
                                </TableBody>
                            </Table>
                        </Card>
                        {page
                            .has_more
                            .then(|| {
                                view! {
                                    <Hint text="More events match — refine the filter.".to_string() />
                                }
                            })}
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// One audit event row: time, action, short actor id, target, decision, payload.
#[component]
fn AuditRow(item: crate::api::AuditEventItem) -> impl IntoView {
    let event = item.event;
    let actor: String = event.actor_id.to_string().chars().take(8).collect();
    let time = event.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string();
    let decision = format!("{:?}/{:?}", event.permission_decision, event.result);
    // Forensic payload as compact JSON; `None` renders as an em dash.
    let payload = item
        .payload
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".to_string());
    view! {
        <TableRow>
            <TableCell>{time}</TableCell>
            <TableCell>
                <span style="font-family: var(--font-mono); font-size: 0.8125rem;">{event.action.clone()}</span>
            </TableCell>
            <TableCell>
                <span style="font-family: var(--font-mono); font-size: 0.8125rem; color: var(--muted-foreground);">
                    {actor}
                </span>
            </TableCell>
            <TableCell>
                {format!("{} {}", event.target_kind, event.target_id.chars().take(8).collect::<String>())}
            </TableCell>
            <TableCell>
                <Badge variant=BadgeVariant::Secondary>{decision}</Badge>
            </TableCell>
            <TableCell>
                <span style="font-family: var(--font-mono); font-size: 0.75rem; color: var(--muted-foreground); \
                             display: inline-block; max-width: 16rem; overflow: hidden; \
                             text-overflow: ellipsis; white-space: nowrap;" title=payload.clone()>
                    {payload.clone()}
                </span>
            </TableCell>
        </TableRow>
    }
}
