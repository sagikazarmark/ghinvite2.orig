//! Retained repository-delivery reads through private ingress.
use crate::{
    RestateClient,
    link_authority::{AuthorityError, Result},
};
use ghinvite_core::{RequestId, delivery::DeliverySnapshot};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct DeliveryAuthority {
    client: Arc<RestateClient>,
}

impl DeliveryAuthority {
    pub fn new(client: Arc<RestateClient>) -> Self {
        Self { client }
    }

    pub async fn refresh_repository(
        &self,
        request_id: RequestId,
        repo_id: u64,
        generation: u64,
    ) -> Result<ghinvite_core::delivery::RepositoryRefresh> {
        self.client
            .invoke(
                "RepositoryDelivery",
                &ghinvite_core::delivery::delivery_key(request_id, repo_id, generation),
                "refresh",
                &(),
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }

    /// The enclosing route must authorize the request and its repository scope.
    pub async fn snapshot(
        &self,
        request_id: RequestId,
        repo_id: u64,
        generation: u64,
    ) -> Result<Option<DeliverySnapshot>> {
        self.client
            .invoke(
                "RepositoryDelivery",
                &ghinvite_core::delivery::delivery_key(request_id, repo_id, generation),
                "status",
                &(),
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub async fn organization_snapshot(
        &self,
        request_id: RequestId,
        org_id: u64,
        generation: u64,
    ) -> Result<Option<ghinvite_core::org_delivery::OrganizationDeliverySnapshot>> {
        self.client
            .invoke(
                "OrganizationDelivery",
                &ghinvite_core::delivery::delivery_key(request_id, org_id, generation),
                "status",
                &(),
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub async fn refresh_organization(
        &self,
        request_id: RequestId,
        org_id: u64,
        generation: u64,
    ) -> Result<ghinvite_core::org_delivery::OrganizationRefresh> {
        self.client
            .invoke(
                "OrganizationDelivery",
                &ghinvite_core::delivery::delivery_key(request_id, org_id, generation),
                "refresh",
                &(),
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
}
