// SPDX-License-Identifier: GPL-3.0-only
//! Branding service implementation (task HARD-02).
//!
//! [`BrandingServiceImpl`] implements
//! [`BrandingService`](crm_domain::branding::BrandingService) over the
//! branding repository. It owns the read/write rules for agency themes:
//! reads return the empty theme (version 0) when branding was never
//! set, and writes record the pinned [`BRANDING_UPDATED`] audit event.
//!
//! Audit failures are swallowed: audit is fail-loud at the HTTP layer
//! and never fails a branding operation (same convention as
//! [`BillingServiceImpl`](crate::billing::BillingServiceImpl)).

use std::future::Future;

use crm_domain::audit::{ActionResult, PermissionDecision};
use crm_domain::branding::{
    BrandingRepository, BrandingService, BrandingTheme, BrandingVersion, BRANDING_UPDATED,
};
use crm_domain::error::DomainError;
use crm_domain::ids::AgencyId;
use crm_domain::services::{ActionContext, AuditService};

/// Repository-backed implementation of
/// [`BrandingService`](crm_domain::branding::BrandingService).
///
/// The service owns two rules:
/// * reads never 404: an agency without branding gets the empty theme
///   at version 0 (the login page needs a theme before authentication);
/// * writes record a [`BRANDING_UPDATED`] audit event with the new
///   version as the after-reference.
#[derive(Debug, Clone)]
pub struct BrandingServiceImpl<R, A> {
    repository: R,
    audit: A,
}

impl<R, A> BrandingServiceImpl<R, A> {
    /// Creates the service over the branding repository and the audit
    /// service.
    pub fn new(repository: R, audit: A) -> Self {
        Self { repository, audit }
    }

    /// Records the audit event for a branding update. Failures are
    /// swallowed (audit is fail-loud at the HTTP layer, never fails
    /// the operation).
    async fn audit_updated(&self, ctx: &ActionContext, agency_id: &AgencyId, version: u64)
    where
        A: AuditService + Send + Sync,
    {
        let _ = self
            .audit
            .record_action(
                ctx,
                BRANDING_UPDATED,
                "branding",
                &agency_id.to_string(),
                PermissionDecision::Allow,
                ActionResult::Success,
                None,
                Some(&version.to_string()),
            )
            .await;
    }
}

// The service trait declares `fn ... -> impl Future<Output = ...> +
// Send + 'a`, so this impl must keep that form (an `async fn` does not
// satisfy the trait). `clippy::manual_async_fn` is allowed here with
// this justification: the shape is forced by the trait, not a style
// choice.
#[allow(clippy::manual_async_fn)]
impl<R, A> BrandingService for BrandingServiceImpl<R, A>
where
    R: BrandingRepository + Send + Sync,
    A: AuditService + Send + Sync,
{
    fn get_branding<'a>(
        &'a self,
        agency_id: &'a AgencyId,
    ) -> impl Future<Output = Result<BrandingVersion, DomainError>> + Send + 'a {
        async move {
            match self.repository.get(agency_id).await? {
                Some(version) => Ok(version),
                // Never set: the empty theme at version 0.
                None => Ok(BrandingVersion {
                    version: 0,
                    theme: BrandingTheme::empty(),
                }),
            }
        }
    }

    fn update_branding<'a>(
        &'a self,
        agency_id: &'a AgencyId,
        theme: BrandingTheme,
        ctx: &'a ActionContext,
    ) -> impl Future<Output = Result<BrandingVersion, DomainError>> + Send + 'a {
        async move {
            let stored = self.repository.upsert(agency_id, &theme).await?;
            self.audit_updated(ctx, agency_id, stored.version).await;
            Ok(stored)
        }
    }
}
