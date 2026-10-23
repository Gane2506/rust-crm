// SPDX-License-Identifier: GPL-3.0-only
//! Billing service implementation (task 12-BILL-02).
//!
//! [`BillingServiceImpl`] implements
//! [`BillingService`](crm_domain::services::BillingService) over the
//! PostgreSQL repositories. It enforces the profile-scope rules of the
//! service contract, keeps the invoice status lifecycle in sync with
//! payments, and records the pinned audit events.
//!
//! Audit failures are swallowed: audit is fail-loud at the HTTP layer
//! and never fails a billing operation (same convention as
//! [`FeedServiceImpl`](crate::feed::FeedServiceImpl)).

use std::future::Future;

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::billing::{
    Invoice, InvoiceLine, InvoiceStatus, Money, Payment, BILLING_INVOICE_CREATED,
    BILLING_INVOICE_ISSUED, BILLING_INVOICE_VOIDED, BILLING_LINE_ADDED, BILLING_PAYMENT_RECORDED,
    BILLING_PAYMENT_REVERSED,
};
use crm_domain::error::DomainError;
use crm_domain::ids::{ClientProfileId, InvoiceId, PaymentId};
use crm_domain::repositories::{InvoiceRepository, PaymentRepository};
use crm_domain::services::{
    ActionContext, AuditService, BillingService, NewInvoice, NewInvoiceLine, NewPayment,
};

/// PostgreSQL-backed implementation of
/// [`BillingService`](crm_domain::services::BillingService).
///
/// The service owns three cross-entity rules:
/// * a payment that covers the invoice total transitions the invoice
///   `Issued → Paid` via [`Invoice::mark_paid`];
/// * reversing a payment reopens a `Paid` invoice back to `Issued`
///   when a balance remains, via [`Invoice::reopen`];
/// * void is idempotent on `Void` and rejected on `Paid`.
#[derive(Debug, Clone)]
pub struct BillingServiceImpl<IR, PR, A> {
    invoices: IR,
    payments: PR,
    audit: A,
}

impl<IR, PR, A> BillingServiceImpl<IR, PR, A> {
    /// Creates the service over the invoice and payment repositories
    /// and the audit service.
    pub fn new(invoices: IR, payments: PR, audit: A) -> Self {
        Self {
            invoices,
            payments,
            audit,
        }
    }

    /// Fetches an invoice and enforces that it is visible in the
    /// caller's profile scope, returning [`DomainError::Forbidden`]
    /// on a mismatch. An agency-scoped context (`None`) passes —
    /// grant checks happen upstream, at the HTTP layer.
    async fn scoped_invoice(
        &self,
        invoice_id: InvoiceId,
        ctx: &ActionContext,
    ) -> Result<Invoice, DomainError>
    where
        IR: InvoiceRepository,
    {
        let invoice = self.invoices.get(invoice_id).await?;
        check_scope(ctx, invoice.client_profile_id)?;
        Ok(invoice)
    }

    /// Records an audit event for a billing operation. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails
    /// the operation).
    async fn audit_billing(
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
}

/// Enforces profile scope on an invoice: a profile-scoped context
/// may only touch its own profile's invoices; an agency-scoped context
/// (`None`) passes because grant checks happen upstream, at the HTTP
/// layer.
fn check_scope(ctx: &ActionContext, client_profile_id: ClientProfileId) -> Result<(), DomainError> {
    match ctx.client_profile_id {
        Some(ctx_profile) if ctx_profile != client_profile_id => Err(DomainError::Forbidden(
            "invoice is not visible in this profile".to_string(),
        )),
        _ => Ok(()),
    }
}

// The service trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl<IR, PR, A> BillingService for BillingServiceImpl<IR, PR, A>
where
    IR: InvoiceRepository + Send + Sync,
    PR: PaymentRepository + Send + Sync,
    A: AuditService + Send + Sync,
{
    fn create_invoice<'a>(
        &'a self,
        input: NewInvoice,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Invoice, DomainError>> + Send + 'a {
        async move {
            check_scope(ctx, input.client_profile_id)?;
            let invoice = Invoice::new(input.client_profile_id, input.currency)?;
            let invoice = self.invoices.create(invoice).await?;
            self.audit_billing(
                ctx,
                BILLING_INVOICE_CREATED,
                "invoice",
                &invoice.invoice_id.to_string(),
            )
            .await;
            Ok(invoice)
        }
    }

    fn add_line<'a>(
        &'a self,
        input: NewInvoiceLine,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<InvoiceLine, DomainError>> + Send + 'a {
        async move {
            let mut invoice = self.scoped_invoice(input.invoice_id, ctx).await?;
            invoice.add_line(input.description, input.quantity, input.unit_price)?;
            // Guarded on `Draft`: a concurrent issue/void that
            // transitioned the invoice first turns this into a
            // `ConcurrencyConflict` instead of adding a line to a
            // non-draft invoice (FIX-03).
            let invoice = self
                .invoices
                .update_if_status(invoice, &[InvoiceStatus::Draft])
                .await?;
            // Re-hydrate the freshly appended line from the persisted
            // invoice so the returned id is the database-assigned one.
            let line = invoice.lines.last().cloned().ok_or_else(|| {
                DomainError::Validation("line was not persisted on the invoice".to_string())
            })?;
            self.audit_billing(
                ctx,
                BILLING_LINE_ADDED,
                "invoice",
                &invoice.invoice_id.to_string(),
            )
            .await;
            Ok(line)
        }
    }

    fn issue_invoice<'a>(
        &'a self,
        invoice_id: InvoiceId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Invoice, DomainError>> + Send + 'a {
        async move {
            let mut invoice = self.scoped_invoice(invoice_id, ctx).await?;
            invoice.issue()?;
            // Guarded on `Draft`: a concurrent transition turns this
            // into a `ConcurrencyConflict` instead of a lost update
            // (FIX-03).
            let invoice = self
                .invoices
                .update_if_status(invoice, &[InvoiceStatus::Draft])
                .await?;
            self.audit_billing(
                ctx,
                BILLING_INVOICE_ISSUED,
                "invoice",
                &invoice.invoice_id.to_string(),
            )
            .await;
            Ok(invoice)
        }
    }

    fn void_invoice<'a>(
        &'a self,
        invoice_id: InvoiceId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Invoice, DomainError>> + Send + 'a {
        async move {
            let mut invoice = self.scoped_invoice(invoice_id, ctx).await?;
            // Capture the pre-transition status: `void()` accepts
            // `Draft` and `Issued` (and is idempotent on `Void`), so
            // the guard must allow whichever of those we saw.
            let expected_status = invoice.status;
            invoice.void()?;
            // Guarded on the observed status: a concurrent transition
            // turns this into a `ConcurrencyConflict` instead of a lost
            // update (FIX-03).
            let invoice = self
                .invoices
                .update_if_status(invoice, &[expected_status])
                .await?;
            self.audit_billing(
                ctx,
                BILLING_INVOICE_VOIDED,
                "invoice",
                &invoice.invoice_id.to_string(),
            )
            .await;
            Ok(invoice)
        }
    }

    fn record_payment<'a>(
        &'a self,
        input: NewPayment,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Payment, DomainError>> + Send + 'a {
        async move {
            let invoice = self.invoices.get(input.invoice_id).await?;
            check_scope(ctx, invoice.client_profile_id)?;
            if input.amount.currency() != invoice.currency {
                return Err(DomainError::Validation(format!(
                    "payment currency {} does not match invoice currency {}",
                    input.amount.currency(),
                    invoice.currency
                )));
            }
            // Payments are only recorded on issued invoices; the
            // repository re-checks the locked status for the concurrent
            // case (FIX-03).
            if invoice.status != InvoiceStatus::Issued {
                return Err(DomainError::Validation(format!(
                    "cannot record a payment on an invoice with status {:?}",
                    invoice.status
                )));
            }
            // `record` rejects zero amounts; nothing is persisted yet.
            let payment = Payment::record(input.invoice_id, input.amount)?;
            // Atomic read-check-write: the repository locks the invoice
            // row, validates coverage against the latest payments, and
            // marks the invoice paid when covered — all in one
            // transaction. Concurrent payments serialize on the lock
            // instead of over-applying (FIX-03).
            let payment = self.payments.record_guarded(payment, &invoice).await?;
            self.audit_billing(
                ctx,
                BILLING_PAYMENT_RECORDED,
                "payment",
                &payment.payment_id.to_string(),
            )
            .await;
            Ok(payment)
        }
    }

    fn reverse_payment<'a>(
        &'a self,
        payment_id: PaymentId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Payment, DomainError>> + Send + 'a {
        async move {
            let payment = self.payments.get(payment_id).await?;
            let invoice = self.invoices.get(payment.invoice_id).await?;
            check_scope(ctx, invoice.client_profile_id)?;
            // Domain flip first (idempotent), then persist. The
            // repository persists the status; the domain flip ensures
            // the returned value carries `Reversed`.
            let mut payment = payment;
            payment.reverse();
            let payment = self.payments.reverse(payment).await?;
            // A reversal that leaves a `Paid` invoice with a balance
            // reopens it to `Issued`.
            let mut invoice = self.invoices.get(payment.invoice_id).await?;
            if invoice.status == crm_domain::billing::InvoiceStatus::Paid {
                let payments = self.payments.list_by_invoice(invoice.invoice_id).await?;
                let due = invoice.remaining_due(&payments)?;
                if due.amount() > 0 {
                    invoice.reopen()?;
                    self.invoices.update(invoice).await?;
                }
            }
            self.audit_billing(
                ctx,
                BILLING_PAYMENT_REVERSED,
                "payment",
                &payment.payment_id.to_string(),
            )
            .await;
            Ok(payment)
        }
    }

    fn list_invoices<'a>(
        &'a self,
        limit: u64,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Vec<Invoice>, DomainError>> + Send + 'a {
        async move {
            let profile = ctx.client_profile_id.ok_or_else(|| {
                DomainError::Forbidden(
                    "invoice listing requires a client-profile scope".to_string(),
                )
            })?;
            self.invoices.list_by_profile(profile, limit).await
        }
    }

    fn invoice_balance<'a>(
        &'a self,
        invoice_id: InvoiceId,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<Money, DomainError>> + Send + 'a {
        async move {
            let invoice = self.scoped_invoice(invoice_id, ctx).await?;
            let payments = self.payments.list_by_invoice(invoice_id).await?;
            invoice.remaining_due(&payments)
        }
    }
}
