// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Agents list + detail pages (AGENT-01).
//!
//! The Agents area had no UI at all before this task: `/agents` was a
//! `ComingSoon` placeholder and `/agents/:id` was a 404, while the
//! backend (create, list, get, transitions, tool calls, handoffs) is
//! complete and unchanged — every action below goes through the
//! existing endpoints in `crates/crm-api/src/jev.rs`.
//!
//! No seed change was needed: the demo user already holds
//! `AgentRun` Create/Read/Update/Approve on the demo profile, and the
//! seed plants a completed `demo-triage` run, so it is visible on
//! first load with the profile selector defaulting to the demo
//! profile.
//!
//! Lifecycle gating follows the domain state machine exactly:
//! request-approval from `Running`; approve/deny from
//! `AwaitingApproval`; complete from `Running` (with an output
//! note); fail from `Running` or `AwaitingApproval`; cancel from any
//! non-terminal state. `Completed`, `Failed`, and `Cancelled` are
//! terminal and offer no actions.
//!
//! Deliberately out of scope: recording tool calls or handoffs from
//! the UI. Both lists render only; recording stays with runs and
//! the API.
//!
//! The list response carries everything the table needs, so —
//! unlike FORM-01's field counts — there is no per-row detail fetch
//! here and no N+1.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::jev::{AgentRun, AgentRunStatus, Handoff, ToolCall, ToolCallStatus};

use crate::api::{
    create_agent_run, fetch_agent_run, fetch_agent_runs, fetch_handoffs, fetch_profiles,
    fetch_tool_calls, transition_agent_run, AgentRunListResponse, ApiError, CreateAgentRunRequest,
    DEMO_PROFILE_ID_STR,
};
use crate::components::{
    CountLine, EmptyState, ErrorAlert, FieldRow, Loading, OkAlert, PageHeader,
};
use crate::ui::{
    push_toast, Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardHeader,
    CardTitle, Input as TextInput, Label, Select, Table, TableBody, TableCell, TableHead,
    TableHeader, TableRow, ToastVariant, Toasts, UiHandler, UiValueHandler,
};

/// Maps an agent-run lifecycle status to its badge variant.
fn run_status_badge(status: AgentRunStatus) -> BadgeVariant {
    match status {
        AgentRunStatus::Pending => BadgeVariant::Secondary,
        AgentRunStatus::Running => BadgeVariant::Info,
        AgentRunStatus::AwaitingApproval => BadgeVariant::Warning,
        AgentRunStatus::Completed => BadgeVariant::Success,
        AgentRunStatus::Failed => BadgeVariant::Destructive,
        AgentRunStatus::Cancelled => BadgeVariant::Secondary,
    }
}

/// Maps a tool-call lifecycle status to its badge variant.
fn tool_call_status_badge(status: ToolCallStatus) -> BadgeVariant {
    match status {
        ToolCallStatus::Pending => BadgeVariant::Secondary,
        ToolCallStatus::Approved => BadgeVariant::Info,
        ToolCallStatus::Denied => BadgeVariant::Destructive,
        ToolCallStatus::Executed => BadgeVariant::Success,
        ToolCallStatus::Failed => BadgeVariant::Destructive,
    }
}

/// Splits a comma-separated adapters input into non-empty trimmed
/// names. `"openrouter, local,, "` → `["openrouter", "local"]`.
fn parse_adapter_names(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Renders a run's input payload as its task label: the string
/// `"task"` field when present, else the compact JSON, truncated so
/// a long payload cannot stretch the table.
fn task_label(input: &serde_json::Value) -> String {
    if let Some(task) = input.get("task").and_then(|value| value.as_str()) {
        return task.to_string();
    }
    let rendered = input.to_string();
    if rendered.chars().count() > 64 {
        let truncated: String = rendered.chars().take(64).collect();
        format!("{truncated}…")
    } else {
        rendered
    }
}

/// Agents page: profile selector, run list, create run.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn AgentsPage(token: String) -> impl IntoView {
    // Scope selector value: a profile UUID. Defaults to the demo
    // profile so the seeded demo-triage run is visible on first load.
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
        <section aria-label="Agents">
            <PageHeader
                title="Agents"
                description="Jev agent runs: lifecycle, tool calls, and handoffs."
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
                                    <Label for_id="agents-profile">"Client profile"</Label>
                                    <Select
                                        id="agents-profile"
                                        options=options
                                        selected=selected
                                    />
                                </div>
                            </CardContent>
                        </Card>
                        <AgentBrowser token={token.clone()} selected=selected />
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Agent-run list + create run for the selected profile.
///
/// Extracted so the token prop is owned here instead of being threaded
/// through nested reactive closures (which would force `FnOnce`).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `selected`: the selected profile UUID.
#[component]
fn AgentBrowser(token: String, selected: RwSignal<String>) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    let runs = RwSignal::new(None::<Result<AgentRunListResponse, ApiError>>);
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
            runs.set(None);
            let my_generation = generation.get_untracked() + 1;
            generation.set(my_generation);
            let token = token.clone();
            spawn_local(async move {
                let result = fetch_agent_runs(&scope, &token).await;
                if generation.get_untracked() != my_generation {
                    return;
                }
                runs.set(Some(result));
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
                <AgentCreateForm
                    token={token.clone()}
                    profile_id=selected.get()
                    on_created=retry
                />
                {move || match runs.get() {
                    None => view! { <Loading label="agent runs" /> }.into_any(),
                    Some(Err(error)) => {
                        view! { <ErrorAlert message=error.friendly_message() on_retry=retry /> }
                            .into_any()
                    }
                    Some(Ok(page)) if page.agent_runs.is_empty() => {
                        view! { <EmptyState message="No agent runs yet." /> }.into_any()
                    }
                    Some(Ok(page)) => {
                        view! {
                            <CountLine count=page.total singular="agent run" plural="agent runs" />
                            <Card>
                                <CardContent>
                                    <Table>
                                        <TableHeader>
                                            <TableRow>
                                                <TableHead>"Agent"</TableHead>
                                                <TableHead>"Status"</TableHead>
                                                <TableHead>"Task"</TableHead>
                                                <TableHead>"Created"</TableHead>
                                            </TableRow>
                                        </TableHeader>
                                        <TableBody>
                                            <For
                                                each=move || page.agent_runs.clone()
                                                key=|r| r.run_id.to_string()
                                                children=move |r| {
                                                    // Per-row strings up front: the
                                                    // `view!` below captures its environment by
                                                    // move, so nothing may be borrowed inside
                                                    // it on every row.
                                                    let id = r.run_id.to_string();
                                                    let href = format!("/agents/{id}");
                                                    let status = r.status;
                                                    let task = task_label(&r.input);
                                                    let kind = r.agent_kind.clone();
                                                    let created = r
                                                        .created_at
                                                        .format("%Y-%m-%d %H:%M")
                                                        .to_string();
                                                    view! {
                                                        <TableRow>
                                                            <TableCell>
                                                                <A
                                                                    href=href
                                                                    attr:style="font-weight: 600; font-size: 0.875rem; \
                                                                                color: var(--foreground); text-decoration: none;"
                                                                >
                                                                    {kind}
                                                                </A>
                                                            </TableCell>
                                                            <TableCell>
                                                                <Badge variant=run_status_badge(status)>
                                                                    {format!("{status:?}")}
                                                                </Badge>
                                                            </TableCell>
                                                            <TableCell>{task}</TableCell>
                                                            <TableCell>{created}</TableCell>
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

/// Create-run form for one client profile.
///
/// A failed create keeps the draft: the fields are cleared only on
/// success, so a validation error never eats what was typed. The
/// task text becomes the run input as `{"task": ...}`, mirroring
/// the seeded demo run's input shape; adapters are comma-separated
/// and the first is the primary.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the owning profile (UUID string).
/// - `on_created`: invoked after a successful create so the list reloads.
#[component]
fn AgentCreateForm(token: String, profile_id: String, on_created: UiHandler) -> impl IntoView {
    let agent_kind = RwSignal::new(String::new());
    let adapters = RwSignal::new(String::new());
    let task = RwSignal::new(String::new());
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
            let kind_value = agent_kind.get().trim().to_string();
            if kind_value.is_empty() {
                status.set(Some(Err("Enter an agent kind.".to_string())));
                return;
            }
            let adapter_values = parse_adapter_names(&adapters.get());
            if adapter_values.is_empty() {
                status.set(Some(Err(
                    "Enter at least one adapter (comma-separated).".to_string()
                )));
                return;
            }
            let task_value = task.get().trim().to_string();
            if task_value.is_empty() {
                status.set(Some(Err("Enter a task.".to_string())));
                return;
            }
            let request = CreateAgentRunRequest {
                client_profile_id: profile_id.clone(),
                agent_kind: kind_value,
                adapter_names: adapter_values,
                input: serde_json::json!({ "task": task_value }),
            };
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match create_agent_run(&request, &token).await {
                    Ok(created) => {
                        status.set(Some(Ok(format!("Run \"{}\" created.", created.agent_kind))));
                        agent_kind.set(String::new());
                        adapters.set(String::new());
                        task.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Run created",
                                Some(created.agent_kind.clone()),
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
                <CardTitle>"New run"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="agents-kind">"Agent kind"</Label>
                        <TextInput
                            id="agents-kind"
                            placeholder="triage"
                            value=agent_kind
                            on_input=UiValueHandler::new(move |v| agent_kind.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="agents-adapters">"Adapters (comma-separated)"</Label>
                        <TextInput
                            id="agents-adapters"
                            placeholder="openrouter, local"
                            value=adapters
                            on_input=UiValueHandler::new(move |v| adapters.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="agents-task">"Task"</Label>
                        <TextInput
                            id="agents-task"
                            placeholder="Triage the inbox"
                            value=task
                            on_input=UiValueHandler::new(move |v| task.set(v))
                        />
                    </div>
                    <p style="margin: 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        "The first adapter is the primary; the rest are the fallback chain. The server starts the run, so a created run is Running."
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
                            "Create run"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Agent-run detail page: full record, tool calls, handoffs, and
/// lifecycle actions.
///
/// A fetch or transition failure never wipes the record: the error is
/// shown as an alert above the content, and the loaded record stays
/// visible underneath (the AUDIT-08 `ProfileDetail` pattern — a failed
/// transition used to replace the whole page with the error alert).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `run_id`: the run UUID from the route.
#[component]
pub fn AgentRunDetail(token: String, run_id: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    // The loaded record, kept across fetch/transition failures.
    let run = RwSignal::new(None::<AgentRun>);
    let tool_calls = RwSignal::new(Vec::<ToolCall>::new());
    let handoffs = RwSignal::new(Vec::<Handoff>::new());
    // True while a fetch is in flight; only the initial load shows a
    // spinner — a reload keeps the previous record on screen.
    let loading = RwSignal::new(true);
    // The last page-level failure (fetch or transition), if any.
    let error = RwSignal::new(Option::<String>::None);
    let working = RwSignal::new(false);
    // Completion-note draft for the complete action.
    let output_note = RwSignal::new(String::new());
    {
        let token = token.clone();
        let run_id = run_id.clone();
        Effect::new(move |_| {
            reload.get();
            loading.set(true);
            let token = token.clone();
            let run_id = run_id.clone();
            spawn_local(async move {
                match fetch_agent_run(&run_id, &token).await {
                    Ok(detail) => {
                        run.set(Some(detail.run));
                        error.set(None);
                        match fetch_tool_calls(&run_id, &token).await {
                            Ok(page) => tool_calls.set(page.tool_calls),
                            Err(e) => error.set(Some(e.friendly_message())),
                        }
                        match fetch_handoffs(&run_id, &token).await {
                            Ok(page) => handoffs.set(page.handoffs),
                            Err(e) => error.set(Some(e.friendly_message())),
                        }
                    }
                    // Friendly message, not the raw server text: the
                    // user needs "We couldn't find that record.", not
                    // "server error 404: Not found — …".
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
        let run_id = run_id.clone();
        move |name: &'static str| {
            // Guard against double-clicks racing the async work: the buttons
            // are disabled via `working`, but check synchronously here too so
            // a second invocation can never start a duplicate transition.
            if working.get() {
                return;
            }
            let token = token.clone();
            let run_id = run_id.clone();
            working.set(true);
            spawn_local(async move {
                match transition_agent_run(&run_id, name, None, &token).await {
                    Ok(updated) => {
                        let verb = match name {
                            "request-approval" => "approval requested",
                            "approve" => "approved",
                            "deny" => "denied",
                            "fail" => "failed",
                            "cancel" => "cancelled",
                            _ => "updated",
                        };
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                format!("Run {verb}"),
                                Some(updated.agent_kind.clone()),
                                ToastVariant::Success,
                            );
                        }
                        run.set(Some(updated));
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

    let complete = {
        let token = token.clone();
        let run_id = run_id.clone();
        let note = output_note;
        move || {
            if working.get() {
                return;
            }
            let token = token.clone();
            let run_id = run_id.clone();
            let output = serde_json::json!({ "note": note.get() });
            working.set(true);
            spawn_local(async move {
                match transition_agent_run(&run_id, "complete", Some(output), &token).await {
                    Ok(updated) => {
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Run completed",
                                Some(updated.agent_kind.clone()),
                                ToastVariant::Success,
                            );
                        }
                        run.set(Some(updated));
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.friendly_message())),
                }
                working.set(false);
            });
        }
    };

    view! {
        <section aria-label="Agent run detail">
            <PageHeader title="Agent run" description="Lifecycle, tool calls, and handoffs.">
                <A
                    href="/agents"
                    attr:style="display: inline-flex; align-items: center; height: 2.5rem; \
                                padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                border: 1px solid var(--input); background: transparent; \
                                color: var(--foreground);"
                >
                    "← Back to agents"
                </A>
            </PageHeader>
            {move || error.get().map(|message| view! {
                <ErrorAlert message=message on_retry=retry />
            })}
            {move || match run.get() {
                Some(r) => {
                    let status = r.status;
                    let is_running = status == AgentRunStatus::Running;
                    let is_awaiting = status == AgentRunStatus::AwaitingApproval;
                    let is_terminal = status.is_terminal();
                    let can_act = !working.get() && !is_terminal;
                    // Clone the transition handlers into locals: the
                    // button `on_click` closures below are `move`, so
                    // using the component-body closures directly would
                    // move those captures out of this render closure and
                    // make it FnOnce (E0525). Locals are fresh per render.
                    let request_approval = transition.clone();
                    let approve = transition.clone();
                    let deny = transition.clone();
                    let fail = transition.clone();
                    let cancel = transition.clone();
                    let complete_run = complete.clone();
                    // Hoist every displayed string before the `view!`:
                    // the `For` children closures below are stored
                    // closures, so borrowing `r` inside them would move
                    // it (E0382).
                    let run_id_value = r.run_id.to_string();
                    let profile_id_value = r.client_profile_id.to_string();
                    let kind_value = r.agent_kind.clone();
                    let adapter_value = r.adapter_name.clone();
                    let chain_value = r.fallback_chain.adapters().join(", ");
                    let input_value = r.input.to_string();
                    let output_value = r
                        .output
                        .as_ref()
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "—".to_string());
                    let gate_text = match &r.approval_gate {
                        Some(gate) => {
                            let decision = gate
                                .decision
                                .map(|d| format!("{d:?}"))
                                .unwrap_or_else(|| "Pending".to_string());
                            let mut text = decision;
                            if let Some(reason) = &gate.reason {
                                text.push_str(" — ");
                                text.push_str(reason);
                            }
                            if let Some(by) = &gate.decided_by {
                                text.push_str(&format!(" (decided by {by})"));
                            }
                            if let Some(at) = &gate.decided_at {
                                text.push_str(&format!(
                                    " at {}",
                                    at.format("%Y-%m-%d %H:%M")
                                ));
                            }
                            text
                        }
                        None => "No decision recorded.".to_string(),
                    };
                    let created_value = r.created_at.format("%Y-%m-%d %H:%M").to_string();
                    let updated_value = r.updated_at.format("%Y-%m-%d %H:%M").to_string();
                    let tool_calls_list = tool_calls.get();
                    let tool_call_count = tool_calls_list.len();
                    let handoffs_list = handoffs.get();
                    let handoff_count = handoffs_list.len();
                    view! {
                        <Card>
                            <CardHeader>
                                <div style="display: flex; align-items: center; gap: 0.625rem;">
                                    <CardTitle>{kind_value}</CardTitle>
                                    <Badge variant=run_status_badge(status)>
                                        {format!("{status:?}")}
                                    </Badge>
                                </div>
                            </CardHeader>
                            <CardContent>
                                <dl style="margin: 0 0 1rem;">
                                    <FieldRow label="Run id">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {run_id_value}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Client profile">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {profile_id_value}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Adapter">{adapter_value}</FieldRow>
                                    <FieldRow label="Fallback chain">{chain_value}</FieldRow>
                                    <FieldRow label="Input">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem; overflow-wrap: anywhere;">
                                            {input_value}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Output">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem; overflow-wrap: anywhere;">
                                            {output_value}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Approval gate">{gate_text}</FieldRow>
                                    <FieldRow label="Created">{created_value}</FieldRow>
                                    <FieldRow label="Updated">{updated_value}</FieldRow>
                                </dl>
                                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                                    <div>
                                        <Label for_id="run-output-note">"Completion note"</Label>
                                        <TextInput
                                            id="run-output-note"
                                            placeholder="What the run produced"
                                            value=output_note
                                            on_input=UiValueHandler::new(move |v| {
                                                output_note.set(v)
                                            })
                                        />
                                    </div>
                                </div>
                                <div style="display: flex; gap: 0.5rem; flex-wrap: wrap; margin-top: 0.875rem;">
                                    <Button
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_running)
                                        on_click={UiHandler::new(move || {
                                            request_approval("request-approval")
                                        })}
                                    >
                                        "Request approval"
                                    </Button>
                                    <Button
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_awaiting)
                                        on_click={UiHandler::new(move || approve("approve"))}
                                    >
                                        "Approve"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Destructive
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_awaiting)
                                        on_click={UiHandler::new(move || deny("deny"))}
                                    >
                                        "Deny"
                                    </Button>
                                    <Button
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act || !is_running)
                                        on_click={UiHandler::new(complete_run)}
                                    >
                                        "Complete"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Destructive
                                        btn_type="button"
                                        disabled=Signal::derive(move || {
                                            !can_act || !(is_running || is_awaiting)
                                        })
                                        on_click={UiHandler::new(move || fail("fail"))}
                                    >
                                        "Fail"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Destructive
                                        btn_type="button"
                                        disabled=Signal::derive(move || !can_act)
                                        on_click={UiHandler::new(move || cancel("cancel"))}
                                    >
                                        "Cancel"
                                    </Button>
                                </div>
                                <p style="margin: 0.75rem 0 0; font-size: 0.75rem; color: var(--muted-foreground);">
                                    "Request approval pauses a Running run for a human decision. Approve resumes it; Deny fails it. Complete needs a Running run and records the note above as its output. Fail works from Running or Awaiting approval. Cancel works from any non-terminal state. Completed, Failed, and Cancelled are terminal."
                                </p>
                            </CardContent>
                        </Card>
                        <Card>
                            <CardHeader>
                                <CardTitle>"Tool calls"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                <p style="margin: 0 0 1rem; font-size: 0.875rem; color: var(--muted-foreground);">
                                    {format!(
                                        "{tool_call_count} tool calls, oldest first. Tool calls are listed here only — recording them from this page is out of scope for AGENT-01."
                                    )}
                                </p>
                                {if tool_calls_list.is_empty() {
                                    view! {
                                        <EmptyState message="No tool calls yet." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <Table>
                                            <TableHeader>
                                                <TableRow>
                                                    <TableHead>"Tool"</TableHead>
                                                    <TableHead>"Status"</TableHead>
                                                    <TableHead>"Needs approval"</TableHead>
                                                    <TableHead>"Input"</TableHead>
                                                    <TableHead>"Output"</TableHead>
                                                    <TableHead>"Created"</TableHead>
                                                </TableRow>
                                            </TableHeader>
                                            <TableBody>
                                                <For
                                                    each=move || tool_calls_list.clone()
                                                    key=|call| call.call_id.to_string()
                                                    children=move |call| {
                                                        // Per-row strings up front: the
                                                        // `view!` below captures its environment by
                                                        // move, so nothing may be borrowed inside
                                                        // it on every row.
                                                        let tool_name = call.tool_name.clone();
                                                        let status = call.status;
                                                        let needs_approval = if call
                                                            .requires_approval
                                                        {
                                                            "Yes".to_string()
                                                        } else {
                                                            "—".to_string()
                                                        };
                                                        let input_text = call.input.to_string();
                                                        let output_text = call
                                                            .output
                                                            .as_ref()
                                                            .map(|value| value.to_string())
                                                            .unwrap_or_else(|| "—".to_string());
                                                        let created_text = call
                                                            .created_at
                                                            .format("%Y-%m-%d %H:%M")
                                                            .to_string();
                                                        view! {
                                                            <TableRow>
                                                                <TableCell>{tool_name}</TableCell>
                                                                <TableCell>
                                                                    <Badge variant=tool_call_status_badge(status)>
                                                                        {format!("{status:?}")}
                                                                    </Badge>
                                                                </TableCell>
                                                                <TableCell>{needs_approval}</TableCell>
                                                                <TableCell>
                                                                    <span style="font-family: var(--font-mono); font-size: 0.8125rem; overflow-wrap: anywhere;">
                                                                        {input_text}
                                                                    </span>
                                                                </TableCell>
                                                                <TableCell>
                                                                    <span style="font-family: var(--font-mono); font-size: 0.8125rem; overflow-wrap: anywhere;">
                                                                        {output_text}
                                                                    </span>
                                                                </TableCell>
                                                                <TableCell>{created_text}</TableCell>
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
                        <Card>
                            <CardHeader>
                                <CardTitle>"Handoffs"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                <p style="margin: 0 0 1rem; font-size: 0.875rem; color: var(--muted-foreground);">
                                    {format!(
                                        "{handoff_count} handoffs, newest first. Handoffs are listed here only — recording them from this page is out of scope for AGENT-01."
                                    )}
                                </p>
                                {if handoffs_list.is_empty() {
                                    view! {
                                        <EmptyState message="No handoffs yet." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <Table>
                                            <TableHeader>
                                                <TableRow>
                                                    <TableHead>"To agent"</TableHead>
                                                    <TableHead>"Context"</TableHead>
                                                    <TableHead>"Created"</TableHead>
                                                </TableRow>
                                            </TableHeader>
                                            <TableBody>
                                                <For
                                                    each=move || handoffs_list.clone()
                                                    key=|h| h.handoff_id.to_string()
                                                    children=move |h| {
                                                        let to_agent = h.to_agent_kind.clone();
                                                        let context_text = h.context.to_string();
                                                        let created_text = h
                                                            .created_at
                                                            .format("%Y-%m-%d %H:%M")
                                                            .to_string();
                                                        view! {
                                                            <TableRow>
                                                                <TableCell>{to_agent}</TableCell>
                                                                <TableCell>
                                                                    <span style="font-family: var(--font-mono); font-size: 0.8125rem; overflow-wrap: anywhere;">
                                                                        {context_text}
                                                                    </span>
                                                                </TableCell>
                                                                <TableCell>{created_text}</TableCell>
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
                    }
                        .into_any()
                }
                None => {
                    if loading.get() {
                        view! { <Loading label="agent run" /> }.into_any()
                    } else {
                        view! {
                            <EmptyState message="Agent run not found — it may have been deleted, or the link may be wrong." />
                        }
                            .into_any()
                    }
                }
            }}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_adapter_names_splits_and_trims() {
        assert_eq!(
            parse_adapter_names("openrouter, local"),
            vec!["openrouter".to_string(), "local".to_string()]
        );
    }

    #[test]
    fn parse_adapter_names_drops_blanks() {
        assert_eq!(
            parse_adapter_names("openrouter,, local, "),
            vec!["openrouter".to_string(), "local".to_string()]
        );
    }

    #[test]
    fn parse_adapter_names_empty_is_empty() {
        assert_eq!(parse_adapter_names(""), Vec::<String>::new());
        assert_eq!(parse_adapter_names("  ,  "), Vec::<String>::new());
    }

    #[test]
    fn task_label_prefers_the_task_field() {
        let input = serde_json::json!({ "task": "triage the inbox", "other": 1 });
        assert_eq!(task_label(&input), "triage the inbox");
    }

    #[test]
    fn task_label_falls_back_to_compact_json() {
        let input = serde_json::json!({ "done": true });
        assert_eq!(task_label(&input), "{\"done\":true}");
    }

    #[test]
    fn task_label_truncates_long_payloads() {
        let long = "x".repeat(100);
        let input = serde_json::json!({ "blob": long });
        let label = task_label(&input);
        assert!(label.chars().count() <= 65);
        assert!(label.ends_with('…'));
    }
}
