//! Shared dependency container threaded through every Restate handler.

use ghinvite_core::storage::{AuditStorage, DeliveryStorage, InstallationStorage, RecordStorage};
use ghinvite_github::InstallationClient;
use std::sync::Arc;

/// The storage parts the workflows use: shared records, installation writers,
/// delivery and settlement, and Audit Log appends.
pub trait WorkflowStorage:
    RecordStorage + InstallationStorage + DeliveryStorage + AuditStorage
{
}

impl<T> WorkflowStorage for T where
    T: RecordStorage + InstallationStorage + DeliveryStorage + AuditStorage
{
}

/// Cloneable bundle of the I/O dependencies handlers need. Constructed once
/// at startup and cloned into every service `impl` struct.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn WorkflowStorage>,
    pub github: Arc<InstallationClient>,
}

impl AppState {
    pub fn new(storage: Arc<dyn WorkflowStorage>, github: Arc<InstallationClient>) -> Self {
        Self { storage, github }
    }
}
