//! Shared dependency container threaded through every route handler.

use crate::commands::RestateCommands;
use crate::config::WebConfig;
use crate::link_authority::LinkAuthority;
use ghinvite_core::storage::{ConsoleStorage, ContinuationStorage, RecordStorage, WebhookStorage};
use ghinvite_github::HttpTransport;
use std::sync::Arc;

/// The storage parts the web app uses: shared records, Console reads, browser
/// attempt continuations and webhook routing. Workflows own every other write.
pub trait WebStorage:
    RecordStorage + ConsoleStorage + ContinuationStorage + WebhookStorage
{
}

impl<T> WebStorage for T where
    T: RecordStorage + ConsoleStorage + ContinuationStorage + WebhookStorage
{
}

/// Cloneable state. Constructed once at startup and held by the axum app.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn WebStorage>,
    pub github_transport: Arc<dyn HttpTransport>,
    pub commands: RestateCommands,
    pub config: WebConfig,
    pub link_authority: LinkAuthority,
    pub request_authority: crate::request_authority::RequestAuthority,
    pub delivery_authority: crate::delivery_authority::DeliveryAuthority,
}

impl AppState {
    /// `restate` is the one ingress client behind both the authoritative link
    /// and request commands and the installation and webhook commands; SQL
    /// only answers eventually consistent reads.
    pub fn new(
        storage: Arc<dyn WebStorage>,
        github_transport: Arc<dyn HttpTransport>,
        restate: Arc<crate::RestateClient>,
        config: WebConfig,
    ) -> Self {
        Self {
            storage,
            github_transport,
            commands: RestateCommands::new(restate.clone()),
            config,
            link_authority: LinkAuthority::new(restate.clone()),
            request_authority: crate::request_authority::RequestAuthority::new(restate.clone()),
            delivery_authority: crate::delivery_authority::DeliveryAuthority::new(restate),
        }
    }
}
