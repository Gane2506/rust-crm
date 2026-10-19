// SPDX-License-Identifier: GPL-3.0-only
//! Branding domain: per-agency visual customization (task UI2-03).
//!
//! Businesses customize their logo, brand colors, and theme through the
//! branding studio. [`BrandingTheme`] is the validated aggregate;
//! [`BrandingRepository`] is the persistence port (PostgreSQL in
//! `crm-db`); [`BrandingService`] is the application service that
//! enforces authorization and records audit events.
//!
//! ## Validation
//!
//! Color fields accept the same bounded CSS color grammar as the
//! frontend's `branded_css()` (`theme.rs`): hex (`#rrggbb`),
//! `rgb()/hsl()/oklch()` functional notation, and CSS color keywords.
//! Anything that could break out of a CSS declaration value
//! (`;`, `{`, `}`, quotes, `url(`, …) is rejected, preventing CSS rule
//! injection when the theme is rendered into a `<style>` tag. The logo
//! URL must use `https://` (or be absent).

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::AgencyId;
use crate::security::{Operation, Resource};
use crate::services::ActionContext;

/// Audit action recorded when branding is updated.
pub const BRANDING_UPDATED: &str = "branding.updated";

/// Maximum length for the logo URL (prevents abuse via huge strings).
pub const MAX_LOGO_URL_LEN: usize = 2048;

/// Maximum length for a single color value.
pub const MAX_COLOR_LEN: usize = 64;

/// Maximum length for the font family stack.
pub const MAX_FONT_FAMILY_LEN: usize = 256;

/// Returns `true` when `value` is a safe CSS color.
///
/// Accepts hex (`#rrggbb`), `rgb()/hsl()/oklch()` functional notation,
/// and CSS color keywords — rejects anything that could break out of a
/// declaration value (`;`, `{`, `}`, quotes, `url(`, …).
///
/// This mirrors the frontend's `is_safe_css_color()` in
/// `crates/crm-web/src/theme.rs`; the two must stay in sync so a theme
/// accepted by the API always renders.
///
/// ```
/// use crm_domain::branding::is_safe_css_color;
///
/// assert!(is_safe_css_color("oklch(0.55 0.2 250)"));
/// assert!(is_safe_css_color("#ff0000"));
/// assert!(is_safe_css_color("rebeccapurple"));
/// assert!(!is_safe_css_color("red; } body { background: evil"));
/// assert!(!is_safe_css_color("url(https://evil.example/x.css)"));
/// ```
pub fn is_safe_css_color(value: &str) -> bool {
    let v = value.trim();
    !v.is_empty()
        && v.len() <= MAX_COLOR_LEN
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || " \t.,%/#()".contains(c))
        && !v.to_ascii_lowercase().contains("url(")
        && !v.contains([';', '{', '}', '"', '\'', '<', '>', '\\'])
}

/// Returns `true` when `url` is a valid logo URL: empty/absent is fine,
/// otherwise it must start with `https://` and fit within
/// [`MAX_LOGO_URL_LEN`].
pub fn is_valid_logo_url(url: &str) -> bool {
    let v = url.trim();
    v.is_empty() || (v.len() <= MAX_LOGO_URL_LEN && v.starts_with("https://"))
}

/// Per-agency branding theme.
///
/// All fields are optional; unset fields fall back to the default
/// design tokens. `Deserialize` is implemented manually through
/// [`BrandingTheme::new`] so deserialized values are validated too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BrandingTheme {
    /// Logo image URL (https only), rendered in the sidebar brand slot.
    logo_url: Option<String>,
    /// Overrides `--primary` (buttons, active states, links).
    primary: Option<String>,
    /// Overrides `--primary-foreground`.
    primary_foreground: Option<String>,
    /// Overrides `--ring` (focus rings, selection accents).
    ring: Option<String>,
    /// Overrides `--chart-1` (primary dashboard accent).
    chart_1: Option<String>,
    /// Overrides `--radius` (e.g. `"0.625rem"`).
    radius: Option<String>,
    /// Overrides `--font-sans` (font family stack).
    font_family: Option<String>,
}

impl<'de> serde::Deserialize<'de> for BrandingTheme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct RawBranding {
            #[serde(default)]
            logo_url: Option<String>,
            #[serde(default)]
            primary: Option<String>,
            #[serde(default)]
            primary_foreground: Option<String>,
            #[serde(default)]
            ring: Option<String>,
            #[serde(default)]
            chart_1: Option<String>,
            #[serde(default)]
            radius: Option<String>,
            #[serde(default)]
            font_family: Option<String>,
        }
        let raw = RawBranding::deserialize(deserializer)?;
        BrandingTheme::new(
            raw.logo_url,
            raw.primary,
            raw.primary_foreground,
            raw.ring,
            raw.chart_1,
            raw.radius,
            raw.font_family,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl BrandingTheme {
    /// Creates a branding theme, validating every set field.
    ///
    /// Rejects: color values outside the bounded CSS grammar; logo URLs
    /// that are not `https://` (or empty); over-long values.
    ///
    /// ```
    /// use crm_domain::branding::BrandingTheme;
    ///
    /// let theme = BrandingTheme::new(
    ///     Some("https://example.com/logo.png".to_string()),
    ///     Some("oklch(0.55 0.2 250)".to_string()),
    ///     None, None, None, None, None,
    /// );
    /// assert!(theme.is_ok());
    ///
    /// let bad = BrandingTheme::new(
    ///     None,
    ///     Some("red; } evil {".to_string()),
    ///     None, None, None, None, None,
    /// );
    /// assert!(bad.is_err());
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        logo_url: Option<String>,
        primary: Option<String>,
        primary_foreground: Option<String>,
        ring: Option<String>,
        chart_1: Option<String>,
        radius: Option<String>,
        font_family: Option<String>,
    ) -> Result<Self, DomainError> {
        let check_color =
            |name: &str, value: Option<String>| -> Result<Option<String>, DomainError> {
                match value {
                    None => Ok(None),
                    Some(v) => {
                        let v = v.trim().to_owned();
                        if v.is_empty() {
                            Ok(None)
                        } else if !is_safe_css_color(&v) {
                            Err(DomainError::Validation(format!(
                                "branding.{name} is not a safe CSS color"
                            )))
                        } else {
                            Ok(Some(v))
                        }
                    }
                }
            };

        let logo_url = match logo_url {
            None => None,
            Some(u) => {
                let u = u.trim().to_owned();
                if u.is_empty() {
                    None
                } else if !is_valid_logo_url(&u) {
                    return Err(DomainError::Validation(
                        "branding.logo_url must be an https:// URL".to_string(),
                    ));
                } else {
                    Some(u)
                }
            }
        };

        let radius = match radius {
            None => None,
            Some(r) => {
                let r = r.trim().to_owned();
                if r.is_empty() {
                    None
                } else if r.len() > 32
                    || !r
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || " .%".contains(c))
                {
                    return Err(DomainError::Validation(
                        "branding.radius is not a valid CSS length".to_string(),
                    ));
                } else {
                    Some(r)
                }
            }
        };

        let font_family = match font_family {
            None => None,
            Some(f) => {
                let f = f.trim().to_owned();
                if f.is_empty() {
                    None
                } else if f.len() > MAX_FONT_FAMILY_LEN
                    || f.contains([';', '{', '}', '"', '\'', '<', '>', '\\'])
                {
                    return Err(DomainError::Validation(
                        "branding.font_family contains unsafe characters".to_string(),
                    ));
                } else {
                    Some(f)
                }
            }
        };

        Ok(Self {
            logo_url,
            primary: check_color("primary", primary)?,
            primary_foreground: check_color("primary_foreground", primary_foreground)?,
            ring: check_color("ring", ring)?,
            chart_1: check_color("chart_1", chart_1)?,
            radius,
            font_family,
        })
    }

    /// An empty theme (all defaults).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Logo image URL, if set.
    pub fn logo_url(&self) -> Option<&str> {
        self.logo_url.as_deref()
    }

    /// Primary brand color, if set.
    pub fn primary(&self) -> Option<&str> {
        self.primary.as_deref()
    }

    /// Primary foreground color, if set.
    pub fn primary_foreground(&self) -> Option<&str> {
        self.primary_foreground.as_deref()
    }

    /// Ring/focus color, if set.
    pub fn ring(&self) -> Option<&str> {
        self.ring.as_deref()
    }

    /// Primary chart accent, if set.
    pub fn chart_1(&self) -> Option<&str> {
        self.chart_1.as_deref()
    }

    /// Border radius override, if set.
    pub fn radius(&self) -> Option<&str> {
        self.radius.as_deref()
    }

    /// Font family stack override, if set.
    pub fn font_family(&self) -> Option<&str> {
        self.font_family.as_deref()
    }
}

/// A versioned branding snapshot: the theme plus its version number.
///
/// Versions start at 1 and bump on every update; the repository stores
/// the full history (like `agency_settings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandingVersion {
    /// Monotonic version, starting at 1.
    pub version: u64,
    /// The theme at this version.
    pub theme: BrandingTheme,
}

/// Persistence port for branding themes.
///
/// Implementations store one versioned theme per agency. `upsert`
/// creates version 1 on first write and bumps the version on every
/// subsequent write.
pub trait BrandingRepository {
    /// Returns the latest branding theme for `agency_id`, or `None`
    /// when it was never set.
    fn get<'a>(
        &'a self,
        agency_id: &'a AgencyId,
    ) -> impl Future<Output = Result<Option<BrandingVersion>, DomainError>> + Send + 'a;

    /// Stores a new branding version for `agency_id`, bumping the
    /// version. Returns the stored version.
    fn upsert<'a>(
        &'a self,
        agency_id: &'a AgencyId,
        theme: &'a BrandingTheme,
    ) -> impl Future<Output = Result<BrandingVersion, DomainError>> + Send + 'a;
}

/// Application service for branding: authorization + audit.
///
/// The service is agency-scoped: every method takes an
/// [`ActionContext`] and denies by default when the caller lacks the
/// grant. Updates record a [`BRANDING_UPDATED`] audit event.
pub trait BrandingService {
    /// Returns the agency's current branding (or the empty theme when
    /// never set). No grant required — the login page needs it.
    fn get_branding<'a>(
        &'a self,
        agency_id: &'a AgencyId,
    ) -> impl Future<Output = Result<BrandingVersion, DomainError>> + Send + 'a;

    /// Stores a new branding version. Requires `AgencySettings:Manage`
    /// (branding is an agency-level setting). Records
    /// [`BRANDING_UPDATED`].
    fn update_branding<'a>(
        &'a self,
        agency_id: &'a AgencyId,
        theme: BrandingTheme,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<BrandingVersion, DomainError>> + Send + 'a;
}

/// Builds the [`Action`] for branding reads (used by API authorization).
pub fn branding_read_action() -> crate::security::Action {
    crate::security::Action {
        resource: Resource::AgencySettings,
        operation: Operation::Read,
    }
}

/// Builds the [`Action`] for branding writes (used by API authorization).
pub fn branding_write_action() -> crate::security::Action {
    crate::security::Action {
        resource: Resource::AgencySettings,
        operation: Operation::Manage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_theme() -> BrandingTheme {
        BrandingTheme::new(
            Some("https://example.com/logo.png".to_string()),
            Some("oklch(0.55 0.2 250)".to_string()),
            Some("#ffffff".to_string()),
            Some("oklch(0.7 0 0)".to_string()),
            Some("oklch(0.8 0.1 250)".to_string()),
            Some("0.625rem".to_string()),
            Some("Inter, sans-serif".to_string()),
        )
        .expect("valid theme")
    }

    #[test]
    fn accepts_valid_theme() {
        let theme = valid_theme();
        assert_eq!(theme.logo_url(), Some("https://example.com/logo.png"));
        assert_eq!(theme.primary(), Some("oklch(0.55 0.2 250)"));
    }

    #[test]
    fn rejects_css_injection() {
        for evil in [
            "red; } body { display: none",
            "url(https://evil.example/x.css)",
            "red\" onload=\"evil",
            "oklch(0.5 0.2 250) <script>",
        ] {
            let result =
                BrandingTheme::new(None, Some(evil.to_string()), None, None, None, None, None);
            assert!(result.is_err(), "should reject: {evil}");
        }
    }

    #[test]
    fn rejects_non_https_logo() {
        let result = BrandingTheme::new(
            Some("http://example.com/logo.png".to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn empty_strings_become_none() {
        let theme = BrandingTheme::new(
            Some("  ".to_string()),
            Some("".to_string()),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("empty strings are fine");
        assert_eq!(theme.logo_url(), None);
        assert_eq!(theme.primary(), None);
    }

    #[test]
    fn deserialize_validates() {
        let json = r#"{"primary": "red; evil"}"#;
        let result: Result<BrandingTheme, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn audit_event_name_is_pinned() {
        assert_eq!(BRANDING_UPDATED, "branding.updated");
    }
}
