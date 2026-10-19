// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Asset pages: list + register metadata for one client profile.
//!
//! Reskinned on the v2 component library (UI2-02): shadcn-style table,
//! cards, badges, and buttons over the design-token system. Behavior is
//! unchanged from the original slice.
//!
//! Assets are profile-scoped (`Asset` grant in the profile). The page
//! first loads the agency's profiles into a selector, then lists the
//! selected profile's assets. Registration is metadata-only — the byte
//! store is out of scope, so the uploader supplies the SHA-256 content
//! address (e.g. computed by the agency's ingestion pipeline); uploads
//! are deduped by content hash within the profile.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crm_domain::asset::Asset;

use crate::api::{
    archive_asset, fetch_assets, fetch_profiles, register_asset, ApiError, AssetListResponse,
    RegisterAssetRequest,
};
use crate::components::{CountLine, EmptyState, ErrorAlert, Loading, OkAlert, PageHeader};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle,
    Input as TextInput, Label, Select, Table, TableBody, TableCell, TableHead, TableHeader,
    TableRow, UiHandler, UiValueHandler,
};

/// Maps an asset kind to its badge variant.
fn asset_badge(kind: &crm_domain::asset::AssetKind) -> BadgeVariant {
    use crm_domain::asset::AssetKind;
    match kind {
        AssetKind::Document => BadgeVariant::Info,
        AssetKind::Image => BadgeVariant::Success,
        AssetKind::Audio => BadgeVariant::Warning,
        AssetKind::Video => BadgeVariant::Info,
        AssetKind::Other => BadgeVariant::Secondary,
    }
}

/// Assets page: profile selector, asset list, register form.
///
/// # Props
/// - `token`: the in-memory bearer token.
#[component]
pub fn AssetsPage(token: String) -> impl IntoView {
    // Selected profile id (UUID string). Empty = none chosen yet.
    let selected = RwSignal::new(String::new());
    let profile_options = RwSignal::new(None::<Result<Vec<(String, String)>, ApiError>>);
    {
        let token = token.clone();
        Effect::new(move |_| {
            let token = token.clone();
            spawn_local(async move {
                match fetch_profiles(&token).await {
                    Ok(page) => profile_options.set(Some(Ok(page
                        .profiles
                        .into_iter()
                        .map(|p| (p.profile_id.to_string(), p.name))
                        .collect()))),
                    Err(error) => profile_options.set(Some(Err(error))),
                }
            });
        });
    }

    // Asset list for the selected profile lives in [`AssetBrowser`] below.

    view! {
        <section aria-label="Assets">
            <PageHeader
                title="Assets"
                description="Content-addressed files, scoped to a client profile."
            />
            {move || match profile_options.get() {
                None => view! { <Loading label="profiles" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() /> }.into_any()
                }
                Some(Ok(options)) if options.is_empty() => {
                    view! { <EmptyState message="Create a client profile first." /> }.into_any()
                }
                Some(Ok(mut options)) => {
                    options.insert(0, (String::new(), "— select —".to_string()));
                    view! {
                        <Card>
                            <CardContent>
                                <div style="max-width: 28rem; padding-top: 0.5rem;">
                                    <Label for_id="asset-profile">"Client profile"</Label>
                                    <Select
                                        id="asset-profile"
                                        options=options
                                        selected=selected
                                    />
                                </div>
                            </CardContent>
                        </Card>
                        <AssetBrowser token={token.clone()} selected=selected />
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// Asset register form + list for the selected profile.
///
/// Extracted so the token prop is owned here instead of being threaded
/// through nested reactive closures (which would force `FnOnce`).
///
/// # Props
/// - `token`: the in-memory bearer token.
/// - `selected`: the selected profile id signal.
#[component]
fn AssetBrowser(token: String, selected: RwSignal<String>) -> impl IntoView {
    let reload = RwSignal::new(0u32);
    let assets = RwSignal::new(None::<Result<AssetListResponse, ApiError>>);
    // Generation counter: each effect run bumps it; a response is applied
    // only if its generation is still current, so a slow earlier request
    // can never overwrite a newer one.
    let generation = RwSignal::new(0u64);
    {
        let token = token.clone();
        Effect::new(move |_| {
            reload.get();
            let profile_id = selected.get();
            // Clear the list when the profile changes (or a reload starts)
            // so stale rows never linger under the new selection.
            assets.set(None);
            if profile_id.is_empty() {
                return;
            }
            let my_generation = generation.get_untracked() + 1;
            generation.set(my_generation);
            let token = token.clone();
            spawn_local(async move {
                let result = fetch_assets(&profile_id, &token).await;
                // Drop stale responses: only the latest generation wins.
                if generation.get_untracked() == my_generation {
                    assets.set(Some(result));
                }
            });
        });
    }
    let retry = UiHandler::new(move || reload.update(|n| *n = n.wrapping_add(1)));

    view! {
        {move || {
            // Clone for the inner asset-list closure so the outer `move`
            // closure keeps its own owned copy (`FnMut`, not `FnOnce`).
            let list_token = token.clone();
            if selected.get().is_empty() {
                view! { <EmptyState message="Select a profile to see its assets." /> }
                    .into_any()
            } else {
                view! {
                    <AssetRegisterForm
                        token={token.clone()}
                        profile_id=selected.get()
                        on_registered=retry
                    />
                    {move || match assets.get() {
                        None => view! { <Loading label="assets" /> }.into_any(),
                        Some(Err(error)) => {
                            view! { <ErrorAlert message=error.to_string() on_retry={retry} /> }
                                .into_any()
                        }
                        Some(Ok(page)) if page.assets.is_empty() => {
                            view! { <EmptyState message="No assets yet." /> }.into_any()
                        }
                        Some(Ok(page)) => {
                            // Own copy for the `For` children closure.
                            let row_token = list_token.clone();
                            view! {
                                <CountLine count=page.total singular="asset" plural="assets" />
                                <Card>
                                    <Table>
                                        <TableHeader>
                                            <TableRow>
                                                <TableHead>"File"</TableHead>
                                                <TableHead>"Kind"</TableHead>
                                                <TableHead>"MIME"</TableHead>
                                                <TableHead>"Size"</TableHead>
                                                <TableHead>"SHA-256"</TableHead>
                                                <TableHead>"Actions"</TableHead>
                                            </TableRow>
                                        </TableHeader>
                                        <TableBody>
                                            <For
                                                each=move || page.assets.clone()
                                                key=|asset| asset.asset_id
                                                let:asset
                                            >
                                                <AssetRow
                                                    asset=asset
                                                    token={row_token.clone()}
                                                    on_archived=retry
                                                />
                                            </For>
                                        </TableBody>
                                    </Table>
                                </Card>
                            }
                                .into_any()
                        }
                    }}
                }
                    .into_any()
            }
        }}
    }
}

/// Register-asset form: file name, MIME type, size, SHA-256 hex.
///
/// POSTs with a fresh `Idempotency-Key`. A `200` (not `201`) means the
/// same bytes were already registered — the form reports the dedupe.
///
/// Uses controlled signals (idiomatic for the v2 [`Input`]) instead of
/// `NodeRef`s; validation and clearing read/write the signals directly.
#[component]
fn AssetRegisterForm(token: String, profile_id: String, on_registered: UiHandler) -> impl IntoView {
    let file_name = RwSignal::new(String::new());
    let mime_type = RwSignal::new(String::new());
    let size_text = RwSignal::new(String::new());
    let sha256_hex = RwSignal::new(String::new());
    let status = RwSignal::new(None::<Result<String, String>>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let file_name_value = file_name.get_untracked().trim().to_string();
        let mime_type_value = mime_type.get_untracked().trim().to_string();
        let size_bytes: u64 = size_text.get_untracked().trim().parse().unwrap_or(0);
        let sha256_value = sha256_hex.get_untracked().trim().to_lowercase();
        if file_name_value.is_empty() || mime_type_value.is_empty() {
            status.set(Some(Err(
                "File name and MIME type are required.".to_string()
            )));
            return;
        }
        if size_bytes == 0 {
            status.set(Some(Err("Size must be greater than zero.".to_string())));
            return;
        }
        if sha256_value.len() != 64 || !sha256_value.bytes().all(|b| b.is_ascii_hexdigit()) {
            status.set(Some(Err("SHA-256 must be 64 hex characters.".to_string())));
            return;
        }
        submitting.set(true);
        status.set(None);
        let request = RegisterAssetRequest {
            client_profile_id: profile_id.clone(),
            file_name: file_name_value,
            mime_type: mime_type_value,
            size_bytes,
            sha256_hex: sha256_value,
        };
        let token = token.clone();
        spawn_local(async move {
            match register_asset(&request, &token).await {
                Ok(result) => {
                    file_name.set(String::new());
                    mime_type.set(String::new());
                    size_text.set(String::new());
                    sha256_hex.set(String::new());
                    // `200 OK` (not `201`) means the same bytes were already
                    // registered under the profile — report the dedupe
                    // distinctly instead of claiming a fresh registration.
                    let message = if result.deduplicated {
                        format!(
                            "Already registered: {} (same bytes exist under this profile).",
                            result.asset.file_name
                        )
                    } else {
                        format!("Asset registered: {}", result.asset.file_name)
                    };
                    status.set(Some(Ok(message)));
                    on_registered.call();
                }
                Err(error) => status.set(Some(Err(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <Card>
            <CardHeader>
                <CardTitle>"Register asset"</CardTitle>
            </CardHeader>
            <CardContent>
                <form on:submit=on_submit>
                    <div style="display: grid; gap: 0.875rem; max-width: 36rem;">
                        <div>
                            <Label for_id="asset-name">"File name"</Label>
                            <TextInput
                                id="asset-name"
                                placeholder="contract-draft.pdf"
                                value=file_name
                                on_input={UiValueHandler::new(move |v: String| file_name.set(v))}
                            />
                        </div>
                        <div>
                            <Label for_id="asset-mime">"MIME type"</Label>
                            <TextInput
                                id="asset-mime"
                                placeholder="application/pdf"
                                value=mime_type
                                on_input={UiValueHandler::new(move |v: String| mime_type.set(v))}
                            />
                        </div>
                        <div>
                            <Label for_id="asset-size">"Size (bytes)"</Label>
                            <TextInput
                                id="asset-size"
                                input_type="number"
                                placeholder="1024"
                                value=size_text
                                on_input={UiValueHandler::new(move |v: String| size_text.set(v))}
                            />
                        </div>
                        <div>
                            <Label for_id="asset-sha">"SHA-256 (hex)"</Label>
                            <TextInput
                                id="asset-sha"
                                placeholder="64 hex characters"
                                value=sha256_hex
                                on_input={UiValueHandler::new(move |v: String| sha256_hex.set(v))}
                            />
                        </div>
                        <div>
                            <Button
                                variant=ButtonVariant::Default
                                btn_type="submit"
                                disabled=Signal::derive(move || submitting.get())
                            >
                                {move || if submitting.get() { "Registering…" } else { "Register asset" }}
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
    }
}

/// One asset row: name, kind, size, short hash, archive action.
///
/// Archive failures surface inline via [`ErrorAlert`]; `on_archived` fires
/// only on success so the list refetch (and its error state) stays
/// consistent.
#[component]
fn AssetRow(asset: Asset, token: String, on_archived: UiHandler) -> impl IntoView {
    let working = RwSignal::new(false);
    let archive_error = RwSignal::new(None::<String>);
    let asset_id = asset.asset_id.to_string();
    let short_hash: String = asset
        .sha256
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect();

    // 0-arg so it can plug straight into [`Button`]'s `on_click`.
    let on_archive = UiHandler::new(move || {
        if working.get() {
            return;
        }
        working.set(true);
        archive_error.set(None);
        let token = token.clone();
        let asset_id = asset_id.clone();
        spawn_local(async move {
            match archive_asset(&asset_id, &token).await {
                Ok(_) => on_archived.call(),
                Err(error) => archive_error.set(Some(error.to_string())),
            }
            working.set(false);
        });
    });

    view! {
        <TableRow>
            <TableCell>
                <strong style="font-weight: 600;">{asset.file_name.clone()}</strong>
            </TableCell>
            <TableCell>
                <Badge variant=asset_badge(&asset.kind)>{format!("{:?}", asset.kind)}</Badge>
            </TableCell>
            <TableCell>
                <span style="color: var(--muted-foreground);">{asset.mime_type.clone()}</span>
            </TableCell>
            <TableCell>{format!("{} bytes", asset.size_bytes)}</TableCell>
            <TableCell>
                <span style="font-family: var(--font-mono); font-size: 0.75rem; color: var(--muted-foreground);">
                    {format!("{short_hash}…")}
                </span>
            </TableCell>
            <TableCell>
                {move || {
                    archive_error
                        .get()
                        .map(|message| view! { <ErrorAlert message=message /> })
                }}
                <Button
                    variant=ButtonVariant::Destructive
                    btn_type="button"
                    disabled=Signal::derive(move || working.get())
                    on_click=on_archive
                >
                    {move || if working.get() { "Archiving…" } else { "Archive" }}
                </Button>
            </TableCell>
        </TableRow>
    }
}
