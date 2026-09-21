//! Authoritative request lifecycle commands and decision/delivery reads.
use crate::{RestateClient, Result};
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionReceipt, RequestStatus};
use std::sync::Arc;

#[async_trait::async_trait]
pub trait RequestLifecycle: Send + Sync + 'static {
    async fn decide(&self, command: DecideRequest) -> Result<DecisionReceipt>;
    /// Read the retained receipt without applying an undecided command on GET.
    async fn decision_status(&self, _command: DecideRequest) -> Result<Option<DecisionReceipt>> {
        // Not being able to read the retained receipt leaves the decision's
        // outcome in doubt, which is what callers must show.
        Err(crate::WebError::Restate(
            crate::error::IngressFailure::unreachable("decision status unavailable"),
        ))
    }
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
                "InvitationLink",
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
                "InvitationLink",
                &query.link_id.to_string(),
                "delivery_progress",
                &query,
            )
            .await
    }
    async fn decide(&self, command: DecideRequest) -> Result<DecisionReceipt> {
        self.client
            .authoritative_call(
                "InvitationLink",
                &command.link_id.to_string(),
                "decide",
                &command,
            )
            .await
    }
}
