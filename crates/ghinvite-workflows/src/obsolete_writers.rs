//! Tombstone contracts for new legacy calls after cutover. Old pinned endpoints
//! MUST remain isolated: registering this deployment does not fence old code.
use crate::{invitation_link::*, invitation_request::*};
use restate_sdk::{
    context::{ObjectContext, SharedWorkflowContext, WorkflowContext},
    errors::TerminalError,
    serde::Json,
};

pub struct Obsolete;
fn obsolete() -> TerminalError {
    TerminalError::new_with_code(
        410,
        "obsolete writer: use canonical InvitationLinkV1 commands",
    )
}

impl InvitationLink for Obsolete {
    async fn create(
        &self,
        _: ObjectContext<'_>,
        _: Json<CreateLinkInput>,
    ) -> Result<Json<CreateLinkOutput>, TerminalError> {
        Err(obsolete())
    }
    async fn revoke(
        &self,
        _: ObjectContext<'_>,
        _: Json<RevokeLinkInput>,
    ) -> Result<(), TerminalError> {
        Err(obsolete())
    }
    async fn update_metadata(
        &self,
        _: ObjectContext<'_>,
        _: Json<UpdateLinkMetadataInput>,
    ) -> Result<(), TerminalError> {
        Err(obsolete())
    }
    async fn tick_expiration(
        &self,
        _: ObjectContext<'_>,
        _: Json<TickExpirationInput>,
    ) -> Result<(), TerminalError> {
        Err(obsolete())
    }
}
impl InvitationRequest for Obsolete {
    async fn submit(
        &self,
        _: WorkflowContext<'_>,
        _: Json<SubmitRequestInput>,
    ) -> Result<Json<ghinvite_core::RequestState>, TerminalError> {
        Err(obsolete())
    }
    async fn decide(
        &self,
        _: SharedWorkflowContext<'_>,
        _: Json<Decision>,
    ) -> Result<(), TerminalError> {
        Err(obsolete())
    }
}

pub struct GithubLifecycle(pub crate::AppState);
use crate::github_invitation::*;
impl GithubInvitation for GithubLifecycle {
    async fn reconcile_v1(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<crate::settlement_v1::ReconcileEvidence>,
    ) -> Result<(), TerminalError> {
        crate::settlement_v1::reconcile(&self.0, ctx, input).await
    }
    async fn on_webhook_v1(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<OnWebhookInput>,
    ) -> Result<(), TerminalError> {
        crate::settlement_v1::webhook(&self.0, ctx, input).await
    }
    async fn cancel_v1(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<CancelInvitationInput>,
    ) -> Result<(), TerminalError> {
        crate::settlement_v1::cancel(&self.0, ctx, input).await
    }
    async fn tick_expire_v1(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<TickExpireInput>,
    ) -> Result<(), TerminalError> {
        crate::settlement_v1::expire(&self.0, ctx, input).await
    }
    async fn create(
        &self,
        _: ObjectContext<'_>,
        _: Json<CreateInvitationInput>,
    ) -> Result<(), TerminalError> {
        Err(obsolete())
    }
    async fn on_webhook(
        &self,
        _: ObjectContext<'_>,
        _: Json<OnWebhookInput>,
    ) -> Result<(), TerminalError> {
        Err(TerminalError::new_with_code(410, "use on_webhook_v1"))
    }
    async fn cancel(
        &self,
        _: ObjectContext<'_>,
        _: Json<CancelInvitationInput>,
    ) -> Result<(), TerminalError> {
        Err(TerminalError::new_with_code(410, "use cancel_v1"))
    }
    async fn tick_expire(
        &self,
        _: ObjectContext<'_>,
        _: Json<TickExpireInput>,
    ) -> Result<(), TerminalError> {
        Err(TerminalError::new_with_code(410, "use tick_expire_v1"))
    }
}

pub struct ReconcileLifecycle(pub crate::AppState);
impl crate::reconcile::Reconcile for ReconcileLifecycle {
    async fn daily_run(
        &self,
        _: restate_sdk::context::Context<'_>,
        _: Json<crate::reconcile::DailyRunInput>,
    ) -> Result<(), TerminalError> {
        Err(TerminalError::new_with_code(410, "use daily_run_v1"))
    }
    async fn daily_run_v1(
        &self,
        ctx: restate_sdk::context::Context<'_>,
        input: Json<crate::reconcile::DailyRunInput>,
    ) -> Result<(), TerminalError> {
        crate::reconcile::ReconcileImpl {
            state: self.0.clone(),
        }
        .daily_run_v1(ctx, input)
        .await
    }
}
