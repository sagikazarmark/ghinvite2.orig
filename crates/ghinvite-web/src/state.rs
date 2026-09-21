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
    pub request_lifecycle: Arc<dyn crate::lifecycle::RequestLifecycle>,
    pub admission: Arc<crate::admission::RestateAdmission>,
    pub(crate) attempt_store: Option<Arc<dyn tower_sessions::SessionStore>>,
}

impl AppState {
    /// `restate` serves the authoritative link and request commands; SQL only
    /// answers eventually consistent reads.
    pub fn new(
        storage: Arc<dyn Storage>,
        github_transport: Arc<dyn HttpTransport>,
        commands: Arc<dyn GhinviteCommands>,
        restate: Arc<crate::RestateClient>,
        config: WebConfig,
    ) -> Self {
        Self {
            storage,
            github_transport,
            commands,
            config,
            request_lifecycle: Arc::new(crate::lifecycle::RestateRequestLifecycle::new(
                restate.clone(),
            )),
            admission: Arc::new(crate::admission::RestateAdmission::new(restate)),
            attempt_store: None,
        }
    }

    /// Replace the request lifecycle client, e.g. with a test double.
    pub fn with_request_lifecycle(
        mut self,
        lifecycle: Arc<dyn crate::lifecycle::RequestLifecycle>,
    ) -> Self {
        self.request_lifecycle = lifecycle;
        self
    }
}
