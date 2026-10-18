// SPDX-License-Identifier: GPL-3.0-only
//! Feed domain: a per-client-profile activity/notification stream.
//!
//! Staff see one unified timeline per client profile surfacing notable
//! events from every workstream: contacts (created, qualified),
//! campaigns (launched), forms (submitted), email (received), tasks
//! (due), notes (added) and agent runs (completed). Feed items are
//! write-once facts produced by the other services; the feed itself
//! only orders, filters and annotates them (read / pinned / hidden).
//!
//! ## Ordering invariant
//!
//! Reads return items **pinned first, then newest-first by
//! `created_at`** ([`compare_feed_items`]). Visibility is sticky:
//! [`FeedItem::hide`] moves an item to [`FeedVisibility::Hidden`]
//! permanently at the domain level — there is deliberately no
//! `unhide()` here; restoring a hidden item requires an explicit
//! service-level decision recorded in the audit log (a 12-FEED-02
//! concern).
//!
//! ## Authorization (deferred)
//!
//! Feed operations will authorize against a `Resource::FeedItem`
//! variant, profile-scoped — the variant and its `"feed_item"`
//! database-string mapping land in task 12-FEED-02 together with the
//! persistence layer (the `resource_as_str` match in `crates/crm-db`
//! is exhaustive).
//!
//! ## Audit
//!
//! Feed mutations are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s by the service
//! implementation, with pinned action names ([`FEED_LISTED`],
//! [`FEED_ITEM_READ`], [`FEED_ITEM_PINNED`], [`FEED_ITEM_HIDDEN`])
//! documented on [`FeedService`](crate::services::FeedService). They
//! are deliberately **not** [`SecurityEvent`](crate::audit::SecurityEvent)
//! variants: that taxonomy covers security-relevant events (auth,
//! RBAC, sessions, rate limits), while the feed is a business
//! operation audited like contacts and campaigns.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::ActorKind;
use crate::error::DomainError;
use crate::ids::{ClientProfileId, FeedItemId};

/// Audit action name recorded when a feed is listed.
pub const FEED_LISTED: &str = "feed.listed";
/// Audit action name recorded when a feed item is marked read.
pub const FEED_ITEM_READ: &str = "feed.item_read";
/// Audit action name recorded when a feed item is pinned.
pub const FEED_ITEM_PINNED: &str = "feed.item_pinned";
/// Audit action name recorded when a feed item is hidden.
pub const FEED_ITEM_HIDDEN: &str = "feed.item_hidden";

/// The workstream event a feed item reports on.
///
/// Every variant names the originating aggregate; the concrete
/// reference (entity type + id) travels on the item's
/// `ref_type`/`ref_id` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeedSource {
    /// A contact was created.
    ContactCreated,
    /// A contact was qualified.
    ContactQualified,
    /// A campaign was launched.
    CampaignLaunched,
    /// A form submission arrived.
    FormSubmitted,
    /// An email was received.
    EmailReceived,
    /// A task became due.
    TaskDue,
    /// A note was added.
    NoteAdded,
    /// An agent run completed.
    AgentRunCompleted,
}

impl FeedSource {
    /// Machine-readable name of the source, used for display grouping.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FeedSource::ContactCreated => "contact.created",
            FeedSource::ContactQualified => "contact.qualified",
            FeedSource::CampaignLaunched => "campaign.launched",
            FeedSource::FormSubmitted => "form.submitted",
            FeedSource::EmailReceived => "email.received",
            FeedSource::TaskDue => "task.due",
            FeedSource::NoteAdded => "note.added",
            FeedSource::AgentRunCompleted => "agent_run.completed",
        }
    }
}

/// Visibility of a feed item.
///
/// `Pinned` items float to the top of reads; `Hidden` items are
/// excluded from reads unless the query opts in. `Hidden` is sticky:
/// the domain offers no `unhide()` — restoring a hidden item is a
/// service-level decision (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeedVisibility {
    /// Normal timeline item.
    Normal,
    /// Stays at the top of reads.
    Pinned,
    /// Excluded from reads by default; sticky.
    Hidden,
}

/// One entry in a client profile's activity feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedItem {
    /// Stable identifier of this item.
    pub feed_item_id: FeedItemId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// The workstream event this item reports on.
    pub source: FeedSource,
    /// Human-readable headline, validated non-blank.
    pub title: String,
    /// Optional longer detail.
    pub body: Option<String>,
    /// Who (or what) caused the underlying event.
    pub actor_kind: ActorKind,
    /// Entity type tag of the source record (e.g. `"contact"`).
    pub ref_type: String,
    /// Id of the source record.
    pub ref_id: Uuid,
    /// Visibility; starts [`FeedVisibility::Normal`].
    pub visibility: FeedVisibility,
    /// Whether staff have read this item.
    pub read: bool,
    /// When the item was created.
    pub created_at: DateTime<Utc>,
}

impl FeedItem {
    /// Creates a feed item with [`FeedVisibility::Normal`] and
    /// `read == false`.
    ///
    /// Rejects: blank `title`, blank `ref_type`.
    ///
    /// ```
    /// use crm_domain::audit::ActorKind;
    /// use crm_domain::feed::{FeedItem, FeedSource, FeedVisibility};
    /// use crm_domain::ids::ClientProfileId;
    /// use uuid::Uuid;
    ///
    /// let item = FeedItem::new(
    ///     ClientProfileId::new(),
    ///     FeedSource::ContactCreated,
    ///     "Ada Lovelace became a prospect".to_string(),
    ///     None,
    ///     ActorKind::Human,
    ///     "contact".to_string(),
    ///     Uuid::new_v4(),
    /// )
    /// .expect("valid item");
    /// assert_eq!(item.visibility, FeedVisibility::Normal);
    /// assert!(!item.read);
    /// ```
    pub fn new(
        client_profile_id: ClientProfileId,
        source: FeedSource,
        title: String,
        body: Option<String>,
        actor_kind: ActorKind,
        ref_type: String,
        ref_id: Uuid,
    ) -> Result<Self, DomainError> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(DomainError::Validation(
                "feed item title must not be blank".to_string(),
            ));
        }
        let ref_type = ref_type.trim().to_owned();
        if ref_type.is_empty() {
            return Err(DomainError::Validation(
                "feed item ref_type must not be blank".to_string(),
            ));
        }
        Ok(Self {
            feed_item_id: FeedItemId::new(),
            client_profile_id,
            source,
            title,
            body: body.map(|b| b.trim().to_owned()).filter(|b| !b.is_empty()),
            actor_kind,
            ref_type,
            ref_id,
            visibility: FeedVisibility::Normal,
            read: false,
            created_at: Utc::now(),
        })
    }

    /// Pins the item so it floats to the top of reads.
    ///
    /// Idempotent on an already-pinned item. Rejects: hidden items —
    /// a hidden item must not reappear without an explicit
    /// service-level restore.
    pub fn pin(&mut self) -> Result<(), DomainError> {
        match self.visibility {
            FeedVisibility::Hidden => Err(DomainError::Validation(
                "cannot pin a hidden feed item".to_string(),
            )),
            FeedVisibility::Normal | FeedVisibility::Pinned => {
                self.visibility = FeedVisibility::Pinned;
                Ok(())
            }
        }
    }

    /// Returns a pinned item to [`FeedVisibility::Normal`].
    ///
    /// Idempotent on a normal item. Rejects: hidden items.
    pub fn unpin(&mut self) -> Result<(), DomainError> {
        match self.visibility {
            FeedVisibility::Hidden => Err(DomainError::Validation(
                "cannot unpin a hidden feed item".to_string(),
            )),
            FeedVisibility::Normal | FeedVisibility::Pinned => {
                self.visibility = FeedVisibility::Normal;
                Ok(())
            }
        }
    }

    /// Hides the item. Sticky: there is no `unhide()` on the domain —
    /// restoring a hidden item requires an explicit service-level
    /// decision (see the module docs).
    ///
    /// Idempotent on an already-hidden item.
    pub fn hide(&mut self) {
        self.visibility = FeedVisibility::Hidden;
    }

    /// Marks the item as read. Idempotent.
    pub fn mark_read(&mut self) {
        self.read = true;
    }
}

/// Ordering comparator for feed reads.
///
/// Pinned items sort before everything else; within the same
/// visibility band, newest-first by `created_at`. Ties on
/// `created_at` break on `feed_item_id` so the order is total and
/// deterministic.
///
/// ```
/// use crm_domain::audit::ActorKind;
/// use crm_domain::feed::{FeedItem, FeedSource, compare_feed_items};
/// use crm_domain::ids::ClientProfileId;
/// use std::cmp::Ordering;
/// use uuid::Uuid;
///
/// let mk = |title: &str| {
///     FeedItem::new(
///         ClientProfileId::new(),
///         FeedSource::NoteAdded,
///         title.to_string(),
///         None,
///         ActorKind::Human,
///         "note".to_string(),
///         Uuid::new_v4(),
///     )
///     .expect("valid item")
/// };
///
/// let mut a = mk("a");
/// let mut b = mk("b");
/// b.pin().expect("pin");
/// // A pinned item sorts before an unpinned one regardless of age.
/// assert_eq!(compare_feed_items(&a, &b), Ordering::Greater);
/// ```
#[must_use]
pub fn compare_feed_items(a: &FeedItem, b: &FeedItem) -> Ordering {
    fn rank(visibility: FeedVisibility) -> u8 {
        match visibility {
            FeedVisibility::Pinned => 0,
            FeedVisibility::Normal => 1,
            FeedVisibility::Hidden => 2,
        }
    }
    rank(a.visibility)
        .cmp(&rank(b.visibility))
        .then_with(|| b.created_at.cmp(&a.created_at))
        .then_with(|| a.feed_item_id.as_uuid().cmp(&b.feed_item_id.as_uuid()))
}

/// Read parameters for a feed listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedQuery {
    /// Maximum number of items to return. Must be positive.
    pub limit: u64,
    /// Whether hidden items are included (default reads exclude them).
    pub include_hidden: bool,
    /// Whether only unread items are returned.
    pub unread_only: bool,
}

impl FeedQuery {
    /// Creates a query, rejecting a zero `limit`.
    pub fn new(limit: u64, include_hidden: bool, unread_only: bool) -> Result<Self, DomainError> {
        if limit == 0 {
            return Err(DomainError::Validation(
                "feed query limit must be positive".to_string(),
            ));
        }
        Ok(Self {
            limit,
            include_hidden,
            unread_only,
        })
    }

    /// Pure predicate: whether `item` survives this query's filters.
    /// Ordering is applied separately by [`compare_feed_items`].
    #[must_use]
    pub fn matches(&self, item: &FeedItem) -> bool {
        if item.visibility == FeedVisibility::Hidden && !self.include_hidden {
            return false;
        }
        if self.unread_only && item.read {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str) -> FeedItem {
        FeedItem::new(
            ClientProfileId::new(),
            FeedSource::ContactCreated,
            title.to_string(),
            None,
            ActorKind::Human,
            "contact".to_string(),
            Uuid::new_v4(),
        )
        .expect("valid item")
    }

    #[test]
    fn new_rejects_blank_title() {
        let err = FeedItem::new(
            ClientProfileId::new(),
            FeedSource::NoteAdded,
            "   ".to_string(),
            None,
            ActorKind::Human,
            "note".to_string(),
            Uuid::new_v4(),
        )
        .expect_err("blank title must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn new_rejects_blank_ref_type() {
        let err = FeedItem::new(
            ClientProfileId::new(),
            FeedSource::NoteAdded,
            "A note".to_string(),
            None,
            ActorKind::Human,
            "  ".to_string(),
            Uuid::new_v4(),
        )
        .expect_err("blank ref_type must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn new_trims_title_and_ref_type() {
        let i = FeedItem::new(
            ClientProfileId::new(),
            FeedSource::EmailReceived,
            "  New email  ".to_string(),
            None,
            ActorKind::Agent,
            "  email  ".to_string(),
            Uuid::new_v4(),
        )
        .expect("valid item");
        assert_eq!(i.title, "New email");
        assert_eq!(i.ref_type, "email");
    }

    #[test]
    fn new_normalizes_blank_body_to_none() {
        let i = FeedItem::new(
            ClientProfileId::new(),
            FeedSource::NoteAdded,
            "Note".to_string(),
            Some("   ".to_string()),
            ActorKind::Human,
            "note".to_string(),
            Uuid::new_v4(),
        )
        .expect("valid item");
        assert_eq!(i.body, None);
    }

    #[test]
    fn pin_and_unpin_round_trip() {
        let mut i = item("x");
        assert_eq!(i.visibility, FeedVisibility::Normal);
        i.pin().expect("pin");
        assert_eq!(i.visibility, FeedVisibility::Pinned);
        i.pin().expect("pin is idempotent");
        i.unpin().expect("unpin");
        assert_eq!(i.visibility, FeedVisibility::Normal);
        i.unpin().expect("unpin is idempotent");
    }

    #[test]
    fn hide_is_sticky() {
        let mut i = item("x");
        i.pin().expect("pin");
        i.hide();
        assert_eq!(i.visibility, FeedVisibility::Hidden);
        // Sticky: no way back through pin/unpin.
        assert!(i.pin().is_err());
        assert!(i.unpin().is_err());
        // Idempotent hide.
        i.hide();
        assert_eq!(i.visibility, FeedVisibility::Hidden);
    }

    #[test]
    fn mark_read_is_idempotent() {
        let mut i = item("x");
        assert!(!i.read);
        i.mark_read();
        assert!(i.read);
        i.mark_read();
        assert!(i.read);
    }

    #[test]
    fn feed_source_as_str_is_stable() {
        assert_eq!(FeedSource::ContactCreated.as_str(), "contact.created");
        assert_eq!(FeedSource::FormSubmitted.as_str(), "form.submitted");
        assert_eq!(
            FeedSource::AgentRunCompleted.as_str(),
            "agent_run.completed"
        );
    }

    #[test]
    fn comparator_pinned_first_then_newest() {
        let old = item("old");
        // Give `new` a strictly later timestamp via construction order.
        let new = item("new");
        assert_eq!(compare_feed_items(&new, &old), Ordering::Less);
        assert_eq!(compare_feed_items(&old, &new), Ordering::Greater);

        let mut pinned = item("pinned");
        pinned.pin().expect("pin");
        // A pinned item sorts before a newer unpinned one.
        assert_eq!(compare_feed_items(&pinned, &new), Ordering::Less);
        assert_eq!(compare_feed_items(&new, &pinned), Ordering::Greater);
    }

    #[test]
    fn comparator_orders_hidden_last() {
        let mut hidden = item("hidden");
        hidden.hide();
        let normal = item("normal");
        assert_eq!(compare_feed_items(&hidden, &normal), Ordering::Greater);
        assert_eq!(compare_feed_items(&normal, &hidden), Ordering::Less);
    }

    #[test]
    fn comparator_is_total_and_deterministic() {
        let a = item("same");
        let b = item("same");
        // Same timestamp band (constructed back-to-back): the id tiebreak
        // keeps the order total — one of them is strictly Less.
        let ab = compare_feed_items(&a, &b);
        let ba = compare_feed_items(&b, &a);
        assert!(matches!(ab, Ordering::Less | Ordering::Greater));
        assert_eq!(ab.reverse(), ba);
    }

    #[test]
    fn query_rejects_zero_limit() {
        let err = FeedQuery::new(0, false, false).expect_err("zero limit must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn query_matches_filters() {
        let q = FeedQuery::new(10, false, false).expect("valid query");
        let normal = item("n");
        assert!(q.matches(&normal));

        let mut hidden = item("h");
        hidden.hide();
        assert!(!q.matches(&hidden));
        let with_hidden = FeedQuery::new(10, true, false).expect("valid query");
        assert!(with_hidden.matches(&hidden));

        let mut read = item("r");
        read.mark_read();
        assert!(q.matches(&read));
        let unread_only = FeedQuery::new(10, false, true).expect("valid query");
        assert!(!unread_only.matches(&read));
        assert!(unread_only.matches(&normal));
    }
}
