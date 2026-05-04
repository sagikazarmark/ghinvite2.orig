//! `GithubInvitation` Virtual Object: create, on_webhook, cancel, tick_expire.

use crate::impl_restate_json_payload;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use domain::{GithubInvitationId, RequestId};
use restate_sdk::context::ObjectContext;
use restate_sdk::errors::TerminalError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub invitation_request_id: RequestId,
    pub installation_id: u64,
    pub repo_id: u64,
    pub repo_full_name: String, // "owner/name"
    pub recipient_login: String,
    pub permission: domain::Permission,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnWebhookInput {
    pub invitation_id: GithubInvitationId,
    pub action: WebhookAction,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookAction {
    Accepted,
    Declined,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CancelInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub by_user: Option<u64>, // None when system-cancelled (cascade in v2)
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TickExpireInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub at: DateTime<Utc>,
}

impl_restate_json_payload!(CreateInvitationInput);
impl_restate_json_payload!(OnWebhookInput);
impl_restate_json_payload!(CancelInvitationInput);
impl_restate_json_payload!(TickExpireInput);

#[restate_sdk::object]
pub trait GithubInvitation {
    async fn create(input: CreateInvitationInput) -> std::result::Result<(), TerminalError>;
    async fn on_webhook(input: OnWebhookInput) -> std::result::Result<(), TerminalError>;
    async fn cancel(input: CancelInvitationInput) -> std::result::Result<(), TerminalError>;
    async fn tick_expire(input: TickExpireInput) -> std::result::Result<(), TerminalError>;
}

pub struct GithubInvitationImpl {
    pub state: AppState,
}

impl GithubInvitation for GithubInvitationImpl {
    async fn create(
        &self,
        _ctx: ObjectContext<'_>,
        _input: CreateInvitationInput,
    ) -> std::result::Result<(), TerminalError> {
        unimplemented!("Task 12")
    }

    async fn on_webhook(
        &self,
        _ctx: ObjectContext<'_>,
        _input: OnWebhookInput,
    ) -> std::result::Result<(), TerminalError> {
        unimplemented!("Task 13")
    }

    async fn cancel(
        &self,
        _ctx: ObjectContext<'_>,
        _input: CancelInvitationInput,
    ) -> std::result::Result<(), TerminalError> {
        unimplemented!("Task 14")
    }

    async fn tick_expire(
        &self,
        _ctx: ObjectContext<'_>,
        _input: TickExpireInput,
    ) -> std::result::Result<(), TerminalError> {
        unimplemented!("Task 14")
    }
}
