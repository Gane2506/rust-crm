// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Analytics vertical slice (task 15-DASH-02).
//!
//! [`PgDashboardRepository`] implements
//! [`crm_domain::repositories::DashboardRepository`] over the
//! `dashboards` and `dashboard_widgets` tables (migration 0015), and
//! [`PgReportRepository`] implements
//! [`crm_domain::repositories::ReportRepository`] over `reports`.
//!
//! Metric definitions travel as JSONB and are hydrated through the
//! domain's manual `Deserialize` impls (validating constructors), so
//! a stored metric can never violate a domain invariant: a corrupt
//! row surfaces as a storage error, never as a bypassed check.
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::dashboard::{
    Dashboard, DashboardStatus, DashboardWidget, Report, WidgetPosition, WidgetType,
};
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, DashboardId, ReportId, StaffId, WidgetId};
use crm_domain::repositories::{DashboardRepository, ReportRepository};

use crate::postgres::{ensure_client_profile, map_storage_error};

/// Renders a [`DashboardStatus`] as its stable database string.
fn dashboard_status_as_str(status: DashboardStatus) -> &'static str {
    status.as_str()
}

/// Parses a [`DashboardStatus`] from its database string.
fn dashboard_status_from_str(raw: &str) -> Result<DashboardStatus, DomainError> {
    match raw {
        "draft" => Ok(DashboardStatus::Draft),
        "published" => Ok(DashboardStatus::Published),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown dashboard status '{other}'"
        ))),
    }
}

/// Renders a [`WidgetType`] as its stable database string.
fn widget_type_as_str(widget_type: WidgetType) -> &'static str {
    widget_type.as_str()
}

/// Serializes a value to JSON, mapping failures to a storage error.
fn to_json<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(value).map_err(|error| {
        DomainError::Validation(format!("storage error: cannot serialize metric: {error}"))
    })
}

/// Loads the widgets of one dashboard, ordered by grid position.
/// Each widget is hydrated through the domain's validating
/// `Deserialize` impl, so a corrupt row fails loudly here.
async fn load_widgets(
    pool: &PgPool,
    dashboard_id: DashboardId,
) -> Result<Vec<serde_json::Value>, DomainError> {
    let rows = sqlx::query(
        "SELECT widget_id, title, widget_type, metric, position_x, position_y \
         FROM dashboard_widgets WHERE dashboard_id = $1 \
         ORDER BY position_y ASC, position_x ASC",
    )
    .bind(dashboard_id.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(map_storage_error)?;
    let mut widgets = Vec::with_capacity(rows.len());
    for row in rows {
        let widget_id = WidgetId::from(
            row.try_get::<Uuid, _>("widget_id")
                .map_err(map_storage_error)?,
        );
        widgets.push(serde_json::json!({
            "widget_id": to_json(&widget_id)?,
            "title": row.try_get::<String, _>("title").map_err(map_storage_error)?,
            "widget_type": row.try_get::<String, _>("widget_type").map_err(map_storage_error)?,
            "metric": row.try_get::<serde_json::Value, _>("metric").map_err(map_storage_error)?,
            "position": {
                "x": row.try_get::<i32, _>("position_x").map_err(map_storage_error)?,
                "y": row.try_get::<i32, _>("position_y").map_err(map_storage_error)?,
            },
        }));
    }
    Ok(widgets)
}

/// Hydrates a `dashboards` row into a [`Dashboard`], validating the
/// whole aggregate through the domain's manual `Deserialize` impl.
async fn row_to_dashboard(
    pool: &PgPool,
    row: &sqlx::postgres::PgRow,
) -> Result<Dashboard, DomainError> {
    let dashboard_id = DashboardId::from(
        row.try_get::<Uuid, _>("dashboard_id")
            .map_err(map_storage_error)?,
    );
    let owner: Option<Uuid> = row
        .try_get::<Option<Uuid>, _>("owner_client_profile_id")
        .map_err(map_storage_error)?;
    let owner = match owner {
        Some(id) => Some(to_json(&ClientProfileId::from(id))?),
        None => None,
    };
    let json = serde_json::json!({
        "dashboard_id": to_json(&dashboard_id)?,
        "name": row.try_get::<String, _>("name").map_err(map_storage_error)?,
        "client_profile_id": owner,
        "widgets": load_widgets(pool, dashboard_id).await?,
        "status": dashboard_status_from_str(
            &row.try_get::<String, _>("status").map_err(map_storage_error)?,
        )?.as_str(),
        "created_at": row.try_get::<DateTime<Utc>, _>("created_at").map_err(map_storage_error)?,
        "updated_at": row.try_get::<DateTime<Utc>, _>("updated_at").map_err(map_storage_error)?,
    });
    serde_json::from_value(json).map_err(|error| {
        DomainError::Validation(format!("storage error: invalid dashboard row: {error}"))
    })
}

/// Hydrates a `reports` row into a [`Report`], validating it through
/// the domain's manual `Deserialize` impl.
fn row_to_report(row: &sqlx::postgres::PgRow) -> Result<Report, DomainError> {
    let report_id = ReportId::from(
        row.try_get::<Uuid, _>("report_id")
            .map_err(map_storage_error)?,
    );
    let owner = StaffId::from(
        row.try_get::<Uuid, _>("owner_staff_id")
            .map_err(map_storage_error)?,
    );
    let json = serde_json::json!({
        "report_id": to_json(&report_id)?,
        "name": row.try_get::<String, _>("name").map_err(map_storage_error)?,
        "owner": to_json(&owner)?,
        "metric": row.try_get::<serde_json::Value, _>("metric").map_err(map_storage_error)?,
        "parameters": row.try_get::<serde_json::Value, _>("parameters").map_err(map_storage_error)?,
        "created_at": row.try_get::<DateTime<Utc>, _>("created_at").map_err(map_storage_error)?,
    });
    serde_json::from_value(json).map_err(|error| {
        DomainError::Validation(format!("storage error: invalid report row: {error}"))
    })
}

/// Inserts one widget row inside an open transaction.
async fn insert_widget(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    dashboard_id: DashboardId,
    widget: &DashboardWidget,
) -> Result<(), DomainError> {
    let position: WidgetPosition = widget.position();
    sqlx::query(
        "INSERT INTO dashboard_widgets \
         (widget_id, dashboard_id, title, widget_type, metric, position_x, position_y) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(widget.widget_id().as_uuid())
    .bind(dashboard_id.as_uuid())
    .bind(widget.title())
    .bind(widget_type_as_str(widget.widget_type()))
    .bind(to_json(widget.metric())?)
    .bind(position.x as i32)
    .bind(position.y as i32)
    .execute(&mut **tx)
    .await
    .map_err(map_storage_error)?;
    Ok(())
}

/// PostgreSQL [`DashboardRepository`](crm_domain::repositories::DashboardRepository).
#[derive(Debug, Clone)]
pub struct PgDashboardRepository {
    pool: PgPool,
}

impl PgDashboardRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl DashboardRepository for PgDashboardRepository {
    async fn save(&self, dashboard: Dashboard) -> Result<Dashboard, DomainError> {
        if let Some(profile_id) = dashboard.client_profile_id() {
            ensure_client_profile(&self.pool, profile_id).await?;
        }
        let mut tx = self.pool.begin().await.map_err(map_storage_error)?;
        let result = sqlx::query(
            "INSERT INTO dashboards \
             (dashboard_id, name, owner_client_profile_id, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(dashboard.dashboard_id().as_uuid())
        .bind(dashboard.name())
        .bind(dashboard.client_profile_id().map(|id| id.as_uuid()))
        .bind(dashboard_status_as_str(dashboard.status()))
        .bind(dashboard.created_at())
        .bind(dashboard.updated_at())
        .execute(&mut *tx)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() != 1 {
            return Err(DomainError::Validation(
                "storage error: dashboard insert affected no rows".to_string(),
            ));
        }
        for widget in dashboard.widgets() {
            insert_widget(&mut tx, dashboard.dashboard_id(), widget).await?;
        }
        tx.commit().await.map_err(map_storage_error)?;
        self.get(dashboard.dashboard_id()).await
    }

    async fn get(&self, dashboard_id: DashboardId) -> Result<Dashboard, DomainError> {
        let row = sqlx::query(
            "SELECT dashboard_id, name, owner_client_profile_id, status, created_at, updated_at \
             FROM dashboards WHERE dashboard_id = $1",
        )
        .bind(dashboard_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_dashboard(&self.pool, &row).await,
            None => Err(DomainError::NotFound {
                kind: "dashboard".to_string(),
                id: dashboard_id.to_string(),
            }),
        }
    }

    async fn update(&self, dashboard: Dashboard) -> Result<Dashboard, DomainError> {
        // The domain aggregate is the source of truth: replace the
        // widget set wholesale inside one transaction so a failed
        // widget insert can never leave a half-written dashboard.
        let mut tx = self.pool.begin().await.map_err(map_storage_error)?;
        let result = sqlx::query(
            "UPDATE dashboards SET name = $2, status = $3, updated_at = $4 \
             WHERE dashboard_id = $1",
        )
        .bind(dashboard.dashboard_id().as_uuid())
        .bind(dashboard.name())
        .bind(dashboard_status_as_str(dashboard.status()))
        .bind(dashboard.updated_at())
        .execute(&mut *tx)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() != 1 {
            return Err(DomainError::NotFound {
                kind: "dashboard".to_string(),
                id: dashboard.dashboard_id().to_string(),
            });
        }
        sqlx::query("DELETE FROM dashboard_widgets WHERE dashboard_id = $1")
            .bind(dashboard.dashboard_id().as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_storage_error)?;
        for widget in dashboard.widgets() {
            insert_widget(&mut tx, dashboard.dashboard_id(), widget).await?;
        }
        tx.commit().await.map_err(map_storage_error)?;
        self.get(dashboard.dashboard_id()).await
    }

    async fn delete(&self, dashboard_id: DashboardId) -> Result<(), DomainError> {
        let result = sqlx::query("DELETE FROM dashboards WHERE dashboard_id = $1")
            .bind(dashboard_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        if result.rows_affected() != 1 {
            return Err(DomainError::NotFound {
                kind: "dashboard".to_string(),
                id: dashboard_id.to_string(),
            });
        }
        Ok(())
    }

    async fn list(
        &self,
        client_profile_id: Option<ClientProfileId>,
        limit: u64,
    ) -> Result<Vec<Dashboard>, DomainError> {
        // `None` is the agency-level master dashboards; a profile
        // lists only its own.
        let rows = match client_profile_id {
            Some(profile_id) => {
                sqlx::query(
                    "SELECT dashboard_id, name, owner_client_profile_id, status, created_at, updated_at \
                     FROM dashboards WHERE owner_client_profile_id = $1 \
                     ORDER BY created_at DESC, dashboard_id DESC LIMIT $2",
                )
                .bind(profile_id.as_uuid())
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(map_storage_error)?
            }
            None => {
                sqlx::query(
                    "SELECT dashboard_id, name, owner_client_profile_id, status, created_at, updated_at \
                     FROM dashboards WHERE owner_client_profile_id IS NULL \
                     ORDER BY created_at DESC, dashboard_id DESC LIMIT $2",
                )
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(map_storage_error)?
            }
        };
        let mut dashboards = Vec::with_capacity(rows.len());
        for row in rows {
            dashboards.push(row_to_dashboard(&self.pool, &row).await?);
        }
        Ok(dashboards)
    }
}

/// PostgreSQL [`ReportRepository`](crm_domain::repositories::ReportRepository).
#[derive(Debug, Clone)]
pub struct PgReportRepository {
    pool: PgPool,
}

impl PgReportRepository {
    /// Creates the repository over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl ReportRepository for PgReportRepository {
    async fn save(&self, report: Report) -> Result<Report, DomainError> {
        let result = sqlx::query(
            "INSERT INTO reports \
             (report_id, name, owner_staff_id, metric, parameters, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(report.report_id().as_uuid())
        .bind(report.name())
        .bind(report.owner().as_uuid())
        .bind(to_json(report.metric())?)
        .bind(to_json(&report.parameters().to_vec())?)
        .bind(report.created_at())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() != 1 {
            return Err(DomainError::Validation(
                "storage error: report insert affected no rows".to_string(),
            ));
        }
        self.get(report.report_id()).await
    }

    async fn get(&self, report_id: ReportId) -> Result<Report, DomainError> {
        let row = sqlx::query(
            "SELECT report_id, name, owner_staff_id, metric, parameters, created_at \
             FROM reports WHERE report_id = $1",
        )
        .bind(report_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_report(&row),
            None => Err(DomainError::NotFound {
                kind: "report".to_string(),
                id: report_id.to_string(),
            }),
        }
    }

    async fn list(&self, limit: u64) -> Result<Vec<Report>, DomainError> {
        let rows = sqlx::query(
            "SELECT report_id, name, owner_staff_id, metric, parameters, created_at \
             FROM reports ORDER BY created_at DESC, report_id DESC LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_report).collect()
    }
}
