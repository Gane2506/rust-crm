// SPDX-License-Identifier: GPL-3.0-only
//! Analytics domain: saved dashboards, metric widgets, and reports.
//!
//! Analytics here is **read-mostly**. A [`Dashboard`] is a saved
//! configuration of [`DashboardWidget`]s; each widget pairs a
//! [`WidgetType`] with a [`MetricDefinition`] — a *pure* metric query
//! definition (entity, aggregation, optional grouping, date range).
//! The domain validates definitions; it never executes them. The
//! service layer ([`DashboardService`](crate::services::DashboardService),
//! task 15-DASH-02) executes a widget's metric against the existing
//! repositories and returns a [`WidgetResult`].
//!
//! ## Dashboard lifecycle
//!
//! Dashboards are authored as [`DashboardStatus::Draft`] and
//! [`Dashboard::publish`]ed once they carry at least one widget. A
//! published dashboard is a frozen configuration: widgets can only be
//! added, removed, or renamed on a draft, so [`Dashboard::unpublish`]
//! it first. Unpublishing a draft is rejected — only a published
//! dashboard can return to draft.
//!
//! ## Metric model
//!
//! A metric names a [`MetricEntity`] (contacts, campaigns, invoices,
//! payments, calls, emails, form submissions, contracts), a
//! [`MetricAggregation`] (`Count`, or `Sum` over a named numeric
//! field), an optional `group_by` field, and a [`DateRange`]. Widget
//! type and metric must agree: [`WidgetType::Breakdown`] requires a
//! `group_by` (a breakdown *is* a grouped count), while
//! [`WidgetType::Count`] rejects one (a grouped count is a breakdown).
//! [`WidgetType::Timeseries`] allows either — one series, or one
//! series per group.
//!
//! ## Authorization
//!
//! Dashboard operations authorize against `Resource::Dashboard` and
//! report operations against `Resource::Report` (profile-scoped; both
//! variants land in task 15-DASH-02 together with the persistence
//! layer — see the service trait docs).
//!
//! ## Audit
//!
//! Dashboard mutations are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s by the service
//! implementation, with pinned action names ([`DASHBOARD_DASHBOARD_CREATED`]
//! … [`DASHBOARD_REPORT_CREATED`]) documented on
//! [`DashboardService`](crate::services::DashboardService). They are
//! deliberately **not** [`SecurityEvent`](crate::audit::SecurityEvent)
//! variants: that taxonomy covers security-relevant events, while
//! dashboards are business configuration audited like campaigns and
//! workflows. Widget *reads* ([`DashboardService::run_widget`](crate::services::DashboardService::run_widget))
//! are not audited — they are read-mostly analytics, not mutations.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{ClientProfileId, DashboardId, ReportId, StaffId, WidgetId};

/// Audit action name recorded when a dashboard is created.
pub const DASHBOARD_DASHBOARD_CREATED: &str = "dashboard.dashboard_created";
/// Audit action name recorded when a dashboard is updated (renamed,
/// widget added or removed).
pub const DASHBOARD_DASHBOARD_UPDATED: &str = "dashboard.dashboard_updated";
/// Audit action name recorded when a dashboard is published.
pub const DASHBOARD_DASHBOARD_PUBLISHED: &str = "dashboard.dashboard_published";
/// Audit action name recorded when a dashboard is deleted (task
/// 15-DASH-02; the dashboard CRUD surface needs deletion, which the
/// 15-DASH-01 trait omitted).
pub const DASHBOARD_DASHBOARD_DELETED: &str = "dashboard.dashboard_deleted";
/// Audit action name recorded when a widget is added to a dashboard.
pub const DASHBOARD_WIDGET_ADDED: &str = "dashboard.widget_added";
/// Audit action name recorded when a report is created.
pub const DASHBOARD_REPORT_CREATED: &str = "dashboard.report_created";

/// The entity a metric is computed over.
///
/// The set is closed: metrics can only reference entities the CRM
/// knows how to count. Unknown entity names are rejected at
/// deserialization, so a saved metric can never name an entity the
/// service cannot execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum MetricEntity {
    /// Contacts (`crm_domain::contact::Contact`).
    #[serde(rename = "contact")]
    Contact,
    /// Campaigns (`crm_domain::campaign::Campaign`).
    #[serde(rename = "campaign")]
    Campaign,
    /// Invoices (`crm_domain::billing::Invoice`).
    #[serde(rename = "invoice")]
    Invoice,
    /// Payment records (`crm_domain::billing::Payment`).
    #[serde(rename = "payment")]
    Payment,
    /// Calls (`crm_domain::voice::Call`).
    #[serde(rename = "call")]
    Call,
    /// Email messages (`crm_domain::email::EmailMessage`).
    #[serde(rename = "email")]
    Email,
    /// Form submissions (`crm_domain::form::FormSubmission`).
    #[serde(rename = "form_submission")]
    FormSubmission,
    /// Contracts (`crm_domain::contract::Contract`).
    #[serde(rename = "contract")]
    Contract,
}

impl MetricEntity {
    /// Machine-readable name of the entity.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MetricEntity::Contact => "contact",
            MetricEntity::Campaign => "campaign",
            MetricEntity::Invoice => "invoice",
            MetricEntity::Payment => "payment",
            MetricEntity::Call => "call",
            MetricEntity::Email => "email",
            MetricEntity::FormSubmission => "form_submission",
            MetricEntity::Contract => "contract",
        }
    }

    /// Parses an entity name, rejecting unknown entities.
    fn parse(name: &str) -> Result<Self, DomainError> {
        match name {
            "contact" => Ok(MetricEntity::Contact),
            "campaign" => Ok(MetricEntity::Campaign),
            "invoice" => Ok(MetricEntity::Invoice),
            "payment" => Ok(MetricEntity::Payment),
            "call" => Ok(MetricEntity::Call),
            "email" => Ok(MetricEntity::Email),
            "form_submission" => Ok(MetricEntity::FormSubmission),
            "contract" => Ok(MetricEntity::Contract),
            other => Err(DomainError::Validation(format!(
                "unknown metric entity {other:?}"
            ))),
        }
    }
}

impl<'de> Deserialize<'de> for MetricEntity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        MetricEntity::parse(&name).map_err(serde::de::Error::custom)
    }
}

/// How a metric aggregates its entity's rows.
///
/// `Sum` needs a named numeric field (e.g. `"total_minor"` on
/// invoices); the field name is validated non-blank at construction
/// and at deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum MetricAggregation {
    /// Counts matching rows.
    #[serde(rename = "count")]
    Count,
    /// Sums a named numeric field over matching rows.
    #[serde(rename = "sum")]
    Sum {
        /// The numeric field to sum (e.g. `"total_minor"`).
        field: String,
    },
}

impl MetricAggregation {
    /// The count aggregation.
    #[must_use]
    pub fn count() -> Self {
        MetricAggregation::Count
    }

    /// The sum aggregation over `field`.
    ///
    /// Rejects: a blank field name.
    pub fn sum(field: String) -> Result<Self, DomainError> {
        let field = field.trim().to_string();
        if field.is_empty() {
            return Err(DomainError::Validation(
                "sum aggregation field must not be blank".to_string(),
            ));
        }
        Ok(MetricAggregation::Sum { field })
    }

    /// Machine-readable name of the aggregation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            MetricAggregation::Count => "count",
            MetricAggregation::Sum { .. } => "sum",
        }
    }
}

impl<'de> Deserialize<'de> for MetricAggregation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawAggregation {
            Named(String),
            Sum { sum: RawSum },
        }
        #[derive(Deserialize)]
        struct RawSum {
            field: String,
        }
        match RawAggregation::deserialize(deserializer)? {
            RawAggregation::Named(name) => match name.as_str() {
                "count" => Ok(MetricAggregation::Count),
                "sum" => Err(serde::de::Error::custom("sum aggregation requires a field")),
                other => Err(serde::de::Error::custom(format!(
                    "unknown metric aggregation {other:?}"
                ))),
            },
            RawAggregation::Sum { sum } => {
                MetricAggregation::sum(sum.field).map_err(serde::de::Error::custom)
            }
        }
    }
}

/// The time window a metric is computed over.
///
/// Construct the `Custom` variant via [`DateRange::custom`]: `from`
/// must not be after `to`. The fields are validated at construction
/// *and* at deserialization — `Deserialize` is implemented manually
/// through [`DateRange::custom`], following the
/// [`PhoneNumber`](crate::voice::PhoneNumber) hardening pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DateRange {
    /// The last 7 days (inclusive of today).
    #[serde(rename = "last_7_days")]
    Last7Days,
    /// The last 30 days (inclusive of today).
    #[serde(rename = "last_30_days")]
    Last30Days,
    /// The last 90 days (inclusive of today).
    #[serde(rename = "last_90_days")]
    Last90Days,
    /// An explicit window. `from` must not be after `to`.
    #[serde(rename = "custom")]
    Custom {
        /// Start of the window (inclusive).
        from: DateTime<Utc>,
        /// End of the window (inclusive).
        to: DateTime<Utc>,
    },
}

impl<'de> Deserialize<'de> for DateRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawDateRange {
            Preset(String),
            Custom { custom: RawCustomRange },
        }
        #[derive(Deserialize)]
        struct RawCustomRange {
            from: DateTime<Utc>,
            to: DateTime<Utc>,
        }
        match RawDateRange::deserialize(deserializer)? {
            RawDateRange::Preset(name) => match name.as_str() {
                "last_7_days" => Ok(DateRange::Last7Days),
                "last_30_days" => Ok(DateRange::Last30Days),
                "last_90_days" => Ok(DateRange::Last90Days),
                other => Err(serde::de::Error::custom(format!(
                    "unknown date range {other:?}"
                ))),
            },
            RawDateRange::Custom { custom } => {
                DateRange::custom(custom.from, custom.to).map_err(serde::de::Error::custom)
            }
        }
    }
}

impl DateRange {
    /// Creates an explicit window.
    ///
    /// Rejects: `from` after `to`.
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use crm_domain::dashboard::DateRange;
    ///
    /// let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    /// let to = Utc.with_ymd_and_hms(2026, 10, 17, 0, 0, 0).unwrap();
    /// assert!(DateRange::custom(from, to).is_ok());
    /// assert!(DateRange::custom(to, from).is_err());
    /// ```
    pub fn custom(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Self, DomainError> {
        if from > to {
            return Err(DomainError::Validation(
                "custom date range: from must not be after to".to_string(),
            ));
        }
        Ok(DateRange::Custom { from, to })
    }

    /// Machine-readable name of the range.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            DateRange::Last7Days => "last_7_days",
            DateRange::Last30Days => "last_30_days",
            DateRange::Last90Days => "last_90_days",
            DateRange::Custom { .. } => "custom",
        }
    }

    /// The preset window length in days, or `None` for a custom range.
    #[must_use]
    pub fn preset_days(&self) -> Option<u32> {
        match self {
            DateRange::Last7Days => Some(7),
            DateRange::Last30Days => Some(30),
            DateRange::Last90Days => Some(90),
            DateRange::Custom { .. } => None,
        }
    }
}

/// A pure metric query definition: entity, aggregation, optional
/// grouping, date range.
///
/// "Pure" means validation only — the domain never executes the
/// query. Execution is the service layer's job (task 15-DASH-02),
/// which reads the definition through the getters below.
///
/// Construct via [`MetricDefinition::new`] — the fields are private
/// so the `group_by` invariant (non-blank when present) holds for
/// deserialized values too: `Deserialize` is implemented manually
/// through [`MetricDefinition::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricDefinition {
    /// The entity the metric is computed over.
    entity: MetricEntity,
    /// How rows are aggregated.
    aggregation: MetricAggregation,
    /// Optional grouping field (e.g. `"status"`); non-blank when set.
    group_by: Option<String>,
    /// The time window the metric covers.
    date_range: DateRange,
}

impl<'de> Deserialize<'de> for MetricDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawMetric {
            entity: MetricEntity,
            aggregation: MetricAggregation,
            #[serde(default)]
            group_by: Option<String>,
            date_range: DateRange,
        }
        let raw = RawMetric::deserialize(deserializer)?;
        MetricDefinition::new(raw.entity, raw.aggregation, raw.group_by, raw.date_range)
            .map_err(serde::de::Error::custom)
    }
}

impl MetricDefinition {
    /// Defines a metric, validating the optional `group_by`.
    ///
    /// Rejects: a blank `group_by` field name.
    ///
    /// ```
    /// use crm_domain::dashboard::{DateRange, MetricAggregation, MetricDefinition, MetricEntity};
    ///
    /// let metric = MetricDefinition::new(
    ///     MetricEntity::Contact,
    ///     MetricAggregation::count(),
    ///     None,
    ///     DateRange::Last30Days,
    /// )
    /// .expect("valid metric");
    /// assert_eq!(metric.entity(), MetricEntity::Contact);
    /// assert!(MetricDefinition::new(
    ///     MetricEntity::Contact,
    ///     MetricAggregation::count(),
    ///     Some("   ".to_string()),
    ///     DateRange::Last30Days,
    /// )
    /// .is_err());
    /// ```
    pub fn new(
        entity: MetricEntity,
        aggregation: MetricAggregation,
        group_by: Option<String>,
        date_range: DateRange,
    ) -> Result<Self, DomainError> {
        let group_by = match group_by {
            Some(field) => {
                let field = field.trim().to_string();
                if field.is_empty() {
                    return Err(DomainError::Validation(
                        "metric group_by must not be blank".to_string(),
                    ));
                }
                Some(field)
            }
            None => None,
        };
        Ok(MetricDefinition {
            entity,
            aggregation,
            group_by,
            date_range,
        })
    }

    /// The entity the metric is computed over.
    #[must_use]
    pub fn entity(&self) -> MetricEntity {
        self.entity
    }

    /// How rows are aggregated.
    #[must_use]
    pub fn aggregation(&self) -> &MetricAggregation {
        &self.aggregation
    }

    /// The optional grouping field.
    #[must_use]
    pub fn group_by(&self) -> Option<&str> {
        self.group_by.as_deref()
    }

    /// The time window the metric covers.
    #[must_use]
    pub fn date_range(&self) -> &DateRange {
        &self.date_range
    }
}

/// The kind of visualization a widget renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WidgetType {
    /// A single headline number.
    #[serde(rename = "count")]
    Count,
    /// A series of values over time.
    #[serde(rename = "timeseries")]
    Timeseries,
    /// One value per group (requires the metric's `group_by`).
    #[serde(rename = "breakdown")]
    Breakdown,
}

impl WidgetType {
    /// Machine-readable name of the widget type.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WidgetType::Count => "count",
            WidgetType::Timeseries => "timeseries",
            WidgetType::Breakdown => "breakdown",
        }
    }
}

/// Grid position of a widget on a dashboard (0-based column/row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WidgetPosition {
    /// 0-based column.
    pub x: u32,
    /// 0-based row.
    pub y: u32,
}

impl WidgetPosition {
    /// Creates a grid position.
    #[must_use]
    pub fn new(x: u32, y: u32) -> Self {
        WidgetPosition { x, y }
    }
}

/// One widget on a dashboard: title, type, metric, and position.
///
/// Construct via [`DashboardWidget::new`] — the fields are private so
/// the title and type/metric-compatibility invariants hold for
/// deserialized values too: `Deserialize` is implemented manually
/// through the validating constructor.
///
/// Type/metric compatibility rules, enforced at construction:
/// - [`WidgetType::Breakdown`] requires the metric's `group_by` (a
///   breakdown *is* a grouped aggregation);
/// - [`WidgetType::Count`] rejects `group_by` (a grouped count is a
///   breakdown — say what you mean);
/// - [`WidgetType::Timeseries`] allows either (one series, or one
///   series per group).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardWidget {
    /// Stable identifier of this widget.
    widget_id: WidgetId,
    /// Display title.
    title: String,
    /// The kind of visualization.
    widget_type: WidgetType,
    /// The metric this widget executes.
    metric: MetricDefinition,
    /// Grid position on the dashboard.
    position: WidgetPosition,
}

impl<'de> Deserialize<'de> for DashboardWidget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawWidget {
            widget_id: WidgetId,
            title: String,
            widget_type: WidgetType,
            metric: MetricDefinition,
            position: WidgetPosition,
        }
        let raw = RawWidget::deserialize(deserializer)?;
        DashboardWidget::with_id(
            raw.widget_id,
            raw.title,
            raw.widget_type,
            raw.metric,
            raw.position,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl DashboardWidget {
    /// Creates a widget, validating the title and the
    /// type/metric compatibility.
    ///
    /// Rejects: a blank title; a [`WidgetType::Breakdown`] without a
    /// metric `group_by`; a [`WidgetType::Count`] with a metric
    /// `group_by`.
    ///
    /// ```
    /// use crm_domain::dashboard::{
    ///     DashboardWidget, DateRange, MetricAggregation, MetricDefinition, MetricEntity,
    ///     WidgetPosition, WidgetType,
    /// };
    ///
    /// let metric = MetricDefinition::new(
    ///     MetricEntity::Invoice,
    ///     MetricAggregation::sum("total_minor".to_string()).expect("valid"),
    ///     Some("status".to_string()),
    ///     DateRange::Last90Days,
    /// )
    /// .expect("valid metric");
    /// let widget = DashboardWidget::new(
    ///     "Revenue by status".to_string(),
    ///     WidgetType::Breakdown,
    ///     metric,
    ///     WidgetPosition::new(0, 0),
    /// )
    /// .expect("valid widget");
    /// assert_eq!(widget.widget_type(), WidgetType::Breakdown);
    /// ```
    pub fn new(
        title: String,
        widget_type: WidgetType,
        metric: MetricDefinition,
        position: WidgetPosition,
    ) -> Result<Self, DomainError> {
        Self::with_id(WidgetId::new(), title, widget_type, metric, position)
    }

    /// Validating constructor that keeps a caller-supplied id (used
    /// by `Deserialize` so a serialized widget round-trips).
    fn with_id(
        widget_id: WidgetId,
        title: String,
        widget_type: WidgetType,
        metric: MetricDefinition,
        position: WidgetPosition,
    ) -> Result<Self, DomainError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(DomainError::Validation(
                "widget title must not be blank".to_string(),
            ));
        }
        match widget_type {
            WidgetType::Breakdown => {
                if metric.group_by().is_none() {
                    return Err(DomainError::Validation(
                        "breakdown widgets require a metric group_by field".to_string(),
                    ));
                }
            }
            WidgetType::Count => {
                if metric.group_by().is_some() {
                    return Err(DomainError::Validation(
                        "count widgets must not set group_by (use a breakdown widget)".to_string(),
                    ));
                }
            }
            WidgetType::Timeseries => {}
        }
        Ok(DashboardWidget {
            widget_id,
            title,
            widget_type,
            metric,
            position,
        })
    }

    /// Stable identifier of this widget.
    #[must_use]
    pub fn widget_id(&self) -> WidgetId {
        self.widget_id
    }

    /// Display title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The kind of visualization.
    #[must_use]
    pub fn widget_type(&self) -> WidgetType {
        self.widget_type
    }

    /// The metric this widget executes.
    #[must_use]
    pub fn metric(&self) -> &MetricDefinition {
        &self.metric
    }

    /// Grid position on the dashboard.
    #[must_use]
    pub fn position(&self) -> WidgetPosition {
        self.position
    }
}

/// Lifecycle of a dashboard configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DashboardStatus {
    /// Being authored; widgets may be added, removed, renamed.
    #[serde(rename = "draft")]
    Draft,
    /// Frozen configuration, safe to render for viewers.
    #[serde(rename = "published")]
    Published,
}

impl DashboardStatus {
    /// Machine-readable name of the status.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DashboardStatus::Draft => "draft",
            DashboardStatus::Published => "published",
        }
    }
}

/// A saved dashboard: a named, scoped set of widgets.
///
/// Created via [`Dashboard::new`] as a [`DashboardStatus::Draft`];
/// widgets are added with [`Dashboard::add_widget`], which rejects a
/// widget whose grid position is already occupied. [`Dashboard::publish`]
/// freezes the configuration (a published dashboard must carry at
/// least one widget); [`Dashboard::unpublish`] returns it to draft
/// for further editing.
///
/// The fields are private so the name and position-uniqueness
/// invariants hold for deserialized values too: `Deserialize` is
/// implemented manually through the validating constructors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dashboard {
    /// Stable identifier of this dashboard.
    dashboard_id: DashboardId,
    /// Display name.
    name: String,
    /// Owner scope: `None` is agency-level (the master dashboard),
    /// `Some` is one client profile's dashboard.
    client_profile_id: Option<ClientProfileId>,
    /// The dashboard's widgets.
    widgets: Vec<DashboardWidget>,
    /// Draft or published.
    status: DashboardStatus,
    /// When the dashboard was created.
    created_at: DateTime<Utc>,
    /// When the dashboard was last changed.
    updated_at: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for Dashboard {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawDashboard {
            dashboard_id: DashboardId,
            name: String,
            #[serde(default)]
            client_profile_id: Option<ClientProfileId>,
            #[serde(default)]
            widgets: Vec<DashboardWidget>,
            status: DashboardStatus,
            created_at: DateTime<Utc>,
            updated_at: DateTime<Utc>,
        }
        let raw = RawDashboard::deserialize(deserializer)?;
        Dashboard::restored(
            raw.dashboard_id,
            raw.name,
            raw.client_profile_id,
            raw.widgets,
            raw.status,
            raw.created_at,
            raw.updated_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl Dashboard {
    /// Creates a draft dashboard.
    ///
    /// Rejects: a blank name.
    ///
    /// ```
    /// use crm_domain::dashboard::{Dashboard, DashboardStatus};
    /// use crm_domain::ids::ClientProfileId;
    ///
    /// let dashboard = Dashboard::new("Master".to_string(), None).expect("valid dashboard");
    /// assert_eq!(dashboard.status(), DashboardStatus::Draft);
    /// assert!(dashboard.client_profile_id().is_none());
    /// let scoped = Dashboard::new("Client".to_string(), Some(ClientProfileId::new()))
    ///     .expect("valid dashboard");
    /// assert!(scoped.client_profile_id().is_some());
    /// ```
    pub fn new(
        name: String,
        client_profile_id: Option<ClientProfileId>,
    ) -> Result<Self, DomainError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "dashboard name must not be blank".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Dashboard {
            dashboard_id: DashboardId::new(),
            name,
            client_profile_id,
            widgets: Vec::new(),
            status: DashboardStatus::Draft,
            created_at: now,
            updated_at: now,
        })
    }

    /// Restores a dashboard from stored parts, keeping ids and
    /// timestamps (used by `Deserialize` so a serialized dashboard
    /// round-trips).
    fn restored(
        dashboard_id: DashboardId,
        name: String,
        client_profile_id: Option<ClientProfileId>,
        widgets: Vec<DashboardWidget>,
        status: DashboardStatus,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "dashboard name must not be blank".to_string(),
            ));
        }
        let mut seen = HashSet::new();
        for widget in &widgets {
            if !seen.insert(widget.position()) {
                return Err(DomainError::Validation(format!(
                    "two widgets occupy position ({}, {})",
                    widget.position().x,
                    widget.position().y
                )));
            }
        }
        if status == DashboardStatus::Published && widgets.is_empty() {
            return Err(DomainError::Validation(
                "cannot restore a published dashboard with no widgets".to_string(),
            ));
        }
        Ok(Dashboard {
            dashboard_id,
            name,
            client_profile_id,
            widgets,
            status,
            created_at,
            updated_at,
        })
    }

    /// Adds a widget to a draft dashboard.
    ///
    /// Rejects: a published dashboard (unpublish first); a widget
    /// whose grid position is already occupied.
    pub fn add_widget(&mut self, widget: DashboardWidget) -> Result<(), DomainError> {
        self.require_draft("add a widget to")?;
        if self
            .widgets
            .iter()
            .any(|w| w.position() == widget.position())
        {
            return Err(DomainError::Validation(format!(
                "position ({}, {}) is already occupied",
                widget.position().x,
                widget.position().y
            )));
        }
        self.widgets.push(widget);
        self.touch();
        Ok(())
    }

    /// Removes a widget from a draft dashboard.
    ///
    /// Rejects: a published dashboard; an unknown widget id.
    pub fn remove_widget(&mut self, widget_id: WidgetId) -> Result<(), DomainError> {
        self.require_draft("remove a widget from")?;
        let before = self.widgets.len();
        self.widgets.retain(|w| w.widget_id() != widget_id);
        if self.widgets.len() == before {
            return Err(DomainError::NotFound {
                kind: "dashboard_widget".to_string(),
                id: widget_id.to_string(),
            });
        }
        self.touch();
        Ok(())
    }

    /// Renames a draft dashboard.
    ///
    /// Rejects: a published dashboard; a blank name.
    pub fn rename(&mut self, name: String) -> Result<(), DomainError> {
        self.require_draft("rename")?;
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "dashboard name must not be blank".to_string(),
            ));
        }
        self.name = name;
        self.touch();
        Ok(())
    }

    /// Publishes a draft dashboard, freezing its configuration.
    ///
    /// Rejects: an already-published dashboard; a dashboard with no
    /// widgets (a published empty dashboard renders nothing).
    pub fn publish(&mut self) -> Result<(), DomainError> {
        self.require_status(DashboardStatus::Draft, "publish")?;
        if self.widgets.is_empty() {
            return Err(DomainError::Validation(
                "cannot publish a dashboard with no widgets".to_string(),
            ));
        }
        self.status = DashboardStatus::Published;
        self.touch();
        Ok(())
    }

    /// Returns a published dashboard to draft for further editing.
    ///
    /// Rejects: a dashboard that is already a draft.
    pub fn unpublish(&mut self) -> Result<(), DomainError> {
        self.require_status(DashboardStatus::Published, "unpublish")?;
        self.status = DashboardStatus::Draft;
        self.touch();
        Ok(())
    }

    /// Stable identifier of this dashboard.
    #[must_use]
    pub fn dashboard_id(&self) -> DashboardId {
        self.dashboard_id
    }

    /// Display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Owner scope: `None` is agency-level, `Some` is one client
    /// profile's dashboard.
    #[must_use]
    pub fn client_profile_id(&self) -> Option<ClientProfileId> {
        self.client_profile_id
    }

    /// The dashboard's widgets.
    #[must_use]
    pub fn widgets(&self) -> &[DashboardWidget] {
        &self.widgets
    }

    /// Draft or published.
    #[must_use]
    pub fn status(&self) -> DashboardStatus {
        self.status
    }

    /// When the dashboard was created.
    #[must_use]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// When the dashboard was last changed.
    #[must_use]
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    /// Rejects the mutation unless the dashboard is a draft.
    fn require_draft(&self, action: &str) -> Result<(), DomainError> {
        if self.status != DashboardStatus::Draft {
            return Err(DomainError::Validation(format!(
                "cannot {action} a published dashboard (unpublish it first)"
            )));
        }
        Ok(())
    }

    /// Rejects the transition unless the dashboard is in `expected`.
    fn require_status(
        &self,
        expected: DashboardStatus,
        transition: &str,
    ) -> Result<(), DomainError> {
        if self.status != expected {
            return Err(DomainError::Validation(format!(
                "cannot {transition} a dashboard in status {:?}",
                self.status
            )));
        }
        Ok(())
    }

    /// Bumps the modification timestamp.
    fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

/// One named parameter of a [`Report`] (e.g. `"status" = "customer"`).
///
/// Construct via [`ReportParameter::new`]: the name must be
/// non-blank; `Deserialize` is implemented manually through
/// [`ReportParameter::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportParameter {
    /// Parameter name.
    name: String,
    /// Parameter value (may be blank — an empty filter is a filter).
    value: String,
}

impl<'de> Deserialize<'de> for ReportParameter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawParameter {
            name: String,
            #[serde(default)]
            value: String,
        }
        let raw = RawParameter::deserialize(deserializer)?;
        ReportParameter::new(raw.name, raw.value).map_err(serde::de::Error::custom)
    }
}

impl ReportParameter {
    /// Creates a parameter.
    ///
    /// Rejects: a blank name.
    pub fn new(name: String, value: String) -> Result<Self, DomainError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "report parameter name must not be blank".to_string(),
            ));
        }
        Ok(ReportParameter { name, value })
    }

    /// Parameter name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Parameter value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// A saved, parameterized metric query.
///
/// A report is a [`MetricDefinition`] plus the concrete parameter
/// values it runs with, owned by the staff member who saved it.
/// Reports are immutable once created — editing one is a new report
/// (the saved query is the audit trail of what was run).
///
/// Construct via [`Report::new`]: the name must be non-blank and
/// parameter names must be unique. `Deserialize` is implemented
/// manually through the validating constructor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Stable identifier of this report.
    report_id: ReportId,
    /// Display name.
    name: String,
    /// The staff member who saved the report.
    owner: StaffId,
    /// The metric the report runs.
    metric: MetricDefinition,
    /// The parameter values the metric runs with.
    parameters: Vec<ReportParameter>,
    /// When the report was created.
    created_at: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for Report {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawReport {
            report_id: ReportId,
            name: String,
            owner: StaffId,
            metric: MetricDefinition,
            #[serde(default)]
            parameters: Vec<ReportParameter>,
            created_at: DateTime<Utc>,
        }
        let raw = RawReport::deserialize(deserializer)?;
        Report::restored(
            raw.report_id,
            raw.name,
            raw.owner,
            raw.metric,
            raw.parameters,
            raw.created_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl Report {
    /// Saves a report.
    ///
    /// Rejects: a blank name; duplicate parameter names.
    ///
    /// ```
    /// use crm_domain::dashboard::{
    ///     DateRange, MetricAggregation, MetricDefinition, MetricEntity, Report,
    ///     ReportParameter,
    /// };
    /// use crm_domain::ids::StaffId;
    ///
    /// let metric = MetricDefinition::new(
    ///     MetricEntity::Contact,
    ///     MetricAggregation::count(),
    ///     None,
    ///     DateRange::Last30Days,
    /// )
    /// .expect("valid metric");
    /// let report = Report::new(
    ///     "New contacts".to_string(),
    ///     StaffId::new(),
    ///     metric,
    ///     vec![ReportParameter::new("source".to_string(), "referral".to_string())
    ///         .expect("valid parameter")],
    /// )
    /// .expect("valid report");
    /// assert_eq!(report.parameters().len(), 1);
    /// ```
    pub fn new(
        name: String,
        owner: StaffId,
        metric: MetricDefinition,
        parameters: Vec<ReportParameter>,
    ) -> Result<Self, DomainError> {
        Self::restored(ReportId::new(), name, owner, metric, parameters, Utc::now())
    }

    /// Restores a report from stored parts, keeping the id and
    /// timestamp (used by `Deserialize` so a serialized report
    /// round-trips).
    fn restored(
        report_id: ReportId,
        name: String,
        owner: StaffId,
        metric: MetricDefinition,
        parameters: Vec<ReportParameter>,
        created_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "report name must not be blank".to_string(),
            ));
        }
        let mut seen = HashSet::new();
        for parameter in &parameters {
            if !seen.insert(parameter.name()) {
                return Err(DomainError::Validation(format!(
                    "duplicate report parameter {:?}",
                    parameter.name()
                )));
            }
        }
        Ok(Report {
            report_id,
            name,
            owner,
            metric,
            parameters,
            created_at,
        })
    }

    /// Stable identifier of this report.
    #[must_use]
    pub fn report_id(&self) -> ReportId {
        self.report_id
    }

    /// Display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The staff member who saved the report.
    #[must_use]
    pub fn owner(&self) -> StaffId {
        self.owner
    }

    /// The metric the report runs.
    #[must_use]
    pub fn metric(&self) -> &MetricDefinition {
        &self.metric
    }

    /// The parameter values the metric runs with.
    #[must_use]
    pub fn parameters(&self) -> &[ReportParameter] {
        &self.parameters
    }

    /// When the report was created.
    #[must_use]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

/// One point of a timeseries widget's data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeseriesPoint {
    /// Bucket label (e.g. `"2026-10-17"`).
    pub label: String,
    /// Aggregated value for the bucket.
    pub value: f64,
}

/// One group of a breakdown widget's data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakdownBucket {
    /// Group key (e.g. `"customer"`).
    pub key: String,
    /// Aggregated value for the group.
    pub value: f64,
}

/// The computed data behind a widget, by widget type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WidgetData {
    /// Headline number of a [`WidgetType::Count`] widget.
    Count(u64),
    /// Series of a [`WidgetType::Timeseries`] widget.
    Timeseries(Vec<TimeseriesPoint>),
    /// Groups of a [`WidgetType::Breakdown`] widget.
    Breakdown(Vec<BreakdownBucket>),
}

/// The executed result of one widget: which widget, its type, and the
/// computed data.
///
/// Produced by the service layer
/// ([`DashboardService::run_widget`](crate::services::DashboardService::run_widget));
/// the domain only defines the shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetResult {
    /// The widget that was executed.
    pub widget_id: WidgetId,
    /// The widget's type (determines the [`WidgetData`] variant).
    pub widget_type: WidgetType,
    /// The computed data.
    pub data: WidgetData,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn contact_count_metric() -> MetricDefinition {
        MetricDefinition::new(
            MetricEntity::Contact,
            MetricAggregation::count(),
            None,
            DateRange::Last30Days,
        )
        .expect("valid metric")
    }

    fn count_widget_at(x: u32, y: u32) -> DashboardWidget {
        DashboardWidget::new(
            "Contacts".to_string(),
            WidgetType::Count,
            contact_count_metric(),
            WidgetPosition::new(x, y),
        )
        .expect("valid widget")
    }

    #[test]
    fn date_range_custom_validates_order() {
        let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 10, 17, 0, 0, 0).unwrap();
        assert!(DateRange::custom(from, to).is_ok());
        assert!(DateRange::custom(from, from).is_ok());
        assert!(DateRange::custom(to, from).is_err());
    }

    #[test]
    fn date_range_names_are_stable() {
        assert_eq!(DateRange::Last7Days.as_str(), "last_7_days");
        assert_eq!(DateRange::Last30Days.as_str(), "last_30_days");
        assert_eq!(DateRange::Last90Days.as_str(), "last_90_days");
        assert_eq!(
            DateRange::custom(
                Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 10, 2, 0, 0, 0).unwrap()
            )
            .expect("valid range")
            .as_str(),
            "custom"
        );
        assert_eq!(DateRange::Last7Days.preset_days(), Some(7));
        assert_eq!(DateRange::Last90Days.preset_days(), Some(90));
    }

    #[test]
    fn date_range_deserialize_rejects_bad_custom() {
        let bad: Result<DateRange, _> = serde_json::from_str(
            r#"{"custom": {"from": "2026-10-17T00:00:00Z", "to": "2026-10-01T00:00:00Z"}}"#,
        );
        assert!(bad.is_err());
        let unknown: Result<DateRange, _> = serde_json::from_str(r#""last_12_days""#);
        assert!(unknown.is_err());
        let ok: DateRange = serde_json::from_str(r#""last_7_days""#).expect("valid range");
        assert_eq!(ok, DateRange::Last7Days);
    }

    #[test]
    fn date_range_round_trips_through_serde() {
        for range in [
            DateRange::Last7Days,
            DateRange::Last30Days,
            DateRange::Last90Days,
            DateRange::custom(
                Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 10, 17, 0, 0, 0).unwrap(),
            )
            .expect("valid range"),
        ] {
            let json = serde_json::to_string(&range).expect("serializes");
            let back: DateRange = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(range, back);
        }
    }

    #[test]
    fn metric_entity_deserialize_rejects_unknown() {
        let bad: Result<MetricEntity, _> = serde_json::from_str(r#""teleporter""#);
        assert!(bad.is_err());
        let ok: MetricEntity = serde_json::from_str(r#""form_submission""#).expect("valid");
        assert_eq!(ok, MetricEntity::FormSubmission);
        assert_eq!(MetricEntity::Call.as_str(), "call");
    }

    #[test]
    fn metric_aggregation_sum_requires_a_field() {
        assert!(MetricAggregation::sum("   ".to_string()).is_err());
        assert!(MetricAggregation::sum("total_minor".to_string()).is_ok());
        let bad: Result<MetricAggregation, _> = serde_json::from_str(r#""sum""#);
        assert!(bad.is_err());
        let bad_field: Result<MetricAggregation, _> =
            serde_json::from_str(r#"{"sum": {"field": "  "}}"#);
        assert!(bad_field.is_err());
        let unknown: Result<MetricAggregation, _> = serde_json::from_str(r#""median""#);
        assert!(unknown.is_err());
    }

    #[test]
    fn metric_definition_rejects_blank_group_by() {
        assert!(MetricDefinition::new(
            MetricEntity::Contact,
            MetricAggregation::count(),
            Some("  ".to_string()),
            DateRange::Last7Days,
        )
        .is_err());
        let json = r#"{
            "entity": "contact",
            "aggregation": "count",
            "group_by": " ",
            "date_range": "last_7_days"
        }"#;
        assert!(serde_json::from_str::<MetricDefinition>(json).is_err());
    }

    #[test]
    fn metric_definition_deserialize_rejects_unknown_entity() {
        let json = r#"{
            "entity": "teleporter",
            "aggregation": "count",
            "date_range": "last_7_days"
        }"#;
        assert!(serde_json::from_str::<MetricDefinition>(json).is_err());
    }

    #[test]
    fn widget_type_and_metric_must_agree() {
        let grouped = MetricDefinition::new(
            MetricEntity::Contact,
            MetricAggregation::count(),
            Some("lifecycle".to_string()),
            DateRange::Last30Days,
        )
        .expect("valid metric");
        // Breakdown without group_by is rejected.
        assert!(DashboardWidget::new(
            "By lifecycle".to_string(),
            WidgetType::Breakdown,
            contact_count_metric(),
            WidgetPosition::new(0, 0),
        )
        .is_err());
        // Breakdown with group_by is accepted.
        assert!(DashboardWidget::new(
            "By lifecycle".to_string(),
            WidgetType::Breakdown,
            grouped.clone(),
            WidgetPosition::new(0, 0),
        )
        .is_ok());
        // Count with group_by is rejected (that is a breakdown).
        assert!(DashboardWidget::new(
            "Grouped count".to_string(),
            WidgetType::Count,
            grouped,
            WidgetPosition::new(0, 0),
        )
        .is_err());
        // Count without group_by is accepted.
        assert!(DashboardWidget::new(
            "Total".to_string(),
            WidgetType::Count,
            contact_count_metric(),
            WidgetPosition::new(0, 0),
        )
        .is_ok());
        // Timeseries allows either.
        assert!(DashboardWidget::new(
            "Over time".to_string(),
            WidgetType::Timeseries,
            contact_count_metric(),
            WidgetPosition::new(0, 0),
        )
        .is_ok());
        // Blank titles are rejected.
        assert!(DashboardWidget::new(
            "   ".to_string(),
            WidgetType::Count,
            contact_count_metric(),
            WidgetPosition::new(0, 0),
        )
        .is_err());
    }

    #[test]
    fn dashboard_rejects_duplicate_widget_positions() {
        let mut dashboard = Dashboard::new("Master".to_string(), None).expect("valid");
        dashboard.add_widget(count_widget_at(0, 0)).expect("add");
        assert!(dashboard.add_widget(count_widget_at(0, 0)).is_err());
        assert!(dashboard.add_widget(count_widget_at(1, 0)).is_ok());
        assert_eq!(dashboard.widgets().len(), 2);
    }

    #[test]
    fn dashboard_lifecycle_publish_requires_a_widget() {
        let mut dashboard = Dashboard::new("Master".to_string(), None).expect("valid");
        assert_eq!(dashboard.status(), DashboardStatus::Draft);
        // Cannot publish an empty dashboard.
        assert!(dashboard.publish().is_err());
        dashboard.add_widget(count_widget_at(0, 0)).expect("add");
        dashboard.publish().expect("publish");
        assert_eq!(dashboard.status(), DashboardStatus::Published);
        // Double publish is rejected.
        assert!(dashboard.publish().is_err());
        // Published dashboards reject mutations.
        assert!(dashboard.add_widget(count_widget_at(1, 0)).is_err());
        assert!(dashboard.remove_widget(WidgetId::new()).is_err());
        assert!(dashboard.rename("New".to_string()).is_err());
        // Unpublish returns to draft; unpublishing a draft is rejected.
        dashboard.unpublish().expect("unpublish");
        assert_eq!(dashboard.status(), DashboardStatus::Draft);
        assert!(dashboard.unpublish().is_err());
        dashboard.rename("Renamed".to_string()).expect("rename");
        assert_eq!(dashboard.name(), "Renamed");
    }

    #[test]
    fn dashboard_rejects_blank_names() {
        assert!(Dashboard::new("   ".to_string(), None).is_err());
        let mut dashboard = Dashboard::new("Master".to_string(), None).expect("valid");
        assert!(dashboard.rename("  ".to_string()).is_err());
    }

    #[test]
    fn dashboard_remove_widget_rejects_unknown_id() {
        let mut dashboard = Dashboard::new("Master".to_string(), None).expect("valid");
        assert!(dashboard.remove_widget(WidgetId::new()).is_err());
        let widget = count_widget_at(0, 0);
        let id = widget.widget_id();
        dashboard.add_widget(widget).expect("add");
        dashboard.remove_widget(id).expect("remove");
        assert!(dashboard.widgets().is_empty());
    }

    #[test]
    fn dashboard_round_trips_through_serde() {
        let mut dashboard = Dashboard::new("Master".to_string(), None).expect("valid");
        dashboard.add_widget(count_widget_at(0, 0)).expect("add");
        dashboard.publish().expect("publish");
        let json = serde_json::to_string(&dashboard).expect("serializes");
        let back: Dashboard = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(dashboard, back);
    }

    #[test]
    fn dashboard_deserialize_rejects_duplicate_positions() {
        let mut dashboard = Dashboard::new("Master".to_string(), None).expect("valid");
        dashboard.add_widget(count_widget_at(0, 0)).expect("add");
        let mut json = serde_json::to_value(&dashboard).expect("serializes");
        // Duplicate the single widget: two widgets at (0, 0).
        let widget = json["widgets"][0].clone();
        json["widgets"].as_array_mut().expect("array").push(widget);
        assert!(serde_json::from_value::<Dashboard>(json).is_err());
    }

    #[test]
    fn report_rejects_duplicate_parameter_names() {
        let param = |name: &str| {
            ReportParameter::new(name.to_string(), "x".to_string()).expect("valid parameter")
        };
        assert!(Report::new(
            "R".to_string(),
            StaffId::new(),
            contact_count_metric(),
            vec![param("a"), param("a")],
        )
        .is_err());
        assert!(Report::new(
            "R".to_string(),
            StaffId::new(),
            contact_count_metric(),
            vec![param("a"), param("b")],
        )
        .is_ok());
        assert!(Report::new(
            "   ".to_string(),
            StaffId::new(),
            contact_count_metric(),
            vec![],
        )
        .is_err());
        assert!(ReportParameter::new("  ".to_string(), "x".to_string()).is_err());
    }

    #[test]
    fn report_round_trips_through_serde() {
        let report = Report::new(
            "Weekly".to_string(),
            StaffId::new(),
            contact_count_metric(),
            vec![
                ReportParameter::new("source".to_string(), "referral".to_string())
                    .expect("valid parameter"),
            ],
        )
        .expect("valid report");
        let json = serde_json::to_string(&report).expect("serializes");
        let back: Report = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(report, back);
        assert_eq!(back.owner(), report.owner());
    }
}
