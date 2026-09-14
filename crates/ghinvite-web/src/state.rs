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
    pub admission: Option<Arc<crate::admission::RestateAdmission>>,
    pub(crate) attempt_store: Option<Arc<dyn tower_sessions::SessionStore>>,
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
            admission: None,
            attempt_store: None,
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

    /// Isolated v1 deployment only, after migration. Neither binary enables it.
    pub fn with_admission(mut self, client: Arc<crate::RestateClient>) -> Self {
        self.request_lifecycle = Some(Arc::new(crate::lifecycle::RestateRequestLifecycle::new(
            client.clone(),
        )));
        self.admission = Some(Arc::new(crate::admission::RestateAdmission::new(client)));
        self
    }
}
