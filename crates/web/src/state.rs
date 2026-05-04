//! Shared dependency container threaded through every route handler.

use crate::config::WebConfig;
use crate::restate_client::RestateClient;
use github::HttpTransport;
use std::sync::Arc;
use storage::Storage;

/// Cloneable state. Constructed once at startup and held by the axum app.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn Storage>,
    pub github_transport: Arc<dyn HttpTransport>,
    pub restate: Arc<RestateClient>,
    pub config: WebConfig,
}

impl AppState {
    pub fn new(
        storage: Arc<dyn Storage>,
        github_transport: Arc<dyn HttpTransport>,
        restate: Arc<RestateClient>,
        config: WebConfig,
    ) -> Self {
        Self {
            storage,
            github_transport,
            restate,
            config,
        }
    }
}
