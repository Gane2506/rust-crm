// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Audit log viewer: query the tamper-evident security log and verify
//! the chain.
//!
//! Both endpoints require `AuditLog:Read` at agency scope — a caller
//! without it gets `403`. Events render in chain order (oldest first);
//! the viewer offers newest-first via a toggle.

use crate::api::{
    fetch_audit_events, verify_audit_chain, ApiError, AuditEventsResponse, AuditQuery,
    VerifyResponse,
};
use crate::components::{EmptyState, ErrorAlert, Loading, PageHeader};
use crate::styles;
use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Audit log page: filterable event table plus chain verification.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn AuditPage(token: String) -> impl IntoView {
    let action_ref = NodeRef::<Input>::new();
    let reload = RwSignal::new(0u32);
    let newest_first = RwSignal::new(true);
    let events = RwSignal::new(None::<Result<AuditEventsResponse, ApiError>>);
    let verification = RwSignal::new(None::<Result<VerifyResponse, ApiError>>);
    let verifying = RwSignal::new(false);

    {
        let token = token.clone();
        Effect::new(move |_| {
            reload.get();
            events.set(None);
            let action = action_ref
                .get()
                .map(|i| i.value().trim().to_string())
                .unwrap_or_default();
            let query = AuditQuery {
                action: if action.is_empty() {
                    None
                } else {
                    Some(action)
                },
                limit: Some(50),
            };
            let token = token.clone();
            spawn_local(async move {
                events.set(Some(fetch_audit_events(&query, &token).await));
            });
        });
    }
    let retry = move || reload.update(|n| *n = n.wrapping_add(1));

    let on_verify = move |_| {
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
    };

    view! {
        <section aria-label="Audit log" style=styles::SECTION>
            <PageHeader title="Audit log" />
            <div style="display: flex; gap: 0.5rem; align-items: end; margin-bottom: 1rem; flex-wrap: wrap;">
                <div>
                    <label for="audit-action" style=styles::LABEL>"Action filter"</label>
                    <input
                        id="audit-action"
                        type="text"
                        node_ref=action_ref
                        placeholder="auth.login_failed"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <button on:click=move |_| retry() style=styles::BTN_PRIMARY>"Query"</button>
                <button on:click=on_verify disabled=move || verifying.get() style=styles::BTN>
                    {move || if verifying.get() { "Verifying…" } else { "Verify chain" }}
                </button>
                <label style="display: flex; gap: 0.375rem; align-items: center; font-size: 0.875rem;">
                    <input
                        type="checkbox"
                        checked=move || newest_first.get()
                        on:change=move |ev| newest_first.set(leptos::leptos_dom::helpers::event_target_checked(&ev))
                    />
                    "Newest first"
                </label>
            </div>
            {move || match verification.get() {
                None => ().into_any(),
                Some(Ok(v)) => {
                    let style = if v.valid { styles::ALERT_OK } else { styles::ALERT_ERROR };
                    view! {
                        <div role="status" style=style>
                            <p style="margin: 0;">
                                {if v.valid {
                                    format!(
                                        "Chain valid — {} events checked.",
                                        v.events_checked
                                    )
                                } else {
                                    format!(
                                        "CHAIN BROKEN at event {}. Treat as a security incident.",
                                        v.first_bad_event.as_deref().unwrap_or("?"),
                                    )
                                }}
                            </p>
                            {v
                                .head
                                .as_ref()
                                .map(|head| {
                                    view! {
                                        <p style=styles::MONO>{format!("head {head}")}</p>
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
                    view! { <ErrorAlert message=error.to_string() on_retry={std::rc::Rc::new(retry) as std::rc::Rc<dyn Fn()>} /> }.into_any()
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
                        <table style=styles::TABLE>
                            <thead>
                                <tr>
                                    <th style=styles::TH>"Time"</th>
                                    <th style=styles::TH>"Action"</th>
                                    <th style=styles::TH>"Actor"</th>
                                    <th style=styles::TH>"Target"</th>
                                    <th style=styles::TH>"Decision"</th>
                                    <th style=styles::TH>"Payload"</th>
                                </tr>
                            </thead>
                            <tbody>
                                <For each=move || rows.clone() key=|item| item.event.event_id.to_string() let:item>
                                    <AuditRow item=item />
                                </For>
                            </tbody>
                        </table>
                        {page
                            .has_more
                            .then(|| {
                                view! {
                                    <p style=styles::MUTED>
                                        "More events match — refine the filter."
                                    </p>
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
        <tr>
            <td style=styles::TD>{time}</td>
            <td style=styles::MONO>{event.action.clone()}</td>
            <td style=styles::MONO>{actor}</td>
            <td style=styles::TD>
                {format!("{} {}", event.target_kind, event.target_id.chars().take(8).collect::<String>())}
            </td>
            <td style=styles::TD>{decision}</td>
            <td style=styles::MONO>{payload}</td>
        </tr>
    }
}
