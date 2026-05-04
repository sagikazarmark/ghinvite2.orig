//! Shared dependency container threaded through every Restate handler.

use github::InstallationClient;
use std::sync::Arc;
use storage::Storage;

/// Cloneable bundle of the I/O dependencies handlers need. Constructed once
/// at startup (Plan 7's `#[event(fetch)]` will build it from `worker::Env`)
/// and cloned into every service `impl` struct.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn Storage>,
    pub github: Arc<InstallationClient>,
}

impl AppState {
    pub fn new(storage: Arc<dyn Storage>, github: Arc<InstallationClient>) -> Self {
        Self { storage, github }
    }
}
