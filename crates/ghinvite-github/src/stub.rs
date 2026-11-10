//! Local HTTP fixtures only. Never mount these routers in a deployed application.
//!
//! [`router`] is a stateful fake GitHub with control routes (`/identity`,
//! `/calls`, `/outcomes`, `/reset`, …) for ghinvite's integration tests. Its
//! token route, `POST /app/installations/{id}/access_tokens`, answers in the
//! shape octoauth parses (`token`, `expires_at`).
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
    installation_account: Option<serde_json::Value>,
    /// The repository IDs the installation makes available, when a fixture
    /// needs the scope to change under it. Defaults to `acme/api` and
    /// `acme/web`.
    installation_repos: Option<Vec<u64>>,
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
    ThrottledOnce,
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

/// Create an isolated local GitHub fixture with control routes; never expose it in a deployed service.
pub fn router() -> Router {
    routes(Arc::new(Mutex::new(StubState::default())))
}

fn routes(state: SharedState) -> Router {
    Router::new()
        .route("/installation-identity", post(|State(state): State<SharedState>, Json(input): Json<serde_json::Value>| async move {
            state.lock().unwrap().installation_account = Some(input);
            StatusCode::NO_CONTENT
        }))
        .route("/app/installations/{id}", get(|Path(id): Path<u64>, State(state): State<SharedState>| async move {
            Json(json!({"id":id,"account":state.lock().unwrap().installation_account.clone().unwrap_or(json!({"id":100,"login":"acme","type":"Organization"})),"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","repository_selection":"all","permissions":{"administration":"write","metadata":"read"},"suspended_at":null}))
        }))
        .route("/installation-repositories", post(|State(state): State<SharedState>, Json(input): Json<Vec<u64>>| async move {
            state.lock().unwrap().installation_repos = Some(input);
            StatusCode::NO_CONTENT
        }))
        .route("/installation/repositories", get(|State(state): State<SharedState>| async move {
            let repos = state.lock().unwrap().installation_repos.clone().unwrap_or_else(|| vec![10, 11]);
            let repositories: Vec<_> = repos.iter().map(|id| json!({"id":id,"full_name":repo_full_name(*id),"private":true,"owner":{"id":100,"login":"acme"}})).collect();
            Json(json!({"total_count":repositories.len(),"repositories":repositories}))
        }))
        .route("/user/{id}", get(|Path(id): Path<u64>, State(state): State<SharedState>| async move { Json(json!({"id":id,"login":state.lock().unwrap().user_login.as_deref().unwrap_or("alice")})) }))
        .route("/users/{login}", get(|Path(login): Path<String>, State(state): State<SharedState>| async move { Json(json!({"id":state.lock().unwrap().addressed_id.unwrap_or(8),"login":login})) }))
        .route("/identity", post(|State(state): State<SharedState>, Json(input): Json<serde_json::Value>| async move {
            let mut state = state.lock().unwrap();
            state.user_login = input["login"].as_str().map(str::to_owned);
            state.addressed_id = input["addressed_id"].as_u64();
            state.access_role = input["role_name"].as_str().map(str::to_owned);
            StatusCode::NO_CONTENT
        }))
        .route("/repos/{owner}/{repo}/collaborators/{user}/permission",get(|Path((_owner, _repo, user)): Path<(String, String, String)>, State(state): State<SharedState>| async move {
            if user == "creator" { return (StatusCode::OK,Json(json!({"user":{"id":7,"login":"creator"},"role_name":"admin","permission":"admin"}))).into_response(); }
            let state = state.lock().unwrap();
            match &state.access_role {
                Some(role) => (StatusCode::OK,Json(json!({"user":{"id":state.addressed_id.unwrap_or(8),"login":state.user_login.as_deref().unwrap_or("alice")},"role_name":role,"permission":if role=="triage" {"read"} else {"write"}}))).into_response(),
                None => (StatusCode::OK,Json(json!({"user":{"id":state.addressed_id.unwrap_or(8),"login":state.user_login.as_deref().unwrap_or("alice")},"role_name":"none","permission":"none"}))).into_response(),
            }
        }))
        .route("/repos/{owner}/{repo}", get(|Path((owner, repo)): Path<(String, String)>| async move { Json(json!({"id":if repo == "api" {10} else {11},"full_name":format!("{owner}/{repo}"),"private":true,"owner":{"id":100,"login":owner}})) }))
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
        .route("/invitation-expired", post(|State(state):State<SharedState>,Json(input):Json<serde_json::Value>| async move {
            let (Some(owner),Some(repo),Some(id),Some(expired))=(input["owner"].as_str(),input["repo"].as_str(),input["id"].as_u64(),input["expired"].as_bool()) else {return StatusCode::BAD_REQUEST;};
            let mut state=state.lock().unwrap();
            let Some(invitation)=state.invitations.get_mut(&(owner.to_owned(),repo.to_owned(),id)) else {return StatusCode::NOT_FOUND;};
            invitation["expired"]=json!(expired);
            StatusCode::NO_CONTENT
        }))
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
    // A throttled refusal answers with GitHub's secondary-limit shape, not the
    // bare body every other controlled failure uses.
    let mut throttled = false;
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
        Some(Outcome::ThrottledOnce) => {
            s.outcomes.remove(&key);
            throttled = true;
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
        if throttled {
            return (
                status,
                [(axum::http::header::RETRY_AFTER, "1")],
                Json(json!({"message": "You have exceeded a secondary rate limit"})),
            )
                .into_response();
        }
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
        "invitee": {"id": s.addressed_id.unwrap_or(8), "login": user},
        "repository": {"full_name": format!("{owner}/{repo}")},
        "permissions": permissions,
        "created_at": Utc::now(),
        "expired": false,
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

/// The two repositories every other fixture already names, and a stable name
/// for any further ID a fixture makes available.
fn repo_full_name(id: u64) -> String {
    match id {
        10 => "acme/api".to_owned(),
        11 => "acme/web".to_owned(),
        other => format!("acme/repo-{other}"),
    }
}

async fn reset(State(state): State<SharedState>) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    // GitHub identities are never reused, even when a scenario clears observations.
    let next_id = s.next_id;
    *s = StubState {
        next_id,
        ..StubState::default()
    };
    StatusCode::OK
}

#[cfg(test)]
mod tests;

mod wire;
/// Create the frozen synthetic HTTP fixture for every typed endpoint. Never
/// deploy this router.
///
/// Requests must match the corpus's literal method, target, authentication,
/// headers and serialized body. The default serves the first reviewed success
/// pair. `POST /__fixture/select` accepts `{ "endpoint": "PutCollaborator",
/// "response": 1 }` to select another listed success response, or
/// `{ "protocol_case": "sso-partial" }` for a named header/status family.
/// `GET /__fixture/summary` returns only request/match/refusal counts. All inputs
/// and responses are synthetic fixtures; they do not establish live GitHub
/// behavior, permissions, or unresolved spike results. Existing consumer
/// [`router`] behavior is independent of this additive fixture.
pub fn wire_contract_router() -> Router {
    wire::router()
}
