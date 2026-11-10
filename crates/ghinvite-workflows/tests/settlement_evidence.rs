//! Real Restate owner transitions driven by typed GitHub HTTP observations.
#![cfg(feature = "integration")]
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use ghinvite_core::{
    GithubInvitationId, InvitationLinkId, RequestId,
    delivery::{CreateCommand, CreateOutcome, DeliverySnapshot},
    storage::{ConsoleStorage, RecordStorage},
};
use ghinvite_e2e::{Call, Harness};
use ghinvite_workflows::settlement_policy::{Evidence, Observation};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Github {
    pending: bool,
    expired: bool,
    role: Option<&'static str>,
    unavailable: bool,
    lose_write_reply: bool,
    calls: Vec<(String, String)>,
}
async fn github(
    State(state): State<Arc<Mutex<Github>>>,
    request: axum::extract::Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().to_string();
    let mut state = state.lock().unwrap();
    state.calls.push((method.clone(), path.clone()));
    let body = match (method.as_str(),path.as_str()) {
        ("POST", "/app/installations/1/access_tokens") => return (axum::http::StatusCode::CREATED,Json(json!({"token":"settlement-fixture-token","expires_at":Utc::now()+chrono::Duration::hours(1)}))).into_response(),
        ("GET", "/app/installations/1") => json!({"id":1,"account":{"id":100,"login":"acme","type":"Organization"},"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","repository_selection":"all","permissions":{"administration":"write","metadata":"read"},"suspended_at":null}),
        ("GET", "/installation/repositories") => json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","private":true}]}),
        ("GET", "/repos/acme/api") if state.unavailable => return axum::http::StatusCode::NOT_FOUND.into_response(),
        ("GET", "/repos/acme/api") => json!({"id":10,"full_name":"acme/api","private":true}),
        ("GET", "/user/8") | ("GET", "/users/alice") => json!({"id":8,"login":"alice"}),
        ("GET", "/repos/acme/api/collaborators/alice/permission") => match state.role {
            Some(role) => json!({"user":{"id":8,"login":"alice"},"permission":"write","role_name":role}),
            None => json!({"user":{"id":8,"login":"alice"},"permission":"none","role_name":"none"}),
        },
        ("GET", "/repos/acme/api/collaborators/alice") => return if state.role.is_some() { axum::http::StatusCode::NO_CONTENT } else { axum::http::StatusCode::NOT_FOUND }.into_response(),
        ("GET", "/repos/acme/api/invitations") => if state.pending { {let mut value=invitation(); value["expired"]=json!(state.expired); Value::Array(vec![value])} } else { json!([]) },
        ("PUT", "/repos/acme/api/collaborators/alice") => {
            state.pending=true;
            return if state.lose_write_reply { axum::http::StatusCode::BAD_GATEWAY.into_response() }
                else { (axum::http::StatusCode::CREATED,Json(invitation())).into_response() };
        }
        // Any DELETE is a test failure even if the handler ignores its response.
        _ => return axum::http::StatusCode::NOT_IMPLEMENTED.into_response(),
    };
    Json(body).into_response()
}
fn invitation() -> Value {
    json!({"id":99,"invitee":{"id":8,"login":"alice"},"permissions":"write","created_at":"2026-05-04T12:00:00Z","expired":false})
}
struct Runtime {
    harness: Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    github: Arc<Mutex<Github>>,
}
impl Runtime {
    async fn start() -> Self {
        let github_state = Arc::new(Mutex::new(Github::default()));
        let router = axum::Router::new()
            .fallback(github)
            .with_state(github_state.clone());
        let harness = Harness::start_with_github_router(router).await;
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        for (user_id, login) in [(7, "creator"), (8, "alice")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let github_transport = ghinvite_e2e::github_transport(harness.client.clone());
        let apps = Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                github_transport,
                &harness.github_url,
                ghinvite_e2e::repository_apps(&harness.github_url),
            )
            .unwrap(),
        );
        let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(apps);
        let endpoint = ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap();
        harness.restate.deploy(endpoint).await;
        let reply=harness.restate.invoke(&Call::object("AccountInstallation", "repo:100","onboard"),Some(&ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":Utc::now()}))),None).await;
        assert_eq!(reply.status, 200, "{reply:?}");
        Self {
            harness,
            storage,
            github: github_state,
        }
    }
    async fn command(&self) -> CreateCommand {
        let link:ghinvite_core::InvitationLink=serde_json::from_value(json!({"secret":{"secret_generation":1,"sha256":"2a".repeat(32)},
            "id":InvitationLinkId::new(),"installation_id":1,"account_id":100,"created_by":7,"created_at":Utc::now(),"expires_at":null,"max_uses":null,"uses_count":1,"approval_required":true,"description":"Settlement evidence","internal_note":null,"revoked_at":null,"revoked_by":null,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
        })).unwrap();
        let request:ghinvite_core::InvitationRequest=serde_json::from_value(json!({"id":RequestId::new(),"invitation_link_id":link.id,"requester_id":8,"justification":"Docs","state":"approved","decided_by":7,"decided_at":Utc::now(),"decline_reason":null,"created_at":Utc::now(),"decision_deadline":null})).unwrap();
        ghinvite_core::storage::projection::fixture::seed(
            self.storage.as_ref(),
            &link,
            std::slice::from_ref(&request),
            1,
        )
        .await
        .unwrap();
        CreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: 1,
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: format!("fixture/{}", request.id),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: ghinvite_core::Permission::Push,
            approved_at: request.decided_at.unwrap(),
        }
    }
    async fn owner(&self, command: &CreateCommand, handler: &str, input: &Value) -> Value {
        let reply = self
            .harness
            .restate
            .invoke(
                &Call::object("RepositoryDelivery", &command.delivery_key(), handler),
                (!input.is_null()).then_some(input),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{handler}: {reply:?}");
        reply.body
    }
    async fn snapshot(&self, command: &CreateCommand) -> DeliverySnapshot {
        serde_json::from_value(self.owner(command, "status", &Value::Null).await).unwrap()
    }
    async fn observe(&self, command: &CreateCommand) {
        let snapshot = self.snapshot(command).await;
        let reply = self
            .harness
            .restate
            .invoke(
                &Call::service("DeliveryObservation", "observe"),
                Some(&json!(snapshot)),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
    }
    async fn projected(&self, expected: &DeliverySnapshot) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let rows = self
                    .storage
                    .list_delivery_for_request(expected.create.command.request_id)
                    .await
                    .unwrap();
                if rows.iter().any(|row| row == expected) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("retained settlement projection must converge");
    }
    fn reset(&self) {
        *self.github.lock().unwrap() = Github::default();
    }
}

#[tokio::test]
async fn real_owner_applies_only_complete_current_evidence() {
    let runtime = Runtime::start().await;
    // AC-ACC-2: base/effective read does not satisfy requested write.
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let created = runtime.snapshot(&command).await;
    assert!(
        matches!(
            created.create.outcome,
            CreateOutcome::Created { upstream_id: 99 }
        ),
        "{created:?}; calls={:?}",
        runtime.github.lock().unwrap().calls
    );
    {
        let mut gh = runtime.github.lock().unwrap();
        gh.pending = false;
        gh.role = Some("read");
    }
    runtime.observe(&command).await;
    let declined = runtime.snapshot(&command).await;
    assert_eq!(
        declined.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Declined
    );
    assert_eq!(declined.create, created.create);
    assert_eq!(
        declined.settlement.as_ref().unwrap().event.metadata["evidence"]["role_name"],
        "read"
    );
    // A duplicate or stale stronger observation cannot replace the winner.
    let stale = Observation::new(
        &created,
        Utc::now(),
        Evidence::Absent {
            availability_verified: true,
            role_name: Some(ghinvite_core::targets::EffectiveRole::Admin),
            direct_collaborator: Some(true),
            membership: None,
        },
    );
    runtime.owner(&command, "reconcile", &json!(stale)).await;
    assert_eq!(runtime.snapshot(&command).await, declined);
    runtime.projected(&declined).await;
    let events = runtime
        .storage
        .list_audit_events(
            100,
            Some(ghinvite_core::audit::EventType::InvitationDeclined),
            ghinvite_core::storage::AuditPosition::Latest,
        )
        .await
        .unwrap()
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.target_id
                == command
                    .invitation_id
                    .expect("retained V1 fixture identity")
                    .to_string())
            .count(),
        1
    );

    // AC-ACC-4: full empty invitation list followed by repository 404 never settles.
    runtime.reset();
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let created = runtime.snapshot(&command).await;
    {
        let mut gh = runtime.github.lock().unwrap();
        gh.pending = false;
        gh.role = Some("write");
        gh.unavailable = true;
        gh.calls.clear();
    }
    runtime.observe(&command).await;
    assert_eq!(runtime.snapshot(&command).await, created);
    {
        let gh = runtime.github.lock().unwrap();
        let list = gh
            .calls
            .iter()
            .position(|(_, p)| p.ends_with("/invitations"))
            .unwrap();
        let verify = gh
            .calls
            .iter()
            .position(|(_, p)| p == "/repos/acme/api")
            .unwrap();
        assert!(list < verify);
    }
    runtime.github.lock().unwrap().unavailable = false;
    runtime.observe(&command).await;
    assert_eq!(
        runtime.snapshot(&command).await.settlement.unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );

    // Existing pending invitations are foreign. Revocation and own expiry stop
    // local tracking, leave upstream pending, and never invent an upstream fact.
    for handler in ["cancel", "tick_expire"] {
        runtime.reset();
        runtime.github.lock().unwrap().pending = true;
        let command = runtime.command().await;
        runtime.owner(&command, "create", &json!(command)).await;
        let foreign = runtime.snapshot(&command).await;
        assert!(foreign.create.outcome.tracked_foreign());
        runtime.github.lock().unwrap().calls.clear();
        runtime.owner(&command,handler,&json!({"invitation_id":command.invitation_id,"installation_id":1,"at":Utc::now(),"by_user":7})).await;
        let stopped = runtime.snapshot(&command).await;
        assert!(stopped.tracking_stopped.is_some());
        assert!(stopped.settlement.is_none());
        assert_eq!(stopped.create, foreign.create);
        runtime.observe(&command).await;
        assert!(runtime.github.lock().unwrap().calls.is_empty());
        assert!(runtime.github.lock().unwrap().pending);
        runtime.projected(&stopped).await;
    }

    // The seven-day tick must not mistake an unexpired pending invitation for
    // GitHub expiry; explicit expired=true is required and preserves create history.
    runtime.reset();
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let created = runtime.snapshot(&command).await;
    let tick = json!({"invitation_id":command.invitation_id,"installation_id":1,"at":Utc::now()});
    runtime.owner(&command, "tick_expire", &tick).await;
    assert_eq!(runtime.snapshot(&command).await, created);
    runtime.github.lock().unwrap().expired = true;
    runtime.owner(&command, "tick_expire", &tick).await;
    let expired = runtime.snapshot(&command).await;
    assert_eq!(
        expired.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Expired
    );
    assert_eq!(expired.create, created.create);
    runtime.projected(&expired).await;
    assert!(
        !runtime
            .github
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(method, _)| method == "DELETE")
    );

    // Ambiguous creation: absence, even with strong role, cannot resolve it.
    runtime.reset();
    runtime.github.lock().unwrap().lose_write_reply = true;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let unknown = runtime.snapshot(&command).await;
    assert_eq!(unknown.create.outcome, CreateOutcome::OutcomeUnknown);
    {
        let mut gh = runtime.github.lock().unwrap();
        gh.pending = false;
        gh.role = Some("admin");
    }
    runtime.observe(&command).await;
    assert_eq!(runtime.snapshot(&command).await, unknown);
    runtime.github.lock().unwrap().pending = true;
    runtime.observe(&command).await;
    let resolved = runtime.snapshot(&command).await;
    assert_eq!(
        resolved.create.outcome,
        CreateOutcome::Created { upstream_id: 99 }
    );
    assert!(resolved.settlement.is_none());
    assert_eq!(
        runtime
            .github
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, _)| m == "PUT")
            .count(),
        1
    );
    runtime.harness.finish().await;
}
