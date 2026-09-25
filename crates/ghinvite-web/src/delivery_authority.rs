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

    /// The enclosing route must authorize the request and its repository scope.
    pub async fn snapshot(
        &self,
        request_id: RequestId,
        repo_id: u64,
    ) -> Result<Option<DeliverySnapshot>> {
        self.client
            .invoke(
                "RepositoryDelivery",
                &ghinvite_core::delivery::delivery_key(request_id, repo_id),
                "status",
                &(),
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
}
