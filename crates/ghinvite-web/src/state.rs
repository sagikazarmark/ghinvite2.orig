//! Shared dependency container threaded through every route handler.

use crate::commands::RestateCommands;
use crate::config::WebConfig;
use crate::link_authority::LinkAuthority;
use ghinvite_core::storage::blocked_digest::DailyDigestStorage;
use ghinvite_core::storage::invite_budget::InviteBudgetStorage;
use ghinvite_core::storage::pending_signin::PendingSignInStorage;
use ghinvite_core::storage::{
    AppInstallationReadStorage, ConsoleStorage, ContinuationStorage, LinkCapabilityStorage,
    OperationalReadStorage, OrgDeliveryReadStorage, OrgWebhookStorage, RecordStorage,
    SigninBudgetStorage,
};
use std::sync::Arc;

/// The storage parts the web app uses: shared records, Console reads, browser
/// attempt continuations, webhook routing and daily projection summaries.
/// Daily summaries never mutate authoritative delivery or audit state.
pub trait WebStorage:
    RecordStorage
    + ConsoleStorage
    + ContinuationStorage
    + AppInstallationReadStorage
    + LinkCapabilityStorage
    + SigninBudgetStorage
    + OperationalReadStorage
    + DailyDigestStorage
    + InviteBudgetStorage
    + OrgDeliveryReadStorage
    + OrgWebhookStorage
    + PendingSignInStorage
{
}

impl<T> WebStorage for T where
    T: RecordStorage
        + ConsoleStorage
        + ContinuationStorage
        + AppInstallationReadStorage
        + LinkCapabilityStorage
        + SigninBudgetStorage
        + OperationalReadStorage
        + DailyDigestStorage
        + InviteBudgetStorage
        + OrgDeliveryReadStorage
        + OrgWebhookStorage
        + PendingSignInStorage
{
}

/// The web app's GitHub access through `ghinvite-github`.
#[derive(Clone)]
pub struct WebGithub {
    /// Credential-free requests (avatars).
    pub transport: Arc<dyn ghinvite_github::HttpTransportDyn>,
    /// The installed apps and their installation-token caches: setup
    /// verification, installation requests and Console authority reads.
    pub apps: ghinvite_github::GithubApps,
}

impl WebGithub {
    /// `transport` and the `installed` apps (e.g. `WebConfig::github_apps`),
    /// calling GitHub's REST API (`api.github.com`). Natively build this once
    /// and keep it: the apps' token caches live in it. On Workers build it
    /// per request: the token caches are isolate-scope (PD-4).
    pub fn new(
        transport: Arc<dyn ghinvite_github::HttpTransportDyn>,
        installed: Vec<ghinvite_github::InstalledApp>,
    ) -> Result<Self, ghinvite_github::ApiError> {
        let env = ghinvite_github::rest::RestEnv::new(
            transport.clone(),
            ghinvite_github::rest::DEFAULT_API_BASE,
            "p1",
        )?;
        Ok(Self {
            transport,
            apps: ghinvite_github::GithubApps::new(env, installed)?,
        })
    }
}

impl std::fmt::Debug for WebGithub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebGithub")
            .field("apps", &self.apps)
            .finish_non_exhaustive()
    }
}

/// Cloneable state. Constructed once at startup and held by the axum app.
#[derive(Clone)]
pub struct AppState {
    pub bulk_authority: crate::bulk_authority::BulkAuthority,
    pub bulk_email_vault: Option<Arc<ghinvite_email_vault::EmailVault>>,
    pub authority_cache: Arc<dyn crate::authority_cache::AuthorityCache>,
    pub edge_limits: Arc<dyn crate::rate_limit::EdgeRateLimiter>,
    pub storage: Arc<dyn WebStorage>,
    /// Credential-free GitHub requests (avatars), through `ghinvite-github`.
    pub github_transport: Arc<dyn ghinvite_github::HttpTransportDyn>,
    /// The installed GitHub Apps with their token caches.
    pub github_apps: ghinvite_github::GithubApps,
    pub commands: RestateCommands,
    pub config: WebConfig,
    pub link_authority: LinkAuthority,
    pub request_authority: crate::request_authority::RequestAuthority,
    pub delivery_authority: crate::delivery_authority::DeliveryAuthority,
}

impl AppState {
    pub fn with_bulk_email_vault(mut self, vault: Arc<ghinvite_email_vault::EmailVault>) -> Self {
        self.bulk_email_vault = Some(vault);
        self
    }
    pub fn with_authority_cache(
        mut self,
        cache: Arc<dyn crate::authority_cache::AuthorityCache>,
    ) -> Self {
        self.authority_cache = cache;
        self
    }

    pub fn with_edge_limits(
        mut self,
        limiter: Arc<dyn crate::rate_limit::EdgeRateLimiter>,
    ) -> Self {
        self.edge_limits = Arc::new(crate::rate_limit::ObservedRateLimiter::new(limiter));
        self
    }

    /// `restate` is the one ingress client behind both the authoritative link
    /// and request commands and the installation and webhook commands; SQL
    /// only answers eventually consistent reads.
    pub fn new(
        storage: Arc<dyn WebStorage>,
        github: WebGithub,
        restate: Arc<crate::RestateClient>,
        config: WebConfig,
    ) -> Self {
        Self {
            bulk_authority: crate::bulk_authority::BulkAuthority::new(restate.clone()),
            bulk_email_vault: None,
            authority_cache: Arc::new(crate::authority_cache::Unconfigured),
            edge_limits: Arc::new(crate::rate_limit::ObservedRateLimiter::new(Arc::new(
                crate::rate_limit::Unconfigured,
            ))),
            storage,
            github_transport: github.transport,
            github_apps: github.apps,
            commands: RestateCommands::new(restate.clone()),
            config,
            link_authority: LinkAuthority::new(restate.clone()),
            request_authority: crate::request_authority::RequestAuthority::new(restate.clone()),
            delivery_authority: crate::delivery_authority::DeliveryAuthority::new(restate),
        }
    }
}
