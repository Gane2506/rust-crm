// SPDX-License-Identifier: GPL-3.0-only
//! Typed UUID newtypes used as stable entity identifiers.
//!
//! Every identifier is an opaque wrapper around [`Uuid`]. The newtypes are
//! `Copy`, hashable and serde-compatible, and they prevent the classic bug
//! of passing e.g. a [`ContactId`] where a [`ContractId`] is expected.
//! IDs are generated with UUID v4 randomness; they carry no business
//! meaning and are never reused after an entity is archived or deleted.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a new random (UUID v4) identifier.
            ///
            /// `Default` is deliberately *not* implemented: a random UUID
            /// is not a neutral fixed value, so `Self::default()` would
            /// return a different id on every call.
            #[allow(clippy::new_without_default)]
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Returns the underlying [`Uuid`].
            #[must_use]
            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id!(
    /// Identifies a client profile (an account/workspace for work performed
    /// on behalf of one business). The tenant boundary for all reads/writes.
    ClientProfileId
);
define_id!(
    /// Identifies an agency staff member (a human operator of the CRM).
    StaffId
);
define_id!(
    /// Identifies a contact (a person record inside a client profile).
    ContactId
);
define_id!(
    /// Identifies a contract (an e-signature envelope).
    ContractId
);
define_id!(
    /// Identifies a single agent tool-call action record.
    AgentActionId
);
define_id!(
    /// Identifies an outreach campaign.
    CampaignId
);
define_id!(
    /// Identifies an audit event in the tamper-evident log.
    AuditEventId
);
define_id!(
    /// Identifies the actor recorded on an [`crate::audit::AuditEvent`].
    ///
    /// For human actors this wraps the [`StaffId`]; for agent actors it
    /// wraps the agent principal id; for system actors a well-known id.
    ActorId
);
define_id!(
    /// Identifies the agency (the operator of the CRM). Staff users and
    /// scopes are bound to one agency; cross-agency access is impossible
    /// by construction in [`crate::security::Scope::covers`].
    AgencyId
);
define_id!(
    /// Identifies a staff login session.
    SessionId
);
define_id!(
    /// Identifies an RBAC grant (one action authorized within one scope).
    GrantId
);
define_id!(
    /// Identifies a named role (a reusable bundle of permissions).
    RoleId
);
define_id!(
    /// Identifies one bounded delegation of grants from a staff user to
    /// an agent principal.
    DelegationId
);
define_id!(
    /// Identifies an AI agent principal acting under delegation.
    AgentPrincipalId
);
define_id!(
    /// Identifies a file/media asset attached to a client profile.
    AssetId
);
define_id!(
    /// Identifies an email message inside a client profile.
    EmailMessageId
);
define_id!(
    /// Identifies an email thread (conversation) inside a client profile.
    EmailThreadId
);
define_id!(
    /// Identifies the link between an email message and its asset bytes.
    EmailAttachmentId
);
define_id!(
    /// Identifies one step in a campaign's outreach sequence.
    CampaignStepId
);
define_id!(
    /// Identifies a named prospect pool inside a client profile.
    ProspectPoolId
);
define_id!(
    /// Identifies one opt-out record (immutable once created).
    OptOutId
);
define_id!(
    /// Identifies one recorded conversion.
    ConversionId
);

impl From<StaffId> for ActorId {
    /// Renders a staff member as the actor of an audit event.
    fn from(staff: StaffId) -> Self {
        Self(staff.as_uuid())
    }
}

impl From<AgentPrincipalId> for ActorId {
    /// Renders an agent principal as the actor of an audit event.
    fn from(agent: AgentPrincipalId) -> Self {
        Self(agent.as_uuid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts serde + Display round-trips for one ID type.
    fn assert_id_round_trip<T>(id: T)
    where
        T: Copy + PartialEq + fmt::Debug + fmt::Display + Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(&id).expect("serialize id");
        let back: T = serde_json::from_str(&json).expect("deserialize id");
        assert_eq!(id, back);
        assert!(!id.to_string().is_empty());
    }

    #[test]
    fn all_ids_round_trip_through_serde() {
        assert_id_round_trip(ClientProfileId::new());
        assert_id_round_trip(StaffId::new());
        assert_id_round_trip(ContactId::new());
        assert_id_round_trip(ContractId::new());
        assert_id_round_trip(AgentActionId::new());
        assert_id_round_trip(CampaignId::new());
        assert_id_round_trip(AuditEventId::new());
        assert_id_round_trip(ActorId::new());
        assert_id_round_trip(AgencyId::new());
        assert_id_round_trip(SessionId::new());
        assert_id_round_trip(GrantId::new());
        assert_id_round_trip(RoleId::new());
        assert_id_round_trip(DelegationId::new());
        assert_id_round_trip(AgentPrincipalId::new());
        assert_id_round_trip(AssetId::new());
        assert_id_round_trip(EmailMessageId::new());
        assert_id_round_trip(EmailThreadId::new());
        assert_id_round_trip(EmailAttachmentId::new());
        assert_id_round_trip(CampaignStepId::new());
        assert_id_round_trip(ProspectPoolId::new());
        assert_id_round_trip(OptOutId::new());
        assert_id_round_trip(ConversionId::new());
    }

    #[test]
    fn fresh_ids_are_unique() {
        // Only uniqueness is asserted, never concrete values.
        assert_ne!(ContactId::new(), ContactId::new());
        assert_ne!(ContractId::new(), ContractId::new());
    }

    #[test]
    fn ids_convert_to_and_from_uuid() {
        let uuid = Uuid::new_v4();
        let id = ContactId::from(uuid);
        assert_eq!(id.as_uuid(), uuid);
        assert_eq!(Uuid::from(id), uuid);
        assert_eq!(id.to_string(), uuid.to_string());
    }

    #[test]
    fn staff_id_converts_to_actor_id() {
        let staff = StaffId::new();
        let actor = ActorId::from(staff);
        assert_eq!(actor.as_uuid(), staff.as_uuid());
    }

    #[test]
    fn agent_principal_id_converts_to_actor_id() {
        let agent = AgentPrincipalId::new();
        let actor = ActorId::from(agent);
        assert_eq!(actor.as_uuid(), agent.as_uuid());
    }
}
