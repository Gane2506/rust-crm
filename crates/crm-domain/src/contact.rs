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

/// Where a contact came from — the acquisition channel.
///
/// Contacts store sources as plain strings (`first_source`,
/// `latest_source`, `source_touches`) for forward compatibility; this
/// enum is the typed vocabulary for producing and interpreting those
/// strings. [`ContactSource::parse`] maps unknown strings to
/// [`ContactSource::Other`] so new channels never break readers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContactSource {
    /// Entered by hand by agency staff.
    Manual,
    /// Bulk import (CSV or similar).
    Import,
    /// Submitted through an embedded web form.
    WebForm,
    /// Created through the API.
    Api,
    /// Produced by an agent action run.
    AgentAction,
    /// Acquired through a campaign.
    Campaign,
    /// Forward-compatible catch-all for channels added later.
    Other(String),
}

impl ContactSource {
    /// The canonical string stored on the contact.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Manual => "manual",
            Self::Import => "import",
            Self::WebForm => "web-form",
            Self::Api => "api",
            Self::AgentAction => "agent-action",
            Self::Campaign => "campaign",
            Self::Other(s) => s.as_str(),
        }
    }

    /// Parses a stored source string. Unknown strings become
    /// [`ContactSource::Other`] — never an error — so readers stay
    /// compatible with channels added after they were written.
    ///
    /// ```
    /// use crm_domain::contact::ContactSource;
    ///
    /// assert_eq!(ContactSource::parse("web-form"), ContactSource::WebForm);
    /// assert_eq!(
    ///     ContactSource::parse("referral"),
    ///     ContactSource::Other("referral".to_string())
    /// );
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Self {
        let normalized = s.trim().to_lowercase();
        match normalized.as_str() {
            "manual" => Self::Manual,
            "import" => Self::Import,
            "web-form" => Self::WebForm,
            "api" => Self::Api,
            "agent-action" => Self::AgentAction,
            "campaign" => Self::Campaign,
            other => Self::Other(other.to_string()),
        }
    }
}

/// One identity signal that two contacts may be the same person.
///
/// Dedupe is always scoped to a single client profile: a contact never
/// dedupes against a contact in another profile, so
/// [`Contact::dedupe_signals`] returns an empty vec across profiles.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DedupeSignal {
    /// A normalized email address present on both contacts.
    SharedEmail(String),
    /// A normalized phone number present on both contacts.
    SharedPhone(String),
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

    /// Records a source touch on the contact.
    ///
    /// Appends the source's canonical string to `source_touches`
    /// (deduplicated, preserving first-seen order) and updates
    /// `latest_source`. `first_source` is set at creation and never
    /// changes — it stays the original acquisition channel.
    ///
    /// ```
    /// use crm_domain::contact::{Contact, ContactSource, Lifecycle};
    /// use crm_domain::ids::{ClientProfileId, ContactId};
    ///
    /// let mut contact = Contact {
    ///     contact_id: ContactId::new(),
    ///     client_profile_id: ClientProfileId::new(),
    ///     normalized_emails: vec![],
    ///     normalized_phones: vec![],
    ///     lifecycle: Lifecycle::Prospect,
    ///     first_source: ContactSource::Import.as_str().to_string(),
    ///     latest_source: ContactSource::Import.as_str().to_string(),
    ///     source_touches: vec![ContactSource::Import.as_str().to_string()],
    ///     consent_records: vec![],
    ///     merged_into: None,
    /// };
    ///
    /// contact.record_source(&ContactSource::WebForm);
    /// assert_eq!(contact.latest_source, "web-form");
    /// assert_eq!(contact.first_source, "import");
    /// assert_eq!(contact.source_touches, vec!["import", "web-form"]);
    /// // Recording the same source twice does not duplicate the touch.
    /// contact.record_source(&ContactSource::WebForm);
    /// assert_eq!(contact.source_touches, vec!["import", "web-form"]);
    /// ```
    pub fn record_source(&mut self, source: &ContactSource) {
        let name = source.as_str().to_owned();
        if !self.source_touches.contains(&name) {
            self.source_touches.push(name.clone());
        }
        self.latest_source = name;
    }

    /// Returns the identity signals shared with `other`.
    ///
    /// Two contacts are the same person when they share at least one
    /// normalized email or one normalized phone number. The comparison
    /// is profile-scoped: contacts in different client profiles never
    /// match, so cross-tenant merges are impossible by construction.
    /// A contact never matches itself.
    ///
    /// ```
    /// use crm_domain::contact::{Contact, DedupeSignal, Lifecycle};
    /// use crm_domain::ids::{ClientProfileId, ContactId};
    ///
    /// let profile = ClientProfileId::new();
    /// let mk = |emails: Vec<&str>| Contact {
    ///     contact_id: ContactId::new(),
    ///     client_profile_id: profile,
    ///     normalized_emails: emails.into_iter().map(str::to_string).collect(),
    ///     normalized_phones: vec![],
    ///     lifecycle: Lifecycle::Prospect,
    ///     first_source: "import".to_string(),
    ///     latest_source: "import".to_string(),
    ///     source_touches: vec!["import".to_string()],
    ///     consent_records: vec![],
    ///     merged_into: None,
    /// };
    ///
    /// let a = mk(vec!["a@example.com"]);
    /// let b = mk(vec!["a@example.com", "b@example.com"]);
    /// assert_eq!(
    ///     a.dedupe_signals(&b),
    ///     vec![DedupeSignal::SharedEmail("a@example.com".to_string())]
    /// );
    /// // A contact never matches itself.
    /// assert!(a.dedupe_signals(&a).is_empty());
    /// ```
    #[must_use]
    pub fn dedupe_signals(&self, other: &Contact) -> Vec<DedupeSignal> {
        if self.contact_id == other.contact_id || self.client_profile_id != other.client_profile_id
        {
            return Vec::new();
        }
        let mut signals = Vec::new();
        for email in &self.normalized_emails {
            if other.normalized_emails.contains(email) {
                signals.push(DedupeSignal::SharedEmail(email.clone()));
            }
        }
        for phone in &self.normalized_phones {
            if other.normalized_phones.contains(phone) {
                signals.push(DedupeSignal::SharedPhone(phone.clone()));
            }
        }
        signals
    }

    /// Merges `absorbed` into `self` (the survivor).
    ///
    /// Field precedence:
    /// * `normalized_emails` / `normalized_phones`: union, deduplicated,
    ///   sorted for determinism.
    /// * `lifecycle`: the more advanced stage wins (`Customer` beats
    ///   `Qualified` beats `Prospect`); an archived survivor stays
    ///   archived. This is not a [`Contact::transition_to`] — the merged
    ///   record legitimately represents the further-along party.
    /// * `first_source`: the survivor's (earliest acquisition wins).
    /// * `latest_source`: the survivor's, unless empty.
    /// * `source_touches`: union, survivor's order first.
    /// * `consent_records`: union, deduplicated by
    ///   channel + grant time + source.
    ///
    /// The absorbed contact is tombstoned via `merged_into` — never
    /// deleted, so the merge stays auditable and reversible by support.
    ///
    /// Rejects: different client profiles, either party already merged,
    /// or merging a contact into itself.
    pub fn merge_from(&mut self, absorbed: &mut Contact) -> Result<(), DomainError> {
        if self.contact_id == absorbed.contact_id {
            return Err(DomainError::Validation(
                "cannot merge a contact into itself".to_string(),
            ));
        }
        if self.client_profile_id != absorbed.client_profile_id {
            return Err(DomainError::Validation(
                "cannot merge contacts from different client profiles".to_string(),
            ));
        }
        if self.merged_into.is_some() {
            return Err(DomainError::Conflict(
                "merge survivor is itself merged away".to_string(),
            ));
        }
        if absorbed.merged_into.is_some() {
            return Err(DomainError::Conflict(
                "absorbed contact is already merged away".to_string(),
            ));
        }

        // Identity: union, deduped, sorted for a deterministic record.
        let mut emails = self.normalized_emails.clone();
        for email in &absorbed.normalized_emails {
            if !emails.contains(email) {
                emails.push(email.clone());
            }
        }
        emails.sort();
        self.normalized_emails = emails;

        let mut phones = self.normalized_phones.clone();
        for phone in &absorbed.normalized_phones {
            if !phones.contains(phone) {
                phones.push(phone.clone());
            }
        }
        phones.sort();
        self.normalized_phones = phones;

        // Lifecycle: the further-along stage wins; archived stays archived.
        if lifecycle_rank(absorbed.lifecycle) > lifecycle_rank(self.lifecycle) {
            self.lifecycle = absorbed.lifecycle;
        }

        // Sources: earliest wins for first_source; survivor's latest stands.
        if self.first_source.is_empty() && !absorbed.first_source.is_empty() {
            self.first_source.clone_from(&absorbed.first_source);
        }
        if self.latest_source.is_empty() && !absorbed.latest_source.is_empty() {
            self.latest_source.clone_from(&absorbed.latest_source);
        }
        for touch in &absorbed.source_touches {
            if !self.source_touches.contains(touch) {
                self.source_touches.push(touch.clone());
            }
        }

        // Consent: union, deduped by channel + grant time + source.
        for record in &absorbed.consent_records {
            let duplicate = self.consent_records.iter().any(|existing| {
                existing.channel == record.channel
                    && existing.granted_at == record.granted_at
                    && existing.source == record.source
            });
            if !duplicate {
                self.consent_records.push(record.clone());
            }
        }

        // Tombstone the absorbed record — never delete.
        absorbed.merged_into = Some(self.contact_id);
        Ok(())
    }
}

/// Rank of a lifecycle stage for merge precedence: further along wins.
fn lifecycle_rank(stage: Lifecycle) -> u8 {
    match stage {
        Lifecycle::Prospect => 0,
        Lifecycle::Qualified => 1,
        Lifecycle::Customer => 2,
        Lifecycle::Archived => 3,
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

    #[test]
    fn contact_source_round_trips() {
        use ContactSource::{AgentAction, Api, Campaign, Import, Manual, Other, WebForm};
        let cases = [
            (Manual, "manual"),
            (Import, "import"),
            (WebForm, "web-form"),
            (Api, "api"),
            (AgentAction, "agent-action"),
            (Campaign, "campaign"),
        ];
        for (source, name) in cases {
            assert_eq!(source.as_str(), name);
            assert_eq!(ContactSource::parse(name), source);
        }
        // Unknown channels survive as Other — forward compatibility.
        assert_eq!(
            ContactSource::parse("referral"),
            Other("referral".to_string())
        );
        assert_eq!(ContactSource::parse("  WEB-FORM  "), WebForm);
    }

    #[test]
    fn dedupe_signals_are_profile_scoped() {
        let profile = ClientProfileId::new();
        let other_profile = ClientProfileId::new();
        let mut a = test_contact(Lifecycle::Prospect);
        a.client_profile_id = profile;
        a.normalized_emails = vec!["a@example.com".to_string()];
        a.normalized_phones = vec!["+15550001111".to_string()];

        let mut b = test_contact(Lifecycle::Prospect);
        b.client_profile_id = profile;
        b.contact_id = ContactId::new();
        b.normalized_emails = vec!["a@example.com".to_string()];
        b.normalized_phones = vec!["+15550002222".to_string()];

        let signals = a.dedupe_signals(&b);
        assert_eq!(
            signals,
            vec![DedupeSignal::SharedEmail("a@example.com".to_string())]
        );

        // Same identity, different profile: no match, ever.
        b.client_profile_id = other_profile;
        assert!(a.dedupe_signals(&b).is_empty());

        // Phone signals work too.
        b.client_profile_id = profile;
        b.normalized_emails = vec!["other@example.com".to_string()];
        b.normalized_phones = vec!["+15550001111".to_string()];
        let signals = a.dedupe_signals(&b);
        assert_eq!(
            signals,
            vec![DedupeSignal::SharedPhone("+15550001111".to_string())]
        );

        // No shared identity: no signals.
        b.normalized_phones = vec!["+15550009999".to_string()];
        assert!(a.dedupe_signals(&b).is_empty());
    }

    #[test]
    fn merge_unions_identity_and_tombstones_absorbed() {
        let profile = ClientProfileId::new();
        let mut survivor = test_contact(Lifecycle::Qualified);
        survivor.client_profile_id = profile;
        survivor.normalized_emails = vec!["a@example.com".to_string()];
        survivor.first_source = "import".to_string();
        survivor.latest_source = "import".to_string();
        survivor.source_touches = vec!["import".to_string()];

        let mut absorbed = test_contact(Lifecycle::Customer);
        absorbed.client_profile_id = profile;
        absorbed.contact_id = ContactId::new();
        absorbed.normalized_emails = vec!["b@example.com".to_string(), "a@example.com".to_string()];
        absorbed.normalized_phones = vec!["+15550001111".to_string()];
        absorbed.first_source = "web-form".to_string();
        absorbed.latest_source = "web-form".to_string();
        absorbed.source_touches = vec!["web-form".to_string(), "import".to_string()];
        absorbed.consent_records = vec![ConsentRecord {
            channel: "email".to_string(),
            granted_at: chrono::Utc::now(),
            source: "web-form".to_string(),
        }];

        let survivor_id = survivor.contact_id;
        survivor.merge_from(&mut absorbed).expect("merge");

        // Identity unioned, deduped, sorted.
        assert_eq!(
            survivor.normalized_emails,
            vec!["a@example.com".to_string(), "b@example.com".to_string()]
        );
        assert_eq!(survivor.normalized_phones, vec!["+15550001111".to_string()]);
        // Lifecycle: further along wins.
        assert_eq!(survivor.lifecycle, Lifecycle::Customer);
        // First source stays the survivor's; touches unioned in order.
        assert_eq!(survivor.first_source, "import");
        assert_eq!(survivor.latest_source, "import");
        assert_eq!(survivor.source_touches, vec!["import", "web-form"]);
        // Consent carried over.
        assert_eq!(survivor.consent_records.len(), 1);
        // Absorbed is tombstoned, never deleted.
        assert_eq!(absorbed.merged_into, Some(survivor_id));
    }

    #[test]
    fn merge_rejects_invalid_pairs() {
        let profile = ClientProfileId::new();
        let mut survivor = test_contact(Lifecycle::Prospect);
        survivor.client_profile_id = profile;

        // Self-merge.
        let mut same = survivor.clone();
        assert!(survivor.merge_from(&mut same).is_err());

        // Cross-profile merge.
        let mut cross = test_contact(Lifecycle::Prospect);
        cross.client_profile_id = ClientProfileId::new();
        cross.contact_id = ContactId::new();
        let err = survivor.merge_from(&mut cross).expect_err("cross-profile");
        assert_eq!(
            err.to_string(),
            "validation failed: cannot merge contacts from different client profiles"
        );

        // Merging an already-merged contact.
        let mut absorbed = test_contact(Lifecycle::Prospect);
        absorbed.client_profile_id = profile;
        absorbed.contact_id = ContactId::new();
        absorbed.merged_into = Some(ContactId::new());
        let err = survivor.merge_from(&mut absorbed).expect_err("tombstone");
        assert!(err.to_string().starts_with("conflict:"));

        // Survivor that is itself merged away.
        survivor.merged_into = Some(ContactId::new());
        let mut fresh = test_contact(Lifecycle::Prospect);
        fresh.client_profile_id = profile;
        fresh.contact_id = ContactId::new();
        let err = survivor
            .merge_from(&mut fresh)
            .expect_err("survivor tombstone");
        assert!(err.to_string().starts_with("conflict:"));
    }

    #[test]
    fn archived_survivor_stays_archived_on_merge() {
        let profile = ClientProfileId::new();
        let mut survivor = test_contact(Lifecycle::Archived);
        survivor.client_profile_id = profile;
        let mut absorbed = test_contact(Lifecycle::Customer);
        absorbed.client_profile_id = profile;
        absorbed.contact_id = ContactId::new();
        survivor.merge_from(&mut absorbed).expect("merge");
        assert_eq!(survivor.lifecycle, Lifecycle::Archived);
    }
}
