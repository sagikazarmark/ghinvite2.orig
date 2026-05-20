use crate::error::Result;
use crate::restate_client::RestateClient;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[async_trait]
pub trait GhinviteCommands: Send + Sync + 'static {
    async fn create_share_link(&self, command: CreateShareLink) -> Result<CreateShareLinkOutput>;
    async fn revoke_share_link(&self, command: RevokeShareLink) -> Result<()>;
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

#[derive(Clone, Debug)]
pub struct RestateCommands {
    restate: Arc<RestateClient>,
}

impl RestateCommands {
    pub fn new(restate: Arc<RestateClient>) -> Self {
        Self { restate }
    }
}

#[async_trait]
impl GhinviteCommands for RestateCommands {
    async fn create_share_link(&self, command: CreateShareLink) -> Result<CreateShareLinkOutput> {
        self.restate
            .call(
                "ShareLink",
                &command.account_id.to_string(),
                "create",
                &command,
            )
            .await
    }

    async fn revoke_share_link(&self, command: RevokeShareLink) -> Result<()> {
        self.restate
            .send(
                "ShareLink",
                &command.account_id.to_string(),
                "revoke",
                &command,
            )
            .await
    }

    async fn submit_invitation_request(&self, command: SubmitInvitationRequest) -> Result<()> {
        self.restate
            .send(
                "InvitationRequest",
                &command.request_id.to_string(),
                "submit",
                &command,
            )
            .await
    }

    async fn decide_invitation_request(&self, command: DecideInvitationRequest) -> Result<()> {
        let _: serde_json::Value = self
            .restate
            .call(
                "InvitationRequest",
                &command.request_id.to_string(),
                "decide",
                &command.decision,
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
pub struct CreateShareLink {
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub permission: domain::Permission,
    pub approval_required: bool,
    pub internal_note: Option<String>,
    pub repos: Vec<domain::ShareLinkRepo>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreateShareLinkOutput {
    pub link_id: domain::ShareLinkId,
    pub slug: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RevokeShareLink {
    #[serde(skip)]
    pub account_id: u64,
    pub link_id: domain::ShareLinkId,
    pub by_user: u64,
    pub when: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SubmitInvitationRequest {
    pub request_id: domain::RequestId,
    pub share_link_id: domain::ShareLinkId,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct DecideInvitationRequest {
    pub request_id: domain::RequestId,
    pub decision: InvitationRequestDecision,
}

#[derive(Clone, Debug, Serialize)]
pub enum InvitationRequestDecision {
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
pub struct OnboardInstallation {
    pub installation_id: u64,
    pub actor_user_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: domain::AccountType,
    pub selected_repos: domain::SelectedRepos,
    pub installed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordRepositorySelectionChange {
    pub installation_id: u64,
    pub selected_repos: domain::SelectedRepos,
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
    pub invitation_id: domain::GithubInvitationId,
    pub action: GithubInvitationWebhookAction,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubInvitationWebhookAction {
    Accepted,
    Declined,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RestateClient;
    use axum::extract::Path;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use chrono::Utc;
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

    fn at(iso: &str) -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn create_share_link_calls_restate_create_and_returns_output() {
        let link_id = domain::ShareLinkId::new();
        let (base, calls) = spawn_restate_recorder(serde_json::json!({
            "link_id": link_id,
            "slug": "0123456789abcdef"
        }))
        .await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));

        let output = commands
            .create_share_link(CreateShareLink {
                installation_id: 77,
                account_id: 9001,
                created_by: 42,
                created_at: at("2026-05-20T10:00:00Z"),
                expires_at: None,
                max_uses: Some(3),
                permission: domain::Permission::Push,
                approval_required: true,
                internal_note: Some("team onboarding".into()),
                repos: vec![domain::ShareLinkRepo {
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
        assert_eq!(call.service, "ShareLink");
        assert_eq!(call.key, "9001");
        assert_eq!(call.method, "create");
        assert!(!call.send);
        assert_eq!(call.body["installation_id"], 77);
        assert_eq!(call.body["account_id"], 9001);
        assert_eq!(call.body["permission"], "push");
        assert_eq!(call.body["repos"][0]["repo_full_name"], "acme/api");
    }

    #[tokio::test]
    async fn submit_invitation_request_sends_restate_workflow() {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));
        let request_id = domain::RequestId::new();
        let share_link_id = domain::ShareLinkId::new();

        commands
            .submit_invitation_request(SubmitInvitationRequest {
                request_id,
                share_link_id,
                requester_id: 42,
                justification: Some("need access".into()),
                created_at: at("2026-05-20T11:00:00Z"),
            })
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "submit");
        assert!(call.send);
        assert_eq!(call.body["request_id"], request_id.to_string());
        assert_eq!(call.body["share_link_id"], share_link_id.to_string());
        assert_eq!(call.body["requester_id"], 42);
    }

    #[tokio::test]
    async fn revoke_share_link_sends_restate_revoke_without_account_id_payload() {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));
        let link_id = domain::ShareLinkId::new();

        commands
            .revoke_share_link(RevokeShareLink {
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
        assert_eq!(call.service, "ShareLink");
        assert_eq!(call.key, "9001");
        assert_eq!(call.method, "revoke");
        assert!(call.send);
        assert_eq!(call.body["link_id"], link_id.to_string());
        assert_eq!(call.body["by_user"], 42);
        assert!(call.body.get("account_id").is_none());
    }

    #[tokio::test]
    async fn decide_invitation_request_calls_restate_decide_with_decision_payload() {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let commands = RestateCommands::new(Arc::new(RestateClient::new(base).unwrap()));
        let request_id = domain::RequestId::new();

        commands
            .decide_invitation_request(DecideInvitationRequest {
                request_id,
                decision: InvitationRequestDecision::Approve {
                    decided_by: 7,
                    decided_at: at("2026-05-20T11:45:00Z"),
                },
            })
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "decide");
        assert!(!call.send);
        assert_eq!(call.body["Approve"]["decided_by"], 7);
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
                account_type: domain::AccountType::Organization,
                selected_repos: domain::SelectedRepos::All,
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
                selected_repos: domain::SelectedRepos::Subset(vec![10]),
                source: RepositorySelectionChangeSource::SetupReturn,
            })
            .await
            .unwrap();
        commands
            .record_repository_selection_change(RecordRepositorySelectionChange {
                installation_id: 77,
                selected_repos: domain::SelectedRepos::Subset(vec![11]),
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
        let accepted_id = domain::GithubInvitationId::new();
        let declined_id = domain::GithubInvitationId::new();

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
