// SPDX-License-Identifier: GPL-3.0-only
//! Contract signing domain: Rust-native templates, signing evidence,
//! and the service/repository ports.
//!
//! This module extends the [`Contract`](crate::contract::Contract)
//! aggregate (task 01-FND-01), which already owns the signing state
//! machine (`Draft → Sent → PartiallySigned → SignedAwaitingPayment →
//! Effective`, plus `Cancelled`/`Expired`/`Disputed`) and the payment
//! gate (`SignedAwaitingPayment → Effective` requires verified
//! payment). Nothing here duplicates that aggregate; instead this
//! module adds the pieces 13-SIGN needs on top of it:
//!
//! - [`SigningTemplate`]: Rust-native contract templates with
//!   `{{placeholder}}` variables. Templates are immutable — a new
//!   version is a new template (same pattern as `WorkflowVersion`).
//! - [`SigningEvidence`]: the forensic record of one signature — who
//!   signed, when, and how. Raw IPs and user agents never persist;
//!   only their SHA-256 fingerprints (see
//!   [`fingerprint`](crate::audit::fingerprint)).
//! - [`ContractDocument`]: the immutable rendered snapshot a contract
//!   is generated from. Contracts reference the snapshot, never the
//!   live template, so later template edits cannot rewrite history.
//!
//! ## Authorization
//!
//! Signing operations authorize against `Resource::Contract`
//! (profile-scoped; the variant lands in task 13-SIGN-02 together with
//! the persistence layer).
//!
//! ## Audit
//!
//! Signing mutations are recorded as generic
//! [`AuditEvent`](crate::audit::AuditEvent)s by the service
//! implementation, with pinned action names ([`SIGNING_CONTRACT_CREATED`]
//! … [`SIGNING_TEMPLATE_CREATED`]) documented on
//! [`SigningService`](crate::services::SigningService). They are
//! deliberately **not** [`SecurityEvent`](crate::audit::SecurityEvent)
//! variants: that taxonomy covers security-relevant events, while
//! signing is a business operation audited like invoices and campaigns.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::audit::fingerprint;
use crate::error::DomainError;
use crate::ids::{ContractId, SigningTemplateId};

/// Audit action name recorded when a contract is created.
pub const SIGNING_CONTRACT_CREATED: &str = "signing.contract_created";
/// Audit action name recorded when a contract is sent for signature.
pub const SIGNING_SIGNATURE_REQUESTED: &str = "signing.signature_requested";
/// Audit action name recorded when a signer signs.
pub const SIGNING_CONTRACT_SIGNED: &str = "signing.contract_signed";
/// Audit action name recorded when a contract becomes effective.
pub const SIGNING_CONTRACT_EFFECTIVE: &str = "signing.contract_effective";
/// Audit action name recorded when the payment gate is satisfied.
pub const SIGNING_PAYMENT_GATE_SATISFIED: &str = "signing.payment_gate_satisfied";
/// Audit action name recorded when a contract is voided/cancelled.
pub const SIGNING_CONTRACT_VOIDED: &str = "signing.contract_voided";
/// Audit action name recorded when a signing template is created.
pub const SIGNING_TEMPLATE_CREATED: &str = "signing.template_created";

/// How a signer applied their signature.
///
/// Typed, not free text: the method is part of the evidence and must
/// be one of the flows the product actually implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignatureMethod {
    /// The signer typed their name into a signature field.
    TypedSignature,
    /// The signer drew their signature (stored as an asset reference
    /// by the service; the method here only records *how*).
    DrawnSignature,
    /// The signer clicked an "I agree / sign" button.
    ClickToSign,
}

impl SignatureMethod {
    /// Machine-readable name of the method.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SignatureMethod::TypedSignature => "typed",
            SignatureMethod::DrawnSignature => "drawn",
            SignatureMethod::ClickToSign => "click_to_sign",
        }
    }
}

/// Forensic record of one signature on a contract.
///
/// The raw IP address and user agent never persist — only their
/// SHA-256 fingerprints (see [`fingerprint`](crate::audit::fingerprint)),
/// so the evidence proves *which* network/client signed without
/// storing PII in the clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningEvidence {
    /// Contract this signature belongs to.
    pub contract_id: ContractId,
    /// Signer's display name.
    pub signer_name: String,
    /// Signer's email address.
    pub signer_email: String,
    /// When the signature was applied.
    pub signed_at: DateTime<Utc>,
    /// SHA-256 hex of the signer's IP address.
    pub ip_hash: String,
    /// SHA-256 hex of the signer's user agent.
    pub user_agent_hash: String,
    /// How the signature was applied.
    pub method: SignatureMethod,
}

impl SigningEvidence {
    /// Records one signature, fingerprinting the network identifiers.
    ///
    /// Rejects: blank signer name/email; blank IP or user agent
    /// (the hashes would be meaningless).
    ///
    /// ```
    /// use crm_domain::ids::ContractId;
    /// use crm_domain::signing::{SignatureMethod, SigningEvidence};
    ///
    /// let evidence = SigningEvidence::new(
    ///     ContractId::new(),
    ///     "Ada Lovelace".to_string(),
    ///     "ada@example.com".to_string(),
    ///     "203.0.113.7".to_string(),
    ///     "TestAgent/1.0".to_string(),
    ///     SignatureMethod::TypedSignature,
    /// )
    /// .expect("valid evidence");
    /// // The raw IP is not recoverable from the evidence.
    /// assert!(!evidence.ip_hash.contains("203.0.113.7"));
    /// assert_eq!(evidence.ip_hash.len(), 64);
    /// ```
    pub fn new(
        contract_id: ContractId,
        signer_name: String,
        signer_email: String,
        ip_address: String,
        user_agent: String,
        method: SignatureMethod,
    ) -> Result<Self, DomainError> {
        if signer_name.trim().is_empty() {
            return Err(DomainError::Validation(
                "signer name must not be blank".to_string(),
            ));
        }
        if signer_email.trim().is_empty() {
            return Err(DomainError::Validation(
                "signer email must not be blank".to_string(),
            ));
        }
        if ip_address.trim().is_empty() {
            return Err(DomainError::Validation(
                "signer IP address must not be blank".to_string(),
            ));
        }
        if user_agent.trim().is_empty() {
            return Err(DomainError::Validation(
                "signer user agent must not be blank".to_string(),
            ));
        }
        Ok(SigningEvidence {
            contract_id,
            signer_name: signer_name.trim().to_string(),
            signer_email: signer_email.trim().to_string(),
            signed_at: Utc::now(),
            ip_hash: fingerprint(ip_address.trim()),
            user_agent_hash: fingerprint(user_agent.trim()),
            method,
        })
    }
}

/// Extracts `{{placeholder}}` names from a template body.
///
/// Placeholder names must be non-empty and alphanumeric + underscore.
/// Returns the names in order of first appearance, deduplicated.
///
/// ```
/// use crm_domain::signing::extract_placeholders;
///
/// let names = extract_placeholders("Dear {{client_name}}, your {{plan}} starts {{start_date}}.")
///     .expect("valid placeholders");
/// assert_eq!(names, vec!["client_name", "plan", "start_date"]);
/// ```
pub fn extract_placeholders(body: &str) -> Result<Vec<String>, DomainError> {
    let mut names = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(DomainError::Validation(
                "template has an unclosed {{ placeholder".to_string(),
            ));
        };
        let name = after_open[..close].trim();
        if name.is_empty() {
            return Err(DomainError::Validation(
                "template placeholder name must not be blank".to_string(),
            ));
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(DomainError::Validation(format!(
                "template placeholder {name:?} must be alphanumeric or underscore"
            )));
        }
        if !names.iter().any(|n: &String| n == name) {
            names.push(name.to_string());
        }
        rest = &after_open[close + 2..];
    }
    Ok(names)
}

/// A Rust-native contract template.
///
/// Templates are immutable: editing a template creates a new
/// [`SigningTemplate`] with a bumped [`SigningTemplate::version`];
/// contracts always reference the rendered [`ContractDocument`]
/// snapshot, never the live template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningTemplate {
    /// Stable identifier of this template version.
    pub template_id: SigningTemplateId,
    /// Human-readable name (non-blank).
    pub name: String,
    /// Template body with `{{placeholder}}` variables.
    pub body: String,
    /// Monotonic version; a new version is a new template.
    pub version: u32,
    /// When this template version was created.
    pub created_at: DateTime<Utc>,
}

impl SigningTemplate {
    /// Creates a template version, validating the name and the
    /// `{{placeholder}}` syntax up front.
    ///
    /// ```
    /// use crm_domain::signing::SigningTemplate;
    ///
    /// let template = SigningTemplate::new(
    ///     "MSA".to_string(),
    ///     "Dear {{client_name}}, welcome.".to_string(),
    ///     1,
    /// )
    /// .expect("valid template");
    /// assert_eq!(template.placeholders(), vec!["client_name".to_string()]);
    /// ```
    pub fn new(name: String, body: String, version: u32) -> Result<Self, DomainError> {
        if name.trim().is_empty() {
            return Err(DomainError::Validation(
                "template name must not be blank".to_string(),
            ));
        }
        if body.trim().is_empty() {
            return Err(DomainError::Validation(
                "template body must not be blank".to_string(),
            ));
        }
        // Validate the placeholder syntax now so render() only has to
        // check variable coverage.
        extract_placeholders(&body)?;
        Ok(SigningTemplate {
            template_id: SigningTemplateId::new(),
            name: name.trim().to_string(),
            body,
            version,
            created_at: Utc::now(),
        })
    }

    /// Placeholder names in order of first appearance.
    ///
    /// Returns an empty vec for a template with no variables. The
    /// constructor already validated the syntax, so this only fails
    /// if the body was changed after construction (which the type
    /// does not allow — but deserialization could).
    pub fn placeholders(&self) -> Vec<String> {
        extract_placeholders(&self.body).unwrap_or_default()
    }

    /// Renders the template with the given variables into an immutable
    /// [`ContractDocument`].
    ///
    /// Rejects: a missing variable for any placeholder; an extra
    /// variable is ignored (templates evolve; callers may pass a
    /// superset).
    ///
    /// ```
    /// use crm_domain::signing::SigningTemplate;
    /// use std::collections::HashMap;
    ///
    /// let template = SigningTemplate::new(
    ///     "MSA".to_string(),
    ///     "Dear {{client_name}}, welcome.".to_string(),
    ///     1,
    /// )
    /// .expect("valid template");
    /// let mut vars = HashMap::new();
    /// vars.insert("client_name".to_string(), "Acme Corp".to_string());
    /// let doc = template.render(&vars).expect("render");
    /// assert_eq!(doc.rendered_body, "Dear Acme Corp, welcome.");
    /// ```
    pub fn render(
        &self,
        variables: &HashMap<String, String>,
    ) -> Result<ContractDocument, DomainError> {
        let mut rendered = self.body.clone();
        for name in extract_placeholders(&self.body)? {
            let Some(value) = variables.get(&name) else {
                return Err(DomainError::Validation(format!(
                    "missing value for template placeholder {name:?}"
                )));
            };
            rendered = rendered.replace(&format!("{{{{{name}}}}}"), value);
        }
        Ok(ContractDocument {
            template_id: self.template_id,
            template_version: self.version,
            rendered_body: rendered.clone(),
            content_hash: fingerprint(&rendered),
        })
    }
}

/// The immutable rendered snapshot a contract is generated from.
///
/// The content hash pins the exact bytes the signer saw; any later
/// template edit produces a different template version and cannot
/// rewrite this document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractDocument {
    /// Template version this was rendered from.
    pub template_id: SigningTemplateId,
    /// Version number of that template.
    pub template_version: u32,
    /// The rendered body the signer saw.
    pub rendered_body: String,
    /// SHA-256 hex of the rendered body.
    pub content_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> SigningTemplate {
        SigningTemplate::new(
            "MSA".to_string(),
            "Dear {{client_name}}, your {{plan}} plan starts {{start_date}}.".to_string(),
            1,
        )
        .expect("valid template")
    }

    #[test]
    fn placeholders_are_extracted_in_order_deduplicated() {
        let names = extract_placeholders("{{b}} and {{a}} and {{b}}").expect("valid placeholders");
        assert_eq!(names, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn template_rejects_unclosed_placeholder() {
        let err = SigningTemplate::new("Bad".to_string(), "Hello {{name.".to_string(), 1)
            .expect_err("unclosed placeholder must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn template_rejects_bad_placeholder_names() {
        for bad in ["{{client-name}}", "{{}}", "{{ client name }}", "{{name!}}"] {
            let err = SigningTemplate::new("Bad".to_string(), format!("Hi {bad}."), 1)
                .expect_err(&format!("{bad:?} must be rejected"));
            assert!(matches!(err, DomainError::Validation(_)));
        }
    }

    #[test]
    fn template_rejects_blank_name_and_body() {
        assert!(SigningTemplate::new("  ".to_string(), "Body".to_string(), 1).is_err());
        assert!(SigningTemplate::new("Name".to_string(), "  ".to_string(), 1).is_err());
    }

    #[test]
    fn render_substitutes_all_placeholders() {
        let t = template();
        let mut vars = HashMap::new();
        vars.insert("client_name".to_string(), "Acme".to_string());
        vars.insert("plan".to_string(), "Pro".to_string());
        vars.insert("start_date".to_string(), "2026-11-01".to_string());
        let doc = t.render(&vars).expect("render");
        assert_eq!(
            doc.rendered_body,
            "Dear Acme, your Pro plan starts 2026-11-01."
        );
        assert_eq!(doc.template_version, 1);
        assert_eq!(doc.content_hash.len(), 64);
    }

    #[test]
    fn render_rejects_missing_variable() {
        let t = template();
        let vars = HashMap::new();
        let err = t
            .render(&vars)
            .expect_err("missing variable must be rejected");
        assert!(matches!(err, DomainError::Validation(_)));
    }

    #[test]
    fn render_ignores_extra_variables() {
        let t = template();
        let mut vars = HashMap::new();
        vars.insert("client_name".to_string(), "Acme".to_string());
        vars.insert("plan".to_string(), "Pro".to_string());
        vars.insert("start_date".to_string(), "2026-11-01".to_string());
        vars.insert("unused".to_string(), "ignored".to_string());
        t.render(&vars).expect("extra variables are fine");
    }

    #[test]
    fn document_hash_is_stable() {
        let t = template();
        let mut vars = HashMap::new();
        vars.insert("client_name".to_string(), "Acme".to_string());
        vars.insert("plan".to_string(), "Pro".to_string());
        vars.insert("start_date".to_string(), "2026-11-01".to_string());
        let a = t.render(&vars).expect("render");
        let b = t.render(&vars).expect("render");
        assert_eq!(a.content_hash, b.content_hash);
    }

    #[test]
    fn evidence_fingerprints_network_identifiers() {
        let e = SigningEvidence::new(
            ContractId::new(),
            "Ada Lovelace".to_string(),
            "ada@example.com".to_string(),
            "203.0.113.7".to_string(),
            "TestAgent/1.0".to_string(),
            SignatureMethod::ClickToSign,
        )
        .expect("valid evidence");
        assert!(!e.ip_hash.contains("203.0.113.7"));
        assert!(!e.user_agent_hash.contains("TestAgent"));
        assert_eq!(e.ip_hash.len(), 64);
        assert_eq!(e.user_agent_hash.len(), 64);
        // Same input → same fingerprint (deterministic).
        let e2 = SigningEvidence::new(
            ContractId::new(),
            "Ada Lovelace".to_string(),
            "ada@example.com".to_string(),
            "203.0.113.7".to_string(),
            "TestAgent/1.0".to_string(),
            SignatureMethod::ClickToSign,
        )
        .expect("valid evidence");
        assert_eq!(e.ip_hash, e2.ip_hash);
    }

    #[test]
    fn evidence_rejects_blank_fields() {
        let bad = SigningEvidence::new(
            ContractId::new(),
            "  ".to_string(),
            "ada@example.com".to_string(),
            "203.0.113.7".to_string(),
            "UA".to_string(),
            SignatureMethod::TypedSignature,
        );
        assert!(bad.is_err());
        let bad = SigningEvidence::new(
            ContractId::new(),
            "Ada".to_string(),
            "ada@example.com".to_string(),
            "  ".to_string(),
            "UA".to_string(),
            SignatureMethod::TypedSignature,
        );
        assert!(bad.is_err());
    }

    #[test]
    fn signature_method_names_are_stable() {
        assert_eq!(SignatureMethod::TypedSignature.as_str(), "typed");
        assert_eq!(SignatureMethod::DrawnSignature.as_str(), "drawn");
        assert_eq!(SignatureMethod::ClickToSign.as_str(), "click_to_sign");
    }
}
