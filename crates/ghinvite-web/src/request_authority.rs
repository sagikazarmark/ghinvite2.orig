//! Concrete private ingress to the request decision and lifecycle owner.
use crate::{
    RestateClient,
    link_authority::{AuthorityError, Result},
};
use ghinvite_core::request_lifecycle::{
    DecideRequest, DecisionReceipt, RequestSnapshot, RequestStatus,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RequestAuthority {
    client: Arc<RestateClient>,
}

impl RequestAuthority {
    pub fn new(client: Arc<RestateClient>) -> Self {
        Self { client }
    }

    pub async fn admin_status(
        &self,
        input: ghinvite_core::request_lifecycle::AdminRequestStatus,
    ) -> Result<RequestSnapshot> {
        self.client
            .invoke(
                "InvitationRequest",
                &input.request_id.to_string(),
                "admin_status",
                &input,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }

    pub async fn decide(&self, input: DecideRequest) -> Result<DecisionReceipt> {
        self.client
            .invoke(
                "InvitationRequest",
                &input.request_id.to_string(),
                "decide",
                &input,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }

    pub async fn decision_status(&self, input: DecideRequest) -> Result<Option<DecisionReceipt>> {
        self.client
            .invoke(
                "InvitationRequest",
                &input.request_id.to_string(),
                "decision_status",
                &input,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }

    pub async fn status(&self, input: RequestStatus) -> Result<RequestSnapshot> {
        self.client
            .invoke(
                "InvitationRequest",
                &input.request_id.to_string(),
                "request_status",
                &input,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }

    pub async fn delivery_progress(
        &self,
        input: RequestStatus,
    ) -> Result<Vec<ghinvite_core::delivery::RepositoryProgress>> {
        self.client
            .invoke(
                "InvitationRequest",
                &input.request_id.to_string(),
                "delivery_progress",
                &input,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
}
