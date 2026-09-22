use crate::error::Result;
use crate::restate_client::RestateClient;
use crate::state::WebStorage;
use chrono::{DateTime, Utc};
use octoevents::{Action, Dispatcher, EventKind, Payload};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RestateCommands {
    restate: Arc<RestateClient>,
}

impl RestateCommands {
    pub fn new(restate: Arc<RestateClient>) -> Self {
        Self { restate }
    }
}

/// Every handler behind [`RestateCommands`] changes state, and none of these
/// call sites can cheaply re-read the result. A refused or unreachable ingress
/// therefore leaves the command's effect in doubt — the invocation may have
/// been persisted before the response went wrong — so each failure is reported
/// as outcome-unknown rather than as a clean refusal that never happened.
fn mutation_outcome(error: crate::WebError) -> crate::WebError {
    match error {
        crate::WebError::Restate(failure) => {
            crate::WebError::Restate(failure.into_outcome_unknown())
        }
        other => other,
    }
}

impl RestateCommands {
    pub async fn onboard_installation(&self, command: OnboardInstallation) -> Result<()> {
        self.restate
            .call(
                "Installation",
                &command.installation_id.to_string(),
                "onboard",
                &command,
            )
            .await
            .map_err(mutation_outcome)
    }

    pub async fn record_repository_selection_change(
        &self,
        command: RecordRepositorySelectionChange,
    ) -> Result<()> {
        match command.source {
            RepositorySelectionChangeSource::SetupReturn => self
                .restate
                .call(
                    "Installation",
                    &command.installation_id.to_string(),
                    "repos_changed",
                    &command,
                )
                .await
                .map_err(mutation_outcome),
            RepositorySelectionChangeSource::Webhook => self
                .restate
                .send(
                    "Installation",
                    &command.installation_id.to_string(),
                    "repos_changed",
                    &command,
                )
                .await
                .map_err(mutation_outcome),
        }
    }

    pub async fn record_installation_uninstalled(
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
            .map_err(mutation_outcome)
    }

    pub async fn route_github_invitation_webhook(
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
            .map_err(mutation_outcome)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
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
    commands: &RestateCommands,
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

#[derive(Deserialize)]
struct WebhookId {
    id: u64,
}

#[derive(Deserialize, Payload)]
#[payload(EventKind::from_static("repository_invitation"))]
struct RepositoryInvitationPayload {
    action: GithubInvitationWebhookAction,
    invitation: WebhookId,
}

#[derive(Deserialize, Payload)]
#[payload(EventKind::Member)]
struct MemberPayload {
    installation: WebhookId,
    repository: MemberRepository,
    member: WebhookId,
}

#[derive(Deserialize)]
struct MemberRepository {
    id: u64,
    owner: WebhookId,
}

#[derive(Deserialize, Payload)]
#[payload(EventKind::Installation)]
struct InstallationPayload {
    installation: WebhookId,
}

#[derive(Deserialize, Payload)]
#[payload(EventKind::InstallationRepositories)]
struct InstallationRepositoriesPayload {
    installation: WebhookId,
    #[serde(rename = "repository_selection")]
    _repository_selection: RepositorySelection,
    #[serde(rename = "repositories_removed")]
    _repositories_removed: Vec<WebhookId>,
    #[serde(rename = "repositories_added")]
    _repositories_added: Vec<WebhookId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RepositorySelection {
    All,
    Selected,
}

/// Builds routes for one delivery, using its receipt time for emitted commands.
pub fn github_webhook_dispatcher(
    storage: Arc<dyn WebStorage>,
    commands: RestateCommands,
    received_at: DateTime<Utc>,
) -> Dispatcher {
    let invitation_storage = storage.clone();
    let invitation_commands = commands.clone();
    let installation_commands = commands.clone();
    let member_commands = commands.clone();

    Dispatcher::builder()
        .on(
            (EventKind::Member, Action::Added),
            move |envelope: octoevents::Envelope| {
                let storage = storage.clone();
                let commands = member_commands.clone();
                async move {
                    use sha2::Digest;
                    let payload: MemberPayload = envelope.decode().map_err(|_| {
                        crate::WebError::BadRequest("Invalid member payload".into())
                    })?;
                    let digest = hex::encode(sha2::Sha256::digest(&envelope.raw_payload));
                    let account = storage.get_installation(payload.installation.id).await?;
                    // Links keep their original installation provenance after reinstall.
                    // Match immutable account/repository/requester IDs, never logins or sender.
                    let rows = if let Some(account) = account
                        && account.account_id == payload.repository.owner.id
                    {
                        storage
                            .member_invitation_candidates(
                                account.account_id,
                                payload.repository.id,
                                payload.member.id,
                            )
                            .await?
                    } else {
                        vec![]
                    };
                    // Include terminal history so redelivery cannot match a later request.
                    let candidate = if let [row] = rows.as_slice()
                        && row.state == ghinvite_core::InvitationState::Sent
                        && row.github_invitation_id.is_some()
                    {
                        Some(row.id)
                    } else {
                        None
                    };
                    if let Some(invitation_id) =
                        storage.bind_member_webhook(&digest, candidate).await?
                    {
                        commands
                            .route_github_invitation_webhook(RouteGithubInvitationWebhook {
                                invitation_id,
                                action: GithubInvitationWebhookAction::Accepted,
                                at: received_at,
                            })
                            .await?;
                    }
                    Result::Ok(())
                }
            },
        )
        .on(
            [
                Action::from_static("accepted"),
                Action::from_static("declined"),
            ],
            move |payload: RepositoryInvitationPayload| {
                let storage = invitation_storage.clone();
                let commands = invitation_commands.clone();
                async move {
                    let Some(invitation) = storage
                        .get_github_invitation_by_github_id(payload.invitation.id)
                        .await?
                    else {
                        return Ok(());
                    };
                    commands
                        .route_github_invitation_webhook(RouteGithubInvitationWebhook {
                            invitation_id: invitation.id,
                            action: payload.action,
                            at: received_at,
                        })
                        .await
                }
            },
        )
        .on(Action::Deleted, move |payload: InstallationPayload| {
            let commands = installation_commands.clone();
            async move {
                commands
                    .record_installation_uninstalled(RecordInstallationUninstalled {
                        installation_id: payload.installation.id,
                        uninstalled_at: received_at,
                    })
                    .await
            }
        })
        .on(
            [Action::Added, Action::Removed],
            move |payload: InstallationRepositoriesPayload| {
                let commands = commands.clone();
                async move {
                    let installation_id = payload.installation.id;
                    commands
                        .record_repository_selection_change(RecordRepositorySelectionChange {
                            installation_id,
                            // Compatibility field only. Serialized processing refreshes
                            // GitHub rather than trusting delivery-time deltas.
                            selected_repos: ghinvite_core::SelectedRepos::Subset(vec![]),
                            source: RepositorySelectionChangeSource::Webhook,
                        })
                        .await
                }
            },
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RestateClient;
    use axum::extract::Path;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use chrono::Utc;
    use ghinvite_core::storage::{DeliveryStorage, InstallationStorage, RecordStorage};

    use ghinvite_core::storage::projection::fixture::Seed;
    use octoevents::{Envelope, Match};
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

    /// The real commands over an ingress that records what reached it.
    async fn recording_commands() -> (RestateCommands, Arc<Mutex<Vec<RecordedCall>>>) {
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        (
            RestateCommands::new(Arc::new(RestateClient::new(base).unwrap())),
            calls,
        )
    }

    /// A webhook-sourced selection change is a one-way send that carries no
    /// delivery-time repositories: processing refreshes them from GitHub.
    fn assert_repos_changed_sent(calls: &[RecordedCall], installation_id: u64) {
        assert_eq!(calls.len(), 1, "{calls:?}");
        let call = &calls[0];
        assert_eq!(call.service, "Installation");
        assert_eq!(call.key, installation_id.to_string());
        assert_eq!(call.method, "repos_changed");
        assert!(call.send);
        assert_eq!(call.body["installation_id"], installation_id);
        assert_eq!(call.body["selected_repos"], serde_json::json!([]));
    }

    fn assert_invitation_webhook_sent(
        call: &RecordedCall,
        invitation_id: ghinvite_core::GithubInvitationId,
        action: &str,
    ) {
        assert_eq!(call.service, "GithubInvitation");
        assert_eq!(call.key, invitation_id.to_string());
        assert_eq!(call.method, "on_webhook");
        assert!(call.send);
        assert_eq!(call.body["invitation_id"], invitation_id.to_string());
        assert_eq!(call.body["action"], action);
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
            .seed_link(&ghinvite_core::InvitationLink {
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
            .seed_request(&ghinvite_core::InvitationRequest {
                id: request_id,
                invitation_link_id: link_id,
                requester_id: 99,
                justification: None,
                state: ghinvite_core::RequestState::Pending,
                decided_by: None,
                decided_at: None,
                decline_reason: None,
                decision_deadline: None,
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
            .debug_set_github_invitation(
                invitation_id,
                ghinvite_core::InvitationState::Sent,
                Some(github_invitation_id),
            )
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
        let (commands, calls) = recording_commands().await;

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

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "Installation");
        assert_eq!(call.key, "77");
        assert_eq!(call.method, "onboard");
        assert!(!call.send);
        assert_eq!(call.body["installation_id"], 77);
        assert_eq!(call.body["actor_user_id"], 42);
        assert_eq!(call.body["account_id"], 9001);
        assert_eq!(call.body["account_login"], "acme");
        assert_eq!(call.body["account_type"], "Organization");
        assert_eq!(call.body["selected_repos"], "all");
        assert_eq!(call.body["installed_at"], "2026-05-20T13:00:00Z");
    }

    #[tokio::test]
    async fn setup_return_update_delegates_to_repository_selection_command() {
        let (commands, calls) = recording_commands().await;

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

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "Installation");
        assert_eq!(call.key, "77");
        assert_eq!(call.method, "repos_changed");
        // A setup return waits for the change, where a webhook only sends it.
        assert!(!call.send);
        assert_eq!(call.body["installation_id"], 77);
        assert_eq!(call.body["selected_repos"], serde_json::json!([10, 11]));
    }

    #[tokio::test]
    async fn github_unsupported_installation_action_without_id_is_ignored() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:00:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::Installation,
                    br#"{"action":"future_action"}"#,
                ))
                .await;

        outcome.result.unwrap();
        assert_eq!(outcome.matched, Match::UnmatchedAction);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn github_installation_deleted_event_delegates_to_uninstall_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:00:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::Installation,
                    br#"{"action":"deleted","installation":{"id":77}}"#,
                ))
                .await;

        outcome.result.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "Installation");
        assert_eq!(call.key, "77");
        assert_eq!(call.method, "uninstall");
        assert!(call.send);
        assert_eq!(call.body["installation_id"], 77);
        assert_eq!(call.body["uninstalled_at"], "2026-05-20T14:00:00Z");
    }

    #[tokio::test]
    async fn signed_member_added_matches_numeric_identities() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        use tower::ServiceExt;

        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let invitation_id = seed_github_invitation(&storage, 99001).await;
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let secret = b"test-webhook-secret";
        let state = crate::AppState::new(
            Arc::new(storage),
            Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
            Arc::new(RestateClient::new(base).unwrap()),
            crate::WebConfig {
                webhook_secret: secret.to_vec(),
                ..crate::WebConfig::for_local_dev_with_secret([7; 32])
            },
        );
        let app = crate::build_app(state, tower_sessions::MemoryStore::default());
        // Names and sender deliberately differ from stored profiles. Only member.id
        // identifies the requester; the event contains no upstream invitation ID.
        let payload = r#"{"action":"added","installation":{"id":1},"repository":{"id":10,"full_name":"renamed/project","owner":{"id":9001}},"member":{"id":99,"login":"renamed-user"},"sender":{"id":42}}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(payload.as_bytes());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/github")
                    .header("content-type", "application/json")
                    .header("x-github-event", "member")
                    .header("x-github-delivery", "member-added-1")
                    .header(
                        "x-hub-signature-256",
                        format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                    )
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "GithubInvitation");
        assert_eq!(calls[0].key, invitation_id.to_string());
        assert_eq!(calls[0].method, "on_webhook");
        assert!(calls[0].send);
        assert_eq!(calls[0].body["action"], "accepted");
    }

    #[tokio::test]
    async fn signed_member_events_cannot_route_unrelated_or_unconfirmed_invitations() {
        use axum::{
            body::{Body, to_bytes},
            http::{Request, StatusCode},
        };
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        use tower::ServiceExt;

        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let id = seed_github_invitation(&storage, 99001).await;
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let secret = b"test-webhook-secret";
        let state = crate::AppState::new(
            storage.clone(),
            Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
            Arc::new(RestateClient::new(base).unwrap()),
            crate::WebConfig {
                webhook_secret: secret.to_vec(),
                ..crate::WebConfig::for_local_dev_with_secret([7; 32])
            },
        );
        let app = crate::build_app(state, tower_sessions::MemoryStore::default());
        let valid = serde_json::json!({"action":"added","installation":{"id":1},
            "repository":{"id":10,"owner":{"id":9001}},"member":{"id":99},"sender":{"id":99}});
        let mut cases = vec![];
        for pointer in [
            "/installation/id",
            "/repository/id",
            "/repository/owner/id",
            "/member/id",
        ] {
            let mut payload = valid.clone();
            *payload.pointer_mut(pointer).unwrap() = serde_json::json!(123456);
            cases.push(("member", payload, false, StatusCode::NO_CONTENT));
        }
        for action in ["removed", "edited", "future_action"] {
            cases.push((
                "member",
                serde_json::json!({"action":action}),
                false,
                StatusCode::NO_CONTENT,
            ));
        }
        cases.push(("membership", valid.clone(), false, StatusCode::NO_CONTENT));
        for value in [
            Value::Null,
            serde_json::json!({}),
            serde_json::json!({"id":"99"}),
            serde_json::json!({"id":-1}),
        ] {
            let mut payload = valid.clone();
            payload["member"] = value;
            // octoevents maps typed payload decode failures to a generic 500.
            cases.push(("member", payload, false, StatusCode::INTERNAL_SERVER_ERROR));
        }
        cases.push(("member", valid.clone(), true, StatusCode::UNAUTHORIZED));
        for (event, payload, tampered, status) in cases {
            let body = payload.to_string();
            let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
            mac.update(if tampered {
                b"different body"
            } else {
                body.as_bytes()
            });
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/webhooks/github")
                        .header("content-type", "application/json")
                        .header("x-github-event", event)
                        .header("x-github-delivery", "negative-member-event")
                        .header(
                            "x-hub-signature-256",
                            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                        )
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{payload}");
            let body =
                String::from_utf8(to_bytes(response.into_body(), 4096).await.unwrap().to_vec())
                    .unwrap();
            assert!(!body.contains(&id.to_string()));
            assert!(!body.contains("AI coding workshop"));
            assert!(calls.lock().unwrap().is_empty(), "{payload}");
        }
        // Uncertain create placeholders belong to the retained create owner.
        storage
            .debug_set_github_invitation(id, ghinvite_core::InvitationState::Sending, None)
            .await
            .unwrap();
        let body = valid.to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body.as_bytes());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/github")
                    .header("content-type", "application/json")
                    .header("x-github-event", "member")
                    .header("x-github-delivery", "unconfirmed-create")
                    .header(
                        "x-hub-signature-256",
                        format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(calls.lock().unwrap().is_empty());
        // An initially untracked/uncertain delivery cannot bind to a later Sent row,
        // even if its unsigned delivery header changes on replay.
        storage
            .debug_set_github_invitation(id, ghinvite_core::InvitationState::Sent, Some(99001))
            .await
            .unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(valid.to_string().as_bytes());
        let response = crate::routes::webhook::router()
            .with_state(crate::AppState::new(
                storage.clone(),
                Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
                Arc::new(RestateClient::new("http://127.0.0.1:1").unwrap()),
                crate::WebConfig {
                    webhook_secret: secret.to_vec(),
                    ..crate::WebConfig::for_local_dev_with_secret([7; 32])
                },
            ))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/github")
                    .header("content-type", "application/json")
                    .header("x-github-event", "member")
                    .header("x-github-delivery", "changed-unsigned-header")
                    .header(
                        "x-hub-signature-256",
                        format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                    )
                    .body(Body::from(valid.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "replay must retain no-match instead of dispatching"
        );
    }

    #[tokio::test]
    async fn signed_member_added_never_reassigns_historical_acceptance_to_another_request() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        use tower::ServiceExt;

        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let old_id = seed_github_invitation(&storage, 99001).await;
        let old = storage
            .get_github_invitation(old_id)
            .await
            .unwrap()
            .unwrap();
        let mut request = storage
            .get_invitation_request(old.invitation_request_id)
            .await
            .unwrap()
            .unwrap();
        let mut link = storage
            .get_invitation_link_by_id(request.invitation_link_id)
            .await
            .unwrap()
            .unwrap();
        link.id = ghinvite_core::InvitationLinkId::new();
        link.slug = ghinvite_core::Slug::from_string("FEDCBA9876543210".into()).unwrap();
        storage.seed_link(&link).await.unwrap();
        request.id = ghinvite_core::RequestId::new();
        request.invitation_link_id = link.id;
        storage.seed_request(&request).await.unwrap();
        let mut newer = old.clone();
        newer.id = ghinvite_core::GithubInvitationId::new();
        newer.invitation_request_id = request.id;
        newer.github_invitation_id = Some(99002);
        storage.insert_github_invitation(&newer).await.unwrap();
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let secret = b"test-webhook-secret";
        let state = crate::AppState::new(
            storage.clone(),
            Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
            Arc::new(RestateClient::new(base).unwrap()),
            crate::WebConfig {
                webhook_secret: secret.to_vec(),
                ..crate::WebConfig::for_local_dev_with_secret([7; 32])
            },
        );
        let app = crate::build_app(state, tower_sessions::MemoryStore::default());
        let payload = r#"{"action":"added","installation":{"id":1},"repository":{"id":10,"owner":{"id":9001}},"member":{"id":99}}"#;
        for state in [
            ghinvite_core::InvitationState::Sent,
            ghinvite_core::InvitationState::Accepted,
        ] {
            storage
                .debug_set_github_invitation(old_id, state, Some(99001))
                .await
                .unwrap();
            let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
            mac.update(payload.as_bytes());
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/webhooks/github")
                        .header("content-type", "application/json")
                        .header("x-github-event", "member")
                        .header("x-github-delivery", "historical-member-event")
                        .header(
                            "x-hub-signature-256",
                            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
                        )
                        .body(Body::from(payload))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert!(
                calls.lock().unwrap().is_empty(),
                "must not guess between historical requests"
            );
        }
    }

    #[tokio::test]
    async fn signed_repository_invitation_webhook_routes_command_and_rejects_tampering() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        use tower::ServiceExt;

        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let invitation_id = seed_github_invitation(&storage, 99001).await;
        let (base, calls) = spawn_restate_recorder(Value::Null).await;
        let secret = b"test-webhook-secret";
        let state = crate::AppState::new(
            Arc::new(storage),
            Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
            Arc::new(RestateClient::new(base).unwrap()),
            crate::WebConfig {
                webhook_secret: secret.to_vec(),
                ..crate::WebConfig::for_local_dev_with_secret([7; 32])
            },
        );
        let app = crate::build_app(state, tower_sessions::MemoryStore::default());
        let payload = r#"{"action":"accepted","invitation":{"id":99001}}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(payload.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        let before = Utc::now();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/github")
                    .header("content-type", "application/json")
                    .header("x-github-event", "repository_invitation")
                    .header("x-github-delivery", "delivery-1")
                    .header("x-hub-signature-256", &signature)
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        let after = Utc::now();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        {
            let calls = calls.lock().unwrap();
            let [call] = calls.as_slice() else {
                panic!("expected one invitation webhook command, got {calls:?}");
            };
            assert_invitation_webhook_sent(call, invitation_id, "accepted");
            let at: chrono::DateTime<Utc> =
                serde_json::from_value(call.body["at"].clone()).unwrap();
            assert!((before..=after).contains(&at));
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/github")
                    .header("content-type", "application/json")
                    .header("x-github-event", "repository_invitation")
                    .header("x-github-delivery", "delivery-2")
                    .header("x-hub-signature-256", &signature)
                    .body(Body::from(
                        r#"{"action":"declined","invitation":{"id":99001}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn github_repository_invitation_event_delegates_to_invitation_webhook_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let invitation_id = seed_github_invitation(&storage, 99001).await;
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:05:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::from_static("repository_invitation"),
                    br#"{"action":"accepted","invitation":{"id":99001}}"#,
                ))
                .await;

        outcome.result.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_invitation_webhook_sent(&calls[0], invitation_id, "accepted");
        assert_eq!(calls[0].body["at"], "2026-05-20T14:05:00Z");
    }

    #[tokio::test]
    async fn github_repository_invitation_declined_event_delegates_to_invitation_webhook_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let invitation_id = seed_github_invitation(&storage, 99002).await;
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:06:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::from_static("repository_invitation"),
                    br#"{"action":"declined","invitation":{"id":99002}}"#,
                ))
                .await;

        outcome.result.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_invitation_webhook_sent(&calls[0], invitation_id, "declined");
        assert_eq!(calls[0].body["at"], "2026-05-20T14:06:00Z");
    }

    #[tokio::test]
    async fn github_installation_repositories_event_delegates_to_repository_selection_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(&storage, ghinvite_core::SelectedRepos::Subset(vec![10, 11]))
            .await;
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:10:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::InstallationRepositories,
                    br#"{
                "action": "added",
                "repository_selection": "selected",
                "installation": {"id": 77},
                "repositories_removed": [{"id": 10}],
                "repositories_added": [{"id": 12}]
            }"#,
                ))
                .await;

        outcome.result.unwrap();
        assert_repos_changed_sent(&calls.lock().unwrap(), 77);
    }

    #[tokio::test]
    async fn github_installation_repositories_all_event_requests_refresh() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(&storage, ghinvite_core::SelectedRepos::Subset(vec![10, 11]))
            .await;
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:15:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::InstallationRepositories,
                    br#"{
                "action": "added",
                "repository_selection": "all",
                "installation": {"id": 77},
                "repositories_removed": [],
                "repositories_added": []
            }"#,
                ))
                .await;

        outcome.result.unwrap();
        assert_repos_changed_sent(&calls.lock().unwrap(), 77);
    }

    #[tokio::test]
    async fn github_installation_repositories_all_to_selected_requests_refresh() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(&storage, ghinvite_core::SelectedRepos::All).await;
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:20:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::InstallationRepositories,
                    br#"{
                "action": "removed",
                "repository_selection": "selected",
                "installation": {"id": 77},
                "repositories_removed": [{"id": 10}],
                "repositories_added": []
            }"#,
                ))
                .await;

        outcome.result.unwrap();
        assert_repos_changed_sent(&calls.lock().unwrap(), 77);
    }

    #[tokio::test]
    async fn github_repository_deltas_request_refresh_and_ignore_extra_fields() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(
            &storage,
            ghinvite_core::SelectedRepos::Subset(vec![20, 11, 10, 11]),
        )
        .await;
        let (commands, calls) = recording_commands().await;

        github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:20:00Z"))
            .dispatch(Envelope::new(
                "delivery-1",
                EventKind::InstallationRepositories,
                br#"{
                "action": "removed",
                "repository_selection": "selected",
                "installation": {"id": 77},
                "repositories_removed": [{"id": 10}, {"id": 12}],
                "repositories_added": [{"id": 12, "full_name": "acme/api"}, {"id": 5}, {"id": 12}],
                "future_field": true
            }"#,
            ))
            .await
            .result
            .unwrap();

        assert_repos_changed_sent(&calls.lock().unwrap(), 77);
    }

    #[tokio::test]
    async fn github_repository_deltas_reject_missing_or_malformed_fields_without_commands() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        seed_installation_with_repos(&storage, ghinvite_core::SelectedRepos::Subset(vec![10]))
            .await;
        let (commands, calls) = recording_commands().await;
        let dispatcher =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:20:00Z"));
        let valid = serde_json::json!({
            "action": "added", "installation": {"id": 77},
            "repository_selection": "selected",
            "repositories_removed": [], "repositories_added": [{"id": 12}]
        });
        for field in [
            "repository_selection",
            "repositories_removed",
            "repositories_added",
        ] {
            let invalid_values = if field == "repository_selection" {
                vec![
                    Value::Null,
                    serde_json::json!(true),
                    serde_json::json!("unknown"),
                ]
            } else {
                vec![
                    Value::Null,
                    serde_json::json!({}),
                    serde_json::json!([{}]),
                    serde_json::json!([{"id": "12"}]),
                    serde_json::json!([{"id": -1}]),
                    serde_json::json!([{"id": 1.5}]),
                    serde_json::json!([null]),
                ]
            };
            for replacement in std::iter::once(None).chain(invalid_values.into_iter().map(Some)) {
                let mut payload = valid.clone();
                match replacement {
                    Some(value) => {
                        payload[field] = value;
                    }
                    None => {
                        payload.as_object_mut().unwrap().remove(field);
                    }
                }
                let outcome = dispatcher
                    .dispatch(Envelope::new(
                        "delivery-1",
                        EventKind::InstallationRepositories,
                        serde_json::to_vec(&payload).unwrap(),
                    ))
                    .await;
                assert!(
                    outcome.result.is_err(),
                    "accepted invalid payload: {payload}"
                );
                assert!(calls.lock().unwrap().is_empty());
            }
        }
    }

    #[tokio::test]
    async fn github_unknown_events_and_unsupported_actions_emit_no_commands() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let (commands, calls) = recording_commands().await;
        let dispatcher =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:20:00Z"));

        for (kind, payload, matched) in [
            (
                EventKind::from_static("future_event"),
                r#"{"action":"deleted"}"#,
                Match::UnmatchedKind,
            ),
            (
                EventKind::from_static("repository_invitation"),
                r#"{"action":"created"}"#,
                Match::UnmatchedAction,
            ),
            (
                EventKind::InstallationRepositories,
                r#"{"action":"future_action"}"#,
                Match::UnmatchedAction,
            ),
            (
                EventKind::InstallationRepositories,
                r#"{"installation":{"id":77}}"#,
                Match::UnmatchedAction,
            ),
        ] {
            let outcome = dispatcher
                .dispatch(Envelope::new("delivery-1", kind, payload))
                .await;
            outcome.result.unwrap();
            assert_eq!(outcome.matched, matched);
        }

        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn github_untracked_repository_invitation_emits_no_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:20:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::from_static("repository_invitation"),
                    br#"{"action":"accepted","invitation":{"id":99001}}"#,
                ))
                .await;

        outcome.result.unwrap();
        assert_eq!(outcome.matched, Match::Matched);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn github_supported_installation_action_without_id_fails_without_emitting_a_command() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let (commands, calls) = recording_commands().await;

        let outcome =
            github_webhook_dispatcher(Arc::new(storage), commands, at("2026-05-20T14:20:00Z"))
                .dispatch(Envelope::new(
                    "delivery-1",
                    EventKind::Installation,
                    br#"{"action":"deleted"}"#,
                ))
                .await;

        assert_eq!(outcome.matched, Match::Matched);
        assert!(outcome.result.is_err());
        assert!(calls.lock().unwrap().is_empty());
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
