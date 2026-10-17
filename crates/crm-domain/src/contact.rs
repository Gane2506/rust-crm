// SPDX-License-Identifier: GPL-3.0-only
//! Contact aggregate: normalized identity, lifecycle and consent records.
//!
//! A contact is a person record owned by one client profile. Emails and
//! phones are stored in normalized form so duplicate detection is a plain
//! equality check. The [`Lifecycle`] state machine is enforced by
//! [`Contact::transition_to`]; consent records capture the channel, time
//! and source of every marketing consent grant.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{ClientProfileId, ContactId};

/// Lifecycle stage of a contact.
///
/// Forward motion is `Prospect → Qualified → Customer`. Archival is
/// allowed from any non-archived stage; [`Lifecycle::Archived`] is
/// terminal — nothing transitions out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lifecycle {
    /// A raw prospect not yet qualified.
    Prospect,
    /// A prospect that passed qualification.
    Qualified,
    /// A converted customer.
    Customer,
    /// Removed from active outreach; terminal.
    Archived,
}

/// One marketing-consent grant for a contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRecord {
    /// Channel the consent applies to, e.g. `"email"`, `"sms"`.
    pub channel: String,
    /// When consent was granted.
    pub granted_at: DateTime<Utc>,
    /// Where the consent came from, e.g. `"web-form"`.
    pub source: String,
}

/// A person record inside a client profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// Stable identifier of this contact.
    pub contact_id: ContactId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Lowercased/trimmed email addresses, deduplicated.
    pub normalized_emails: Vec<String>,
    /// E.164-normalized phone numbers, deduplicated.
    pub normalized_phones: Vec<String>,
    /// Current lifecycle stage.
    pub lifecycle: Lifecycle,
    /// First acquisition source, e.g. `"csv-import"`.
    pub first_source: String,
    /// Most recent touch source.
    pub latest_source: String,
    /// Attribution history of every source touch.
    pub source_touches: Vec<String>,
    /// Marketing consent grants.
    pub consent_records: Vec<ConsentRecord>,
    /// When this record was merged away, the surviving contact.
    pub merged_into: Option<ContactId>,
}

impl Contact {
    /// Moves the contact to the next lifecycle stage.
    ///
    /// Rules: `Prospect → Qualified → Customer` forward only (skipping a
    /// stage is rejected); any non-archived stage may move to `Archived`;
    /// `Archived` is terminal. Transitioning to the current stage is an
    /// idempotent no-op so retried commands stay safe.
    ///
    /// ```
    /// use crm_domain::contact::{Contact, Lifecycle};
    /// use crm_domain::ids::{ClientProfileId, ContactId};
    ///
    /// let mut contact = Contact {
    ///     contact_id: ContactId::new(),
    ///     client_profile_id: ClientProfileId::new(),
    ///     normalized_emails: vec!["a@example.com".to_string()],
    ///     normalized_phones: vec![],
    ///     lifecycle: Lifecycle::Prospect,
    ///     first_source: "import".to_string(),
    ///     latest_source: "import".to_string(),
    ///     source_touches: vec!["import".to_string()],
    ///     consent_records: vec![],
    ///     merged_into: None,
    /// };
    ///
    /// assert!(contact.transition_to(Lifecycle::Qualified).is_ok());
    /// assert_eq!(contact.lifecycle, Lifecycle::Qualified);
    /// // Backward motion is rejected.
    /// assert!(contact.transition_to(Lifecycle::Prospect).is_err());
    /// ```
    pub fn transition_to(&mut self, next: Lifecycle) -> Result<(), DomainError> {
        if next == self.lifecycle {
            return Ok(());
        }
        let allowed = match self.lifecycle {
            Lifecycle::Prospect => matches!(next, Lifecycle::Qualified | Lifecycle::Archived),
            Lifecycle::Qualified => matches!(next, Lifecycle::Customer | Lifecycle::Archived),
            Lifecycle::Customer => matches!(next, Lifecycle::Archived),
            Lifecycle::Archived => false,
        };
        if allowed {
            self.lifecycle = next;
            Ok(())
        } else {
            Err(DomainError::Validation(format!(
                "illegal lifecycle transition from {:?} to {:?}",
                self.lifecycle, next
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_contact(lifecycle: Lifecycle) -> Contact {
        Contact {
            contact_id: ContactId::new(),
            client_profile_id: ClientProfileId::new(),
            normalized_emails: vec!["a@example.com".to_string()],
            normalized_phones: vec![],
            lifecycle,
            first_source: "import".to_string(),
            latest_source: "import".to_string(),
            source_touches: vec!["import".to_string()],
            consent_records: vec![],
            merged_into: None,
        }
    }

    #[test]
    fn lifecycle_transition_table() {
        use Lifecycle::{Archived, Customer, Prospect, Qualified};

        // (from, to, expected_ok)
        let cases = [
            (Prospect, Qualified, true),
            (Qualified, Customer, true),
            (Prospect, Archived, true),
            (Qualified, Archived, true),
            (Customer, Archived, true),
            // Idempotent no-ops.
            (Prospect, Prospect, true),
            (Archived, Archived, true),
            // Illegal: backward motion.
            (Qualified, Prospect, false),
            (Customer, Qualified, false),
            (Customer, Prospect, false),
            // Illegal: skipping a stage.
            (Prospect, Customer, false),
            // Illegal: anything out of Archived.
            (Archived, Prospect, false),
            (Archived, Qualified, false),
            (Archived, Customer, false),
        ];
        for (from, to, expected_ok) in cases {
            let mut contact = test_contact(from);
            let result = contact.transition_to(to);
            assert_eq!(result.is_ok(), expected_ok, "transition {from:?} -> {to:?}");
            if expected_ok && from != to {
                assert_eq!(contact.lifecycle, to);
            } else if !expected_ok {
                assert_eq!(contact.lifecycle, from, "failed transition must not mutate");
            }
        }
    }

    #[test]
    fn illegal_transition_returns_validation_error() {
        let mut contact = test_contact(Lifecycle::Customer);
        let err = contact
            .transition_to(Lifecycle::Prospect)
            .expect_err("must fail");
        assert_eq!(
            err.to_string(),
            "validation failed: illegal lifecycle transition from Customer to Prospect"
        );
    }
}
