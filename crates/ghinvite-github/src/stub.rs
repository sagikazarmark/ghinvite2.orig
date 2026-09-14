//! Local HTTP fixture only. Never mount this router in a deployed application.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Default)]
struct StubState {
    user_login: Option<String>,
    addressed_id: Option<u64>,
    access_role: Option<String>,
    calls: Vec<serde_json::Value>,
    collaborators: HashMap<(String, String, String), bool>,
    invitations: HashMap<(String, String, u64), serde_json::Value>,
    next_id: u64,
    outcomes: HashMap<(String, String, String), Outcome>,
}

impl StubState {
    fn record(&mut self, method: &str, path: String, permission: Option<&str>, status: StatusCode) {
        self.calls.push(json!({"method": method, "path": path, "permission": permission, "status": status.as_u16()}));
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    AlreadyCollaborator,
    TerminalFailure,
    TransientOnce,
    CreatedResponseLost,
    CreatedThenDeclined,
    AccessLostOnce,
}

#[derive(Deserialize)]
struct ConfigureOutcome {
    owner: String,
    repo: String,
    user: String,
    outcome: Outcome,
}

async fn configure_outcome(
    State(state): State<SharedState>,
    Json(input): Json<ConfigureOutcome>,
) -> StatusCode {
    state
        .lock()
        .unwrap()
        .outcomes
        .insert((input.owner, input.repo, input.user), input.outcome);
    StatusCode::NO_CONTENT
}

type SharedState = Arc<Mutex<StubState>>;

pub fn router() -> Router {
    routes(Arc::new(Mutex::new(StubState::default())))
}

fn routes(state: SharedState) -> Router {
    Router::new()
        .route("/user/{id}", get(|Path(id): Path<u64>, State(state): State<SharedState>| async move { Json(json!({"id":id,"login":state.lock().unwrap().user_login.as_deref().unwrap_or("alice")})) }))
        .route("/users/{login}", get(|Path(login): Path<String>, State(state): State<SharedState>| async move { Json(json!({"id":state.lock().unwrap().addressed_id.unwrap_or(8),"login":login})) }))
        .route("/identity", post(|State(state): State<SharedState>, Json(input): Json<serde_json::Value>| async move {
            let mut state = state.lock().unwrap();
            state.user_login = input["login"].as_str().map(str::to_owned);
            state.addressed_id = input["addressed_id"].as_u64();
            state.access_role = input["role_name"].as_str().map(str::to_owned);
            StatusCode::NO_CONTENT
        }))
        .route("/repos/{owner}/{repo}/collaborators/{user}/permission",get(|State(state): State<SharedState>| async move {
            let state = state.lock().unwrap();
            match &state.access_role {
                Some(role) => (StatusCode::OK,Json(json!({"user":{"id":8,"login":"alice"},"role_name":role,"permission":if role=="triage" {"read"} else {"write"}}))).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }))
        .route("/repos/{owner}/{repo}", get(|Path((owner, repo)): Path<(String, String)>| async move { Json(json!({"id":if repo == "api" {10} else {11},"full_name":format!("{owner}/{repo}"),"private":true})) }))
        .route(
            "/app/installations/{id}/access_tokens",
            post(create_access_token),
        )
        .route(
            "/repos/{owner}/{repo}/collaborators/{user}",
            put(add_collaborator),
        )
        .route(
            "/repos/{owner}/{repo}/collaborators/{user}",
            get(check_collaborator),
        )
        .route(
            "/repos/{owner}/{repo}/collaborators/{user}",
            delete(remove_collaborator),
        )
        .route("/calls", get(get_calls))
        .route("/outcomes", post(configure_outcome))
        .route("/repos/{owner}/{repo}/invitations", get(list_invitations))
        .route(
            "/repos/{owner}/{repo}/invitations/{id}",
            delete(delete_invitation),
        )
        .route("/reset", delete(reset))
        .with_state(state)
}

async fn create_access_token(
    Path(id): Path<String>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    {
        let mut s = state.lock().unwrap();
        s.record(
            "POST",
            format!("/app/installations/{id}/access_tokens"),
            None,
            StatusCode::CREATED,
        );
    }
    let expires_at =
        (Utc::now() + Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let body = json!({
        "token": format!("test-token-for-{id}"),
        "expires_at": expires_at,
    });
    (StatusCode::CREATED, Json(body))
}

async fn add_collaborator(
    Path((owner, repo, user)): Path<(String, String, String)>,
    State(state): State<SharedState>,
    Json(input): Json<AddCollaborator>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    let key = (owner.clone(), repo.clone(), user.clone());
    let lost = matches!(
        s.outcomes.get(&key),
        Some(Outcome::CreatedResponseLost | Outcome::CreatedThenDeclined)
    );
    let declined = matches!(s.outcomes.get(&key), Some(Outcome::CreatedThenDeclined));
    let status = match s.outcomes.get(&key) {
        Some(Outcome::AlreadyCollaborator) => {
            s.collaborators.insert(key.clone(), true);
            StatusCode::NO_CONTENT
        }
        Some(Outcome::TerminalFailure) => StatusCode::UNPROCESSABLE_ENTITY,
        Some(Outcome::TransientOnce) => {
            s.outcomes.remove(&key);
            StatusCode::BAD_GATEWAY
        }
        Some(Outcome::AccessLostOnce) => {
            s.outcomes.remove(&key);
            StatusCode::FORBIDDEN
        }
        Some(Outcome::CreatedResponseLost | Outcome::CreatedThenDeclined) => StatusCode::CREATED,
        None if s.collaborators.contains_key(&key) => StatusCode::NO_CONTENT,
        None => StatusCode::CREATED,
    };
    s.record(
        "PUT",
        format!("/repos/{owner}/{repo}/collaborators/{user}"),
        Some(&input.permission),
        status,
    );
    if status == StatusCode::NO_CONTENT {
        return StatusCode::NO_CONTENT.into_response();
    }
    if !status.is_success() {
        return (status, Json(json!({"message": "Controlled stub failure"}))).into_response();
    }
    s.next_id += 1;
    let id = s.next_id;
    let permissions = match input.permission.as_str() {
        "pull" => "read",
        "push" => "write",
        other => other,
    };
    let invitation = json!({
        "id": id,
        "invitee": {"id": 8, "login": user},
        "repository": {"full_name": format!("{owner}/{repo}")},
        "permissions": permissions,
        "created_at": Utc::now(),
    });
    if !declined {
        s.invitations.insert((owner, repo, id), invitation.clone());
    }
    if lost {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    (StatusCode::CREATED, Json(invitation)).into_response()
}

#[derive(Deserialize)]
struct AddCollaborator {
    permission: String,
}

async fn list_invitations(
    Path((owner, repo)): Path<(String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    s.record(
        "GET",
        format!("/repos/{owner}/{repo}/invitations"),
        None,
        StatusCode::OK,
    );
    Json(
        s.invitations
            .iter()
            .filter(|((o, r, _), _)| o == &owner && r == &repo)
            .map(|(_, invitation)| invitation.clone())
            .collect::<Vec<_>>(),
    )
}

async fn delete_invitation(
    Path((owner, repo, id)): Path<(String, String, u64)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    let status = if s
        .invitations
        .remove(&(owner.clone(), repo.clone(), id))
        .is_some()
    {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    };
    s.record(
        "DELETE",
        format!("/repos/{owner}/{repo}/invitations/{id}"),
        None,
        status,
    );
    status
}

async fn check_collaborator(
    Path((owner, repo, user)): Path<(String, String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    let key = (owner.clone(), repo.clone(), user.clone());
    let status = if s.collaborators.contains_key(&key) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    };
    s.record(
        "GET",
        format!("/repos/{owner}/{repo}/collaborators/{user}"),
        None,
        status,
    );
    status
}

async fn remove_collaborator(
    Path((owner, repo, user)): Path<(String, String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    s.record(
        "DELETE",
        format!("/repos/{owner}/{repo}/collaborators/{user}"),
        None,
        StatusCode::NO_CONTENT,
    );
    let key = (owner, repo, user);
    s.collaborators.remove(&key);
    StatusCode::NO_CONTENT
}

#[derive(Serialize)]
struct CallsResponse {
    count: usize,
    requests: Vec<serde_json::Value>,
}

async fn get_calls(State(state): State<SharedState>) -> impl IntoResponse {
    let s = state.lock().unwrap();
    Json(CallsResponse {
        count: s.calls.len(),
        requests: s.calls.clone(),
    })
}

async fn reset(State(state): State<SharedState>) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    *s = StubState::default();
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstallationClient, jwt::AppJwtSigner, transport::ReqwestTransport};
    use ghinvite_core::Permission;

    #[tokio::test]
    async fn controlled_outcomes_record_safe_calls_and_retry_once() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, router()).await.unwrap();
        });
        let http = reqwest::Client::new();
        let github = InstallationClient::new(
            Arc::new(ReqwestTransport::new().unwrap()),
            AppJwtSigner::from_pem(123, include_str!("jwt_test_key.pem")).unwrap(),
        )
        .with_base(&base);
        for (repo, outcome) in [
            ("member", "already_collaborator"),
            ("denied", "terminal_failure"),
            ("retry", "transient_once"),
        ] {
            let response = http
                .post(format!("{base}/outcomes"))
                .json(&json!({
                    "owner": "acme", "repo": repo, "user": "alice", "outcome": outcome
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }
        assert_eq!(
            github
                .add_collaborator(1, "acme", "member", "alice", Permission::Pull)
                .await
                .unwrap(),
            None
        );
        assert!(
            github
                .is_collaborator(1, "acme", "member", "alice")
                .await
                .unwrap()
        );
        assert_eq!(
            github
                .add_collaborator(1, "acme", "denied", "alice", Permission::Pull)
                .await
                .unwrap_err()
                .status(),
            Some(422)
        );
        assert_eq!(
            github
                .add_collaborator(1, "acme", "retry", "alice", Permission::Pull)
                .await
                .unwrap_err()
                .status(),
            Some(502)
        );
        assert!(
            github
                .add_collaborator(1, "acme", "retry", "alice", Permission::Pull)
                .await
                .unwrap()
                .is_some()
        );
        let calls: serde_json::Value = http
            .get(format!("{base}/calls"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let puts: Vec<_> = calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["method"] == "PUT")
            .collect();
        assert_eq!(
            puts,
            vec![
                &json!({"method":"PUT", "path":"/repos/acme/member/collaborators/alice", "permission":"pull", "status":204}),
                &json!({"method":"PUT", "path":"/repos/acme/denied/collaborators/alice", "permission":"pull", "status":422}),
                &json!({"method":"PUT", "path":"/repos/acme/retry/collaborators/alice", "permission":"pull", "status":502}),
                &json!({"method":"PUT", "path":"/repos/acme/retry/collaborators/alice", "permission":"pull", "status":201}),
            ]
        );
        assert!(!calls.to_string().contains("test-token"));
        assert!(!calls.to_string().contains("Bearer"));
    }

    #[tokio::test]
    async fn successful_invitation_is_pending_and_can_be_listed_and_deleted() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, router()).await.unwrap();
        });
        let github = InstallationClient::new(
            Arc::new(ReqwestTransport::new().unwrap()),
            AppJwtSigner::from_pem(123, include_str!("jwt_test_key.pem")).unwrap(),
        )
        .with_base(base);
        let id = github
            .add_collaborator(1, "acme", "api", "alice", Permission::Push)
            .await
            .expect("stub must return a decodable 201 invitation")
            .unwrap();
        assert!(id > 0);
        assert!(
            !github
                .is_collaborator(1, "acme", "api", "alice")
                .await
                .unwrap()
        );
        let pending = github.list_invitations(1, "acme", "api").await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].invitee.login, "alice");
        assert_eq!(pending[0].permissions, "write");
        github
            .delete_invitation(1, "acme", "api", id)
            .await
            .unwrap();
        assert!(
            github
                .list_invitations(1, "acme", "api")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            github
                .delete_invitation(1, "acme", "api", id)
                .await
                .unwrap_err()
                .status(),
            Some(404)
        );
    }
}
