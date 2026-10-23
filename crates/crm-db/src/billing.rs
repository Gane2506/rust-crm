// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repositories for the Billing vertical slice (task 12-BILL-02).
//!
//! [`PgInvoiceRepository`] implements
//! [`crm_domain::repositories::InvoiceRepository`] over the `invoices`
//! and `invoice_lines` tables (migration 0012); [`PgPaymentRepository`]
//! implements [`crm_domain::repositories::PaymentRepository`] over
//! `payments`.
//!
//! Invoices are hydrated whole: `get` and `list_by_profile` return the
//! invoice with its lines in the order added. Lines are never updated
//! or deleted — the domain freezes them at issue — so `update`
//! persists the invoice header and inserts any lines not yet stored.
//! Payments are append-only; `reverse` flips the status.
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crm_domain::billing::{Invoice, InvoiceLine, InvoiceStatus, Money, Payment, PaymentStatus};
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, InvoiceId, InvoiceLineId, PaymentId};
use crm_domain::repositories::{InvoiceRepository, PaymentRepository};

use crate::postgres::{ensure_client_profile, map_storage_error};

/// Renders an [`InvoiceStatus`] as its stable database string.
fn invoice_status_as_str(status: InvoiceStatus) -> &'static str {
    match status {
        InvoiceStatus::Draft => "draft",
        InvoiceStatus::Issued => "issued",
        InvoiceStatus::Paid => "paid",
        InvoiceStatus::Void => "void",
    }
}

/// Parses an [`InvoiceStatus`] from its database string.
fn invoice_status_from_str(raw: &str) -> Result<InvoiceStatus, DomainError> {
    match raw {
        "draft" => Ok(InvoiceStatus::Draft),
        "issued" => Ok(InvoiceStatus::Issued),
        "paid" => Ok(InvoiceStatus::Paid),
        "void" => Ok(InvoiceStatus::Void),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown invoice status '{other}'"
        ))),
    }
}

/// Renders a [`PaymentStatus`] as its stable database string.
fn payment_status_as_str(status: PaymentStatus) -> &'static str {
    match status {
        PaymentStatus::Recorded => "recorded",
        PaymentStatus::Reversed => "reversed",
    }
}

/// Parses a [`PaymentStatus`] from its database string.
fn payment_status_from_str(raw: &str) -> Result<PaymentStatus, DomainError> {
    match raw {
        "recorded" => Ok(PaymentStatus::Recorded),
        "reversed" => Ok(PaymentStatus::Reversed),
        other => Err(DomainError::Validation(format!(
            "storage error: unknown payment status '{other}'"
        ))),
    }
}

/// Hydrates one [`InvoiceLine`] row. The line fields are all `pub`, so
/// hydration builds the struct directly (the validated entry point
/// [`Invoice::add_line`](crm_domain::billing::Invoice::add_line) is
/// for new lines).
fn row_to_invoice_line(row: &sqlx::postgres::PgRow) -> Result<InvoiceLine, DomainError> {
    let unit_price = Money::new(
        row.try_get::<i64, _>("unit_price_amount")
            .map_err(map_storage_error)?,
        row.try_get::<String, _>("unit_price_currency")
            .map_err(map_storage_error)?,
    )
    .map_err(|error| {
        DomainError::Validation(format!("storage error: invalid line money: {error}"))
    })?;
    Ok(InvoiceLine {
        line_id: InvoiceLineId::from(
            row.try_get::<Uuid, _>("line_id")
                .map_err(map_storage_error)?,
        ),
        invoice_id: InvoiceId::from(
            row.try_get::<Uuid, _>("invoice_id")
                .map_err(map_storage_error)?,
        ),
        description: row.try_get("description").map_err(map_storage_error)?,
        quantity: row
            .try_get::<i32, _>("quantity")
            .map_err(map_storage_error)?
            .try_into()
            .map_err(|_| {
                DomainError::Validation("storage error: negative line quantity".to_owned())
            })?,
        unit_price,
    })
}

/// Loads the lines of `invoice_id`, in the order added.
async fn load_lines(pool: &PgPool, invoice_id: InvoiceId) -> Result<Vec<InvoiceLine>, DomainError> {
    let rows = sqlx::query(
        "SELECT line_id, invoice_id, description, quantity, unit_price_amount, unit_price_currency\n         FROM invoice_lines\n         WHERE invoice_id = $1\n         ORDER BY position ASC",
    )
    .bind(invoice_id.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(map_storage_error)?;
    rows.iter().map(row_to_invoice_line).collect()
}

/// Hydrates an `invoices` row plus its lines into a full [`Invoice`].
async fn row_to_invoice(
    pool: &PgPool,
    row: &sqlx::postgres::PgRow,
) -> Result<Invoice, DomainError> {
    let invoice_id = InvoiceId::from(
        row.try_get::<Uuid, _>("invoice_id")
            .map_err(map_storage_error)?,
    );
    let lines = load_lines(pool, invoice_id).await?;
    Ok(Invoice {
        invoice_id,
        client_profile_id: ClientProfileId::from(
            row.try_get::<Uuid, _>("client_profile_id")
                .map_err(map_storage_error)?,
        ),
        status: invoice_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        currency: row.try_get("currency").map_err(map_storage_error)?,
        lines,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(map_storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(map_storage_error)?,
    })
}

/// Columns selected for an `invoices` row.
const INVOICE_COLUMNS: &str =
    "invoice_id, client_profile_id, status, currency, created_at, updated_at";

/// SQLx implementation of [`InvoiceRepository`].
#[derive(Debug, Clone)]
pub struct PgInvoiceRepository {
    pool: PgPool,
}

impl PgInvoiceRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Inserts the lines of `invoice` that are not yet stored. Lines
    /// are never updated or deleted, so `ON CONFLICT DO NOTHING` is
    /// the correct upsert: a re-persisted line is a no-op.
    async fn insert_new_lines(&self, invoice: &Invoice) -> Result<(), DomainError> {
        for (position, line) in invoice.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO invoice_lines (line_id, invoice_id, description, quantity, \
                 unit_price_amount, unit_price_currency, position)\n                 VALUES ($1, $2, $3, $4, $5, $6, $7)\n                 ON CONFLICT (line_id) DO NOTHING",
            )
            .bind(line.line_id.as_uuid())
            .bind(invoice.invoice_id.as_uuid())
            .bind(&line.description)
            .bind(i32::try_from(line.quantity).map_err(|_| {
                DomainError::Validation("line quantity does not fit in i32".to_owned())
            })?)
            .bind(line.unit_price.amount())
            .bind(line.unit_price.currency())
            .bind(position as i32)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        }
        Ok(())
    }
}

impl InvoiceRepository for PgInvoiceRepository {
    async fn create(&self, invoice: Invoice) -> Result<Invoice, DomainError> {
        ensure_client_profile(&self.pool, invoice.client_profile_id).await?;
        sqlx::query(
            "INSERT INTO invoices (invoice_id, client_profile_id, status, currency, created_at, \
             updated_at)\n             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(invoice.invoice_id.as_uuid())
        .bind(invoice.client_profile_id.as_uuid())
        .bind(invoice_status_as_str(invoice.status))
        .bind(&invoice.currency)
        .bind(invoice.created_at)
        .bind(invoice.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        self.insert_new_lines(&invoice).await?;
        Ok(invoice)
    }

    async fn get(&self, invoice_id: InvoiceId) -> Result<Invoice, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {INVOICE_COLUMNS} FROM invoices WHERE invoice_id = $1"
        ))
        .bind(invoice_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_invoice(&self.pool, &row).await,
            None => Err(DomainError::NotFound {
                kind: "invoice".to_owned(),
                id: invoice_id.to_string(),
            }),
        }
    }

    async fn update(&self, invoice: Invoice) -> Result<Invoice, DomainError> {
        let result = sqlx::query(
            "UPDATE invoices\n             SET status = $2, updated_at = $3\n             WHERE invoice_id = $1",
        )
        .bind(invoice.invoice_id.as_uuid())
        .bind(invoice_status_as_str(invoice.status))
        .bind(invoice.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "invoice".to_owned(),
                id: invoice.invoice_id.to_string(),
            });
        }
        self.insert_new_lines(&invoice).await?;
        Ok(invoice)
    }

    async fn update_if_status(
        &self,
        invoice: Invoice,
        expected_status: &[InvoiceStatus],
    ) -> Result<Invoice, DomainError> {
        // The status predicate is part of the UPDATE itself, so the
        // check and the write are atomic: a concurrent transition
        // wins or loses here, never silently overwrites.
        let expected: Vec<String> = expected_status
            .iter()
            .map(|status| invoice_status_as_str(*status).to_owned())
            .collect();
        let result = sqlx::query(
            "UPDATE invoices\n             SET status = $2, updated_at = $3\n             WHERE invoice_id = $1 AND status = ANY($4)",
        )
        .bind(invoice.invoice_id.as_uuid())
        .bind(invoice_status_as_str(invoice.status))
        .bind(invoice.updated_at)
        .bind(expected)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            // Distinguish "no such invoice" from "status moved".
            let exists = sqlx::query("SELECT 1 FROM invoices WHERE invoice_id = $1")
                .bind(invoice.invoice_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
            if exists.is_none() {
                return Err(DomainError::NotFound {
                    kind: "invoice".to_owned(),
                    id: invoice.invoice_id.to_string(),
                });
            }
            return Err(DomainError::ConcurrencyConflict);
        }
        self.insert_new_lines(&invoice).await?;
        Ok(invoice)
    }

    async fn list_by_profile(
        &self,
        client_profile_id: ClientProfileId,
        limit: u64,
    ) -> Result<Vec<Invoice>, DomainError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(&format!(
            "SELECT {INVOICE_COLUMNS} FROM invoices\n             WHERE client_profile_id = $1\n             ORDER BY created_at DESC, invoice_id DESC\n             LIMIT $2"
        ))
        .bind(client_profile_id.as_uuid())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        let mut invoices = Vec::with_capacity(rows.len());
        for row in &rows {
            invoices.push(row_to_invoice(&self.pool, row).await?);
        }
        Ok(invoices)
    }
}

/// Hydrates a `payments` row into a [`Payment`].
fn row_to_payment(row: &sqlx::postgres::PgRow) -> Result<Payment, DomainError> {
    let amount = Money::new(
        row.try_get::<i64, _>("amount").map_err(map_storage_error)?,
        row.try_get::<String, _>("currency")
            .map_err(map_storage_error)?,
    )
    .map_err(|error| {
        DomainError::Validation(format!("storage error: invalid payment money: {error}"))
    })?;
    Ok(Payment {
        payment_id: PaymentId::from(
            row.try_get::<Uuid, _>("payment_id")
                .map_err(map_storage_error)?,
        ),
        invoice_id: InvoiceId::from(
            row.try_get::<Uuid, _>("invoice_id")
                .map_err(map_storage_error)?,
        ),
        amount,
        status: payment_status_from_str(
            row.try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?,
        recorded_at: row
            .try_get::<DateTime<Utc>, _>("recorded_at")
            .map_err(map_storage_error)?,
    })
}

/// Columns selected for a `payments` row.
const PAYMENT_COLUMNS: &str = "payment_id, invoice_id, amount, currency, status, recorded_at";

/// SQLx implementation of [`PaymentRepository`].
#[derive(Debug, Clone)]
pub struct PgPaymentRepository {
    pool: PgPool,
}

impl PgPaymentRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl PaymentRepository for PgPaymentRepository {
    async fn record(&self, payment: Payment) -> Result<Payment, DomainError> {
        sqlx::query(
            "INSERT INTO payments (payment_id, invoice_id, amount, currency, status, recorded_at)\n             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(payment.payment_id.as_uuid())
        .bind(payment.invoice_id.as_uuid())
        .bind(payment.amount.amount())
        .bind(payment.amount.currency())
        .bind(payment_status_as_str(payment.status))
        .bind(payment.recorded_at)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(payment)
    }

    async fn get(&self, payment_id: PaymentId) -> Result<Payment, DomainError> {
        let row = sqlx::query(&format!(
            "SELECT {PAYMENT_COLUMNS} FROM payments WHERE payment_id = $1"
        ))
        .bind(payment_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_storage_error)?;
        match row {
            Some(row) => row_to_payment(&row),
            None => Err(DomainError::NotFound {
                kind: "payment".to_owned(),
                id: payment_id.to_string(),
            }),
        }
    }

    async fn list_by_invoice(&self, invoice_id: InvoiceId) -> Result<Vec<Payment>, DomainError> {
        let rows = sqlx::query(&format!(
            "SELECT {PAYMENT_COLUMNS} FROM payments\n             WHERE invoice_id = $1\n             ORDER BY recorded_at ASC"
        ))
        .bind(invoice_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_storage_error)?;
        rows.iter().map(row_to_payment).collect()
    }

    async fn reverse(&self, payment: Payment) -> Result<Payment, DomainError> {
        let result = sqlx::query(
            "UPDATE payments\n             SET status = 'reversed'\n             WHERE payment_id = $1",
        )
        .bind(payment.payment_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound {
                kind: "payment".to_owned(),
                id: payment.payment_id.to_string(),
            });
        }
        Ok(payment)
    }

    async fn record_guarded(
        &self,
        payment: Payment,
        invoice: &Invoice,
    ) -> Result<Payment, DomainError> {
        let mut tx = self.pool.begin().await.map_err(map_storage_error)?;
        // Lock the invoice row: concurrent `record_guarded` calls (and
        // concurrent status updates) serialize here. The lock is held
        // until commit, so the payments read below are the latest
        // committed state at the moment we act.
        let locked = sqlx::query("SELECT status FROM invoices WHERE invoice_id = $1 FOR UPDATE")
            .bind(payment.invoice_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_storage_error)?;
        let Some(locked) = locked else {
            return Err(DomainError::NotFound {
                kind: "invoice".to_owned(),
                id: payment.invoice_id.to_string(),
            });
        };
        let locked_status = invoice_status_from_str(
            locked
                .try_get::<String, _>("status")
                .map_err(map_storage_error)?
                .as_str(),
        )?;
        // Re-read the payments inside the transaction.
        let rows = sqlx::query(&format!(
            "SELECT {PAYMENT_COLUMNS} FROM payments\n             WHERE invoice_id = $1\n             ORDER BY recorded_at ASC"
        ))
        .bind(payment.invoice_id.as_uuid())
        .fetch_all(&mut *tx)
        .await
        .map_err(map_storage_error)?;
        let payments: Vec<Payment> = rows.iter().map(row_to_payment).collect::<Result<_, _>>()?;
        // Coverage check against the locked state: a payment that would
        // exceed the remaining balance is rejected here, so two
        // concurrent payments can never over-apply.
        let due = invoice.remaining_due(&payments)?;
        if payment.amount.amount() > due.amount() {
            return Err(DomainError::Validation(format!(
                "payment of {} exceeds the remaining balance of {}",
                payment.amount.amount(),
                due.amount()
            )));
        }
        sqlx::query(
            "INSERT INTO payments (payment_id, invoice_id, amount, currency, status, recorded_at)\n             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(payment.payment_id.as_uuid())
        .bind(payment.invoice_id.as_uuid())
        .bind(payment.amount.amount())
        .bind(payment.amount.currency())
        .bind(payment_status_as_str(payment.status))
        .bind(payment.recorded_at)
        .execute(&mut *tx)
        .await
        .map_err(map_storage_error)?;
        // Mark paid when this payment covers the total, in the same
        // transaction — no window for a second writer to see a covered
        // invoice that is still `Issued`.
        let mut payments = payments;
        payments.push(payment.clone());
        let due = invoice.remaining_due(&payments)?;
        if due.amount() == 0 && locked_status == InvoiceStatus::Issued {
            sqlx::query("UPDATE invoices\n             SET status = 'paid', updated_at = $2\n             WHERE invoice_id = $1")
                .bind(payment.invoice_id.as_uuid())
                .bind(Utc::now())
                .execute(&mut *tx)
                .await
                .map_err(map_storage_error)?;
        }
        tx.commit().await.map_err(map_storage_error)?;
        Ok(payment)
    }
}
