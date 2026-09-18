//! Explicit opt-in to the isolated authoritative request path.
use crate::{RestateClient, Result};
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionReceipt, RequestStatus};
use ghinvite_core::storage::projection::RequestSnapshot;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait RequestLifecycle: Send + Sync + 'static {
    async fn decide(&self, command: DecideRequest) -> Result<DecisionReceipt>;
    /// Read the retained receipt without applying an undecided command on GET.
    async fn decision_status(&self, _command: DecideRequest) -> Result<Option<DecisionReceipt>> {
        Err(crate::WebError::Restate(
            "Decision status unavailable".into(),
        ))
    }
    async fn status(&self, query: RequestStatus) -> Result<RequestSnapshot>;
    async fn delivery_progress(
        &self,
        _query: RequestStatus,
    ) -> Result<Vec<ghinvite_core::delivery::RepositoryProgress>> {
        Ok(Vec::new())
    }
}

pub struct RestateRequestLifecycle {
    client: Arc<RestateClient>,
}
impl RestateRequestLifecycle {
    pub fn new(client: Arc<RestateClient>) -> Self {
        Self { client }
    }
}
#[async_trait::async_trait]
impl RequestLifecycle for RestateRequestLifecycle {
    async fn decision_status(&self, command: DecideRequest) -> Result<Option<DecisionReceipt>> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "decision_status",
                &command,
            )
            .await
    }
    async fn delivery_progress(
        &self,
        query: RequestStatus,
    ) -> Result<Vec<ghinvite_core::delivery::RepositoryProgress>> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &query.link_id.to_string(),
                "delivery_progress",
                &query,
            )
            .await
    }
    async fn decide(&self, command: DecideRequest) -> Result<DecisionReceipt> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &command.link_id.to_string(),
                "decide",
                &command,
            )
            .await
    }
    async fn status(&self, query: RequestStatus) -> Result<RequestSnapshot> {
        self.client
            .authoritative_call(
                "InvitationLinkV1",
                &query.link_id.to_string(),
                "request_status",
                &query,
            )
            .await
    }
}
