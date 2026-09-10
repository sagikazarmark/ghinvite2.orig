use crate::error::{Result, WebError};
use crate::restate_client::RestateClient;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;

#[async_trait]
pub trait GhinviteCommands: Send + Sync + 'static {
    async fn create_invitation_link(
        &self,
        command: CreateInvitationLink,
    ) -> Result<CreateInvitationLinkOutput>;
    async fn update_invitation_link_metadata(
        &self,
        command: UpdateInvitationLinkMetadata,
    ) -> Result<()>;
    async fn revoke_invitation_link(&self, command: RevokeInvitationLink) -> Result<()>;
    async fn submit_invitation_request(&self, command: SubmitInvitationRequest) -> Result<()>;
    async fn decide_invitation_request(&self, command: DecideInvitationRequest) -> Result<()>;
    async fn onboard_installation(&self, command: OnboardInstallation) -> Result<()>;
    async fn record_repository_selection_change(
        &self,
        command: RecordRepositorySelectionChange,
    ) -> Result<()>;
    async fn record_installation_uninstalled(
        &self,
        command: RecordInstallationUninstalled,
    ) -> Result<()>;
    async fn route_github_invitation_webhook(
        &self,
        command: RouteGithubInvitationWebhook,
    ) -> Result<()>;
}

const INVITATION_LINK_SERVICE: &str = "InvitationLink";
const CREATE_INVITATION_LINK_METHOD: &str = "create";
const UPDATE_INVITATION_LINK_METADATA_METHOD: &str = "update_metadata";
const REVOKE_INVITATION_LINK_METHOD: &str = "revoke";
const INVITATION_REQUEST_SERVICE: &str = "InvitationRequest";
const SUBMIT_INVITATION_REQUEST_METHOD: &str = "submit";
const DECIDE_INVITATION_REQUEST_METHOD: &str = "decide";

#[async_trait]
pub(crate) trait RestateCommandAdapter: Send + Sync + 'static {
    async fn send<I: Serialize + Send + Sync>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<()>;

    async fn call<I: Serialize + Send + Sync, O: DeserializeOwned + Send>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<O>;
}

#[async_trait]
impl RestateCommandAdapter for RestateClient {
    async fn send<I: Serialize + Send + Sync>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<()> {
        RestateClient::send(self, service, key, method, input).await
    }

    async fn call<I: Serialize + Send + Sync, O: DeserializeOwned + Send>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<O> {
        RestateClient::call(self, service, key, method, input).await
    }
}

#[derive(Clone, Debug)]
pub struct RestateCommands<R = RestateClient> {
    restate: Arc<R>,
}

impl<R> RestateCommands<R> {
    pub fn new(restate: Arc<R>) -> Self {
        Self { restate }
    }
}

fn invitation_link_command_key(account_id: u64) -> String {
    account_id.to_string()
}

fn invitation_request_command_key(request_id: ghinvite_core::RequestId) -> String {
    request_id.to_string()
}

#[async_trait]
impl<R> GhinviteCommands for RestateCommands<R>
where
    R: RestateCommandAdapter,
{
    async fn create_invitation_link(
        &self,
        command: CreateInvitationLink,
    ) -> Result<CreateInvitationLinkOutput> {
        let key = invitation_link_command_key(command.account_id);
        self.restate
            .call(
                INVITATION_LINK_SERVICE,
                &key,
                CREATE_INVITATION_LINK_METHOD,
                &command,
            )
            .await
    }

    async fn update_invitation_link_metadata(
        &self,
        command: UpdateInvitationLinkMetadata,
    ) -> Result<()> {
        let key = invitation_link_command_key(command.account_id);
        self.restate
            .call(
                INVITATION_LINK_SERVICE,
                &key,
                UPDATE_INVITATION_LINK_METADATA_METHOD,
                &command,
            )
            .await
    }

    async fn revoke_invitation_link(&self, command: RevokeInvitationLink) -> Result<()> {
        let key = invitation_link_command_key(command.account_id);
        self.restate
            .send(
                INVITATION_LINK_SERVICE,
                &key,
                REVOKE_INVITATION_LINK_METHOD,
                &command,
            )
            .await
    }

    async fn submit_invitation_request(&self, command: SubmitInvitationRequest) -> Result<()> {
        let key = invitation_request_command_key(command.request_id);
        let payload = SubmitInvitationRequestPayload::from(command);
        self.restate
            .send(
                INVITATION_REQUEST_SERVICE,
                &key,
                SUBMIT_INVITATION_REQUEST_METHOD,
                &payload,
            )
            .await
    }

    async fn decide_invitation_request(&self, command: DecideInvitationRequest) -> Result<()> {
        let key = invitation_request_command_key(command.request_id);
        let payload = InvitationRequestDecisionPayload::from(command.decision);
        let _: serde_json::Value = self
            .restate
            .call(
                INVITATION_REQUEST_SERVICE,
                &key,
                DECIDE_INVITATION_REQUEST_METHOD,
                &payload,
            )
            .await?;
        Ok(())
    }

    async fn onboard_installation(&self, command: OnboardInstallation) -> Result<()> {
        self.restate
            .call(
                "Installation",
                &command.installation_id.to_string(),
                "onboard",
                &command,
            )
            .await
    }

    async fn record_repository_selection_change(
        &self,
        command: RecordRepositorySelectionChange,
    ) -> Result<()> {
        match command.source {
            RepositorySelectionChangeSource::SetupReturn => {
                self.restate
                    .call(
                        "Installation",
                        &command.installation_id.to_string(),
                        "repos_changed",
                        &command,
                    )
                    .await
            }
            RepositorySelectionChangeSource::Webhook => {
                self.restate
                    .send(
                        "Installation",
                        &command.installation_id.to_string(),
                        "repos_changed",
                        &command,
                    )
                    .await
            }
        }
    }

    async fn record_installation_uninstalled(
        &self,
        command: RecordInstallationUninstalled,
    ) -> Result<()> {
        self.restate
            .send(
                "Installation",
                &command.installation_id.to_string(),
                "uninstall",
                &command,
            )
            .await
    }

    async fn route_github_invitation_webhook(
        &self,
        command: RouteGithubInvitationWebhook,
    ) -> Result<()> {
        self.restate
            .send(
                "GithubInvitation",
                &command.invitation_id.to_string(),
                "on_webhook",
                &command,
            )
            .await
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CreateInvitationLink {
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub permission: ghinvite_core::Permission,
    pub approval_required: bool,
    pub description: String,
    pub internal_note: Option<String>,
    pub repos: Vec<ghinvite_core::InvitationLinkRepo>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreateInvitationLinkOutput {
    pub link_id: ghinvite_core::InvitationLinkId,
    pub slug: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateInvitationLinkMetadata {
    pub account_id: u64,
    pub link_id: ghinvite_core::InvitationLinkId,
    pub by_user: u64,
    pub description: String,
    pub internal_note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RevokeInvitationLink {
    #[serde(skip)]
    pub account_id: u64,
    pub link_id: ghinvite_core::InvitationLinkId,
    pub by_user: u64,
    pub when: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct SubmitInvitationRequest {
    pub request_id: ghinvite_core::RequestId,
    pub invitation_link_id: ghinvite_core::InvitationLinkId,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl SubmitInvitationRequest {
    pub fn new(
        request_id: ghinvite_core::RequestId,
        invitation_link_id: ghinvite_core::InvitationLinkId,
        requester_id: u64,
        justification: Option<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            request_id,
            invitation_link_id,
            requester_id,
            justification,
            created_at,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct SubmitInvitationRequestPayload {
    request_id: ghinvite_core::RequestId,
    invitation_link_id: ghinvite_core::InvitationLinkId,
    requester_id: u64,
    justification: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<SubmitInvitationRequest> for SubmitInvitationRequestPayload {
    fn from(command: SubmitInvitationRequest) -> Self {
        Self {
            request_id: command.request_id,
            invitation_link_id: command.invitation_link_id,
            requester_id: command.requester_id,
            justification: command.justification,
            created_at: command.created_at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DecideInvitationRequest {
    pub request_id: ghinvite_core::RequestId,
    decision: InvitationRequestDecision,
}

impl DecideInvitationRequest {
    pub fn approve(
        request_id: ghinvite_core::RequestId,
        decided_by: u64,
        decided_at: DateTime<Utc>,
    ) -> Self {
        Self {
            request_id,
            decision: InvitationRequestDecision::Approve {
                decided_by,
                decided_at,
            },
        }
    }

    pub fn decline(
        request_id: ghinvite_core::RequestId,
        decided_by: u64,
        decided_at: DateTime<Utc>,
        reason: Option<String>,
    ) -> Self {
        Self {
            request_id,
            decision: InvitationRequestDecision::Decline {
                decided_by,
                decided_at,
                reason,
            },
        }
    }

    pub fn decision(&self) -> InvitationRequestDecisionView<'_> {
        match &self.decision {
            InvitationRequestDecision::Approve {
                decided_by,
                decided_at,
            } => InvitationRequestDecisionView::Approve {
                decided_by: *decided_by,
                decided_at: decided_at.to_owned(),
            },
            InvitationRequestDecision::Decline {
                decided_by,
                decided_at,
                reason,
            } => InvitationRequestDecisionView::Decline {
                decided_by: *decided_by,
                decided_at: decided_at.to_owned(),
                reason: reason.as_deref(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvitationRequestDecisionView<'a> {
    Approve {
        decided_by: u64,
        decided_at: DateTime<Utc>,
    },
    Decline {
        decided_by: u64,
        decided_at: DateTime<Utc>,
        reason: Option<&'a str>,
    },
}

#[derive(Clone, Debug, Serialize)]
enum InvitationRequestDecision {
    Approve {
        decided_by: u64,
        decided_at: DateTime<Utc>,
    },
    Decline {
        decided_by: u64,
        decided_at: DateTime<Utc>,
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize)]
enum InvitationRequestDecisionPayload {
    Approve {
        decided_by: u64,
        decided_at: DateTime<Utc>,
    },
    Decline {
        decided_by: u64,
        decided_at: DateTime<Utc>,
        reason: Option<String>,
    },
}

impl From<InvitationRequestDecision> for InvitationRequestDecisionPayload {
    fn from(decision: InvitationRequestDecision) -> Self {
        match decision {
            InvitationRequestDecision::Approve {
                decided_by,
                decided_at,
            } => Self::Approve {
                decided_by,
                decided_at,
            },
            InvitationRequestDecision::Decline {
                decided_by,
                decided_at,
                reason,
            } => Self::Decline {
                decided_by,
                decided_at,
                reason,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OnboardInstallation {
    pub installation_id: u64,
    pub actor_user_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: ghinvite_core::AccountType,
    pub selected_repos: ghinvite_core::SelectedRepos,
    pub installed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordRepositorySelectionChange {
    pub installation_id: u64,
    pub selected_repos: ghinvite_core::SelectedRepos,
    #[serde(skip)]
    pub source: RepositorySelectionChangeSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositorySelectionChangeSource {
    SetupReturn,
    Webhook,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordInstallationUninstalled {
    pub installation_id: u64,
    pub uninstalled_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RouteGithubInvitationWebhook {
    pub invitation_id: ghinvite_core::GithubInvitationId,
    pub action: GithubInvitationWebhookAction,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubInvitationWebhookAction {
    Accepted,
    Declined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupReturnAction {
    Install,
    Update,
}

#[derive(Clone, Debug)]
pub struct SetupReturn {
    pub action: SetupReturnAction,
    pub installation_id: u64,
    pub actor_user_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: ghinvite_core::AccountType,
    pub selected_repos: ghinvite_core::SelectedRepos,
    pub returned_at: DateTime<Utc>,
}

pub async fn handle_setup_return(
    commands: &dyn GhinviteCommands,
    setup_return: SetupReturn,
) -> Result<()> {
    match setup_return.action {
        SetupReturnAction::Install => {
            commands
                .onboard_installation(OnboardInstallation {
                    installation_id: setup_return.installation_id,
                    actor_user_id: setup_return.actor_user_id,
                    account_id: setup_return.account_id,
                    account_login: setup_return.account_login,
                    account_type: setup_return.account_type,
                    selected_repos: setup_return.selected_repos,
                    installed_at: setup_return.returned_at,
                })
                .await
        }
        SetupReturnAction::Update => {
            commands
                .record_repository_selection_change(RecordRepositorySelectionChange {
                    installation_id: setup_return.installation_id,
                    selected_repos: setup_return.selected_repos,
                    source: RepositorySelectionChangeSource::SetupReturn,
                })
                .await
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GithubWebhookDispatchOutcome {
    Routed,
    Ignored,
}

pub async fn dispatch_github_webhook(
    storage: &dyn ghinvite_core::storage::Storage,
    commands: &dyn GhinviteCommands,
    event_type: &str,
    payload: &serde_json::Value,
    received_at: DateTime<Utc>,
) -> Result<GithubWebhookDispatchOutcome> {
    match event_type {
        "repository_invitation" => {
            dispatch_repository_invitation_webhook(storage, commands, payload, received_at).await
        }
        "installation" => dispatch_installation_webhook(commands, payload, received_at).await,
        "installation_repositories" => {
            dispatch_installation_repositories_webhook(storage, commands, payload).await
        }
        _ => Ok(GithubWebhookDispatchOutcome::Ignored),
    }
}

async fn dispatch_repository_invitation_webhook(
    storage: &dyn ghinvite_core::storage::Storage,
    commands: &dyn GhinviteCommands,
    payload: &serde_json::Value,
    received_at: DateTime<Utc>,
) -> Result<GithubWebhookDispatchOutcome> {
    let action = payload["action"].as_str().unwrap_or("");
    let github_invitation_id = payload["invitation"]["id"]
        .as_u64()
        .ok_or_else(|| WebError::Internal("repository_invitation: missing invitation.id".into()))?;

    let action = match action {
        "accepted" => GithubInvitationWebhookAction::Accepted,
        "declined" => GithubInvitationWebhookAction::Declined,
        _ => return Ok(GithubWebhookDispatchOutcome::Ignored),
    };

    let Some(invitation) = storage
        .get_github_invitation_by_github_id(github_invitation_id)
        .await?
    else {
        return Ok(GithubWebhookDispatchOutcome::Ignored);
    };

    commands
        .route_github_invitation_webhook(RouteGithubInvitationWebhook {
            invitation_id: invitation.id,
            action,
            at: received_at,
        })
        .await?;

    Ok(GithubWebhookDispatchOutcome::Routed)
}

async fn dispatch_installation_webhook(
    commands: &dyn GhinviteCommands,
    payload: &serde_json::Value,
    received_at: DateTime<Utc>,
) -> Result<GithubWebhookDispatchOutcome> {
    let action = payload["action"].as_str().unwrap_or("");
    let installation_id = payload["installation"]["id"]
        .as_u64()
        .ok_or_else(|| WebError::Internal("installation: missing installation.id".into()))?;

    match action {
        "deleted" => {
            commands
                .record_installation_uninstalled(RecordInstallationUninstalled {
                    installation_id,
                    uninstalled_at: received_at,
                })
                .await?;
            Ok(GithubWebhookDispatchOutcome::Routed)
        }
        _ => Ok(GithubWebhookDispatchOutcome::Ignored),
    }
}

fn repository_ids(value: &serde_json::Value) -> Vec<u64> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|repo| repo.get("id").and_then(|id| id.as_u64()))
        .collect()
}

fn selected_repos_from_repository_event(
    payload: &serde_json::Value,
    current: &ghinvite_core::SelectedRepos,
) -> Result<Option<ghinvite_core::SelectedRepos>> {
    match payload["repository_selection"]
        .as_str()
        .unwrap_or("selected")
    {
        "all" => Ok(Some(ghinvite_core::SelectedRepos::All)),
        "selected" => {
            let mut ids: BTreeSet<u64> = match current {
                ghinvite_core::SelectedRepos::All => return Ok(None),
                ghinvite_core::SelectedRepos::Subset(existing) => {
                    existing.iter().copied().collect()
                }
            };
            for id in repository_ids(&payload["repositories_removed"]) {
                ids.remove(&id);
            }
            for id in repository_ids(&payload["repositories_added"]) {
                ids.insert(id);
            }
            Ok(Some(ghinvite_core::SelectedRepos::Subset(
                ids.into_iter().collect(),
            )))
        }
        other => Err(WebError::Internal(format!(
            "unsupported repository_selection: {other}"
        ))),
    }
}

async fn dispatch_installation_repositories_webhook(
    storage: &dyn ghinvite_core::storage::Storage,
    commands: &dyn GhinviteCommands,
    payload: &serde_json::Value,
) -> Result<GithubWebhookDispatchOutcome> {
    let installation_id = payload["installation"]["id"].as_u64().ok_or_else(|| {
        WebError::Internal("installation_repositories: missing installation.id".into())
    })?;

    let current = storage
        .get_installation(installation_id)
        .await?
        .ok_or_else(|| {
            WebError::Internal(format!(
                "installation_repositories: unknown installation {installation_id}"
            ))
        })?;
    let Some(selected_repos) =
        selected_repos_from_repository_event(payload, &current.selected_repos)?
    else {
        return Ok(GithubWebhookDispatchOutcome::Ignored);
    };

    commands
        .record_repository_selection_change(RecordRepositorySelectionChange {
            installation_id,
            selected_repos,
            source: RepositorySelectionChangeSource::Webhook,
        })
        .await?;

    Ok(GithubWebhookDispatchOutcome::Routed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RestateClient;
    use axum::extract::Path;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use chrono::Utc;
    use ghinvite_core::storage::Storage;
    use serde::de::DeserializeOwned;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct RecordedCall {
        service: String,
        key: String,
        method: String,
        send: bool,
        body: Value,
    }

    #[derive(Clone, Debug)]
    struct RecordingRestateClient {
        calls: Arc<Mutex<Vec<RecordedCall>>>,
        response: Value,
    }

    impl RecordingRestateClient {
        fn new(response: Value) -> (Self, Arc<Mutex<Vec<RecordedCall>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: calls.clone(),
                    response,
                },
                calls,
            )
        }

        async fn send<I: Serialize + Send + Sync>(
            &self,
            service: &str,
            key: &str,
            method: &str,
            input: &I,
        ) -> Result<()> {
            self.calls.lock().unwrap().push(RecordedCall {
                service: service.into(),
                key: key.into(),
                method: method.into(),
                send: true,
                body: serde_json::to_value(input).unwrap(),
            });
            Ok(())
        }

        async fn call<I: Serialize + Send + Sync, O: DeserializeOwned + Send>(
            &self,
            service: &str,
            key: &str,
            method: &str,
            input: &I,
        ) -> Result<O> {
            self.calls.lock().unwrap().push(RecordedCall {
                service: service.into(),
                key: key.into(),
                method: method.into(),
                send: false,
                body: serde_json::to_value(input).unwrap(),
            });
            serde_json::from_value(self.response.clone())
                .map_err(|e| WebError::Restate(format!("decoding fake response: {e}")))
        }
    }

    #[async_trait]
    impl RestateCommandAdapter for RecordingRestateClient {
        async fn send<I: Serialize + Send + Sync>(
            &self,
            service: &str,
            key: &str,
            method: &str,
            input: &I,
        ) -> Result<()> {
            RecordingRestateClient::send(self, service, key, method, input).await
        }

        async fn call<I: Serialize + Send + Sync, O: DeserializeOwned + Send>(
            &self,
            service: &str,
            key: &str,
            method: &str,
            input: &I,
        ) -> Result<O> {
            RecordingRestateClient::call(self, service, key, method, input).await
        }
    }

    async fn spawn_restate_recorder(response: Value) -> (String, Arc<Mutex<Vec<RecordedCall>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));

        let call_recorder = calls.clone();
        let call_response = response.clone();
        let send_recorder = calls.clone();

        let app = axum::Router::new()
            .route(
                "/{service}/{key}/{method}",
                post(
                    move |Path((service, key, method)): Path<(String, String, String)>,
                          axum::Json(body): axum::Json<Value>| {
                        let calls = call_recorder.clone();
                        let response = call_response.clone();
                        async move {
                            calls.lock().unwrap().push(RecordedCall {
                                service,
                                key,
                                method,
                                send: false,
                                body,
                            });
                            axum::Json(response).into_response()
                        }
                    },
                ),
            )
            .route(
                "/{service}/{key}/{method}/send",
                post(
                    move |Path((service, key, method)): Path<(String, String, String)>,
                          axum::Json(body): axum::Json<Value>| {
                        let calls = send_recorder.clone();
                        async move {
                            calls.lock().unwrap().push(RecordedCall {
                                service,
                                key,
                                method,
                                send: true,
                                body,
                            });
                            axum::Json(Value::Null).into_response()
                        }
                    },
                ),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    async fn spawn_restate_empty_call_recorder() -> (String, Arc<Mutex<Vec<RecordedCall>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let call_recorder = calls.clone();

        let app = axum::Router::new().route(
            "/{service}/{key}/{method}",
            post(
                move |Path((service, key, method)): Path<(String, String, String)>,
                      axum::Json(body): axum::Json<Value>| {
                    let calls = call_recorder.clone();
                    async move {
                        calls.lock().unwrap().push(RecordedCall {
                            service,
                            key,
                            method,
                            send: false,
                            body,
                        });
                        axum::http::StatusCode::OK.into_response()
                    }
                },
            ),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    fn at(iso: &str) -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[derive(Default)]
    struct RecordingCommands {
        calls: Arc<Mutex<Vec<RecordedCommand>>>,
    }

    #[derive(Debug, PartialEq)]
    enum RecordedCommand {
        OnboardInstallation {
            installation_id: u64,
            actor_user_id: u64,
            account_id: u64,
            account_login: String,
            account_type: ghinvite_core::AccountType,
            selected_repos: ghinvite_core::SelectedRepos,
            installed_at: chrono::DateTime<Utc>,
        },
        RecordRepositorySelectionChange {
            installation_id: u64,
            selected_repos: ghinvite_core::SelectedRepos,
            source: RepositorySelectionChangeSource,
        },
        RecordInstallationUninstalled {
            installation_id: u64,
            uninstalled_at: chrono::DateTime<Utc>,
        },
        RouteGithubInvitationWebhook {
            invitation_id: ghinvite_core::GithubInvitationId,
            action: GithubInvitationWebhookAction,
            at: chrono::DateTime<Utc>,
        },
    }

    #[async_trait]
    impl GhinviteCommands for RecordingCommands {
        async fn create_invitation_link(
            &self,
            _command: CreateInvitationLink,
        ) -> Result<CreateInvitationLinkOutput> {
            panic!("unexpected create_invitation_link command")
        }

        async fn update_invitation_link_metadata(
            &self,
            _command: UpdateInvitationLinkMetadata,
        ) -> Result<()> {
            panic!("unexpected update_invitation_link_metadata command")
        }

        async fn revoke_invitation_link(&self, _command: RevokeInvitationLink) -> Result<()> {
            panic!("unexpected revoke_invitation_link command")
        }

        async fn submit_invitation_request(&self, _command: SubmitInvitationRequest) -> Result<()> {
            panic!("unexpected submit_invitation_request command")
        }

        async fn decide_invitation_request(&self, _command: DecideInvitationRequest) -> Result<()> {
            panic!("unexpected decide_invitation_request command")
        }

        async fn onboard_installation(&self, command: OnboardInstallation) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(RecordedCommand::OnboardInstallation {
                    installation_id: command.installation_id,
                    actor_user_id: command.actor_user_id,
                    account_id: command.account_id,
                    account_login: command.account_login,
                    account_type: command.account_type,
                    selected_repos: command.selected_repos,
                    installed_at: command.installed_at,
                });
            Ok(())
        }

        async fn record_repository_selection_change(
            &self,
            command: RecordRepositorySelectionChange,
        ) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(RecordedCommand::RecordRepositorySelectionChange {
                    installation_id: command.installation_id,
                    selected_repos: command.selected_repos,
                    source: command.source,
                });
            Ok(())
        }

        async fn record_installation_uninstalled(
            &self,
            command: RecordInstallationUninstalled,
        ) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(RecordedCommand::RecordInstallationUninstalled {
                    installation_id: command.installation_id,
                    uninstalled_at: command.uninstalled_at,
                });
            Ok(())
        }

        async fn route_github_invitation_webhook(
            &self,
            command: RouteGithubInvitationWebhook,
        ) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(RecordedCommand::RouteGithubInvitationWebhook {
                    invitation_id: command.invitation_id,
                    action: command.action,
                    at: command.at,
                });
            Ok(())
        }
    }

    async fn seed_github_invitation(
        storage: &ghinvite_storage_sqlx::SqlxStorage,
        github_invitation_id: u64,
    ) -> ghinvite_core::GithubInvitationId {
        let account = ghinvite_core::Account {
            installation_id: 1,
            account_id: 9001,
            account_login: "acme".into(),
            account_type: ghinvite_core::AccountType::Organization,
            installed_at: at("2026-05-20T13:30:00Z"),
            uninstalled_at: None,
            selected_repos: ghinvite_core::SelectedRepos::All,
        };
        storage.insert_installation(&account).await.unwrap();
        storage
            .upsert_user(&ghinvite_core::User {
                user_id: 42,
                login: "admin".into(),
                avatar_url: None,
                last_seen_at: at("2026-05-20T13:30:00Z"),
            })
            .await
            .unwrap();
        storage
            .upsert_user(&ghinvite_core::User {
                user_id: 99,
                login: "recipient".into(),
                avatar_url: None,
                last_seen_at: at("2026-05-20T13:30:00Z"),
            })
            .await
            .unwrap();

        let link_id = ghinvite_core::InvitationLinkId::new();
        storage
            .insert_invitation_link(&ghinvite_core::InvitationLink {
                id: link_id,
                slug: ghinvite_core::Slug::from_string("0123456789ABCDEF".into()).unwrap(),
                installation_id: 1,
                account_id: 9001,
                created_by: 42,
                created_at: at("2026-05-20T13:30:00Z"),
                expires_at: None,
                max_uses: None,
                uses_count: 0,
                permission: ghinvite_core::Permission::Pull,
                approval_required: false,
                description: "AI coding workshop".into(),
                internal_note: None,
                revoked_at: None,
                revoked_by: None,
                repos: vec![ghinvite_core::InvitationLinkRepo {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                }],
            })
            .await
            .unwrap();

        let request_id = ghinvite_core::RequestId::new();
        storage
            .insert_invitation_request_and_increment_uses(&ghinvite_core::InvitationRequest {
                id: request_id,
                invitation_link_id: link_id,
                requester_id: 99,
                justification: None,
                state: ghinvite_core::RequestState::Pending,
                decided_by: None,
                decided_at: None,
                decline_reason: None,
                created_at: at("2026-05-20T13:35:00Z"),
            })
            .await
            .unwrap();

        let invitation_id = ghinvite_core::GithubInvitationId::new();
        storage
            .insert_github_invitation(&ghinvite_core::GithubInvitation {
                id: invitation_id,
                invitation_request_id: request_id,
                repo_id: 10,
                github_invitation_id: None,
                state: ghinvite_core::InvitationState::Sending,
                error_message: None,
                created_at: at("2026-05-20T13:40:00Z"),
                updated_at: at("2026-05-20T13:40:00Z"),
            })
            .await
            .unwrap();
        storage
            .update_github_invitation(&ghinvite_core::storage::GithubInvitationUpdate {
                id: invitation_id,
                state: ghinvite_core::InvitationState::Sent,
                github_invitation_id: Some(github_invitation_id),
                error_message: None,
                updated_at: at("2026-05-20T13:41:00Z"),
            })
            .await
            .unwrap();

        invitation_id
    }

    async fn seed_installation_with_repos(
        storage: &ghinvite_storage_sqlx::SqlxStorage,
        selected_repos: ghinvite_core::SelectedRepos,
    ) {
        storage
            .insert_installation(&ghinvite_core::Account {
                installation_id: 77,
                account_id: 9001,
                account_login: "acme".into(),
                account_type: ghinvite_core::AccountType::Organization,
                installed_at: at("2026-05-20T13:30:00Z"),
                uninstalled_at: None,
                selected_repos,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn setup_return_install_delegates_to_onboard_command() {
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        handle_setup_return(
            &commands,
            SetupReturn {
                action: SetupReturnAction::Install,
                installation_id: 77,
                actor_user_id: 42,
                account_id: 9001,
                account_login: "acme".into(),
                account_type: ghinvite_core::AccountType::Organization,
                selected_repos: ghinvite_core::SelectedRepos::All,
                returned_at: at("2026-05-20T13:00:00Z"),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [RecordedCommand::OnboardInstallation {
                installation_id: 77,
                actor_user_id: 42,
                account_id: 9001,
                account_login: "acme".into(),
                account_type: ghinvite_core::AccountType::Organization,
                selected_repos: ghinvite_core::SelectedRepos::All,
                installed_at: at("2026-05-20T13:00:00Z"),
            }]
        );
    }

    #[tokio::test]
    async fn setup_return_update_delegates_to_repository_selection_command() {
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        handle_setup_return(
            &commands,
            SetupReturn {
                action: SetupReturnAction::Update,
                installation_id: 77,
                actor_user_id: 42,
                account_id: 9001,
                account_login: "acme".into(),
                account_type: ghinvite_core::AccountType::Organization,
                selected_repos: ghinvite_core::SelectedRepos::Subset(vec![10, 11]),
                returned_at: at("2026-05-20T13:05:00Z"),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [RecordedCommand::RecordRepositorySelectionChange {
                installation_id: 77,
                selected_repos: ghinvite_core::SelectedRepos::Subset(vec![10, 11]),
                source: RepositorySelectionChangeSource::SetupReturn,
            }]
        );
    }

    #[tokio::test]
    async fn github_installation_deleted_event_delegates_to_uninstall_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        let outcome = dispatch_github_webhook(
            &storage,
            &commands,
            "installation",
            &serde_json::json!({
                "action": "deleted",
                "installation": {"id": 77}
            }),
            at("2026-05-20T14:00:00Z"),
        )
        .await
        .unwrap();

        assert_eq!(outcome, GithubWebhookDispatchOutcome::Routed);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [RecordedCommand::RecordInstallationUninstalled {
                installation_id: 77,
                uninstalled_at: at("2026-05-20T14:00:00Z"),
            }]
        );
    }

    #[tokio::test]
    async fn github_repository_invitation_event_delegates_to_invitation_webhook_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let invitation_id = seed_github_invitation(&storage, 99001).await;
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        let outcome = dispatch_github_webhook(
            &storage,
            &commands,
            "repository_invitation",
            &serde_json::json!({
                "action": "accepted",
                "invitation": {"id": 99001}
            }),
            at("2026-05-20T14:05:00Z"),
        )
        .await
        .unwrap();

        assert_eq!(outcome, GithubWebhookDispatchOutcome::Routed);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [RecordedCommand::RouteGithubInvitationWebhook {
                invitation_id,
                action: GithubInvitationWebhookAction::Accepted,
                at: at("2026-05-20T14:05:00Z"),
            }]
        );
    }

    #[tokio::test]
    async fn github_repository_invitation_declined_event_delegates_to_invitation_webhook_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let invitation_id = seed_github_invitation(&storage, 99002).await;
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        let outcome = dispatch_github_webhook(
            &storage,
            &commands,
            "repository_invitation",
            &serde_json::json!({
                "action": "declined",
                "invitation": {"id": 99002}
            }),
            at("2026-05-20T14:06:00Z"),
        )
        .await
        .unwrap();

        assert_eq!(outcome, GithubWebhookDispatchOutcome::Routed);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [RecordedCommand::RouteGithubInvitationWebhook {
                invitation_id,
                action: GithubInvitationWebhookAction::Declined,
                at: at("2026-05-20T14:06:00Z"),
            }]
        );
    }

    #[tokio::test]
    async fn github_installation_repositories_event_delegates_to_repository_selection_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(&storage, ghinvite_core::SelectedRepos::Subset(vec![10, 11]))
            .await;
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        let outcome = dispatch_github_webhook(
            &storage,
            &commands,
            "installation_repositories",
            &serde_json::json!({
                "repository_selection": "selected",
                "installation": {"id": 77},
                "repositories_removed": [{"id": 10}],
                "repositories_added": [{"id": 12}]
            }),
            at("2026-05-20T14:10:00Z"),
        )
        .await
        .unwrap();

        assert_eq!(outcome, GithubWebhookDispatchOutcome::Routed);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [RecordedCommand::RecordRepositorySelectionChange {
                installation_id: 77,
                selected_repos: ghinvite_core::SelectedRepos::Subset(vec![11, 12]),
                source: RepositorySelectionChangeSource::Webhook,
            }]
        );
    }

    #[tokio::test]
    async fn github_installation_repositories_all_event_delegates_to_all_repository_selection_command()
     {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(&storage, ghinvite_core::SelectedRepos::Subset(vec![10, 11]))
            .await;
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        let outcome = dispatch_github_webhook(
            &storage,
            &commands,
            "installation_repositories",
            &serde_json::json!({
                "repository_selection": "all",
                "installation": {"id": 77},
                "repositories_removed": [],
                "repositories_added": []
            }),
            at("2026-05-20T14:15:00Z"),
        )
        .await
        .unwrap();

        assert_eq!(outcome, GithubWebhookDispatchOutcome::Routed);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [RecordedCommand::RecordRepositorySelectionChange {
                installation_id: 77,
                selected_repos: ghinvite_core::SelectedRepos::All,
                source: RepositorySelectionChangeSource::Webhook,
            }]
        );
    }

    #[tokio::test]
    async fn github_installation_repositories_all_to_selected_delta_is_ignored() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(&storage, ghinvite_core::SelectedRepos::All).await;
        let commands = RecordingCommands::default();
        let calls = commands.calls.clone();

        let outcome = dispatch_github_webhook(
            &storage,
            &commands,
            "installation_repositories",
            &serde_json::json!({
                "repository_selection": "selected",
                "installation": {"id": 77},
                "repositories_removed": [{"id": 10}],
                "repositories_added": []
            }),
            at("2026-05-20T14:20:00Z"),
        )
        .await
        .unwrap();

        assert_eq!(outcome, GithubWebhookDispatchOutcome::Ignored);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_invitation_link_calls_restate_create_and_returns_output() {
        let link_id = ghinvite_core::InvitationLinkId::new();
        let (restate, calls) = RecordingRestateClient::new(serde_json::json!({
            "link_id": link_id,
            "slug": "0123456789abcdef"
        }));
        let commands = RestateCommands::new(Arc::new(restate));

        let output = commands
            .create_invitation_link(CreateInvitationLink {
                installation_id: 77,
                account_id: 9001,
                created_by: 42,
                created_at: at("2026-05-20T10:00:00Z"),
                expires_at: None,
                max_uses: Some(3),
                permission: ghinvite_core::Permission::Push,
                approval_required: true,
                description: "AI coding workshop".into(),
                internal_note: Some("team onboarding".into()),
                repos: vec![ghinvite_core::InvitationLinkRepo {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                }],
            })
            .await
            .unwrap();

        assert_eq!(output.link_id, link_id);
        assert_eq!(output.slug, "0123456789abcdef");

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationLink");
        assert_eq!(call.key, "9001");
        assert_eq!(call.method, "create");
        assert!(!call.send);
        assert_eq!(call.body["installation_id"], 77);
        assert_eq!(call.body["account_id"], 9001);
        assert_eq!(call.body["permission"], "push");
        assert_eq!(call.body["description"], "AI coding workshop");
        assert_eq!(call.body["repos"][0]["repo_full_name"], "acme/api");
    }

    #[tokio::test]
    async fn submit_invitation_request_uses_invitation_request_command_adapter() {
        let (restate, calls) = RecordingRestateClient::new(Value::Null);
        let commands = RestateCommands::new(Arc::new(restate));
        let request_id = ghinvite_core::RequestId::new();
        let invitation_link_id = ghinvite_core::InvitationLinkId::new();

        commands
            .submit_invitation_request(SubmitInvitationRequest::new(
                request_id,
                invitation_link_id,
                42,
                Some("need access".into()),
                at("2026-05-20T11:00:00Z"),
            ))
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "submit");
        assert!(call.send);
        assert_eq!(
            call.body,
            serde_json::json!({
                "request_id": request_id.to_string(),
                "invitation_link_id": invitation_link_id.to_string(),
                "requester_id": 42,
                "justification": "need access",
                "created_at": "2026-05-20T11:00:00Z"
            })
        );
    }

    #[tokio::test]
    async fn update_invitation_link_metadata_calls_restate_with_all_fields_and_unit_output() {
        let (base, calls) = spawn_restate_empty_call_recorder().await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));
        let link_id = ghinvite_core::InvitationLinkId::new();

        for internal_note in [Some("team onboarding\nupdated context"), None] {
            let () = commands
                .update_invitation_link_metadata(UpdateInvitationLinkMetadata {
                    account_id: 9001,
                    link_id,
                    by_user: 42,
                    description: "Updated workshop".into(),
                    internal_note: internal_note.map(str::to_owned),
                })
                .await
                .unwrap();
        }

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        for (call, internal_note) in calls
            .iter()
            .zip([Some("team onboarding\nupdated context"), None])
        {
            assert_eq!(call.service, "InvitationLink");
            assert_eq!(call.key, "9001");
            assert_eq!(call.method, "update_metadata");
            assert!(!call.send);
            assert_eq!(
                call.body,
                serde_json::json!({
                    "account_id": 9001,
                    "link_id": link_id.to_string(),
                    "by_user": 42,
                    "description": "Updated workshop",
                    "internal_note": internal_note,
                })
            );
        }
    }

    #[tokio::test]
    async fn revoke_invitation_link_sends_restate_revoke_without_account_id_payload() {
        let (restate, calls) = RecordingRestateClient::new(Value::Null);
        let commands = RestateCommands::new(Arc::new(restate));
        let link_id = ghinvite_core::InvitationLinkId::new();

        commands
            .revoke_invitation_link(RevokeInvitationLink {
                account_id: 9001,
                link_id,
                by_user: 42,
                when: at("2026-05-20T11:30:00Z"),
            })
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationLink");
        assert_eq!(call.key, "9001");
        assert_eq!(call.method, "revoke");
        assert!(call.send);
        assert_eq!(call.body["link_id"], link_id.to_string());
        assert_eq!(call.body["by_user"], 42);
        assert!(call.body.get("account_id").is_none());
    }

    #[tokio::test]
    async fn approve_invitation_request_uses_decision_payload() {
        let (restate, calls) = RecordingRestateClient::new(Value::Null);
        let commands = RestateCommands::new(Arc::new(restate));
        let request_id = ghinvite_core::RequestId::new();

        commands
            .decide_invitation_request(DecideInvitationRequest::approve(
                request_id,
                7,
                at("2026-05-20T11:45:00Z"),
            ))
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "decide");
        assert!(!call.send);
        assert_eq!(
            call.body,
            serde_json::json!({
                "Approve": {
                    "decided_by": 7,
                    "decided_at": "2026-05-20T11:45:00Z"
                }
            })
        );
    }

    #[tokio::test]
    async fn decline_invitation_request_uses_decision_payload() {
        let (restate, calls) = RecordingRestateClient::new(Value::Null);
        let commands = RestateCommands::new(Arc::new(restate));
        let request_id = ghinvite_core::RequestId::new();

        commands
            .decide_invitation_request(DecideInvitationRequest::decline(
                request_id,
                8,
                at("2026-05-20T12:15:00Z"),
                Some("not enough context".into()),
            ))
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "decide");
        assert!(!call.send);
        assert_eq!(
            call.body,
            serde_json::json!({
                "Decline": {
                    "decided_by": 8,
                    "decided_at": "2026-05-20T12:15:00Z",
                    "reason": "not enough context"
                }
            })
        );
    }

    #[tokio::test]
    async fn onboard_installation_calls_restate_onboard() {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));

        commands
            .onboard_installation(OnboardInstallation {
                installation_id: 77,
                actor_user_id: 42,
                account_id: 9001,
                account_login: "acme".into(),
                account_type: ghinvite_core::AccountType::Organization,
                selected_repos: ghinvite_core::SelectedRepos::All,
                installed_at: at("2026-05-20T11:50:00Z"),
            })
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "Installation");
        assert_eq!(call.key, "77");
        assert_eq!(call.method, "onboard");
        assert!(!call.send);
        assert_eq!(call.body["installation_id"], 77);
        assert_eq!(call.body["actor_user_id"], 42);
        assert_eq!(call.body["account_type"], "Organization");
        assert_eq!(call.body["selected_repos"], "all");
    }

    #[tokio::test]
    async fn onboard_installation_accepts_empty_restate_response_body() {
        let (base, calls) = spawn_restate_empty_call_recorder().await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));

        let result = commands
            .onboard_installation(OnboardInstallation {
                installation_id: 77,
                actor_user_id: 42,
                account_id: 9001,
                account_login: "acme".into(),
                account_type: ghinvite_core::AccountType::Organization,
                selected_repos: ghinvite_core::SelectedRepos::All,
                installed_at: at("2026-05-20T11:50:00Z"),
            })
            .await;

        assert!(
            result.is_ok(),
            "empty success response should not fail decode: {result:?}"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "Installation");
        assert_eq!(calls[0].key, "77");
        assert_eq!(calls[0].method, "onboard");
        assert!(!calls[0].send);
    }

    #[tokio::test]
    async fn record_installation_uninstalled_sends_restate_uninstall() {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));

        commands
            .record_installation_uninstalled(RecordInstallationUninstalled {
                installation_id: 77,
                uninstalled_at: at("2026-05-20T11:55:00Z"),
            })
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "Installation");
        assert_eq!(call.key, "77");
        assert_eq!(call.method, "uninstall");
        assert!(call.send);
        assert_eq!(call.body["installation_id"], 77);
        assert_eq!(call.body["uninstalled_at"], "2026-05-20T11:55:00Z");
    }

    #[tokio::test]
    async fn repository_selection_change_preserves_setup_call_and_webhook_send_modes() {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));

        commands
            .record_repository_selection_change(RecordRepositorySelectionChange {
                installation_id: 77,
                selected_repos: ghinvite_core::SelectedRepos::Subset(vec![10]),
                source: RepositorySelectionChangeSource::SetupReturn,
            })
            .await
            .unwrap();
        commands
            .record_repository_selection_change(RecordRepositorySelectionChange {
                installation_id: 77,
                selected_repos: ghinvite_core::SelectedRepos::Subset(vec![11]),
                source: RepositorySelectionChangeSource::Webhook,
            })
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].service, "Installation");
        assert_eq!(calls[0].key, "77");
        assert_eq!(calls[0].method, "repos_changed");
        assert!(!calls[0].send);
        assert_eq!(calls[0].body["selected_repos"], serde_json::json!([10]));

        assert_eq!(calls[1].service, "Installation");
        assert_eq!(calls[1].key, "77");
        assert_eq!(calls[1].method, "repos_changed");
        assert!(calls[1].send);
        assert_eq!(calls[1].body["selected_repos"], serde_json::json!([11]));
    }

    #[tokio::test]
    async fn github_invitation_webhook_actions_serialize_as_snake_case() {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));
        let accepted_id = ghinvite_core::GithubInvitationId::new();
        let declined_id = ghinvite_core::GithubInvitationId::new();

        commands
            .route_github_invitation_webhook(RouteGithubInvitationWebhook {
                invitation_id: accepted_id,
                action: GithubInvitationWebhookAction::Accepted,
                at: at("2026-05-20T12:00:00Z"),
            })
            .await
            .unwrap();
        commands
            .route_github_invitation_webhook(RouteGithubInvitationWebhook {
                invitation_id: declined_id,
                action: GithubInvitationWebhookAction::Declined,
                at: at("2026-05-20T12:01:00Z"),
            })
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].service, "GithubInvitation");
        assert_eq!(calls[0].key, accepted_id.to_string());
        assert_eq!(calls[0].method, "on_webhook");
        assert!(calls[0].send);
        assert_eq!(calls[0].body["action"], "accepted");
        assert_eq!(calls[1].body["action"], "declined");
    }
}
