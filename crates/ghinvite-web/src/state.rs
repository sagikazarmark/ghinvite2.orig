//! Shared dependency container threaded through every route handler.

use crate::commands::GhinviteCommands;
use crate::config::WebConfig;
use ghinvite_core::storage::Storage;
use ghinvite_github::HttpTransport;
use std::sync::Arc;

/// Cloneable state. Constructed once at startup and held by the axum app.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn Storage>,
    pub github_transport: Arc<dyn HttpTransport>,
    pub commands: Arc<dyn GhinviteCommands>,
    pub config: WebConfig,
    pub request_lifecycle: Option<Arc<dyn crate::lifecycle::RequestLifecycle>>,
}

impl AppState {
    pub fn new(
        storage: Arc<dyn Storage>,
        github_transport: Arc<dyn HttpTransport>,
        commands: Arc<dyn GhinviteCommands>,
        config: WebConfig,
    ) -> Self {
        Self {
            storage,
            github_transport,
            commands,
            config,
            request_lifecycle: None,
        }
    }

    /// For an isolated deployment whose requests are owned by InvitationLinkV1.
    /// Rollout/migration decides when this is enabled; no per-row fallback.
    pub fn with_request_lifecycle(
        mut self,
        lifecycle: Arc<dyn crate::lifecycle::RequestLifecycle>,
    ) -> Self {
        self.request_lifecycle = Some(lifecycle);
        self
    }
}
