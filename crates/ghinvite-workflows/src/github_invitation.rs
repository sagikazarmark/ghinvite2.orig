//! `GithubInvitation` Virtual Object: settles one GitHub invitation from
//! reconcile evidence, member webhooks, cancellation, and expiry.

use crate::state::AppState;
use chrono::{DateTime, Utc};
use ghinvite_core::GithubInvitationId;
use restate_sdk::context::ObjectContext;
use restate_sdk::errors::TerminalError;
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct OnWebhookInput {
    pub invitation_id: GithubInvitationId,
    pub action: WebhookAction,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookAction {
    Accepted,
    Declined,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct CancelInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub by_user: Option<u64>, // None when system-cancelled (cascade in v2)
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct TickExpireInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub at: DateTime<Utc>,
}

pub struct GithubInvitation {
    pub state: AppState,
}

#[restate_sdk::object]
impl GithubInvitation {
    #[handler]
    async fn reconcile(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<crate::settlement::ReconcileEvidence>,
    ) -> std::result::Result<(), TerminalError> {
        crate::settlement::reconcile(&self.state, ctx, input).await
    }
    #[handler]
    async fn on_webhook(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<OnWebhookInput>,
    ) -> std::result::Result<(), TerminalError> {
        crate::settlement::webhook(&self.state, ctx, input).await
    }
    #[handler]
    async fn cancel(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<CancelInvitationInput>,
    ) -> std::result::Result<(), TerminalError> {
        crate::settlement::cancel(&self.state, ctx, input).await
    }
    #[handler]
    async fn tick_expire(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<TickExpireInput>,
    ) -> std::result::Result<(), TerminalError> {
        crate::settlement::expire(&self.state, ctx, input).await
    }
}
