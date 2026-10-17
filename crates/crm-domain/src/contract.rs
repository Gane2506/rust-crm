// SPDX-License-Identifier: GPL-3.0-only
//! Contract aggregate: signing state machine and the payment gate.
//!
//! The core legal invariant of this module: **signed does not equal
//! effective**. A contract becomes [`ContractState::Effective`] only after
//! all signers have signed *and* the required payment is verified — see
//! [`Contract::is_effective`] and [`Contract::transition_to`].
//!
//! Legal transitions:
//!
//! ```text
//! Draft -> Sent -> PartiallySigned -> SignedAwaitingPayment -> Effective -> Disputed
//!   |        |           |                  |                     |            |-> Cancelled
//!   v        v           v                  v                     v            v-> Effective (reinstated)
//! Cancelled  Cancelled   Cancelled          Cancelled          Disputed
//!            Expired     Expired            Expired
//! ```
//!
//! `Cancelled` and `Expired` are terminal. `Effective` may only move to
//! `Disputed`; a resolved dispute may reinstate `Effective` or move to
//! `Cancelled`. The `SignedAwaitingPayment -> Effective` edge requires
//! `payment_verified == true`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::ids::{ClientProfileId, ContractId};

/// Signing state of a contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContractState {
    /// Created but not yet sent to signers.
    Draft,
    /// Sent to signers, none signed yet.
    Sent,
    /// At least one signer signed, not all.
    PartiallySigned,
    /// All signers signed; waiting for the required payment.
    SignedAwaitingPayment,
    /// Signed and payment verified: legally effective.
    Effective,
    /// Cancelled before effectiveness (or from dispute).
    Cancelled,
    /// Expired before effectiveness.
    Expired,
    /// Effectiveness is being disputed.
    Disputed,
}

/// What must be paid before the contract becomes effective.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentRequirement {
    /// No payment required; the payment gate passes trivially.
    None,
    /// A deposit must be verified before effectiveness.
    Deposit {
        /// Amount in the smallest currency unit.
        amount_cents: u64,
        /// ISO-4217 currency code, e.g. `"CAD"`.
        currency: String,
    },
    /// The full amount must be verified before effectiveness.
    Full {
        /// Amount in the smallest currency unit.
        amount_cents: u64,
        /// ISO-4217 currency code, e.g. `"CAD"`.
        currency: String,
    },
}

/// One signer on a contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signer {
    /// Signer's display name.
    pub name: String,
    /// Signer's email address.
    pub email: String,
    /// When this signer signed, if they have.
    pub signed_at: Option<DateTime<Utc>>,
}

/// An e-signature contract envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    /// Stable identifier of this contract.
    pub contract_id: ContractId,
    /// Owning client profile (tenant boundary).
    pub client_profile_id: ClientProfileId,
    /// Revision of the template this contract was generated from.
    pub template_revision: String,
    /// The required signers and their signing status.
    pub signers: Vec<Signer>,
    /// Reference to the tamper-evident evidence bundle, if sealed.
    pub evidence_bundle_ref: Option<String>,
    /// Current signing state.
    pub state: ContractState,
    /// What must be paid before the contract becomes effective.
    pub payment_requirement: PaymentRequirement,
    /// When the contract became effective, if it has.
    pub effective_at: Option<DateTime<Utc>>,
}

impl Contract {
    /// Returns `true` only when the state is [`ContractState::Effective`].
    ///
    /// Fully signed but awaiting payment is **not** effective.
    ///
    /// ```
    /// use crm_domain::contract::{Contract, ContractState, PaymentRequirement};
    /// use crm_domain::ids::{ClientProfileId, ContractId};
    ///
    /// let mut contract = Contract {
    ///     contract_id: ContractId::new(),
    ///     client_profile_id: ClientProfileId::new(),
    ///     template_revision: "v3".to_string(),
    ///     signers: vec![],
    ///     evidence_bundle_ref: None,
    ///     state: ContractState::SignedAwaitingPayment,
    ///     payment_requirement: PaymentRequirement::None,
    ///     effective_at: None,
    /// };
    /// // Signed by everyone, but payment not verified: not effective.
    /// assert!(!contract.is_effective());
    /// contract.transition_to(ContractState::Effective, true).unwrap();
    /// assert!(contract.is_effective());
    /// assert!(contract.effective_at.is_some());
    /// ```
    #[must_use]
    pub fn is_effective(&self) -> bool {
        self.state == ContractState::Effective
    }

    /// Moves the contract to the next state, enforcing the legal
    /// transition table (see module docs).
    ///
    /// `payment_verified` must be `true` to take
    /// `SignedAwaitingPayment -> Effective`; the caller (payment
    /// reconciliation) supplies it. Transitioning to the current state
    /// is an idempotent no-op. Entering `Effective` stamps
    /// [`Contract::effective_at`].
    pub fn transition_to(
        &mut self,
        next: ContractState,
        payment_verified: bool,
    ) -> Result<(), DomainError> {
        use ContractState::{
            Cancelled, Disputed, Draft, Effective, Expired, PartiallySigned, Sent,
            SignedAwaitingPayment,
        };
        if next == self.state {
            return Ok(());
        }
        let allowed = match (&self.state, &next) {
            (Draft, Sent) | (Draft, Cancelled) => true,
            (Sent, PartiallySigned) | (Sent, Cancelled) | (Sent, Expired) => true,
            (PartiallySigned, SignedAwaitingPayment)
            | (PartiallySigned, Cancelled)
            | (PartiallySigned, Expired) => true,
            (SignedAwaitingPayment, Effective) => payment_verified,
            (SignedAwaitingPayment, Cancelled) | (SignedAwaitingPayment, Expired) => true,
            (Effective, Disputed) => true,
            (Disputed, Effective) | (Disputed, Cancelled) => true,
            _ => false,
        };
        if !allowed {
            if self.state == SignedAwaitingPayment && next == Effective && !payment_verified {
                return Err(DomainError::Validation(
                    "contract cannot become effective before the required payment is verified"
                        .to_string(),
                ));
            }
            return Err(DomainError::Validation(format!(
                "illegal contract transition from {:?} to {:?}",
                self.state, next
            )));
        }
        if next == Effective {
            self.effective_at = Some(Utc::now());
        }
        self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ContractState::{
        Cancelled, Disputed, Draft, Effective, Expired, PartiallySigned, Sent,
        SignedAwaitingPayment,
    };

    fn test_contract(state: ContractState) -> Contract {
        Contract {
            contract_id: ContractId::new(),
            client_profile_id: ClientProfileId::new(),
            template_revision: "v3".to_string(),
            signers: vec![],
            evidence_bundle_ref: None,
            state,
            payment_requirement: PaymentRequirement::Full {
                amount_cents: 50_000,
                currency: "CAD".to_string(),
            },
            effective_at: None,
        }
    }

    /// (from, to, payment_verified, expected_ok)
    #[test]
    fn contract_transition_table() {
        let cases = [
            // Happy path: draft to effective through the payment gate.
            (Draft, Sent, false, true),
            (Sent, PartiallySigned, false, true),
            (PartiallySigned, SignedAwaitingPayment, false, true),
            (SignedAwaitingPayment, Effective, true, true),
            // Payment gate: unverified payment blocks effectiveness.
            (SignedAwaitingPayment, Effective, false, false),
            // Cancellation / expiry from pre-effective states.
            (Draft, Cancelled, false, true),
            (Sent, Cancelled, false, true),
            (Sent, Expired, false, true),
            (PartiallySigned, Cancelled, false, true),
            (PartiallySigned, Expired, false, true),
            (SignedAwaitingPayment, Cancelled, false, true),
            (SignedAwaitingPayment, Expired, false, true),
            // Effective is terminal except -> Disputed.
            (Effective, Disputed, false, true),
            (Effective, Cancelled, false, false),
            (Effective, Expired, false, false),
            (Effective, Draft, false, false),
            // Dispute resolution paths.
            (Disputed, Effective, true, true),
            (Disputed, Cancelled, false, true),
            (Disputed, Draft, false, false),
            // Terminal states: nothing moves out.
            (Cancelled, Draft, false, false),
            (Cancelled, Effective, true, false),
            (Expired, Draft, false, false),
            (Expired, Sent, false, false),
            // Skipping stages is rejected.
            (Draft, PartiallySigned, false, false),
            (Draft, Effective, true, false),
            (Sent, SignedAwaitingPayment, false, false),
            (Sent, Effective, true, false),
            // Backward motion is rejected.
            (Sent, Draft, false, false),
            (Effective, SignedAwaitingPayment, false, false),
            // Idempotent no-ops.
            (Draft, Draft, false, true),
            (Effective, Effective, false, true),
            (Cancelled, Cancelled, false, true),
        ];
        for (from, to, payment_verified, expected_ok) in cases {
            let mut contract = test_contract(from);
            let result = contract.transition_to(to, payment_verified);
            assert_eq!(
                result.is_ok(),
                expected_ok,
                "transition {from:?} -> {to:?} (payment_verified={payment_verified})"
            );
            if expected_ok && from != to {
                assert_eq!(contract.state, to);
            } else if !expected_ok {
                assert_eq!(contract.state, from, "failed transition must not mutate");
            }
        }
    }

    #[test]
    fn signed_without_verified_payment_is_not_effective() {
        let contract = test_contract(SignedAwaitingPayment);
        assert!(!contract.is_effective());
    }

    #[test]
    fn entering_effective_stamps_effective_at() {
        let mut contract = test_contract(SignedAwaitingPayment);
        assert!(contract.effective_at.is_none());
        contract
            .transition_to(Effective, true)
            .expect("payment verified");
        assert!(contract.is_effective());
        assert!(contract.effective_at.is_some());
    }

    #[test]
    fn payment_gate_error_message_is_explicit() {
        let mut contract = test_contract(SignedAwaitingPayment);
        let err = contract
            .transition_to(Effective, false)
            .expect_err("must fail");
        assert_eq!(
            err.to_string(),
            "validation failed: contract cannot become effective before the required payment is verified"
        );
    }
}
