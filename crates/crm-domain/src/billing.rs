// SPDX-License-Identifier: GPL-3.0-only
//! Billing domain: invoices and payment records for agency clients.
//!
//! The CRM is agency-operated: staff raise invoices against a client
//! profile for services rendered, and record payments against them.
//! This module is **record-keeping** — it tracks invoice lifecycles
//! and payment coverage. It is not a payment processor integration:
//! no charge is executed here (a Stripe/processor adapter is a later
//! workstream's concern).
//!
//! ## Money
//!
//! Amounts are [`Money`]: minor units (`i64`, never negative) plus a
//! validated ISO 4217 currency code (three uppercase ASCII letters).
//! Currency is part of the value — [`Money::add`] and
//! [`Money::sub`] reject mixed-currency arithmetic, and every line on
//! an invoice must share the invoice's currency.
//!
//! ## Invoice lifecycle
//!
//! `Draft → Issued → Paid`, with `Void` reachable from `Draft` or
//! `Issued`. `Paid` and `Void` are terminal. Lines may only be added
//! while the invoice is a draft: once issued, the total is a financial
//! fact and must not move. [`Invoice::mark_paid`] only performs the
//! `Issued → Paid` transition; the *coverage* check (recorded payments
//! cover the total) is the service's job via
//! [`Invoice::remaining_due`].
//!
//! ## Payments
//!
//! [`Payment`]s are append-only. A payment is `Recorded`, and can be
//! explicitly `Reversed` — there is no deletion. Reversed payments are
//! excluded from coverage math. A payment must match the invoice's
//! currency, and payments may never exceed the invoice total.
//!
//! ## Authorization
//!
//! Billing operations authorize against
//! [`Resource::Invoice`](crate::security::Resource::Invoice)
//! (profile-scoped; landed in task 12-BILL-02 together with the
//! persistence layer).
//!
//! ## Audit
//!
//! Billing mutations are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s by the service
//! implementation, with pinned action names
//! ([`BILLING_INVOICE_CREATED`], [`BILLING_LINE_ADDED`],
//! [`BILLING_INVOICE_ISSUED`], [`BILLING_INVOICE_VOIDED`],
//! [`BILLING_PAYMENT_RECORDED`], [`BILLING_PAYMENT_REVERSED`])
//! documented on [`BillingService`](crate::services::BillingService).
//! They are deliberately **not**
//! [`SecurityEvent`](crate::audit::SecurityEvent) variants: that
//! taxonomy covers security-relevant events (auth, RBAC, sessions,
//! rate limits), while billing is a business operation audited like
//! contacts and campaigns.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{ClientProfileId, InvoiceId, InvoiceLineId, PaymentId};

/// Audit action name recorded when an invoice is created.
pub const BILLING_INVOICE_CREATED: &str = "billing.invoice_created";
/// Audit action name recorded when a line is added to a draft invoice.
pub const BILLING_LINE_ADDED: &str = "billing.line_added";
/// Audit action name recorded when an invoice is issued.
pub const BILLING_INVOICE_ISSUED: &str = "billing.invoice_issued";
/// Audit action name recorded when an invoice is voided.
pub const BILLING_INVOICE_VOIDED: &str = "billing.invoice_voided";
/// Audit action name recorded when a payment is recorded.
pub const BILLING_PAYMENT_RECORDED: &str = "billing.payment_recorded";
/// Audit action name recorded when a payment is reversed.
pub const BILLING_PAYMENT_REVERSED: &str = "billing.payment_reversed";

/// Money: a non-negative amount in minor units plus a validated ISO
/// 4217 currency code.
///
/// Minor units keep arithmetic exact (cents, paise, …). Currency is
/// part of the value: mixing currencies is a [`DomainError::Validation`].
///
/// `Deserialize` is implemented manually through [`Money::new`] so the
/// amount/currency invariants hold for deserialized values too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Money {
    /// Amount in minor units. Never negative.
    amount: i64,
    /// ISO 4217 code: exactly three uppercase ASCII letters.
    currency: String,
}

impl<'de> serde::Deserialize<'de> for Money {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct RawMoney {
            amount: i64,
            currency: String,
        }
        let raw = RawMoney::deserialize(deserializer)?;
        Money::new(raw.amount, raw.currency).map_err(serde::de::Error::custom)
    }
}

impl Money {
    /// Creates money from minor units and a currency code.
    ///
    /// Rejects: negative amounts; currency codes that are not exactly
    /// three uppercase ASCII letters.
    ///
    /// ```
    /// use crm_domain::billing::Money;
    ///
    /// let price = Money::new(1999, "USD".to_string()).expect("valid money");
    /// assert_eq!(price.amount(), 1999);
    /// assert_eq!(price.currency(), "USD");
    /// assert!(Money::new(-1, "USD".to_string()).is_err());
    /// assert!(Money::new(100, "usd".to_string()).is_err());
    /// assert!(Money::new(100, "US".to_string()).is_err());
    /// ```
    pub fn new(amount: i64, currency: String) -> Result<Self, DomainError> {
        if amount < 0 {
            return Err(DomainError::Validation(
                "money amount must not be negative".to_string(),
            ));
        }
        let currency = currency.trim().to_owned();
        if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(DomainError::Validation(
                "currency must be a 3-letter uppercase ISO 4217 code".to_string(),
            ));
        }
        Ok(Self { amount, currency })
    }

    /// Zero in the given currency.
    ///
    /// ```
    /// use crm_domain::billing::Money;
    ///
    /// let zero = Money::zero("EUR".to_string()).expect("valid currency");
    /// assert_eq!(zero.amount(), 0);
    /// ```
    pub fn zero(currency: String) -> Result<Self, DomainError> {
        Self::new(0, currency)
    }

    /// Amount in minor units.
    #[must_use]
    pub fn amount(&self) -> i64 {
        self.amount
    }

    /// ISO 4217 currency code.
    #[must_use]
    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// Adds two amounts. Rejects mixed currencies.
    ///
    /// ```
    /// use crm_domain::billing::Money;
    ///
    /// let a = Money::new(100, "USD".to_string()).expect("valid");
    /// let b = Money::new(50, "USD".to_string()).expect("valid");
    /// assert_eq!(a.add(&b).expect("same currency").amount(), 150);
    /// let eur = Money::new(50, "EUR".to_string()).expect("valid");
    /// assert!(a.add(&eur).is_err());
    /// ```
    pub fn add(&self, other: &Money) -> Result<Money, DomainError> {
        self.require_same_currency(other)?;
        let amount = self
            .amount
            .checked_add(other.amount)
            .ok_or_else(|| DomainError::Validation("money addition overflowed".to_string()))?;
        Ok(Money {
            amount,
            currency: self.currency.clone(),
        })
    }

    /// Subtracts `other` from `self`. Rejects mixed currencies and
    /// results that would go negative.
    ///
    /// ```
    /// use crm_domain::billing::Money;
    ///
    /// let a = Money::new(100, "USD".to_string()).expect("valid");
    /// let b = Money::new(30, "USD".to_string()).expect("valid");
    /// assert_eq!(a.sub(&b).expect("valid").amount(), 70);
    /// assert!(b.sub(&a).is_err());
    /// ```
    pub fn sub(&self, other: &Money) -> Result<Money, DomainError> {
        self.require_same_currency(other)?;
        let amount = self
            .amount
            .checked_sub(other.amount)
            .ok_or_else(|| DomainError::Validation("money subtraction overflowed".to_string()))?;
        if amount < 0 {
            return Err(DomainError::Validation(
                "money subtraction would go negative".to_string(),
            ));
        }
        Ok(Money {
            amount,
            currency: self.currency.clone(),
        })
    }

    /// Multiplies by a non-negative factor. Used for line totals
    /// (`quantity × unit_price`). Rejects overflow.
    pub(crate) fn mul(&self, factor: u32) -> Result<Money, DomainError> {
        let amount = self.amount.checked_mul(i64::from(factor)).ok_or_else(|| {
            DomainError::Validation("money multiplication overflowed".to_string())
        })?;
        Ok(Money {
            amount,
            currency: self.currency.clone(),
        })
    }

    fn require_same_currency(&self, other: &Money) -> Result<(), DomainError> {
        if self.currency != other.currency {
            return Err(DomainError::Validation(format!(
                "currency mismatch: {} vs {}",
                self.currency, other.currency
            )));
        }
        Ok(())
    }
}

/// Lifecycle of an invoice.
///
/// `Draft → Issued → Paid`; `Void` is reachable from `Draft` or
/// `Issued`. `Paid` and `Void` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvoiceStatus {
    /// Being prepared; lines may still be added.
    Draft,
    /// Sent to the client; the total is now a financial fact.
    Issued,
    /// Recorded payments cover the total.
    Paid,
    /// Cancelled before (or without) payment.
    Void,
}

/// One line on an invoice: what was billed, how many, at what price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceLine {
    /// Stable identifier of this line.
    pub line_id: InvoiceLineId,
    /// The invoice this line belongs to.
    pub invoice_id: InvoiceId,
    /// Human-readable description, validated non-blank.
    pub description: String,
    /// Quantity billed. Positive.
    pub quantity: u32,
    /// Price per unit. Must share the invoice's currency.
    pub unit_price: Money,
}

impl InvoiceLine {
    /// Creates a line. The currency check against the invoice happens
    /// in [`Invoice::add_line`]; this constructor validates the line
    /// itself.
    ///
    /// Rejects: blank descriptions, zero quantities, arithmetic
    /// overflow in `quantity × unit_price`.
    pub(crate) fn new(
        invoice_id: InvoiceId,
        description: String,
        quantity: u32,
        unit_price: Money,
    ) -> Result<Self, DomainError> {
        let description = description.trim().to_owned();
        if description.is_empty() {
            return Err(DomainError::Validation(
                "invoice line description must not be blank".to_string(),
            ));
        }
        if quantity == 0 {
            return Err(DomainError::Validation(
                "invoice line quantity must be positive".to_string(),
            ));
        }
        // Fail fast on overflow so `line_total` stays infallible.
        unit_price.mul(quantity)?;
        Ok(Self {
            line_id: InvoiceLineId::new(),
            invoice_id,
            description,
            quantity,
            unit_price,
        })
    }

    /// Extended price: `quantity × unit_price`. Infallible — overflow
    /// is rejected at construction.
    #[must_use]
    pub fn line_total(&self) -> Money {
        // Construction rejected overflow, so this cannot fail.
        self.unit_price
            .mul(self.quantity)
            .expect("line total overflow was rejected at construction")
    }
}

/// An invoice raised against a client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invoice {
    /// Stable identifier of this invoice.
    pub invoice_id: InvoiceId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Lifecycle state; starts [`InvoiceStatus::Draft`].
    pub status: InvoiceStatus,
    /// Currency of every line and of the total.
    pub currency: String,
    /// Billed lines, in the order added.
    pub lines: Vec<InvoiceLine>,
    /// When the invoice was created.
    pub created_at: DateTime<Utc>,
    /// When the invoice was last changed.
    pub updated_at: DateTime<Utc>,
}

impl Invoice {
    /// Creates a draft invoice with no lines.
    ///
    /// ```
    /// use crm_domain::billing::{Invoice, InvoiceStatus};
    /// use crm_domain::ids::ClientProfileId;
    ///
    /// let invoice = Invoice::new(ClientProfileId::new(), "USD".to_string())
    ///     .expect("valid invoice");
    /// assert_eq!(invoice.status, InvoiceStatus::Draft);
    /// assert!(invoice.lines.is_empty());
    /// ```
    pub fn new(client_profile_id: ClientProfileId, currency: String) -> Result<Self, DomainError> {
        // Validate the currency code up front so every line and the
        // total inherit a known-good code.
        let currency = Money::zero(currency)?.currency().to_owned();
        let now = Utc::now();
        Ok(Self {
            invoice_id: InvoiceId::new(),
            client_profile_id,
            status: InvoiceStatus::Draft,
            currency,
            lines: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Adds a line to a draft invoice and returns it.
    ///
    /// Rejects: non-draft invoices (once issued, the total is a
    /// financial fact); blank descriptions; zero quantities; line
    /// currencies that differ from the invoice's; arithmetic overflow
    /// of the running total.
    ///
    /// ```
    /// use crm_domain::billing::{Invoice, Money};
    /// use crm_domain::ids::ClientProfileId;
    ///
    /// let mut invoice = Invoice::new(ClientProfileId::new(), "USD".to_string())
    ///     .expect("valid invoice");
    /// invoice
    ///     .add_line(
    ///         "Monthly retainer".to_string(),
    ///         1,
    ///         Money::new(2_5000, "USD".to_string()).expect("valid"),
    ///     )
    ///     .expect("valid line");
    /// assert_eq!(invoice.total().amount(), 2_5000);
    /// ```
    pub fn add_line(
        &mut self,
        description: String,
        quantity: u32,
        unit_price: Money,
    ) -> Result<InvoiceLine, DomainError> {
        if self.status != InvoiceStatus::Draft {
            return Err(DomainError::Validation(
                "lines may only be added to a draft invoice".to_string(),
            ));
        }
        if unit_price.currency() != self.currency {
            return Err(DomainError::Validation(format!(
                "line currency {} does not match invoice currency {}",
                unit_price.currency(),
                self.currency
            )));
        }
        let line = InvoiceLine::new(self.invoice_id, description, quantity, unit_price)?;
        // Keep `total()` infallible: reject a line that would overflow
        // the running total.
        self.total().add(&line.line_total())?;
        self.lines.push(line.clone());
        self.updated_at = Utc::now();
        Ok(line)
    }

    /// Sum of the line totals, in the invoice's currency. Infallible:
    /// overflow is rejected in [`Invoice::add_line`].
    #[must_use]
    pub fn total(&self) -> Money {
        let mut total = Money::zero(self.currency.clone())
            .expect("invoice currency was validated at construction");
        for line in &self.lines {
            // `add_line` rejected any line that would overflow this sum.
            total = total
                .add(&line.line_total())
                .expect("invoice total overflow was rejected in add_line");
        }
        total
    }

    /// Issues the invoice: `Draft → Issued`.
    ///
    /// Rejects: non-draft invoices; invoices with no lines (a zero-line
    /// invoice bills nothing and must not be issued).
    pub fn issue(&mut self) -> Result<(), DomainError> {
        if self.status != InvoiceStatus::Draft {
            return Err(DomainError::Validation(
                "only a draft invoice can be issued".to_string(),
            ));
        }
        if self.lines.is_empty() {
            return Err(DomainError::Validation(
                "an invoice with no lines cannot be issued".to_string(),
            ));
        }
        self.status = InvoiceStatus::Issued;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Voids the invoice. Reachable from `Draft` and `Issued`;
    /// idempotent on `Void`. Rejects: `Paid` invoices — money has
    /// changed hands, so cancellation must go through a refund flow
    /// (a later task), not a void.
    pub fn void(&mut self) -> Result<(), DomainError> {
        match self.status {
            InvoiceStatus::Paid => Err(DomainError::Validation(
                "a paid invoice cannot be voided; issue a refund instead".to_string(),
            )),
            InvoiceStatus::Void => Ok(()),
            InvoiceStatus::Draft | InvoiceStatus::Issued => {
                self.status = InvoiceStatus::Void;
                self.updated_at = Utc::now();
                Ok(())
            }
        }
    }

    /// Marks the invoice paid: `Issued → Paid`.
    ///
    /// This performs only the transition. The *coverage* check —
    /// recorded payments cover the total — is the service's job via
    /// [`Invoice::remaining_due`]; call this after that check passes.
    pub fn mark_paid(&mut self) -> Result<(), DomainError> {
        if self.status != InvoiceStatus::Issued {
            return Err(DomainError::Validation(
                "only an issued invoice can be marked paid".to_string(),
            ));
        }
        self.status = InvoiceStatus::Paid;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Reopens a paid invoice: `Paid → Issued`.
    ///
    /// This is the reversal path: when a payment that completed the
    /// coverage is reversed, the invoice is no longer fully paid, so
    /// it returns to `Issued` rather than sitting in `Paid` with a
    /// positive balance. The service calls this from `reverse_payment`
    /// when `remaining_due` becomes positive after the reversal.
    pub fn reopen(&mut self) -> Result<(), DomainError> {
        if self.status != InvoiceStatus::Paid {
            return Err(DomainError::Validation(
                "only a paid invoice can be reopened".to_string(),
            ));
        }
        self.status = InvoiceStatus::Issued;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Amount still owed: total minus non-reversed payments.
    ///
    /// Rejects: payments for another invoice; payments in another
    /// currency; payment sets whose non-reversed sum exceeds the total
    /// (payments may never exceed the invoice total).
    ///
    /// ```
    /// use crm_domain::billing::{Invoice, Money, Payment};
    /// use crm_domain::ids::ClientProfileId;
    ///
    /// let mut invoice = Invoice::new(ClientProfileId::new(), "USD".to_string())
    ///     .expect("valid invoice");
    /// invoice
    ///     .add_line(
    ///         "Retainer".to_string(),
    ///         1,
    ///         Money::new(10_000, "USD".to_string()).expect("valid"),
    ///     )
    ///     .expect("valid line");
    /// invoice.issue().expect("issue");
    ///
    /// let half = Payment::record(
    ///     invoice.invoice_id,
    ///     Money::new(4_000, "USD".to_string()).expect("valid"),
    /// )
    /// .expect("valid payment");
    /// assert_eq!(
    ///     invoice.remaining_due(&[half]).expect("valid").amount(),
    ///     6_000
    /// );
    /// ```
    pub fn remaining_due(&self, payments: &[Payment]) -> Result<Money, DomainError> {
        let mut paid = Money::zero(self.currency.clone())
            .expect("invoice currency was validated at construction");
        for payment in payments {
            if payment.invoice_id != self.invoice_id {
                return Err(DomainError::Validation(
                    "payment is for a different invoice".to_string(),
                ));
            }
            if payment.amount.currency() != self.currency {
                return Err(DomainError::Validation(format!(
                    "payment currency {} does not match invoice currency {}",
                    payment.amount.currency(),
                    self.currency
                )));
            }
            if payment.status == PaymentStatus::Reversed {
                continue;
            }
            paid = paid.add(&payment.amount)?;
        }
        let total = self.total();
        if paid.amount() > total.amount() {
            return Err(DomainError::Validation(
                "recorded payments exceed the invoice total".to_string(),
            ));
        }
        total.sub(&paid)
    }
}

/// Lifecycle of a payment record.
///
/// Payments are append-only: `Recorded` is the initial state and
/// `Reversed` is an explicit, audited reversal. There is no deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaymentStatus {
    /// Recorded against the invoice.
    Recorded,
    /// Explicitly reversed; excluded from coverage math.
    Reversed,
}

/// A payment recorded against an invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    /// Stable identifier of this payment.
    pub payment_id: PaymentId,
    /// The invoice this payment is recorded against.
    pub invoice_id: InvoiceId,
    /// Amount paid. Positive; must match the invoice's currency
    /// (checked by [`Invoice::remaining_due`], not here — a payment
    /// record on its own does not know the invoice).
    pub amount: Money,
    /// Record state; starts [`PaymentStatus::Recorded`].
    pub status: PaymentStatus,
    /// When the payment was recorded.
    pub recorded_at: DateTime<Utc>,
}

impl Payment {
    /// Records a payment.
    ///
    /// Rejects: zero amounts (a payment must move money). Currency
    /// matching against the invoice happens in
    /// [`Invoice::remaining_due`].
    ///
    /// ```
    /// use crm_domain::billing::{Money, Payment, PaymentStatus};
    /// use crm_domain::ids::InvoiceId;
    ///
    /// let payment = Payment::record(
    ///     InvoiceId::new(),
    ///     Money::new(5_000, "USD".to_string()).expect("valid"),
    /// )
    /// .expect("valid payment");
    /// assert_eq!(payment.status, PaymentStatus::Recorded);
    /// assert!(Payment::record(
    ///     InvoiceId::new(),
    ///     Money::zero("USD".to_string()).expect("valid"),
    /// )
    /// .is_err());
    /// ```
    pub fn record(invoice_id: InvoiceId, amount: Money) -> Result<Self, DomainError> {
        if amount.amount() == 0 {
            return Err(DomainError::Validation(
                "payment amount must be positive".to_string(),
            ));
        }
        Ok(Self {
            payment_id: PaymentId::new(),
            invoice_id,
            amount,
            status: PaymentStatus::Recorded,
            recorded_at: Utc::now(),
        })
    }

    /// Reverses the payment. Idempotent on an already-reversed
    /// payment. Reversed payments are excluded from coverage math in
    /// [`Invoice::remaining_due`].
    pub fn reverse(&mut self) {
        self.status = PaymentStatus::Reversed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(amount: i64) -> Money {
        Money::new(amount, "USD".to_string()).expect("valid money")
    }

    fn draft_invoice() -> Invoice {
        Invoice::new(ClientProfileId::new(), "USD".to_string()).expect("valid invoice")
    }

    fn issued_invoice() -> Invoice {
        let mut invoice = draft_invoice();
        invoice
            .add_line("Retainer".to_string(), 1, usd(10_000))
            .expect("valid line");
        invoice.issue().expect("issue");
        invoice
    }

    #[test]
    fn money_rejects_negative_amount() {
        let err = Money::new(-1, "USD".to_string()).expect_err("negative must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn money_deserialize_rejects_invalid() {
        // Negative amount via JSON bypasses `Money::new` unless
        // Deserialize is routed through it.
        let bad_amount: Result<Money, _> =
            serde_json::from_str(r#"{"amount": -5000, "currency": "USD"}"#);
        assert!(bad_amount.is_err());
        // Lowercase currency is rejected too.
        let bad_ccy: Result<Money, _> =
            serde_json::from_str(r#"{"amount": 5000, "currency": "usd"}"#);
        assert!(bad_ccy.is_err());
        // Valid JSON round-trips.
        let ok: Money = serde_json::from_str(r#"{"amount": 5000, "currency": "USD"}"#)
            .expect("valid money JSON");
        assert_eq!(ok, usd(5000));
    }

    #[test]
    fn invoice_reopen_returns_paid_to_issued() {
        let mut invoice = issued_invoice();
        invoice.mark_paid().expect("mark paid");
        assert_eq!(invoice.status, InvoiceStatus::Paid);
        invoice.reopen().expect("reopen");
        assert_eq!(invoice.status, InvoiceStatus::Issued);
        // Reopening a non-paid invoice is rejected.
        assert!(invoice.reopen().is_err());
    }

    #[test]
    fn money_rejects_bad_currency_codes() {
        for bad in ["usd", "US", "USDD", "U1D", "   ", "US$"] {
            let err =
                Money::new(100, bad.to_string()).expect_err(&format!("{bad:?} must be rejected"));
            assert!(matches!(err, DomainError::Validation(_)));
        }
    }

    #[test]
    fn money_add_requires_same_currency() {
        let err = usd(100)
            .add(&Money::new(100, "EUR".to_string()).expect("valid"))
            .expect_err("mixed currency must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn money_sub_rejects_negative_result() {
        let err = usd(30)
            .sub(&usd(100))
            .expect_err("negative result must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn invoice_starts_draft_with_validated_currency() {
        let invoice = draft_invoice();
        assert_eq!(invoice.status, InvoiceStatus::Draft);
        assert!(invoice.lines.is_empty());
        assert_eq!(invoice.currency, "USD");
        assert!(Invoice::new(ClientProfileId::new(), "usd".to_string()).is_err());
    }

    #[test]
    fn add_line_validates() {
        let mut invoice = draft_invoice();
        // Blank description.
        assert!(invoice.add_line("  ".to_string(), 1, usd(10)).is_err());
        // Zero quantity.
        assert!(invoice.add_line("Work".to_string(), 0, usd(10)).is_err());
        // Currency mismatch.
        assert!(invoice
            .add_line(
                "Work".to_string(),
                1,
                Money::new(10, "EUR".to_string()).expect("valid")
            )
            .is_err());
        assert!(invoice.lines.is_empty());
    }

    #[test]
    fn add_line_rejects_overflow() {
        let mut invoice = draft_invoice();
        let err = invoice
            .add_line("Huge".to_string(), u32::MAX, usd(i64::MAX))
            .expect_err("overflow must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
        assert!(invoice.lines.is_empty());
    }

    #[test]
    fn add_line_rejected_once_issued() {
        let mut invoice = issued_invoice();
        let err = invoice
            .add_line("Extra".to_string(), 1, usd(10))
            .expect_err("lines are frozen after issue");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn total_sums_line_totals() {
        let mut invoice = draft_invoice();
        invoice
            .add_line("A".to_string(), 2, usd(2_500))
            .expect("valid line");
        invoice
            .add_line("B".to_string(), 1, usd(1_000))
            .expect("valid line");
        let total = invoice.total();
        assert_eq!(total.amount(), 6_000);
        assert_eq!(total.currency(), "USD");
    }

    #[test]
    fn issue_requires_draft_and_lines() {
        // No lines.
        let mut empty = draft_invoice();
        assert!(empty.issue().is_err());
        // Double issue.
        let mut invoice = issued_invoice();
        assert!(invoice.issue().is_err());
        // From void.
        let mut voided = draft_invoice();
        voided
            .add_line("A".to_string(), 1, usd(10))
            .expect("valid line");
        voided.void().expect("void");
        assert!(voided.issue().is_err());
    }

    #[test]
    fn void_is_idempotent_and_rejects_paid() {
        let mut invoice = issued_invoice();
        invoice.void().expect("void from issued");
        assert_eq!(invoice.status, InvoiceStatus::Void);
        invoice.void().expect("void is idempotent");

        let mut paid = issued_invoice();
        paid.mark_paid().expect("mark paid");
        let err = paid.void().expect_err("paid invoice must not be voided");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn mark_paid_only_from_issued() {
        let mut draft = draft_invoice();
        assert!(draft.mark_paid().is_err());
        let mut voided = issued_invoice();
        voided.void().expect("void");
        assert!(voided.mark_paid().is_err());
    }

    #[test]
    fn payment_rejects_zero_amount() {
        let err = Payment::record(
            InvoiceId::new(),
            Money::zero("USD".to_string()).expect("valid"),
        )
        .expect_err("zero payment must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn reverse_is_idempotent() {
        let mut payment = Payment::record(InvoiceId::new(), usd(10)).expect("valid");
        assert_eq!(payment.status, PaymentStatus::Recorded);
        payment.reverse();
        assert_eq!(payment.status, PaymentStatus::Reversed);
        payment.reverse();
        assert_eq!(payment.status, PaymentStatus::Reversed);
    }

    #[test]
    fn remaining_due_tracks_coverage() {
        let invoice = issued_invoice(); // total 10_000
        assert_eq!(invoice.remaining_due(&[]).expect("valid").amount(), 10_000);

        let p1 = Payment::record(invoice.invoice_id, usd(4_000)).expect("valid");
        assert_eq!(invoice.remaining_due(&[p1]).expect("valid").amount(), 6_000);

        // Reversed payments are excluded.
        let mut p2 = Payment::record(invoice.invoice_id, usd(4_000)).expect("valid");
        p2.reverse();
        let p3 = Payment::record(invoice.invoice_id, usd(4_000)).expect("valid");
        assert_eq!(
            invoice.remaining_due(&[p3, p2]).expect("valid").amount(),
            6_000
        );
    }

    #[test]
    fn remaining_due_rejects_overpayment() {
        let invoice = issued_invoice(); // total 10_000
        let too_much = Payment::record(invoice.invoice_id, usd(1_0001)).expect("valid");
        let err = invoice
            .remaining_due(&[too_much])
            .expect_err("overpayment must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn remaining_due_rejects_foreign_payments() {
        let invoice = issued_invoice();
        // Another invoice's payment.
        let foreign = Payment::record(InvoiceId::new(), usd(10)).expect("valid");
        assert!(invoice.remaining_due(&[foreign]).is_err());
        // Another currency.
        let eur = Payment::record(
            invoice.invoice_id,
            Money::new(10, "EUR".to_string()).expect("valid"),
        )
        .expect("valid");
        assert!(invoice.remaining_due(&[eur]).is_err());
    }

    #[test]
    fn full_payment_flow_marks_paid() {
        let mut invoice = issued_invoice(); // total 10_000
        let full = Payment::record(invoice.invoice_id, usd(10_000)).expect("valid");
        let payments = [full];
        assert_eq!(invoice.remaining_due(&payments).expect("valid").amount(), 0);
        // Service-side flow: coverage confirmed, then transition.
        invoice.mark_paid().expect("mark paid");
        assert_eq!(invoice.status, InvoiceStatus::Paid);
    }
}
