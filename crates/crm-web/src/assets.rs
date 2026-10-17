// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! Asset pages: list + register metadata for one client profile.
//!
//! Assets are profile-scoped (`Asset` grant in the profile). The page
//! first loads the agency's profiles into a selector, then lists the
//! selected profile's assets. Registration is metadata-only — the byte
//! store is out of scope, so the uploader supplies the SHA-256 content
//! address (e.g. computed by the agency's ingestion pipeline); uploads
//! are deduped by content hash within the profile.

use leptos::html::{Input, Select};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crm_domain::asset::Asset;

use crate::api::{
    archive_asset, fetch_assets, fetch_profiles, register_asset, ApiError, AssetListResponse,
    RegisterAssetRequest,
};
use crate::components::{EmptyState, ErrorAlert, Loading, OkAlert, PageHeader};
use crate::styles;

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
        <section aria-label="Assets" style=styles::SECTION>
            <PageHeader title="Assets" />
            {move || match profile_options.get() {
                None => view! { <Loading label="profiles" /> }.into_any(),
                Some(Err(error)) => {
                    view! { <ErrorAlert message=error.to_string() /> }.into_any()
                }
                Some(Ok(options)) if options.is_empty() => {
                    view! { <EmptyState message="Create a client profile first." /> }.into_any()
                }
                Some(Ok(options)) => {
                    view! {
                        <ProfileSelect options=options selected=selected />
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
            let my_generation = generation.get() + 1;
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
    let retry = move || reload.update(|n| *n = n.wrapping_add(1));

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
                    <h3 style=styles::H3>"Assets"</h3>
                    {move || match assets.get() {
                        None => view! { <Loading label="assets" /> }.into_any(),
                        Some(Err(error)) => {
                            view! { <ErrorAlert message=error.to_string() on_retry={std::rc::Rc::new(retry) as std::rc::Rc<dyn Fn()>} /> }
                                .into_any()
                        }
                        Some(Ok(page)) if page.assets.is_empty() => {
                            view! { <EmptyState message="No assets yet." /> }.into_any()
                        }
                        Some(Ok(page)) => {
                            // Own copy for the `For` children closure.
                            let row_token = list_token.clone();
                            let total = page.total;
                            view! {
                                <p style=styles::MUTED>
                                    {format!(
                                        "{} asset{} total",
                                        total,
                                        if total == 1 { "" } else { "s" }
                                    )}
                                </p>
                                <ul style="list-style: none; margin: 0; padding: 0; display: grid; gap: 0.75rem;">
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
                                </ul>
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

/// Profile `<select>` driving the asset list.
///
/// # Props
/// - `options`: `(profile_id, name)` pairs.
/// - `selected`: write signal for the chosen profile id.
#[component]
fn ProfileSelect(options: Vec<(String, String)>, selected: RwSignal<String>) -> impl IntoView {
    let select_ref = NodeRef::<Select>::new();
    let on_change = move |_| {
        let value = select_ref.get().map(|el| el.value()).unwrap_or_default();
        selected.set(value);
    };
    view! {
        <div style="margin-bottom: 1rem; max-width: 32rem;">
            <label for="asset-profile" style=styles::LABEL>"Client profile"</label>
            <select id="asset-profile" node_ref=select_ref on:change=on_change style=styles::INPUT>
                <option value="">"— select —"</option>
                {options
                    .into_iter()
                    .map(|(id, name)| {
                        view! { <option value=id>{name}</option> }
                    })
                    .collect::<Vec<_>>()}
            </select>
        </div>
    }
}

/// Register-asset form: file name, MIME type, size, SHA-256 hex.
///
/// POSTs with a fresh `Idempotency-Key`. A `200` (not `201`) means the
/// same bytes were already registered — the form reports the dedupe.
#[component]
fn AssetRegisterForm(
    token: String,
    profile_id: String,
    on_registered: impl Fn() + Clone + 'static,
) -> impl IntoView {
    let name_ref = NodeRef::<Input>::new();
    let mime_ref = NodeRef::<Input>::new();
    let size_ref = NodeRef::<Input>::new();
    let sha_ref = NodeRef::<Input>::new();
    let status = RwSignal::new(None::<Result<String, String>>);
    let submitting = RwSignal::new(false);

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        let file_name = name_ref
            .get()
            .map(|i| i.value().trim().to_string())
            .unwrap_or_default();
        let mime_type = mime_ref
            .get()
            .map(|i| i.value().trim().to_string())
            .unwrap_or_default();
        let size_bytes: u64 = size_ref
            .get()
            .map(|i| i.value().trim().parse().unwrap_or(0))
            .unwrap_or(0);
        let sha256_hex = sha_ref
            .get()
            .map(|i| i.value().trim().to_lowercase())
            .unwrap_or_default();
        if file_name.is_empty() || mime_type.is_empty() {
            status.set(Some(Err(
                "File name and MIME type are required.".to_string()
            )));
            return;
        }
        if size_bytes == 0 {
            status.set(Some(Err("Size must be greater than zero.".to_string())));
            return;
        }
        if sha256_hex.len() != 64 || !sha256_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            status.set(Some(Err("SHA-256 must be 64 hex characters.".to_string())));
            return;
        }
        submitting.set(true);
        status.set(None);
        let request = RegisterAssetRequest {
            client_profile_id: profile_id.clone(),
            file_name,
            mime_type,
            size_bytes,
            sha256_hex,
        };
        let on_registered = on_registered.clone();
        let token = token.clone();
        spawn_local(async move {
            match register_asset(&request, &token).await {
                Ok(result) => {
                    for field in [&name_ref, &mime_ref, &size_ref, &sha_ref] {
                        if let Some(input) = field.get() {
                            input.set_value("");
                        }
                    }
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
                    on_registered();
                }
                Err(error) => status.set(Some(Err(error.to_string()))),
            }
            submitting.set(false);
        });
    };

    view! {
        <form on:submit=on_submit style=styles::CARD>
            <h3 style="margin: 0 0 0.5rem;">"Register asset"</h3>
            <div style=styles::FORM>
                <div>
                    <label for="asset-name" style=styles::LABEL>"File name"</label>
                    <input
                        id="asset-name"
                        type="text"
                        node_ref=name_ref
                        placeholder="contract-draft.pdf"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <label for="asset-mime" style=styles::LABEL>"MIME type"</label>
                    <input
                        id="asset-mime"
                        type="text"
                        node_ref=mime_ref
                        placeholder="application/pdf"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <label for="asset-size" style=styles::LABEL>"Size (bytes)"</label>
                    <input
                        id="asset-size"
                        type="number"
                        min="1"
                        node_ref=size_ref
                        placeholder="1024"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <label for="asset-sha" style=styles::LABEL>"SHA-256 (hex)"</label>
                    <input
                        id="asset-sha"
                        type="text"
                        node_ref=sha_ref
                        placeholder="64 hex characters"
                        autocomplete="off"
                        style=styles::INPUT
                    />
                </div>
                <div>
                    <button type="submit" disabled=move || submitting.get() style=styles::BTN_PRIMARY>
                        {move || if submitting.get() { "Registering…" } else { "Register asset" }}
                    </button>
                </div>
                {move || match status.get() {
                    None => ().into_any(),
                    Some(Ok(message)) => view! { <OkAlert message=message /> }.into_any(),
                    Some(Err(message)) => view! { <ErrorAlert message=message /> }.into_any(),
                }}
            </div>
        </form>
    }
}

/// One asset row: name, kind, size, short hash, archive action.
///
/// Archive failures surface inline via [`ErrorAlert`]; `on_archived` fires
/// only on success so the list refetch (and its error state) stays
/// consistent.
#[component]
fn AssetRow(
    asset: Asset,
    token: String,
    on_archived: impl Fn() + Clone + 'static,
) -> impl IntoView {
    let working = RwSignal::new(false);
    let archive_error = RwSignal::new(None::<String>);
    let asset_id = asset.asset_id.to_string();
    let short_hash: String = asset
        .sha256
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect();

    let on_archive = move |_| {
        if working.get() {
            return;
        }
        working.set(true);
        archive_error.set(None);
        let token = token.clone();
        let asset_id = asset_id.clone();
        let on_archived = on_archived.clone();
        spawn_local(async move {
            match archive_asset(&asset_id, &token).await {
                Ok(_) => on_archived(),
                Err(error) => archive_error.set(Some(error.to_string())),
            }
            working.set(false);
        });
    };

    view! {
        <li style=styles::CARD>
            <div style="display: flex; justify-content: space-between; align-items: baseline; gap: 1rem;">
                <strong>{asset.file_name.clone()}</strong>
                <span style=styles::PILL>{format!("{:?}", asset.kind)}</span>
            </div>
            <dl style=styles::DL>
                <dt style=styles::DT>"MIME"</dt>
                <dd style="margin: 0;">{asset.mime_type.clone()}</dd>
                <dt style=styles::DT>"Size"</dt>
                <dd style="margin: 0;">{format!("{} bytes", asset.size_bytes)}</dd>
                <dt style=styles::DT>"SHA-256"</dt>
                <dd style=styles::MONO>{format!("{short_hash}…")}</dd>
            </dl>
            {move || {
                archive_error
                    .get()
                    .map(|message| view! { <ErrorAlert message=message /> })
            }}
            <div style="margin-top: 0.75rem;">
                <button
                    type="button"
                    on:click=on_archive
                    disabled=move || working.get()
                    style=styles::BTN_DANGER
                >
                    {move || if working.get() { "Archiving…" } else { "Archive" }}
                </button>
            </div>
        </li>
    }
}
