// SPDX-License-Identifier: GPL-3.0-only
//! Branding studio (task UI2-03): per-business visual customization.
//!
//! Businesses edit their logo URL, brand colors, radius, and font
//! family; a live preview panel renders v2 components under the
//! draft theme. Saving `PUT`s the theme; the shell re-injects the
//! `<style>` so the whole app re-themes without a reload.

use leptos::prelude::*;

use crate::api::{fetch_branding, update_branding, BrandingResponse, BrandingThemeDto};
use crate::theme::{branded_css, BrandingTheme};
use crate::ui::{
    Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader,
    CardTitle, Input, Label, UiHandler, UiValueHandler,
};

/// Converts the API DTO to the render theme.
fn dto_to_theme(dto: &BrandingThemeDto) -> BrandingTheme {
    BrandingTheme {
        primary: dto.primary.clone(),
        primary_foreground: dto.primary_foreground.clone(),
        ring: dto.ring.clone(),
        chart_1: dto.chart_1.clone(),
        business_name: None,
        logo_url: dto.logo_url.clone(),
    }
}

/// Re-injects the branding `<style>` tag in the document head.
///
/// Called after save so the whole app re-themes without a reload.
pub fn apply_branding_style(theme: &BrandingTheme) {
    use leptos::leptos_dom::helpers::document;
    let css = branded_css(theme);
    if let Some(head) = document().head() {
        // Remove the previous branding style, if any.
        if let Some(old) = document().get_element_by_id("crm-branding-style") {
            old.remove();
        }
        if !css.is_empty() {
            if let Ok(style) = document().create_element("style") {
                let _ = style.set_attribute("id", "crm-branding-style");
                style.set_text_content(Some(&css));
                let _ = head.append_child(&style);
            }
        }
    }
}

/// One color row: native color picker + manual oklch text field.
#[component]
fn ColorRow(label: &'static str, signal: RwSignal<String>) -> impl IntoView {
    let on_text = UiValueHandler::new(move |v: String| signal.set(v));
    view! {
        <div style="display: flex; flex-direction: column; gap: 0.375rem;">
            <Label>{label}</Label>
            <div style="display: flex; gap: 0.5rem; align-items: center;">
                <input
                    type="color"
                    prop:value=move || signal.get()
                    on:input=move |ev| {
                        use leptos::leptos_dom::helpers::event_target_value;
                        signal.set(event_target_value(&ev));
                    }
                    style="width: 2.5rem; height: 2.5rem; padding: 0; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--card); cursor: pointer;"
                    aria-label={format!("{label} color picker")}
                />
                <div style="flex: 1;">
                    <Input
                        placeholder="oklch(0.55 0.2 250) or #rrggbb"
                        value=signal
                        on_input=on_text
                    />
                </div>
            </div>
        </div>
    }
}

/// Branding studio page at `/branding`.
///
/// # Props
/// - `token`: bearer token for the authenticated `PUT`.
#[component]
pub fn BrandingPage(token: String) -> impl IntoView {
    let primary = RwSignal::new(String::new());
    let primary_foreground = RwSignal::new(String::new());
    let ring = RwSignal::new(String::new());
    let chart_1 = RwSignal::new(String::new());
    let logo_url = RwSignal::new(String::new());
    let radius = RwSignal::new("0.625rem".to_string());
    let font_family = RwSignal::new("Inter, sans-serif".to_string());
    let status = RwSignal::new(String::new());
    let version = RwSignal::new(0u64);

    // Load the current theme on mount.
    {
        Effect::new(move |_| {
            leptos::task::spawn_local(async move {
                match fetch_branding().await {
                    Ok(BrandingResponse { version: v, theme }) => {
                        version.set(v);
                        if let Some(v) = theme.primary {
                            primary.set(v);
                        }
                        if let Some(v) = theme.primary_foreground {
                            primary_foreground.set(v);
                        }
                        if let Some(v) = theme.ring {
                            ring.set(v);
                        }
                        if let Some(v) = theme.chart_1 {
                            chart_1.set(v);
                        }
                        if let Some(v) = theme.logo_url {
                            logo_url.set(v);
                        }
                        if let Some(v) = theme.radius {
                            radius.set(v);
                        }
                        if let Some(v) = theme.font_family {
                            font_family.set(v);
                        }
                    }
                    Err(e) => status.set(format!("Could not load branding: {e:?}")),
                }
            });
        });
    }

    let on_save = {
        let token = token.clone();
        UiHandler::new(move || {
            let token = token.clone();
            let dto = BrandingThemeDto {
                logo_url: non_empty(logo_url.get_untracked()),
                primary: non_empty(primary.get_untracked()),
                primary_foreground: non_empty(primary_foreground.get_untracked()),
                ring: non_empty(ring.get_untracked()),
                chart_1: non_empty(chart_1.get_untracked()),
                radius: non_empty(radius.get_untracked()),
                font_family: non_empty(font_family.get_untracked()),
            };
            leptos::task::spawn_local(async move {
                status.set("Saving…".to_string());
                match update_branding(&dto, &token).await {
                    Ok(resp) => {
                        version.set(resp.version);
                        apply_branding_style(&dto_to_theme(&resp.theme));
                        status.set(format!("Saved (version {})", resp.version));
                    }
                    Err(e) => status.set(format!("Save failed: {e:?}")),
                }
            });
        })
    };

    let on_reset = UiHandler::new(move || {
        primary.set(String::new());
        primary_foreground.set(String::new());
        ring.set(String::new());
        chart_1.set(String::new());
        logo_url.set(String::new());
        radius.set("0.625rem".to_string());
        font_family.set("Inter, sans-serif".to_string());
        apply_branding_style(&BrandingTheme {
            primary: None,
            primary_foreground: None,
            ring: None,
            chart_1: None,
            business_name: None,
            logo_url: None,
        });
        status.set("Reset to defaults (not saved yet)".to_string());
    });

    view! {
        <div style="display: flex; flex-direction: column; gap: 1.5rem; max-width: 72rem;">
            <div>
                <h1 style="font-size: 1.5rem; font-weight: 600; margin: 0 0 0.25rem 0;">
                    "Branding studio"
                </h1>
                <p style="color: var(--muted-foreground); margin: 0; font-size: 0.875rem;">
                    "Customize your business logo, colors, and theme. Changes apply instantly across the app."
                </p>
            </div>

            <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr)); gap: 1.5rem;">
                <Card>
                    <CardHeader>
                        <CardTitle>"Brand colors"</CardTitle>
                        <CardDescription>
                            "Pick a color or type any CSS color (oklch, hex, keyword)."
                        </CardDescription>
                    </CardHeader>
                    <CardContent>
                        <div style="display: flex; flex-direction: column; gap: 1rem;">
                            <ColorRow label="Primary" signal=primary />
                            <ColorRow label="Primary foreground" signal=primary_foreground />
                            <ColorRow label="Ring (focus)" signal=ring />
                            <ColorRow label="Chart accent" signal=chart_1 />
                        </div>
                    </CardContent>
                </Card>

                <Card>
                    <CardHeader>
                        <CardTitle>"Logo & theme"</CardTitle>
                        <CardDescription>"Logo URL, radius, and font family."</CardDescription>
                    </CardHeader>
                    <CardContent>
                        <div style="display: flex; flex-direction: column; gap: 1rem;">
                            <div>
                                <Label>"Logo URL (https)"</Label>
                                <Input
                                    placeholder="https://example.com/logo.png"
                                    value=logo_url
                                    on_input=UiValueHandler::new(move |v: String| logo_url.set(v))
                                />
                                {move || {
                                    let url = logo_url.get();
                                    (!url.trim().is_empty()).then(|| view! {
                                        <img
                                            src=url
                                            alt="Logo preview"
                                            style="margin-top: 0.5rem; max-height: 3rem; border-radius: var(--radius-sm); border: 1px solid var(--border);"
                                        />
                                    })
                                }}
                            </div>
                            <div>
                                <Label>"Border radius"</Label>
                                <select
                                    prop:value=move || radius.get()
                                    on:change=move |ev| {
                                        use leptos::leptos_dom::helpers::event_target_value;
                                        radius.set(event_target_value(&ev));
                                    }
                                    style="width: 100%; padding: 0.5rem; border: 1px solid var(--input); border-radius: var(--radius-md); background: var(--card); color: var(--foreground);"
                                >
                                    <option value="0">"Sharp (0)"</option>
                                    <option value="0.375rem">"Small (0.375rem)"</option>
                                    <option value="0.625rem">"Default (0.625rem)"</option>
                                    <option value="1rem">"Large (1rem)"</option>
                                </select>
                            </div>
                            <div>
                                <Label>"Font family"</Label>
                                <select
                                    prop:value=move || font_family.get()
                                    on:change=move |ev| {
                                        use leptos::leptos_dom::helpers::event_target_value;
                                        font_family.set(event_target_value(&ev));
                                    }
                                    style="width: 100%; padding: 0.5rem; border: 1px solid var(--input); border-radius: var(--radius-md); background: var(--card); color: var(--foreground);"
                                >
                                    <option value="Inter, sans-serif">"Inter"</option>
                                    <option value="system-ui, sans-serif">"System"</option>
                                    <option value="Georgia, serif">"Georgia (serif)"</option>
                                    <option value="ui-monospace, monospace">"Monospace"</option>
                                </select>
                            </div>
                        </div>
                    </CardContent>
                </Card>
            </div>

            <Card>
                <CardHeader>
                    <CardTitle>"Live preview"</CardTitle>
                    <CardDescription>
                        "Components rendered with your draft theme."
                    </CardDescription>
                </CardHeader>
                <CardContent>
                    <div style="display: flex; gap: 0.75rem; flex-wrap: wrap; align-items: center;">
                        <Button variant=ButtonVariant::Default>"Primary button"</Button>
                        <Button variant=ButtonVariant::Secondary>"Secondary"</Button>
                        <Button variant=ButtonVariant::Outline>"Outline"</Button>
                        <Badge>"New"</Badge>
                        <Badge variant=BadgeVariant::Secondary>"Beta"</Badge>
                    </div>
                </CardContent>
            </Card>

            <div style="display: flex; gap: 0.75rem; align-items: center;">
                <Button variant=ButtonVariant::Default on_click=on_save>
                    "Save branding"
                </Button>
                <Button variant=ButtonVariant::Outline on_click=on_reset>
                    "Reset to defaults"
                </Button>
                <span style="color: var(--muted-foreground); font-size: 0.875rem;">
                    {move || status.get()}
                </span>
                <span style="margin-left: auto; color: var(--muted-foreground); font-size: 0.75rem;">
                    {move || format!("version {}", version.get())}
                </span>
            </div>
        </div>
    }
}

/// Returns `Some(trimmed)` when non-empty, else `None`.
fn non_empty(s: String) -> Option<String> {
    let t = s.trim().to_owned();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}
