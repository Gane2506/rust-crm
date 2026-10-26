// SPDX-License-Identifier: GPL-3.0-only
//! SQLx repository for Stripe webhook event claims (STRIPE-01).
//!
//! [`PgStripeEventRepository`] owns the `stripe_webhook_events` table
//! (migration 0020). One row per Stripe event id; the row is claimed
//! atomically (`INSERT ... ON CONFLICT DO NOTHING`) before the
//! webhook's payment is recorded, so a redelivered event can never
//! double-record. `payment_id` is filled in once the payment exists
//! (NULL while the claim is in flight).
//!
//! All queries use the dynamic `sqlx::query` form (never the `query!`
//! macros), so this crate compiles with no database available; column
//! types are checked at runtime instead.

use sqlx::PgPool;
use uuid::Uuid;

use crm_domain::error::DomainError;
use crm_domain::ids::{InvoiceId, PaymentId};

use crate::postgres::map_storage_error;

/// SQLx repository for Stripe webhook event claims.
#[derive(Debug, Clone)]
pub struct PgStripeEventRepository {
    pool: PgPool,
}

impl PgStripeEventRepository {
    /// Creates a repository backed by `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Atomically claims `event_id` for this webhook delivery.
    ///
    /// Returns `true` when this caller won the claim (the row did not
    /// exist and was just inserted); `false` when another delivery
    /// already claimed the event. The claim is the dedupe arbiter:
    /// the loser must not record a payment, only look up the payment
    /// the winner recorded.
    pub async fn claim(
        &self,
        event_id: &str,
        event_type: &str,
        invoice_id: Option<InvoiceId>,
    ) -> Result<bool, DomainError> {
        let result = sqlx::query(
            "INSERT INTO stripe_webhook_events (event_id, event_type, invoice_id) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(event_id)
        .bind(event_type)
        .bind(invoice_id.map(|id| id.as_uuid()))
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Links the payment the claim winner recorded to the event, so a
    /// later redelivery (or an operator) can see which payment the
    /// event produced.
    pub async fn link_payment(
        &self,
        event_id: &str,
        payment_id: PaymentId,
    ) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE stripe_webhook_events SET payment_id = $1 \
             WHERE event_id = $2",
        )
        .bind(payment_id.as_uuid())
        .bind(event_id)
        .execute(&self.pool)
        .await
        .map_err(map_storage_error)?;
        Ok(())
    }

    /// Returns the payment id this event already produced, if any.
    /// A claim row with no payment yet (`payment_id` NULL) decodes
    /// to `None`, not an error: `query_scalar` must decode the
    /// column as `Option<Uuid>` or a NULL errors the lookup and
    /// every in-flight claim reads as a storage failure.
    pub async fn payment_for_event(
        &self,
        event_id: &str,
    ) -> Result<Option<PaymentId>, DomainError> {
        let row: Option<Option<Uuid>> =
            sqlx::query_scalar("SELECT payment_id FROM stripe_webhook_events WHERE event_id = $1")
                .bind(event_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_storage_error)?;
        Ok(row.flatten().map(PaymentId::from))
    }

    /// Returns when the event was claimed, if the claim row exists.
    /// The webhook uses it to tell a live claim (the winner is still
    /// processing) from a stale one (the winner died mid-flight).
    pub async fn received_at(
        &self,
        event_id: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, DomainError> {
        sqlx::query_scalar("SELECT received_at FROM stripe_webhook_events WHERE event_id = $1")
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_storage_error)
    }

    /// Releases a claim row: the event may be claimed again by a
    /// later delivery. Used when the winner exits without recording
    /// a payment (permanent failure, transient conflict, or a stale
    /// claim being reclaimed) — a held claim must never block the
    /// next delivery forever.
    pub async fn release(&self, event_id: &str) -> Result<(), DomainError> {
        sqlx::query("DELETE FROM stripe_webhook_events WHERE event_id = $1")
            .bind(event_id)
            .execute(&self.pool)
            .await
            .map_err(map_storage_error)?;
        Ok(())
    }
}
