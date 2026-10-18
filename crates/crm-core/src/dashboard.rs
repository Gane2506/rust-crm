// SPDX-License-Identifier: GPL-3.0-only
//! Analytics service implementation (task 15-DASH-02).
//!
//! [`DashboardServiceImpl`] implements
//! [`DashboardService`](crm_domain::services::DashboardService) over
//! the PostgreSQL dashboard/report repositories. It drives the
//! [`Dashboard`](crm_domain::dashboard::Dashboard) lifecycle, and
//! executes widget metrics directly against the entity tables with
//! SQL built *only* from a hardcoded allowlist: the entity, the
//! grouping field, and the sum field are all resolved through
//! [`entity_plan`] match arms, so no caller-controlled string ever
//! reaches the query text. Dates and the profile filter travel as
//! bound parameters.
//!
//! Scope rules:
//! - A profile-scoped context may only touch dashboards owned by its
//!   own profile; agency-level dashboards require an agency-scoped
//!   context (a profile must never see agency-wide aggregates).
//! - An agency-scoped context may run any dashboard; the metric
//!   executes in the dashboard's own scope (`None` = all profiles).
//!
//! Audit failures are swallowed: audit is fail-loud at the HTTP layer
//! and never fails an analytics operation (same convention as
//! [`VoiceServiceImpl`](crate::voice::VoiceServiceImpl)).
//!
//! ## Metric execution
//!
//! Each [`MetricEntity`](crm_domain::dashboard::MetricEntity) maps to
//! one table and one timestamp column (see [`entity_plan`]). `Sum` is
//! only supported for `payment`/`amount` — every other entity has no
//! natural numeric column, and the service rejects the metric with a
//! clear validation error rather than guessing. Timeseries widgets
//! bucket by day and zero-fill missing days (ungrouped); a grouped
//! timeseries emits one point per group per day with the label
//! `"<key> · <date>"`.

use std::collections::HashMap;
use std::future::Future;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::{PgPool, Row};

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::dashboard::{
    BreakdownBucket, Dashboard, DateRange, MetricAggregation, MetricDefinition, MetricEntity,
    TimeseriesPoint, WidgetData, WidgetResult, WidgetType, DASHBOARD_DASHBOARD_CREATED,
    DASHBOARD_DASHBOARD_DELETED, DASHBOARD_DASHBOARD_UPDATED, DASHBOARD_REPORT_CREATED,
};
use crm_domain::error::DomainError;
use crm_domain::ids::ClientProfileId;
use crm_domain::ids::{DashboardId, WidgetId};
use crm_domain::repositories::{DashboardRepository, ReportRepository};
use crm_domain::services::{
    ActionContext, AuditService, DashboardService, NewDashboard, NewReport, UpdateDashboard,
};

/// The query plan for one metric entity: which table to count, which
/// timestamp column the date range applies to, which text columns may
/// be grouped by, and which numeric columns may be summed.
///
/// The table and column names here are the *only* strings that ever
/// reach a query's text; [`MetricDefinition::group_by`] and the sum
/// field are validated against these allowlists first.
struct EntityPlan {
    /// Entity table.
    table: &'static str,
    /// Timestamp column the [`DateRange`] filters on.
    timestamp_col: &'static str,
    /// Allowed `(field name, column)` groupings.
    group_cols: &'static [(&'static str, &'static str)],
    /// Allowed `(field name, column)` sums.
    sum_cols: &'static [(&'static str, &'static str)],
}

/// Resolves the query plan for an entity. Every string in the
/// returned plan is a compile-time constant.
fn entity_plan(entity: MetricEntity) -> EntityPlan {
    match entity {
        MetricEntity::Contact => EntityPlan {
            table: "contacts",
            timestamp_col: "created_at",
            group_cols: &[("lifecycle", "lifecycle")],
            sum_cols: &[],
        },
        MetricEntity::Campaign => EntityPlan {
            table: "campaigns",
            timestamp_col: "created_at",
            group_cols: &[("delivery_state", "delivery_state"), ("channel", "channel")],
            sum_cols: &[],
        },
        MetricEntity::Invoice => EntityPlan {
            table: "invoices",
            timestamp_col: "created_at",
            group_cols: &[("status", "status")],
            sum_cols: &[],
        },
        MetricEntity::Payment => EntityPlan {
            table: "payments",
            timestamp_col: "recorded_at",
            group_cols: &[("status", "status")],
            sum_cols: &[("amount", "amount")],
        },
        MetricEntity::Call => EntityPlan {
            table: "calls",
            timestamp_col: "created_at",
            group_cols: &[("status", "status"), ("direction", "direction")],
            sum_cols: &[],
        },
        MetricEntity::Email => EntityPlan {
            table: "email_messages",
            timestamp_col: "created_at",
            group_cols: &[("status", "status"), ("direction", "direction")],
            sum_cols: &[],
        },
        MetricEntity::FormSubmission => EntityPlan {
            table: "form_submissions",
            timestamp_col: "submitted_at",
            group_cols: &[("status", "status")],
            sum_cols: &[],
        },
        MetricEntity::Contract => EntityPlan {
            table: "contracts",
            timestamp_col: "created_at",
            group_cols: &[("status", "status")],
            sum_cols: &[],
        },
    }
}

/// Resolves a date range to an inclusive `(from, to)` window. Preset
/// ranges count back from now.
fn window(range: &DateRange) -> (DateTime<Utc>, DateTime<Utc>) {
    let now = Utc::now();
    match range {
        DateRange::Last7Days => (now - Duration::days(7), now),
        DateRange::Last30Days => (now - Duration::days(30), now),
        DateRange::Last90Days => (now - Duration::days(90), now),
        DateRange::Custom { from, to } => (*from, *to),
    }
}

/// PostgreSQL-backed implementation of
/// [`DashboardService`](crm_domain::services::DashboardService).
///
/// Type parameters: `DR` dashboard repository, `RR` report
/// repository, `A` audit service. Metric queries run against `pool`
/// directly (read-only); the entity tables live in the same
/// database.
#[derive(Debug, Clone)]
pub struct DashboardServiceImpl<DR, RR, A> {
    dashboards: DR,
    reports: RR,
    pool: PgPool,
    audit: A,
}

impl<DR, RR, A> DashboardServiceImpl<DR, RR, A> {
    /// Creates the service over the repositories, the database pool
    /// (for metric execution), and the audit service.
    pub fn new(dashboards: DR, reports: RR, pool: PgPool, audit: A) -> Self {
        Self {
            dashboards,
            reports,
            pool,
            audit,
        }
    }

    /// Fetches a dashboard and enforces that it is visible in the
    /// caller's scope: a profile-scoped context may only touch its
    /// own profile's dashboards, and agency-level dashboards require
    /// an agency-scoped context. An agency-scoped context (`None`)
    /// passes — grant checks happen upstream, at the HTTP layer.
    async fn scoped_dashboard(
        &self,
        dashboard_id: DashboardId,
        ctx: &ActionContext,
    ) -> Result<Dashboard, DomainError>
    where
        DR: DashboardRepository,
    {
        let dashboard = self.dashboards.get(dashboard_id).await?;
        check_dashboard_scope(ctx, &dashboard)?;
        Ok(dashboard)
    }

    /// Records an audit event for an analytics operation. Failures
    /// are swallowed (audit is fail-loud at the HTTP layer, never
    /// fails the operation).
    async fn audit_dashboard(
        &self,
        ctx: &ActionContext,
        action: &str,
        target_kind: &str,
        target_id: &str,
    ) where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                action,
                target_kind,
                target_id,
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                None,
            )
            .await;
    }

    /// Executes one widget's metric and returns the typed result.
    /// The `scope` is the profile the metric counts over (`None` =
    /// all profiles, for agency-level dashboards viewed with an
    /// agency-scoped context).
    async fn execute_widget(
        &self,
        widget_id: WidgetId,
        widget_type: WidgetType,
        metric: &MetricDefinition,
        scope: Option<ClientProfileId>,
    ) -> Result<WidgetResult, DomainError> {
        let plan = entity_plan(metric.entity());
        let group_col = match metric.group_by() {
            None => None,
            Some(field) => Some(
                plan.group_cols
                    .iter()
                    .find(|(name, _)| *name == field)
                    .map(|(_, col)| *col)
                    .ok_or_else(|| {
                        DomainError::Validation(format!(
                            "unsupported group_by field {field:?} for entity {:?}",
                            metric.entity()
                        ))
                    })?,
            ),
        };
        let sum_col = match metric.aggregation() {
            MetricAggregation::Count => None,
            MetricAggregation::Sum { field } => Some(
                plan.sum_cols
                    .iter()
                    .find(|(name, _)| *name == field)
                    .map(|(_, col)| *col)
                    .ok_or_else(|| {
                        DomainError::Validation(format!(
                            "sum over field {field:?} is not supported for entity {:?}",
                            metric.entity()
                        ))
                    })?,
            ),
        };
        let (from, to) = window(metric.date_range());
        let data = match widget_type {
            WidgetType::Count => self
                .run_count(&plan, group_col, sum_col, scope, from, to)
                .await
                .map(WidgetData::Count)?,
            WidgetType::Breakdown => WidgetData::Breakdown(
                self.run_breakdown(&plan, group_col, sum_col, scope, from, to)
                    .await?,
            ),
            WidgetType::Timeseries => WidgetData::Timeseries(
                self.run_timeseries(&plan, group_col, sum_col, scope, from, to)
                    .await?,
            ),
        };
        Ok(WidgetResult {
            widget_id,
            widget_type,
            data,
        })
    }

    /// Builds the shared `FROM … WHERE` clause. Only allowlisted
    /// identifiers are interpolated; every value is a bound
    /// parameter. Date bounds are `${p}` markers, resolved by
    /// [`bind_dates`](Self::bind_dates).
    fn where_clause(plan: &EntityPlan, has_scope: bool) -> String {
        let mut clause = format!("FROM {} WHERE ", plan.table);
        if has_scope {
            clause.push_str("client_profile_id = $1 AND ");
        }
        clause.push_str(&format!(
            "{} >= ${{p}} AND {} <= ${{p}}",
            plan.timestamp_col, plan.timestamp_col
        ));
        clause
    }

    /// Runs a headline count/sum metric.
    async fn run_count(
        &self,
        plan: &EntityPlan,
        group_col: Option<&'static str>,
        sum_col: Option<&'static str>,
        scope: Option<ClientProfileId>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<u64, DomainError> {
        let _ = group_col;
        let select = match sum_col {
            None => "SELECT COUNT(*)::BIGINT AS v ".to_string(),
            Some(col) => format!("SELECT COALESCE(SUM({col}), 0)::BIGINT AS v "),
        };
        let mut sql = Self::where_clause(plan, scope.is_some());
        sql.insert_str(0, &select);
        let sql = Self::bind_dates(sql, scope.is_some());
        let mut query = sqlx::query_scalar::<sqlx::Postgres, i64>(&sql);
        if let Some(profile_id) = scope {
            query = query.bind(profile_id.as_uuid());
        }
        let value: i64 = query
            .bind(from)
            .bind(to)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                DomainError::Validation(format!("storage error: metric query failed: {error}"))
            })?;
        Ok(value.max(0) as u64)
    }

    /// Runs a breakdown metric: one value per group.
    async fn run_breakdown(
        &self,
        plan: &EntityPlan,
        group_col: Option<&'static str>,
        sum_col: Option<&'static str>,
        scope: Option<ClientProfileId>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<BreakdownBucket>, DomainError> {
        // The domain guarantees a breakdown widget carries a
        // `group_by`; this is defense in depth.
        let group_col = group_col.ok_or_else(|| {
            DomainError::Validation("breakdown metrics require a group_by field".to_string())
        })?;
        let value_expr = match sum_col {
            None => "COUNT(*)::BIGINT".to_string(),
            Some(col) => format!("COALESCE(SUM({col}), 0)::DOUBLE PRECISION"),
        };
        let select = format!("SELECT {group_col} AS k, {value_expr} AS v ");
        let mut sql = Self::where_clause(plan, scope.is_some());
        sql.insert_str(0, &select);
        sql.push_str(" GROUP BY 1 ORDER BY 2 DESC");
        let sql = Self::bind_dates(sql, scope.is_some());
        let mut query = sqlx::query(&sql);
        if let Some(profile_id) = scope {
            query = query.bind(profile_id.as_uuid());
        }
        let rows = query
            .bind(from)
            .bind(to)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                DomainError::Validation(format!("storage error: metric query failed: {error}"))
            })?;
        let mut buckets = Vec::with_capacity(rows.len());
        for row in rows {
            let key: String = row.try_get("k").map_err(|error| {
                DomainError::Validation(format!("storage error: bad breakdown row: {error}"))
            })?;
            let value: f64 = if sum_col.is_some() {
                row.try_get("v")
            } else {
                row.try_get::<i64, _>("v").map(|v| v as f64)
            }
            .map_err(|error| {
                DomainError::Validation(format!("storage error: bad breakdown row: {error}"))
            })?;
            buckets.push(BreakdownBucket { key, value });
        }
        Ok(buckets)
    }

    /// Runs a timeseries metric, bucketed by day. Ungrouped series
    /// zero-fill missing days; a grouped series emits one point per
    /// group per day labeled `"<key> · <date>"`.
    async fn run_timeseries(
        &self,
        plan: &EntityPlan,
        group_col: Option<&'static str>,
        sum_col: Option<&'static str>,
        scope: Option<ClientProfileId>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TimeseriesPoint>, DomainError> {
        let value_expr = match sum_col {
            None => "COUNT(*)::BIGINT".to_string(),
            Some(col) => format!("COALESCE(SUM({col}), 0)::DOUBLE PRECISION"),
        };
        let select = match group_col {
            None => format!(
                "SELECT date_trunc('day', {ts})::DATE AS d, {value_expr} AS v ",
                ts = plan.timestamp_col
            ),
            Some(col) => format!(
                "SELECT {col} AS k, date_trunc('day', {ts})::DATE AS d, {value_expr} AS v ",
                ts = plan.timestamp_col
            ),
        };
        let mut sql = Self::where_clause(plan, scope.is_some());
        sql.insert_str(0, &select);
        match group_col {
            None => sql.push_str(" GROUP BY 1 ORDER BY 1"),
            Some(_) => sql.push_str(" GROUP BY 1, 2 ORDER BY 2, 1"),
        }
        let sql = Self::bind_dates(sql, scope.is_some());
        let mut query = sqlx::query(&sql);
        if let Some(profile_id) = scope {
            query = query.bind(profile_id.as_uuid());
        }
        let rows = query
            .bind(from)
            .bind(to)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                DomainError::Validation(format!("storage error: metric query failed: {error}"))
            })?;
        let as_f64 = |row: &sqlx::postgres::PgRow| -> Result<f64, DomainError> {
            if sum_col.is_some() {
                row.try_get("v")
            } else {
                row.try_get::<i64, _>("v").map(|v| v as f64)
            }
            .map_err(|error| {
                DomainError::Validation(format!("storage error: bad timeseries row: {error}"))
            })
        };
        match group_col {
            None => {
                let mut by_day: HashMap<NaiveDate, f64> = HashMap::new();
                for row in &rows {
                    let day: NaiveDate = row.try_get("d").map_err(|error| {
                        DomainError::Validation(format!(
                            "storage error: bad timeseries row: {error}"
                        ))
                    })?;
                    by_day.insert(day, as_f64(row)?);
                }
                let mut points = Vec::new();
                let mut day = from.date_naive();
                let last = to.date_naive();
                while day <= last {
                    points.push(TimeseriesPoint {
                        label: day.format("%Y-%m-%d").to_string(),
                        value: by_day.get(&day).copied().unwrap_or(0.0),
                    });
                    day = day.succ_opt().ok_or_else(|| {
                        DomainError::Validation("date overflow in timeseries".to_string())
                    })?;
                }
                Ok(points)
            }
            Some(_) => {
                let mut points = Vec::with_capacity(rows.len());
                for row in &rows {
                    let key: String = row.try_get("k").map_err(|error| {
                        DomainError::Validation(format!(
                            "storage error: bad timeseries row: {error}"
                        ))
                    })?;
                    let day: NaiveDate = row.try_get("d").map_err(|error| {
                        DomainError::Validation(format!(
                            "storage error: bad timeseries row: {error}"
                        ))
                    })?;
                    points.push(TimeseriesPoint {
                        label: format!("{key} · {}", day.format("%Y-%m-%d")),
                        value: as_f64(row)?,
                    });
                }
                Ok(points)
            }
        }
    }

    /// Replaces the `${p}` markers in a clause with the concrete
    /// parameter indexes. `has_profile` shifts the date parameters
    /// when the profile filter occupies `$1`.
    fn bind_dates(sql: String, has_profile: bool) -> String {
        let (p1, p2) = if has_profile { (2, 3) } else { (1, 2) };
        sql.replacen("${p}", &format!("${p1}"), 1)
            .replacen("${p}", &format!("${p2}"), 1)
    }
}

/// Enforces dashboard scope: a profile-scoped context may only touch
/// its own profile's dashboards; agency-level dashboards require an
/// agency-scoped context (a profile must never see agency-wide
/// aggregates). An agency-scoped context (`None`) passes.
fn check_dashboard_scope(ctx: &ActionContext, dashboard: &Dashboard) -> Result<(), DomainError> {
    match (ctx.client_profile_id, dashboard.client_profile_id()) {
        (None, _) => Ok(()),
        (Some(ctx_profile), Some(dashboard_profile)) if ctx_profile == dashboard_profile => Ok(()),
        (Some(_), Some(_)) => Err(DomainError::Forbidden(
            "dashboard is not visible in this profile".to_string(),
        )),
        (Some(_), None) => Err(DomainError::Forbidden(
            "agency dashboards require agency scope".to_string(),
        )),
    }
}

/// Enforces scope on dashboard creation: a profile-scoped context may
/// only create dashboards for its own profile, and never
/// agency-level ones.
fn check_create_scope(
    ctx: &ActionContext,
    scope: Option<ClientProfileId>,
) -> Result<(), DomainError> {
    match (ctx.client_profile_id, scope) {
        (None, _) => Ok(()),
        (Some(ctx_profile), Some(new_profile)) if ctx_profile == new_profile => Ok(()),
        (Some(_), Some(_)) => Err(DomainError::Forbidden(
            "cannot create a dashboard for another profile".to_string(),
        )),
        (Some(_), None) => Err(DomainError::Forbidden(
            "agency dashboards require agency scope".to_string(),
        )),
    }
}

// The service trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl<DR, RR, A> DashboardService for DashboardServiceImpl<DR, RR, A>
where
    DR: DashboardRepository + Send + Sync,
    RR: ReportRepository + Send + Sync,
    A: AuditService + Send + Sync,
{
    fn create_dashboard<'a>(
        &'a self,
        input: NewDashboard,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Dashboard, DomainError>> + Send + 'a {
        async move {
            check_create_scope(ctx, input.client_profile_id)?;
            let dashboard = Dashboard::new(input.name, input.client_profile_id)?;
            let dashboard = self.dashboards.save(dashboard).await?;
            self.audit_dashboard(
                ctx,
                DASHBOARD_DASHBOARD_CREATED,
                "dashboard",
                &dashboard.dashboard_id().to_string(),
            )
            .await;
            Ok(dashboard)
        }
    }

    fn update_dashboard<'a>(
        &'a self,
        dashboard_id: DashboardId,
        input: UpdateDashboard,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Dashboard, DomainError>> + Send + 'a {
        async move {
            let mut dashboard = self.scoped_dashboard(dashboard_id, ctx).await?;
            if let Some(name) = input.name {
                dashboard.rename(name)?;
            }
            for widget in input.widgets_to_add {
                dashboard.add_widget(widget)?;
            }
            for widget_id in input.widget_ids_to_remove {
                dashboard.remove_widget(widget_id)?;
            }
            let dashboard = self.dashboards.update(dashboard).await?;
            self.audit_dashboard(
                ctx,
                DASHBOARD_DASHBOARD_UPDATED,
                "dashboard",
                &dashboard.dashboard_id().to_string(),
            )
            .await;
            Ok(dashboard)
        }
    }

    fn get_dashboard<'a>(
        &'a self,
        dashboard_id: DashboardId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Dashboard, DomainError>> + Send + 'a {
        async move { self.scoped_dashboard(dashboard_id, ctx).await }
    }

    fn delete_dashboard<'a>(
        &'a self,
        dashboard_id: DashboardId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<(), DomainError>> + Send + 'a {
        async move {
            self.scoped_dashboard(dashboard_id, ctx).await?;
            self.dashboards.delete(dashboard_id).await?;
            self.audit_dashboard(
                ctx,
                DASHBOARD_DASHBOARD_DELETED,
                "dashboard",
                &dashboard_id.to_string(),
            )
            .await;
            Ok(())
        }
    }

    fn list_dashboards<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<Dashboard>, DomainError>> + Send + 'a {
        async move { self.dashboards.list(ctx.client_profile_id, limit).await }
    }

    fn run_widget<'a>(
        &'a self,
        dashboard_id: DashboardId,
        widget_id: WidgetId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<WidgetResult, DomainError>> + Send + 'a {
        async move {
            let dashboard = self.scoped_dashboard(dashboard_id, ctx).await?;
            let widget = dashboard
                .widgets()
                .iter()
                .find(|widget| widget.widget_id() == widget_id)
                .ok_or_else(|| DomainError::NotFound {
                    kind: "dashboard_widget".to_string(),
                    id: widget_id.to_string(),
                })?;
            // The metric executes in the dashboard's scope when the
            // context is agency-scoped, and in the caller's profile
            // otherwise (which the scope check above already matched
            // to the dashboard's).
            let scope = match ctx.client_profile_id {
                Some(profile_id) => Some(profile_id),
                None => dashboard.client_profile_id(),
            };
            // Read-only: no audit event.
            self.execute_widget(widget_id, widget.widget_type(), widget.metric(), scope)
                .await
        }
    }

    fn create_report<'a>(
        &'a self,
        input: NewReport,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<crm_domain::dashboard::Report, DomainError>> + Send + 'a {
        async move {
            let report = crm_domain::dashboard::Report::new(
                input.name,
                input.owner,
                input.metric,
                input.parameters,
            )?;
            let report = self.reports.save(report).await?;
            self.audit_dashboard(
                ctx,
                DASHBOARD_REPORT_CREATED,
                "report",
                &report.report_id().to_string(),
            )
            .await;
            Ok(report)
        }
    }

    fn list_reports<'a>(
        &'a self,
        limit: u64,
        _ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<crm_domain::dashboard::Report>, DomainError>> + Send + 'a
    {
        async move { self.reports.list(limit).await }
    }
}
