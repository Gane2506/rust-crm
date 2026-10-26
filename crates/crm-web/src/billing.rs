// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Billing list + detail pages (BILL-01).
//!
//! `/billing` was a `ComingSoon` placeholder while the backend is
//! complete — invoices, lines, issue/void, balances, recorded payments
//! and reversals all exist in `crates/crm-api/src/billing.rs`. This
//! module is the frontend only; the backend is unchanged.
//!
//! Billing here is **record-keeping**, not a payment processor: the
//! domain documents that no charge is executed anywhere (a
//! Stripe/processor adapter is a later workstream's concern), so
//! payments are recorded against invoices, never charged. The UI
//! presents them that way and adds no Stripe copy or charge flow.
//!
//! Three API gaps shape the page (flagged, not papered over):
//! - Invoices carry no human-readable number, so the table shows the
//!   short invoice id (first 8 characters).
//! - Invoices carry no issued date (`created_at`/`updated_at` only),
//!   so the table shows the created date.
//! - The list response carries no balance, so the list fetches each
//!   invoice's balance (`GET /invoices/{id}/balance`) to fill the
//!   Balance column. That is one extra request per row, bounded by the
//!   page size (20); a `balance` field on the list response would
//!   remove it.
//!
//! Invoices are profile-scoped and the list endpoint requires a
//! profile, so there is no agency-wide list: the page selects one
//! profile (defaulting to the demo profile) and lists its invoices.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crm_domain::billing::{Invoice, InvoiceStatus, Money, Payment, PaymentStatus};

use crate::api::{
    add_invoice_line, create_invoice, fetch_invoice, fetch_invoice_balance, fetch_invoices,
    fetch_payments, fetch_profiles, record_payment, reverse_payment, transition_invoice,
    AddInvoiceLineRequest, ApiError, CreateInvoiceRequest, InvoiceListResponse,
    RecordPaymentRequest, DEMO_PROFILE_ID_STR,
};
use crate::components::{
    CountLine, EmptyState, ErrorAlert, FieldRow, Loading, OkAlert, PageHeader,
};
use crate::ui::{
    push_toast, Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardHeader,
    CardTitle, Input as TextInput, Label, Select, Table, TableBody, TableCell, TableHead,
    TableHeader, TableRow, ToastVariant, Toasts, UiHandler, UiValueHandler,
};

/// Maps an invoice lifecycle state to its badge variant.
fn invoice_status_badge(status: &InvoiceStatus) -> BadgeVariant {
    match status {
        InvoiceStatus::Draft => BadgeVariant::Secondary,
        InvoiceStatus::Issued => BadgeVariant::Info,
        InvoiceStatus::Paid => BadgeVariant::Success,
        InvoiceStatus::Void => BadgeVariant::Destructive,
    }
}

/// Maps a payment record state to its badge variant.
fn payment_status_badge(status: &PaymentStatus) -> BadgeVariant {
    match status {
        PaymentStatus::Recorded => BadgeVariant::Success,
        PaymentStatus::Reversed => BadgeVariant::Secondary,
    }
}

/// Shortens a UUID string to its first 8 characters for table display.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Formats money as `"USD 100.00"`. Amounts are stored in minor units
/// (cents), never negative.
fn format_money(money: &Money) -> String {
    format!(
        "{} {}.{:02}",
        money.currency(),
        money.amount() / 100,
        money.amount() % 100
    )
}

/// Parses a decimal amount in major units (`"100"`, `"100.5"`,
/// `"100.50"`) into minor units. Rejects negatives, more than two
/// decimal places, and anything that is not a number.
fn parse_major_units(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('-') {
        return None;
    }
    let (whole, frac) = match text.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (text, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if frac.len() > 2 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut minor: i64 = whole.parse().ok()?;
    minor = minor.checked_mul(100)?;
    let frac_minor: i64 = if frac.is_empty() {
        0
    } else {
        // "5" means 50 cents; "05" means 5.
        format!("{frac:0<2}").parse().ok()?
    };
    minor.checked_add(frac_minor)
}

/// Invoices page: profile selector, invoice list, create form.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn InvoicesPage(token: String) -> impl IntoView {
    // Scope selector value: a profile UUID. Invoices are
    // profile-scoped and the list endpoint requires a profile, so
    // there is no agency-wide list. Defaults to the demo profile.
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
        <section aria-label="Billing">
            <PageHeader
                title="Billing"
                description="Invoices and recorded payments for client profiles."
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
                                    <Label for_id="billing-profile">"Client profile"</Label>
                                    <Select
                                        id="billing-profile"
                                        options=options
                                        selected=selected
                                    />
                                </div>
                            </CardContent>
                        </Card>
                        <InvoiceBrowser token={token.clone()} selected=selected />
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Invoice list + create form for the selected profile.
///
/// Extracted so the token prop is owned here instead of being threaded
/// through nested reactive closures (which would force `FnOnce`).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `selected`: the selected profile UUID.
#[component]
fn InvoiceBrowser(token: String, selected: RwSignal<String>) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    let invoices = RwSignal::new(None::<Result<InvoiceListResponse, ApiError>>);
    // Balances keyed by invoice id. The list response carries no
    // balance, so each row's balance is fetched once (bounded by the
    // page size, 20) and cached here.
    let balances = RwSignal::new(HashMap::<String, Money>::new());
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
            invoices.set(None);
            balances.set(HashMap::new());
            let my_generation = generation.get_untracked() + 1;
            generation.set(my_generation);
            let token = token.clone();
            spawn_local(async move {
                let result = fetch_invoices(&scope, &token).await;
                if generation.get_untracked() != my_generation {
                    return;
                }
                if let Ok(page) = &result {
                    // Fill the Balance column per invoice.
                    for invoice in &page.invoices {
                        let invoice_id = invoice.invoice_id.to_string();
                        let token = token.clone();
                        spawn_local(async move {
                            if let Ok(balance) = fetch_invoice_balance(&invoice_id, &token).await {
                                if generation.get_untracked() == my_generation {
                                    balances.update(|map| {
                                        map.insert(invoice_id, balance);
                                    });
                                }
                            }
                        });
                    }
                }
                invoices.set(Some(result));
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
                <InvoiceCreateForm
                    token={token.clone()}
                    profile_id=selected.get()
                    on_created=retry
                />
                {move || match invoices.get() {
                    None => view! { <Loading label="invoices" /> }.into_any(),
                    Some(Err(error)) => {
                        view! { <ErrorAlert message=error.friendly_message() on_retry=retry /> }
                            .into_any()
                    }
                    Some(Ok(page)) if page.invoices.is_empty() => {
                        view! { <EmptyState message="No invoices yet." /> }.into_any()
                    }
                    Some(Ok(page)) => {
                        view! {
                            <CountLine count=page.total singular="invoice" plural="invoices" />
                            <Card>
                                <CardContent>
                                    <Table>
                                        <TableHeader>
                                            <TableRow>
                                                <TableHead>"Invoice"</TableHead>
                                                <TableHead>"Client profile"</TableHead>
                                                <TableHead>"Status"</TableHead>
                                                <TableHead>"Total"</TableHead>
                                                <TableHead>"Balance"</TableHead>
                                                <TableHead>"Created"</TableHead>
                                            </TableRow>
                                        </TableHeader>
                                        <TableBody>
                                            <For
                                                each=move || page.invoices.clone()
                                                key=|i| i.invoice_id.to_string()
                                                children=move |i| {
                                                    let id = i.invoice_id.to_string();
                                                    // Compute the per-row strings up front: the
                                                    // `view!` below captures its environment by
                                                    // move, so the id and total cannot be
                                                    // borrowed inside it on every row.
                                                    let href = format!("/billing/{id}");
                                                    let total = format_money(&i.total());
                                                    let profile =
                                                        short_id(&i.client_profile_id.to_string());
                                                    let balance = {
                                                        let id = id.clone();
                                                        move || {
                                                            balances
                                                                .get()
                                                                .get(id.as_str())
                                                                .map(format_money)
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
                                                                    {short_id(&id)}
                                                                </A>
                                                            </TableCell>
                                                            <TableCell>{profile}</TableCell>
                                                            <TableCell>
                                                                <Badge variant=invoice_status_badge(&i.status)>
                                                                    {format!("{:?}", i.status)}
                                                                </Badge>
                                                            </TableCell>
                                                            <TableCell>{total}</TableCell>
                                                            <TableCell>
                                                                {move || match balance() {
                                                                    Some(amount) => amount,
                                                                    None => "…".to_string(),
                                                                }}
                                                            </TableCell>
                                                            <TableCell>
                                                                {i.created_at.format("%Y-%m-%d %H:%M").to_string()}
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

/// Create-invoice form for one client profile.
///
/// The invoice itself only needs a profile and a currency; the form
/// also accepts an optional first line (description, quantity, unit
/// price), which is added in a second call after the invoice is
/// created. If the line add fails, the invoice still exists — the
/// status says so, and lines can be added on the detail page.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `profile_id`: the owning profile (UUID string).
/// - `on_created`: invoked after a successful create so the list reloads.
#[component]
fn InvoiceCreateForm(token: String, profile_id: String, on_created: UiHandler) -> impl IntoView {
    let currency = RwSignal::new("USD".to_string());
    let line_description = RwSignal::new(String::new());
    // Blank by default: a prefilled quantity would make the optional
    // first line look "given" and force validation on every create.
    let line_quantity = RwSignal::new(String::new());
    let line_price = RwSignal::new(String::new());
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
            let currency_value = currency.get().trim().to_uppercase();
            if currency_value.len() != 3 || !currency_value.bytes().all(|b| b.is_ascii_uppercase())
            {
                status.set(Some(Err(
                    "Enter a 3-letter currency code, e.g. USD.".to_string()
                )));
                return;
            }
            // The first line is optional, but all-or-nothing: a half
            // filled line is a mistake, not a line. Quantity alone
            // does not count as "given" (it is blank by default); a
            // blank quantity on a given line means 1.
            let description_value = line_description.get().trim().to_string();
            let quantity_raw = line_quantity.get();
            let price_raw = line_price.get();
            let line_given = !description_value.is_empty() || !price_raw.trim().is_empty();
            let first_line = if !line_given {
                None
            } else {
                if description_value.is_empty() {
                    status.set(Some(Err(
                        "Enter a description for the first line.".to_string()
                    )));
                    return;
                }
                let quantity_value = if quantity_raw.trim().is_empty() {
                    1
                } else {
                    match quantity_raw.trim().parse::<u32>() {
                        Ok(n) if n > 0 => n,
                        _ => {
                            status.set(Some(Err(
                                "Quantity must be a whole number above 0.".to_string()
                            )));
                            return;
                        }
                    }
                };
                let price_minor = match parse_major_units(&price_raw) {
                    Some(minor) => minor,
                    None => {
                        status.set(Some(Err("Enter a unit price like 100.00.".to_string())));
                        return;
                    }
                };
                let unit_price = match Money::new(price_minor, currency_value.clone()) {
                    Ok(money) => money,
                    Err(_) => {
                        status.set(Some(Err("Enter a unit price like 100.00.".to_string())));
                        return;
                    }
                };
                Some(AddInvoiceLineRequest {
                    description: description_value,
                    quantity: quantity_value,
                    unit_price,
                })
            };
            let request = CreateInvoiceRequest {
                client_profile_id: profile_id.clone(),
                currency: currency_value,
            };
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match create_invoice(&request, &token).await {
                    Ok(created) => {
                        let invoice_id = created.invoice_id.to_string();
                        // Optional first line, second call.
                        let mut line_error = None;
                        if let Some(line) = first_line {
                            if let Err(e) = add_invoice_line(&invoice_id, &line, &token).await {
                                line_error = Some(e.friendly_message());
                            }
                        }
                        match line_error {
                            None => {
                                status.set(Some(Ok("Invoice created.".to_string())));
                                if let Some(toasts) = toasts {
                                    push_toast(
                                        toasts,
                                        "Invoice created",
                                        Some(format_money(&created.total())),
                                        ToastVariant::Success,
                                    );
                                }
                            }
                            // The invoice exists; only the line failed.
                            // Say so plainly and let the list reload so
                            // the draft is visible for line entry.
                            Some(message) => {
                                status.set(Some(Err(format!(
                                    "Invoice created, but the first line could not be added: {message}"
                                ))));
                            }
                        }
                        line_description.set(String::new());
                        line_quantity.set(String::new());
                        line_price.set(String::new());
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
                <CardTitle>"New invoice"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="invoice-currency">"Currency"</Label>
                        <TextInput
                            id="invoice-currency"
                            placeholder="USD"
                            value=currency
                            on_input=UiValueHandler::new(move |v| currency.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="invoice-line-description">"First line (optional)"</Label>
                        <TextInput
                            id="invoice-line-description"
                            placeholder="Monthly retainer"
                            value=line_description
                            on_input=UiValueHandler::new(move |v| line_description.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="invoice-line-quantity">"Quantity"</Label>
                        <TextInput
                            id="invoice-line-quantity"
                            input_type="number"
                            placeholder="1"
                            value=line_quantity
                            on_input=UiValueHandler::new(move |v| line_quantity.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="invoice-line-price">"Unit price"</Label>
                        <TextInput
                            id="invoice-line-price"
                            placeholder="100.00"
                            value=line_price
                            on_input=UiValueHandler::new(move |v| line_price.set(v))
                        />
                    </div>
                    <p style="margin: 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        "Amounts are in the invoice currency. More lines can be added on the invoice page."
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
                            "Create invoice"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Invoice detail page: full record, lines, balance, payments, and
/// lifecycle actions.
///
/// A fetch or transition failure never wipes the record: the error is
/// shown as an alert above the content, and the loaded record stays
/// visible underneath (the AUDIT-08 `ProfileDetail` pattern — a failed
/// transition used to replace the whole page with the error alert).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `invoice_id`: the invoice UUID from the route.
#[component]
pub fn InvoiceDetail(token: String, invoice_id: String) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    // The loaded record, kept across fetch/transition failures.
    let invoice = RwSignal::new(None::<Invoice>);
    // Amount still owed; `None` while it is loading or failed.
    let balance = RwSignal::new(None::<Money>);
    // The invoice's payments, oldest first.
    let payments = RwSignal::new(Vec::<Payment>::new());
    // True while a fetch is in flight; only the initial load shows a
    // spinner — a reload keeps the previous record on screen.
    let loading = RwSignal::new(true);
    // The last page-level failure (fetch or transition), if any.
    let error = RwSignal::new(Option::<String>::None);
    let working = RwSignal::new(false);
    {
        let token = token.clone();
        let invoice_id = invoice_id.clone();
        Effect::new(move |_| {
            reload.get();
            loading.set(true);
            let token = token.clone();
            let invoice_id = invoice_id.clone();
            spawn_local(async move {
                match fetch_invoice(&invoice_id, &token).await {
                    Ok(inv) => {
                        error.set(None);
                        let id = inv.invoice_id.to_string();
                        match fetch_invoice_balance(&id, &token).await {
                            Ok(amount) => balance.set(Some(amount)),
                            Err(e) => error.set(Some(e.friendly_message())),
                        }
                        match fetch_payments(&id, &token).await {
                            Ok(page) => payments.set(page.payments),
                            Err(e) => error.set(Some(e.friendly_message())),
                        }
                        invoice.set(Some(inv));
                    }
                    // Friendly message, not the raw server text: the
                    // user needs "You don't have access to do that.",
                    // not "server error 403: Forbidden — …".
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
        let invoice_id = invoice_id.clone();
        move |name: &'static str| {
            // Guard against double-clicks racing the async work: the buttons
            // are disabled via `working`, but check synchronously here too so
            // a second invocation can never start a duplicate transition.
            if working.get() {
                return;
            }
            let token = token.clone();
            let invoice_id = invoice_id.clone();
            working.set(true);
            spawn_local(async move {
                match transition_invoice(&invoice_id, name, &token).await {
                    Ok(updated) => {
                        let verb = match name {
                            "issue" => "issued",
                            "void" => "voided",
                            _ => "updated",
                        };
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                format!("Invoice {verb}"),
                                Some(format_money(&updated.total())),
                                ToastVariant::Success,
                            );
                        }
                        invoice.set(Some(updated));
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

    // Reversing a payment changes the balance and can reopen a Paid
    // invoice to Issued, so a success reloads the whole detail.
    let reverse = {
        let token = token.clone();
        move |payment_id: String| {
            if working.get() {
                return;
            }
            let token = token.clone();
            working.set(true);
            spawn_local(async move {
                match reverse_payment(&payment_id, &token).await {
                    Ok(reversed) => {
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Payment reversed",
                                Some(format_money(&reversed.amount)),
                                ToastVariant::Success,
                            );
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

    view! {
        <section aria-label="Invoice detail">
            <PageHeader title="Invoice" description="Lines, balance, payments, and lifecycle.">
                <A
                    href="/billing"
                    attr:style="display: inline-flex; align-items: center; height: 2.5rem; \
                                padding: 0 1rem; border-radius: var(--radius-md); \
                                font-size: 0.875rem; font-weight: 500; text-decoration: none; \
                                border: 1px solid var(--input); background: transparent; \
                                color: var(--foreground);"
                >
                    "← Back to billing"
                </A>
            </PageHeader>
            {move || error.get().map(|message| view! {
                <ErrorAlert message=message on_retry=retry />
            })}
            {move || match invoice.get() {
                Some(inv) => {
                    let issue = transition.clone();
                    let void = transition.clone();
                    // Clone per render: the payments table's `For`
                    // children closure below captures its environment
                    // by move, so it must take this fresh clone — the
                    // render closure itself must never move `reverse`.
                    let reverse_handler = reverse.clone();
                    let status = inv.status;
                    let is_draft = status == InvoiceStatus::Draft;
                    let is_issued = status == InvoiceStatus::Issued;
                    let is_terminal =
                        matches!(status, InvoiceStatus::Paid | InvoiceStatus::Void);
                    let can_act = !working.get() && !is_terminal;
                    // Copy out before the view: a `Signal::derive(move
                    // || ...)` closure below must not move `inv`.
                    let has_lines = !inv.lines.is_empty();
                    let total = format_money(&inv.total());
                    let balance_text = match balance.get() {
                        Some(amount) => format_money(&amount),
                        None => "…".to_string(),
                    };
                    let balance_value = balance.get();
                    let lines = inv.lines.clone();
                    let payments_list = payments.get();
                    // One fresh clone per use: a `view!` child/prop is a
                    // `move` closure, so the first use of a `String`
                    // would move it out and the next use would fail
                    // (E0382). Each form and the header takes its own.
                    let currency_header = inv.currency.clone();
                    let line_token = token.clone();
                    let line_currency = inv.currency.clone();
                    let payment_token = token.clone();
                    let payment_currency = inv.currency.clone();
                    view! {
                        <Card>
                            <CardHeader>
                                <div style="display: flex; align-items: center; gap: 0.625rem;">
                                    <CardTitle>{short_id(&inv.invoice_id.to_string())}</CardTitle>
                                    <Badge variant=invoice_status_badge(&status)>
                                        {format!("{status:?}")}
                                    </Badge>
                                </div>
                            </CardHeader>
                            <CardContent>
                                <dl style="margin: 0 0 1rem;">
                                    <FieldRow label="Invoice id">
                                        <span style="font-family: var(--font-mono); font-size: 0.8125rem;">
                                            {inv.invoice_id.to_string()}
                                        </span>
                                    </FieldRow>
                                    <FieldRow label="Client profile">
                                        {short_id(&inv.client_profile_id.to_string())}
                                    </FieldRow>
                                    <FieldRow label="Currency">{currency_header}</FieldRow>
                                    <FieldRow label="Total">{total}</FieldRow>
                                    <FieldRow label="Balance due">{balance_text}</FieldRow>
                                    <FieldRow label="Created">
                                        {inv.created_at.format("%Y-%m-%d %H:%M").to_string()}
                                    </FieldRow>
                                    <FieldRow label="Updated">
                                        {inv.updated_at.format("%Y-%m-%d %H:%M").to_string()}
                                    </FieldRow>
                                </dl>
                                <div style="display: flex; gap: 0.5rem; flex-wrap: wrap;">
                                    <Button
                                        btn_type="button"
                                        disabled=Signal::derive(move || {
                                            !can_act || !is_draft || !has_lines
                                        })
                                        on_click={UiHandler::new({
                                            let issue = issue.clone();
                                            move || issue("issue")
                                        })}
                                    >
                                        "Issue"
                                    </Button>
                                    <Button
                                        variant=ButtonVariant::Destructive
                                        btn_type="button"
                                        disabled=Signal::derive(move || {
                                            !can_act || !(is_draft || is_issued)
                                        })
                                        on_click={UiHandler::new({
                                            let void = void.clone();
                                            move || void("void")
                                        })}
                                    >
                                        "Void"
                                    </Button>
                                </div>
                                <p style="margin: 0.75rem 0 0; font-size: 0.75rem; color: var(--muted-foreground);">
                                    "Paid and Void are terminal. An invoice with no lines cannot be issued, and a paid invoice cannot be voided."
                                </p>
                            </CardContent>
                        </Card>
                        <Card>
                            <CardHeader>
                                <CardTitle>"Lines"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                {if lines.is_empty() {
                                    view! {
                                        <EmptyState message="No lines yet — add the first line below." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <Table>
                                            <TableHeader>
                                                <TableRow>
                                                    <TableHead>"Description"</TableHead>
                                                    <TableHead>"Quantity"</TableHead>
                                                    <TableHead>"Unit price"</TableHead>
                                                    <TableHead>"Line total"</TableHead>
                                                </TableRow>
                                            </TableHeader>
                                            <TableBody>
                                                <For
                                                    each=move || lines.clone()
                                                    key=|l| l.line_id.to_string()
                                                    children=move |l| {
                                                        // Compute the money strings up front:
                                                        // the `view!` below captures its
                                                        // environment by move.
                                                        let unit = format_money(&l.unit_price);
                                                        let line_total =
                                                            format_money(&l.line_total());
                                                        view! {
                                                            <TableRow>
                                                                <TableCell>
                                                                    {l.description.clone()}
                                                                </TableCell>
                                                                <TableCell>
                                                                    {l.quantity.to_string()}
                                                                </TableCell>
                                                                <TableCell>{unit}</TableCell>
                                                                <TableCell>{line_total}</TableCell>
                                                            </TableRow>
                                                        }
                                                    }
                                                />
                                            </TableBody>
                                        </Table>
                                    }
                                        .into_any()
                                }}
                                {if is_draft {
                                    view! {
                                        <AddInvoiceLineForm
                                            token={line_token}
                                            invoice_id={inv.invoice_id.to_string()}
                                            currency={line_currency}
                                            on_added=retry
                                        />
                                    }
                                        .into_any()
                                } else {
                                    ().into_any()
                                }}
                            </CardContent>
                        </Card>
                        <Card>
                            <CardHeader>
                                <CardTitle>"Payments"</CardTitle>
                            </CardHeader>
                            <CardContent>
                                {if payments_list.is_empty() {
                                    view! {
                                        <EmptyState message="No payments recorded yet." />
                                    }
                                        .into_any()
                                } else {
                                    view! {
                                        <Table>
                                            <TableHeader>
                                                <TableRow>
                                                    <TableHead>"Amount"</TableHead>
                                                    <TableHead>"Status"</TableHead>
                                                    <TableHead>"Recorded"</TableHead>
                                                    <TableHead>"Actions"</TableHead>
                                                </TableRow>
                                            </TableHeader>
                                            <TableBody>
                                                <For
                                                    each=move || payments_list.clone()
                                                    key=|p| p.payment_id.to_string()
                                                    children=move |p| {
                                                        let amount = format_money(&p.amount);
                                                        let payment_id =
                                                            p.payment_id.to_string();
                                                        let is_recorded =
                                                            p.status == PaymentStatus::Recorded;
                                                        // Clone in the body, not in the `view!`:
                                                        // the on_click prop's `move` closure
                                                        // would move `reverse_handler` out of
                                                        // this `Fn` closure on the first row.
                                                        let reverse = reverse_handler.clone();
                                                        view! {
                                                            <TableRow>
                                                                <TableCell>{amount}</TableCell>
                                                                <TableCell>
                                                                    <Badge variant=payment_status_badge(&p.status)>
                                                                        {format!("{:?}", p.status)}
                                                                    </Badge>
                                                                </TableCell>
                                                                <TableCell>
                                                                    {p.recorded_at.format("%Y-%m-%d %H:%M").to_string()}
                                                                </TableCell>
                                                                <TableCell>
                                                                    {if is_recorded {
                                                                        view! {
                                                                            <Button
                                                                                variant=ButtonVariant::Secondary
                                                                                btn_type="button"
                                                                                disabled=Signal::derive(move || {
                                                                                    working.get()
                                                                                })
                                                                                on_click={UiHandler::new(move || {
                                                                                    reverse(
                                                                                        payment_id.clone(),
                                                                                    )
                                                                                })}
                                                                            >
                                                                                "Reverse"
                                                                            </Button>
                                                                        }
                                                                            .into_any()
                                                                    } else {
                                                                        ().into_any()
                                                                    }}
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
                                {if is_issued {
                                    match balance_value {
                                        Some(amount) => view! {
                                            <RecordPaymentForm
                                                token={payment_token}
                                                invoice_id={inv.invoice_id.to_string()}
                                                currency={payment_currency}
                                                balance=amount
                                                on_recorded=retry
                                            />
                                        }
                                            .into_any(),
                                        None => ().into_any(),
                                    }
                                } else {
                                    ().into_any()
                                }}
                            </CardContent>
                        </Card>
                    }
                        .into_any()
                }
                None => {
                    if loading.get() {
                        view! { <Loading label="invoice" /> }.into_any()
                    } else {
                        view! {
                            <EmptyState message="Invoice not found — it may have been deleted, or the link may be wrong." />
                        }
                            .into_any()
                    }
                }
            }}
        </section>
    }
}

/// Add-line form for a draft invoice. The server rejects lines once
/// the invoice leaves `Draft`, so this form only renders on drafts.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `invoice_id`: the invoice UUID.
/// - `currency`: the invoice's currency code (all lines share it).
/// - `on_added`: invoked after a successful add so the detail reloads.
#[component]
fn AddInvoiceLineForm(
    token: String,
    invoice_id: String,
    currency: String,
    on_added: UiHandler,
) -> impl IntoView {
    let description = RwSignal::new(String::new());
    let quantity = RwSignal::new("1".to_string());
    let unit_price = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let invoice_id = invoice_id.clone();
        let currency = currency.clone();
        move || {
            if working.get() {
                return;
            }
            let description_value = description.get().trim().to_string();
            if description_value.is_empty() {
                status.set(Some(Err("Enter a description.".to_string())));
                return;
            }
            let quantity_value = match quantity.get().trim().parse::<u32>() {
                Ok(n) if n > 0 => n,
                _ => {
                    status.set(Some(Err(
                        "Quantity must be a whole number above 0.".to_string()
                    )));
                    return;
                }
            };
            let price_minor = match parse_major_units(&unit_price.get()) {
                Some(minor) => minor,
                None => {
                    status.set(Some(Err("Enter a unit price like 100.00.".to_string())));
                    return;
                }
            };
            let unit_price_value = match Money::new(price_minor, currency.clone()) {
                Ok(money) => money,
                Err(_) => {
                    status.set(Some(Err("Enter a unit price like 100.00.".to_string())));
                    return;
                }
            };
            let request = AddInvoiceLineRequest {
                description: description_value,
                quantity: quantity_value,
                unit_price: unit_price_value,
            };
            let token = token.clone();
            let invoice_id = invoice_id.clone();
            working.set(true);
            spawn_local(async move {
                match add_invoice_line(&invoice_id, &request, &token).await {
                    Ok(line) => {
                        status.set(Some(Ok("Line added.".to_string())));
                        description.set(String::new());
                        quantity.set("1".to_string());
                        unit_price.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Line added",
                                Some(line.description.clone()),
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

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Add line"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="line-description">"Description"</Label>
                        <TextInput
                            id="line-description"
                            placeholder="Monthly retainer"
                            value=description
                            on_input=UiValueHandler::new(move |v| description.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="line-quantity">"Quantity"</Label>
                        <TextInput
                            id="line-quantity"
                            input_type="number"
                            value=quantity
                            on_input=UiValueHandler::new(move |v| quantity.set(v))
                        />
                    </div>
                    <div>
                        <Label for_id="line-unit-price">"Unit price"</Label>
                        <TextInput
                            id="line-unit-price"
                            placeholder="100.00"
                            value=unit_price
                            on_input=UiValueHandler::new(move |v| unit_price.set(v))
                        />
                    </div>
                    <p style="margin: 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        {format!("Amounts are in {currency}. Lines can only be added while the invoice is a draft.")}
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
                            "Add line"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}

/// Record-payment form for an issued invoice. The server requires
/// `Issued` state and rejects payments above the balance, so this form
/// only renders on issued invoices with a known balance, and validates
/// both client-side.
///
/// Payments are records, not charges: recording one notes money
/// already received; no charge is executed.
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `invoice_id`: the invoice UUID.
/// - `currency`: the invoice's currency code (payments must match it).
/// - `balance`: the amount still owed.
/// - `on_recorded`: invoked after a successful record so the detail reloads.
#[component]
fn RecordPaymentForm(
    token: String,
    invoice_id: String,
    currency: String,
    balance: Money,
    on_recorded: UiHandler,
) -> impl IntoView {
    let amount = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let working = RwSignal::new(false);
    let toasts = use_context::<Toasts>();

    let submit = {
        let token = token.clone();
        let invoice_id = invoice_id.clone();
        let currency = currency.clone();
        let balance = balance.clone();
        move || {
            if working.get() {
                return;
            }
            let amount_minor = match parse_major_units(&amount.get()) {
                Some(minor) if minor > 0 => minor,
                _ => {
                    status.set(Some(
                        Err("Enter an amount above 0, like 60.00.".to_string()),
                    ));
                    return;
                }
            };
            if amount_minor > balance.amount() {
                status.set(Some(Err(format!(
                    "Amount is above the balance due ({}).",
                    format_money(&balance)
                ))));
                return;
            }
            let payment = match Money::new(amount_minor, currency.clone()) {
                Ok(money) => money,
                Err(_) => {
                    status.set(Some(
                        Err("Enter an amount above 0, like 60.00.".to_string()),
                    ));
                    return;
                }
            };
            let request = RecordPaymentRequest { amount: payment };
            let token = token.clone();
            let invoice_id = invoice_id.clone();
            working.set(true);
            spawn_local(async move {
                match record_payment(&invoice_id, &request, &token).await {
                    Ok(recorded) => {
                        status.set(Some(Ok("Payment recorded.".to_string())));
                        amount.set(String::new());
                        if let Some(toasts) = toasts {
                            push_toast(
                                toasts,
                                "Payment recorded",
                                Some(format_money(&recorded.amount)),
                                ToastVariant::Success,
                            );
                        }
                        on_recorded.call();
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
                <CardTitle>"Record payment"</CardTitle>
            </CardHeader>
            <CardContent>
                <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                    <div>
                        <Label for_id="payment-amount">"Amount"</Label>
                        <TextInput
                            id="payment-amount"
                            placeholder="60.00"
                            value=amount
                            on_input=UiValueHandler::new(move |v| amount.set(v))
                        />
                    </div>
                    <p style="margin: 0; font-size: 0.75rem; color: var(--muted-foreground);">
                        {format!(
                            "Balance due: {}. A fully-covering payment marks the invoice Paid. This records money already received — no charge is made.",
                            format_money(&balance)
                        )}
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
                            "Record payment"
                        </Button>
                    </div>
                </div>
            </CardContent>
        </Card>
    }
}
