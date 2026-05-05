use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Router,
};
use chrono::{Duration, Utc};
use clap::Parser;
use serde::Serialize;
use serde_json::json;

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value_t = 3001)]
    port: u16,
}

#[derive(Default)]
struct StubState {
    calls: u64,
    collaborators: HashMap<(String, String, String), bool>,
}

type SharedState = Arc<Mutex<StubState>>;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let state: SharedState = Arc::new(Mutex::new(StubState::default()));

    let app = Router::new()
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
        .route("/reset", delete(reset))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    eprintln!("github-stub listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn create_access_token(
    Path(id): Path<String>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    {
        let mut s = state.lock().unwrap();
        s.calls += 1;
    }
    let expires_at = (Utc::now() + Duration::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let body = json!({
        "token": format!("test-token-for-{id}"),
        "expires_at": expires_at,
    });
    (StatusCode::CREATED, Json(body))
}

async fn add_collaborator(
    Path((owner, repo, user)): Path<(String, String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    s.calls += 1;
    let key = (owner, repo, user);
    if s.collaborators.contains_key(&key) {
        s.collaborators.insert(key, true);
        StatusCode::NO_CONTENT
    } else {
        s.collaborators.insert(key, true);
        StatusCode::CREATED
    }
}

async fn check_collaborator(
    Path((owner, repo, user)): Path<(String, String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    s.calls += 1;
    let key = (owner, repo, user);
    if s.collaborators.contains_key(&key) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn remove_collaborator(
    Path((owner, repo, user)): Path<(String, String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    s.calls += 1;
    let key = (owner, repo, user);
    s.collaborators.remove(&key);
    StatusCode::NO_CONTENT
}

#[derive(Serialize)]
struct CallsResponse {
    count: u64,
}

async fn get_calls(State(state): State<SharedState>) -> impl IntoResponse {
    let s = state.lock().unwrap();
    Json(CallsResponse { count: s.calls })
}

async fn reset(State(state): State<SharedState>) -> impl IntoResponse {
    let mut s = state.lock().unwrap();
    s.calls = 0;
    s.collaborators.clear();
    StatusCode::OK
}
