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
    async fn create(
        &self,
        _: ObjectContext<'_>,
        _: Json<CreateInvitationInput>,
    ) -> Result<(), TerminalError> {
        Err(obsolete())
    }
    async fn on_webhook(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<OnWebhookInput>,
    ) -> Result<(), TerminalError> {
        GithubInvitationImpl {
            state: self.0.clone(),
        }
        .on_webhook(ctx, input)
        .await
    }
    async fn cancel(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<CancelInvitationInput>,
    ) -> Result<(), TerminalError> {
        GithubInvitationImpl {
            state: self.0.clone(),
        }
        .cancel(ctx, input)
        .await
    }
    async fn tick_expire(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<TickExpireInput>,
    ) -> Result<(), TerminalError> {
        GithubInvitationImpl {
            state: self.0.clone(),
        }
        .tick_expire(ctx, input)
        .await
    }
}
