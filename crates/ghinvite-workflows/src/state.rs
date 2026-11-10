//! Shared dependency container threaded through every Restate handler.

use ghinvite_core::storage::{
    AppInstallationStorage, AuditStorage, DeliveryStorage, RecordStorage,
};
use std::sync::Arc;

/// The installed GitHub apps for the workflows: one
/// [`GithubApps`](ghinvite_github::GithubApps) token context with an
/// octotoken cache per app. On Workers its providers are isolate-wide
/// (PD-4), so a context built per request still shares their tokens.
pub struct RepositoryGithub {
    /// The installed apps with their octotoken caches (GM-02).
    pub apps: ghinvite_github::GithubApps,
}

impl RepositoryGithub {
    /// [`GithubApps`](ghinvite_github::GithubApps) over `github_transport`
    /// with `installed` (e.g. `MintApps::installed`). REST calls go to
    /// `api_base`; `installed` apps create tokens at the base URL they were
    /// built with.
    pub fn new(
        github_transport: Arc<dyn ghinvite_github::HttpTransportDyn>,
        api_base: &str,
        installed: Vec<ghinvite_github::InstalledApp>,
    ) -> Result<Self, &'static str> {
        let env = ghinvite_github::rest::RestEnv::new(github_transport, api_base, "p1")
            .map_err(|_| "GitHub API origin invalid")?;
        let apps = ghinvite_github::GithubApps::new(env, installed)
            .map_err(|_| "installed app configuration invalid")?;
        Ok(Self::with_apps(apps))
    }

    /// Around a ready [`GithubApps`](ghinvite_github::GithubApps), e.g.
    /// `GithubApps::for_tests` with `FixedTokens`.
    pub fn with_apps(apps: ghinvite_github::GithubApps) -> Self {
        Self { apps }
    }
}

/// The storage parts the workflows use: shared records, installation writers,
/// delivery and settlement, and Audit Log appends.
pub trait WorkflowStorage:
    RecordStorage + AppInstallationStorage + DeliveryStorage + AuditStorage
{
}

impl<T> WorkflowStorage for T where
    T: RecordStorage + AppInstallationStorage + DeliveryStorage + AuditStorage
{
}

/// Cloneable bundle of the I/O dependencies handlers need. Constructed once
/// at startup and cloned into every service `impl` struct.
#[derive(Clone)]
pub struct AppState {
    pub(crate) reissue_enabled: bool,
    pub(crate) access_removal_enabled: bool,
    pub(crate) bulk: Option<Arc<dyn ghinvite_core::storage::bulk::BulkStorage>>,
    pub(crate) email_deliveries: Option<crate::email_delivery::Dependencies>,
    pub(crate) access_grants:
        Option<Arc<dyn ghinvite_core::storage::access_grants::AccessGrantStorage>>,
    pub storage: Arc<dyn WorkflowStorage>,
    /// Explicit GitHub dependency. Typed delivery/settlement
    /// handlers must fail closed when absent; never fall back to a blind write.
    pub github_apps: Option<Arc<RepositoryGithub>>,
    /// Explicit local M1 composition; requires the real InviteBudget binder.
    pub(crate) cancellation_enabled: bool,
    /// Explicit M1 composition; production activation remains gated on SP-08.
    pub(crate) invite_budget:
        Option<Arc<dyn ghinvite_core::storage::invite_budget::InviteBudgetStorage>>,
    /// Explicit M1 organization composition. Its endpoint also needs InviteBudget.
    pub(crate) organization_delivery:
        Option<Arc<dyn ghinvite_core::storage::org_delivery::OrgDeliveryStorage>>,
    #[cfg(feature = "integration")]
    pub installation_clock: Option<Arc<std::sync::atomic::AtomicI64>>,
    #[cfg(feature = "integration")]
    pub request_faults: Option<Arc<crate::request_owner::Faults>>,
    #[cfg(feature = "integration")]
    pub email_audit_faults: Option<Arc<crate::email_delivery::EmailAuditFaults>>,
    #[cfg(feature = "integration")]
    pub installation_audit_fault: Option<Arc<crate::availability::InstallationAuditFault>>,
    #[cfg(feature = "integration")]
    pub sweep_clock: Option<Arc<std::sync::atomic::AtomicI64>>,
}

impl AppState {
    /// Bulk issuance is opt-in and always shares the real invitation budget.
    pub fn with_bulk_invites<S>(mut self, storage: Arc<S>) -> Self
    where
        S: ghinvite_core::storage::bulk::BulkStorage
            + ghinvite_core::storage::invite_budget::InviteBudgetStorage,
    {
        self.invite_budget = Some(storage.clone());
        self.bulk = Some(storage);
        self
    }
    /// Email delivery is opt-in and always binds its real org/content budget.
    pub fn with_email_deliveries<S>(
        mut self,
        storage: Arc<S>,
        vault: Arc<ghinvite_email_vault::EmailVault>,
    ) -> Self
    where
        S: ghinvite_core::storage::email_delivery::EmailDeliveryStorage
            + ghinvite_core::storage::email_vault::EmailVaultStorage
            + ghinvite_core::storage::invite_budget::InviteBudgetStorage,
    {
        self.invite_budget = Some(storage.clone());
        self.email_deliveries = Some(crate::email_delivery::Dependencies {
            storage: storage.clone(),
            vault,
            records: storage,
        });
        self
    }
    /// Explicit administrator reissue composition. Old-generation cancellation
    /// and access-removal barriers always use their real retained owners.
    pub fn with_reissues<S>(mut self, storage: Arc<S>) -> Self
    where
        S: ghinvite_core::storage::access_grants::AccessGrantStorage
            + ghinvite_core::storage::invite_budget::InviteBudgetStorage,
    {
        self.invite_budget = Some(storage.clone());
        self.access_grants = Some(storage);
        self.cancellation_enabled = true;
        self.reissue_enabled = true;
        self
    }
    /// Explicit M2 composition; always binds the real content-write budget.
    pub fn with_access_grants<S>(mut self, storage: Arc<S>) -> Self
    where
        S: ghinvite_core::storage::access_grants::AccessGrantStorage
            + ghinvite_core::storage::invite_budget::InviteBudgetStorage,
    {
        self.access_removal_enabled = true;
        self.invite_budget = Some(storage.clone());
        self.access_grants = Some(storage);
        self
    }
    /// Enable organization delivery together with its required real budget.
    /// Live activation remains an explicit composition decision.
    pub fn with_organization_delivery<S>(mut self, storage: Arc<S>) -> Self
    where
        S: ghinvite_core::storage::org_delivery::OrgDeliveryStorage
            + ghinvite_core::storage::invite_budget::InviteBudgetStorage,
    {
        self.invite_budget = Some(storage.clone());
        self.organization_delivery = Some(storage);
        self
    }
    pub fn new(storage: Arc<dyn WorkflowStorage>) -> Self {
        Self {
            reissue_enabled: false,
            access_removal_enabled: false,
            bulk: None,
            storage,
            github_apps: None,
            access_grants: None,
            email_deliveries: None,
            cancellation_enabled: false,
            invite_budget: None,
            organization_delivery: None,
            #[cfg(feature = "integration")]
            installation_clock: None,
            #[cfg(feature = "integration")]
            request_faults: None,
            #[cfg(feature = "integration")]
            email_audit_faults: None,
            #[cfg(feature = "integration")]
            installation_audit_fault: None,
            #[cfg(feature = "integration")]
            sweep_clock: None,
        }
    }

    /// Enable only in a composition that also binds InviteBudget. Production
    /// activation and live budget evidence remain separate release gates.
    pub fn with_pre_activation_cancellation(mut self) -> Self {
        self.cancellation_enabled = true;
        self
    }

    /// Enable S3 and provide the storage used by the standard endpoint's real
    /// budget projector. Custom endpoint builders must also bind InviteBudget.
    pub fn with_invite_budget(
        mut self,
        storage: Arc<dyn ghinvite_core::storage::invite_budget::InviteBudgetStorage>,
    ) -> Self {
        self.invite_budget = Some(storage);
        self
    }

    pub fn with_github_apps(mut self, github_apps: Arc<RepositoryGithub>) -> Self {
        self.github_apps = Some(github_apps);
        self
    }
}
