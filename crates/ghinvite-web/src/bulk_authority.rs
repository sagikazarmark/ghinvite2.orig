//! Concrete private ingress. Routes verify current authority over the retained
//! batch targets before mutations; uncertain replies retry the same command.
use crate::{
    RestateClient,
    link_authority::{AuthorityError, Result},
};
use ghinvite_core::{
    bulk::Review,
    bulk_commands::{BatchProgress, BatchQuery, CreateBatch, EditBatch, SubmitBatch},
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct BulkAuthority {
    client: Arc<RestateClient>,
}
impl BulkAuthority {
    pub async fn cancel_remaining(
        &self,
        command: ghinvite_core::bulk_commands::CancelBatch,
    ) -> Result<ghinvite_core::bulk_commands::CancelBatchReceipt> {
        self.client
            .invoke(
                "BulkInviteBatch",
                &command.query.batch_id.to_string(),
                "cancel_remaining",
                &command,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub fn new(client: Arc<RestateClient>) -> Self {
        Self { client }
    }
    pub fn review_service(
        &self,
        authority: crate::authority::AuthorityView,
    ) -> crate::bulk_review::BulkReviewService {
        crate::bulk_review::BulkReviewService::new(authority, self.client.clone())
    }
    pub async fn review(&self, query: BatchQuery) -> Result<Review> {
        self.client
            .invoke(
                "BulkInviteBatch",
                &query.batch_id.to_string(),
                "review",
                &query,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub async fn create(&self, command: CreateBatch) -> Result<Review> {
        self.client
            .invoke(
                "BulkInviteBatch",
                &command.review.manifest.batch_id.to_string(),
                "create",
                &command,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub async fn remove_rows(&self, command: EditBatch) -> Result<Review> {
        self.client
            .invoke(
                "BulkInviteBatch",
                &command.query.batch_id.to_string(),
                "remove_rows",
                &command,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub async fn submit(&self, command: SubmitBatch) -> Result<Review> {
        self.client
            .invoke(
                "BulkInviteBatch",
                &command.query.batch_id.to_string(),
                "submit",
                &command,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub async fn progress_page(
        &self,
        query: ghinvite_core::bulk_commands::ProgressPageQuery,
    ) -> Result<BatchProgress> {
        self.client
            .invoke(
                "BulkInviteBatch",
                &query.query.batch_id.to_string(),
                "progress_page",
                &query,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
    pub async fn progress(&self, query: BatchQuery) -> Result<BatchProgress> {
        self.client
            .invoke(
                "BulkInviteBatch",
                &query.batch_id.to_string(),
                "progress",
                &query,
            )
            .await
            .map_err(AuthorityError::from_ingress)
    }
}
