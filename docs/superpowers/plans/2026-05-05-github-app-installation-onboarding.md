<!-- /autoplan restore point: /home/laborant/.gstack/projects/sagikazarmark-ghinvite2.orig/main-autoplan-restore-20260505-230750.md -->
# GitHub App Installation Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make GitHub App installation return users to ghinvite, verify the installation belongs to the signed-in user, and persist the installation before redirecting to the account dashboard.

**Architecture:** Add a Setup URL route at `/setup/github` that treats `installation_id` as untrusted, verifies it via `GET /user/installations`, lists selected repositories when needed, and calls Restate synchronously before redirecting. `setup_action=install` calls `Installation::onboard`; `setup_action=update` calls `Installation::repos_changed` so repository-selection changes do not no-op on duplicate installations. Keep webhooks as reconciliation signals, including fixing `installation_repositories` to send the `SelectedRepos` wire format Restate already accepts. Update setup docs and stale logs to make the Setup URL the documented primary path.

**Tech Stack:** Rust, Axum, tower-sessions, GitHub REST API, Restate HTTP ingress, sqlx-backed storage tests, `github::mocks::MockTransport`.

**Execution constraint:** Do not run `git commit` from this plan unless the user explicitly asks for commits in the current session. Treat task-level checkpoint steps as status checkpoints only.

---

## Design Source

Spec: `docs/superpowers/specs/2026-05-05-github-app-installation-onboarding-design.md`

## File Structure

- Modify: `crates/github/src/payloads.rs` — add user installation response types and serde tests.
- Modify: `crates/github/src/oauth.rs` — add `UserApiClient::list_user_installations()` and transport tests.
- Modify: `crates/web/src/session.rs` — allow safe `/setup/github?...` return paths without reopening open redirects.
- Create: `crates/web/src/routes/setup.rs` — implement verified setup return route and pure mapping helpers.
- Modify: `crates/web/src/routes/mod.rs` — expose the setup route module.
- Modify: `crates/web/src/lib.rs` — register `routes::setup::router()`.
- Create: `crates/web/tests/setup_flow.rs` — exercise unauthenticated redirect, spoof rejection, and verified org install with a local Restate recorder.
- Modify: `crates/web/tests/route_smoke.rs` — add route smoke for missing setup query.
- Modify: `crates/web/src/routes/webhook.rs` — fix `installation_repositories` reconciliation payload shape and stale Setup URL logs.
- Modify: `crates/web/src/routes/oauth.rs` — update stale callback log wording so operators do not look for webhook onboarding.
- Modify: `README.md` — correct GitHub App setup instructions.
- Modify: `docs/deploy.md` — correct production setup instructions.

## Task 1: GitHub Payloads for User Installations

**Files:**
- Modify: `crates/github/src/payloads.rs`

- [ ] **Step 1: Add failing decode tests**

Append these tests inside `#[cfg(test)] mod tests` in `crates/github/src/payloads.rs`:

```rust
#[test]
fn user_installation_list_decodes_org_installation() {
    let raw = br#"{
        "total_count": 1,
        "installations": [{
            "id": 77,
            "account": {
                "id": 9001,
                "login": "acme",
                "avatar_url": "https://avatars.example/u/9001",
                "type": "Organization"
            },
            "repository_selection": "selected",
            "target_type": "Organization",
            "target_id": 9001
        }]
    }"#;
    let list: GhUserInstallationList = serde_json::from_slice(raw).unwrap();
    assert_eq!(list.total_count, 1);
    assert_eq!(list.installations[0].id, 77);
    assert_eq!(list.installations[0].account.login, "acme");
    assert_eq!(
        list.installations[0].account.account_type.as_deref(),
        Some("Organization")
    );
    assert_eq!(list.installations[0].repository_selection, "selected");
}

#[test]
fn user_installation_list_decodes_user_installation() {
    let raw = br#"{
        "total_count": 1,
        "installations": [{
            "id": 88,
            "account": {"id": 42, "login": "octocat", "type": "User"},
            "repository_selection": "all",
            "target_type": "User",
            "target_id": 42
        }]
    }"#;
    let list: GhUserInstallationList = serde_json::from_slice(raw).unwrap();
    assert_eq!(list.installations[0].id, 88);
    assert_eq!(list.installations[0].account.login, "octocat");
    assert_eq!(list.installations[0].repository_selection, "all");
    assert_eq!(list.installations[0].target_type, "User");
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test -p github user_installation_list_decodes -- --nocapture`

Expected: compile failure because `GhUserInstallationList` does not exist.

- [ ] **Step 3: Add the payload types**

Add these structs after `GhInstallationRepos` in `crates/github/src/payloads.rs`:

```rust
/// `GET /user/installations` response envelope.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhUserInstallationList {
    pub total_count: u64,
    pub installations: Vec<GhUserInstallation>,
}

/// One GitHub App installation visible to a user access token.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhUserInstallation {
    pub id: u64,
    pub account: GhUser,
    pub repository_selection: String,
    pub target_type: String,
    pub target_id: u64,
}
```

- [ ] **Step 4: Run the payload tests and verify they pass**

Run: `cargo test -p github user_installation_list_decodes -- --nocapture`

Expected: both new tests pass.

- [ ] **Step 5: Checkpoint**

Do not commit unless the user explicitly asks for commits. Record that Task 1 is complete before moving on.

## Task 2: User API Method for `GET /user/installations`

**Files:**
- Modify: `crates/github/src/oauth.rs`

- [ ] **Step 1: Add the failing user API test**

In `crates/github/src/oauth.rs`, update the import at the top from:

```rust
use crate::payloads::{GhInstallationRepos, GhMembership, GhTokenResponse, GhUser};
```

to:

```rust
use crate::payloads::{
    GhInstallationRepos, GhMembership, GhTokenResponse, GhUser, GhUserInstallationList,
};
```

Then append this test inside `mod user_api_tests`:

```rust
#[tokio::test]
async fn list_user_installations_decodes_response() {
    let mock = MockTransport::scripted(vec![Expectation {
        method: Method::Get,
        url: "https://api.github.test/user/installations?per_page=100".into(),
        required_headers: {
            let mut h = BTreeMap::new();
            h.insert("authorization".into(), "Bearer u_xxx".into());
            h.insert("accept".into(), "application/vnd.github+json".into());
            h
        },
        expected_body: None,
        response: Response {
            status: 200,
            headers: BTreeMap::new(),
            body: br#"{
                "total_count": 1,
                "installations": [{
                    "id": 77,
                    "account": {"id": 9001, "login": "acme", "type": "Organization"},
                    "repository_selection": "selected",
                    "target_type": "Organization",
                    "target_id": 9001
                }]
            }"#
            .to_vec(),
        },
    }]);
    let resp = client_with(mock).list_user_installations().await.unwrap();
    assert_eq!(resp.total_count, 1);
    assert_eq!(resp.installations[0].id, 77);
    assert_eq!(resp.installations[0].account.login, "acme");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p github list_user_installations_decodes_response -- --nocapture`

Expected: compile failure because `list_user_installations` is not implemented.

- [ ] **Step 3: Add the API method**

Add this method to `impl UserApiClient` after `get_org_membership` and before `list_user_installation_repos`:

```rust
/// `GET /user/installations` — installations of this GitHub App that the
/// signed-in user can access. Used by setup-return handling to verify the
/// untrusted `installation_id` query parameter from GitHub.
#[tracing::instrument(skip(self), fields(method = "list_user_installations"))]
pub async fn list_user_installations(&self) -> Result<GhUserInstallationList> {
    let req = self.auth_request(Method::Get, "/user/installations?per_page=100");
    let resp = self.transport.send(req).await?;
    if !(200..300).contains(&resp.status) {
        tracing::warn!(status = resp.status, "github GET /user/installations returned non-2xx");
    }
    resp.ensure_success()?.json()
}
```

- [ ] **Step 4: Run the test and verify it passes**

Run: `cargo test -p github list_user_installations_decodes_response -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Checkpoint**

Do not commit unless the user explicitly asks for commits. Record that Task 2 is complete before moving on.

## Task 3: Safe Setup Return Paths in Sessions

**Files:**
- Modify: `crates/web/src/session.rs`

- [ ] **Step 1: Add failing validation tests**

Replace the existing `return_to_validation` test in `crates/web/src/session.rs` with:

```rust
#[test]
fn return_to_validation() {
    use super::validate_return_to;
    assert_eq!(
        validate_return_to("/i/AAAAAAAAAAAAAAAA/request"),
        Some("/i/AAAAAAAAAAAAAAAA/request".to_string())
    );
    assert_eq!(
        validate_return_to("/setup/github?installation_id=77&setup_action=install"),
        Some("/setup/github?installation_id=77&setup_action=install".to_string())
    );
    assert_eq!(validate_return_to("/"), None);
    assert_eq!(validate_return_to("/setup"), None);
    assert_eq!(validate_return_to("/setup/github/extra"), None);
    assert_eq!(validate_return_to("https://evil.com"), None);
    assert_eq!(validate_return_to("//evil.com/i/foo"), None);
    assert_eq!(validate_return_to("/i/../../admin"), None);
    assert_eq!(validate_return_to("/setup/github?installation_id=77/../admin"), None);
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p web return_to_validation -- --nocapture`

Expected: setup path assertion fails because only `/i/` is allowed.

- [ ] **Step 3: Update `validate_return_to`**

Replace `validate_return_to` with:

```rust
pub fn validate_return_to(raw: &str) -> Option<String> {
    if raw.starts_with("//") || raw.contains("..") {
        return None;
    }

    if raw.starts_with("/i/") {
        return Some(raw.to_string());
    }

    if raw == "/setup/github" || raw.starts_with("/setup/github?") {
        return Some(raw.to_string());
    }

    None
}
```

- [ ] **Step 4: Run the test and verify it passes**

Run: `cargo test -p web return_to_validation -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Checkpoint**

Do not commit unless the user explicitly asks for commits. Record that Task 3 is complete before moving on.

## Task 4: Setup Route Implementation

**Files:**
- Create: `crates/web/src/routes/setup.rs`
- Modify: `crates/web/src/routes/mod.rs`
- Modify: `crates/web/src/lib.rs`

- [ ] **Step 1: Create the route file with helper tests first**

Create `crates/web/src/routes/setup.rs` with this content:

```rust
//! GitHub App Setup URL return handling.

use crate::error::{Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::Utc;
use domain::{AccountType, SelectedRepos};
use github::payloads::GhUserInstallation;
use github::oauth::UserApiClient;
use serde::Deserialize;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new().route("/setup/github", get(handle_github_setup))
}

#[derive(Debug, Deserialize)]
struct SetupQuery {
    installation_id: Option<u64>,
    setup_action: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupAction {
    Install,
    Update,
}

impl SetupAction {
    fn as_query_value(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
        }
    }
}

async fn handle_github_setup(
    State(state): State<AppState>,
    tower: TowerSession,
    Query(q): Query<SetupQuery>,
) -> Result<Response> {
    let installation_id = q
        .installation_id
        .ok_or_else(|| WebError::BadRequest("missing 'installation_id'".into()))?;
    let action = normalize_setup_action(q.setup_action.as_deref());
    let return_to = setup_return_to(installation_id, action);

    let session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    if !session.is_authenticated() {
        return Ok(login_redirect(&return_to).into_response());
    }

    let user_api = UserApiClient::new(state.github_transport.clone(), session.access_token.clone());
    let installations = user_api.list_user_installations().await?;
    let installation = installations
        .installations
        .into_iter()
        .find(|candidate| candidate.id == installation_id)
        .ok_or_else(|| WebError::OAuth("installation is not visible to signed-in user".into()))?;

    let account_type = account_type_for(&installation)?;
    let selected_repos = selected_repos_for(&user_api, &installation).await?;
    let account_login = installation.account.login.clone();

    match action {
        SetupAction::Install => {
            let input = serde_json::json!({
                "installation_id": installation.id,
                "actor_user_id": session.user_id,
                "account_id": installation.account.id,
                "account_login": account_login.clone(),
                "account_type": account_type,
                "selected_repos": selected_repos,
                "installed_at": Utc::now(),
            });
            state
                .restate
                .call::<_, ()>("Installation", &installation.id.to_string(), "onboard", &input)
                .await?;
        }
        SetupAction::Update => {
            let input = serde_json::json!({
                "installation_id": installation.id,
                "selected_repos": selected_repos,
            });
            state
                .restate
                .call::<_, ()>(
                    "Installation",
                    &installation.id.to_string(),
                    "repos_changed",
                    &input,
                )
                .await?;
        }
    }

    Ok(Redirect::to(&format!("/accounts/{account_login}")).into_response())
}

fn normalize_setup_action(action: Option<&str>) -> SetupAction {
    match action {
        Some("update") => SetupAction::Update,
        _ => SetupAction::Install,
    }
}

fn setup_return_to(installation_id: u64, action: SetupAction) -> String {
    let mut path = format!("/setup/github?installation_id={installation_id}");
    path.push_str("&setup_action=");
    path.push_str(action.as_query_value());
    path
}

fn login_redirect(return_to: &str) -> Redirect {
    let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    Redirect::to(&format!("/login?return_to={encoded}"))
}

fn account_type_for(installation: &GhUserInstallation) -> Result<AccountType> {
    let raw = installation
        .account
        .account_type
        .as_deref()
        .unwrap_or(installation.target_type.as_str());
    raw.parse::<AccountType>()
        .map_err(|_| WebError::BadRequest(format!("unsupported installation account type: {raw}")))
}

async fn selected_repos_for(
    user_api: &UserApiClient,
    installation: &GhUserInstallation,
) -> Result<SelectedRepos> {
    match installation.repository_selection.as_str() {
        "all" => Ok(SelectedRepos::All),
        "selected" => {
            let repos = user_api.list_user_installation_repos(installation.id).await?;
            Ok(SelectedRepos::Subset(
                repos.repositories.into_iter().map(|repo| repo.id).collect(),
            ))
        }
        other => Err(WebError::BadRequest(format!(
            "unsupported repository_selection: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_return_to_preserves_known_action() {
        assert_eq!(
            setup_return_to(77, SetupAction::Install),
            "/setup/github?installation_id=77&setup_action=install"
        );
        assert_eq!(
            setup_return_to(77, SetupAction::Update),
            "/setup/github?installation_id=77&setup_action=update"
        );
    }

    #[test]
    fn normalize_setup_action_defaults_unknown_values_to_install() {
        assert_eq!(normalize_setup_action(Some("install")), SetupAction::Install);
        assert_eq!(normalize_setup_action(Some("update")), SetupAction::Update);
        assert_eq!(
            normalize_setup_action(Some("install&return_to=//evil.test")),
            SetupAction::Install
        );
        assert_eq!(normalize_setup_action(None), SetupAction::Install);
    }

    #[test]
    fn login_redirect_url_encodes_return_to() {
        let redirect = login_redirect("/setup/github?installation_id=77&setup_action=install")
            .into_response();
        let location = redirect.headers().get("location").unwrap().to_str().unwrap();
        assert_eq!(
            location,
            "/login?return_to=%2Fsetup%2Fgithub%3Finstallation_id%3D77%26setup_action%3Dinstall"
        );
    }
}
```

- [ ] **Step 2: Run setup helper tests and verify route is not registered yet**

Run: `cargo test -p web setup_return_to -- --nocapture`

Expected: no setup helper tests run yet because `routes::setup` is not exposed. This confirms the file exists but is not part of the crate until the next step.

- [ ] **Step 3: Register the module**

Add this line to `crates/web/src/routes/mod.rs`:

```rust
pub mod setup;
```

Add this line to `build_app()` in `crates/web/src/lib.rs` after `routes::oauth::router()`:

```rust
        .merge(routes::setup::router())
```

- [ ] **Step 4: Run helper tests and route compilation**

Run: `cargo test -p web setup_ -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Checkpoint**

Do not commit unless the user explicitly asks for commits. Record that Task 4 is complete before moving on.

## Task 5: Setup Route Integration Tests

**Files:**
- Create: `crates/web/tests/setup_flow.rs`
- Modify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Add setup flow integration tests**

Create `crates/web/tests/setup_flow.rs`:

```rust
use axum::body::Body;
use axum::extract::Path;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use github::mocks::{Expectation, MockTransport};
use github::transport::{Method, Response};
use http_body_util::BodyExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use web::{AppState, RestateClient, WebConfig, build_app};

async fn spawn_restate_recorder() -> (String, Arc<Mutex<Vec<Value>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_route = calls.clone();
    let app = axum::Router::new().route(
        "/Installation/{id}/{method}",
        post(move |Path((id, method)): Path<(String, String)>, axum::Json(body): axum::Json<Value>| {
            let calls = calls_for_route.clone();
            async move {
                calls.lock().unwrap().push(serde_json::json!({
                    "id": id,
                    "method": method,
                    "body": body,
                }));
                axum::Json(Value::Null).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), calls)
}

async fn build_app_with(mock: MockTransport, restate_base: &str) -> axum::Router {
    let storage: Arc<dyn storage::Storage> =
        Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let transport: Arc<dyn github::HttpTransport> = Arc::new(mock);
    let restate = Arc::new(RestateClient::new(restate_base).unwrap());
    let state = AppState::new(storage, transport, restate, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

fn session_cookie(resp: &axum::response::Response, fallback: Option<String>) -> String {
    resp.headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .or(fallback)
        .expect("session cookie available")
}

fn state_from_location(location: &str) -> &str {
    location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
}

async fn sign_in(app: axum::Router) -> (axum::Router, String) {
    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie1 = session_cookie(&resp1, None);
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state = state_from_location(location);

    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state}"))
                .header("cookie", &cookie1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
    let cookie2 = session_cookie(&resp2, Some(cookie1));
    (app, cookie2)
}

#[tokio::test]
async fn setup_unauthenticated_redirects_to_login_with_return_to() {
    let (restate_base, _calls) = spawn_restate_recorder().await;
    let app = build_app_with(MockTransport::scripted(vec![]), &restate_base).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=77&setup_action=install")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        "/login?return_to=%2Fsetup%2Fgithub%3Finstallation_id%3D77%26setup_action%3Dinstall"
    );
}

#[tokio::test]
async fn setup_rejects_spoofed_installation_id() {
    let (restate_base, calls) = spawn_restate_recorder().await;
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user read:org"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/installations?per_page=100",
            serde_json::json!({
                "total_count": 1,
                "installations": [{
                    "id": 77,
                    "account": {"id": 9001, "login": "acme", "type": "Organization"},
                    "repository_selection": "all",
                    "target_type": "Organization",
                    "target_id": 9001
                }]
            }),
        ),
    ]);
    let app = build_app_with(mock, &restate_base).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=999&setup_action=install")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn setup_verified_org_install_calls_onboard_and_redirects() {
    let (restate_base, calls) = spawn_restate_recorder().await;
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user read:org"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/installations?per_page=100",
            serde_json::json!({
                "total_count": 1,
                "installations": [{
                    "id": 77,
                    "account": {"id": 9001, "login": "acme", "type": "Organization"},
                    "repository_selection": "selected",
                    "target_type": "Organization",
                    "target_id": 9001
                }]
            }),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/installations/77/repositories?per_page=100",
            serde_json::json!({
                "total_count": 2,
                "repositories": [
                    {"id": 10, "full_name": "acme/api", "private": true},
                    {"id": 11, "full_name": "acme/web", "private": false}
                ]
            }),
        ),
    ]);
    let app = build_app_with(mock, &restate_base).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=77&setup_action=install")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/accounts/acme"
    );

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "77");
    assert_eq!(calls[0]["method"], "onboard");
    assert_eq!(calls[0]["body"]["installation_id"], 77);
    assert_eq!(calls[0]["body"]["actor_user_id"], 42);
    assert_eq!(calls[0]["body"]["account_id"], 9001);
    assert_eq!(calls[0]["body"]["account_login"], "acme");
    assert_eq!(calls[0]["body"]["account_type"], "Organization");
    assert_eq!(calls[0]["body"]["selected_repos"], serde_json::json!([10, 11]));
}

#[tokio::test]
async fn setup_verified_user_install_calls_onboard_and_redirects() {
    let (restate_base, calls) = spawn_restate_recorder().await;
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user read:org"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/installations?per_page=100",
            serde_json::json!({
                "total_count": 1,
                "installations": [{
                    "id": 88,
                    "account": {"id": 42, "login": "octocat", "type": "User"},
                    "repository_selection": "all",
                    "target_type": "User",
                    "target_id": 42
                }]
            }),
        ),
    ]);
    let app = build_app_with(mock, &restate_base).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=88&setup_action=install")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/accounts/octocat"
    );

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "88");
    assert_eq!(calls[0]["method"], "onboard");
    assert_eq!(calls[0]["body"]["account_type"], "User");
    assert_eq!(calls[0]["body"]["selected_repos"], "all");
}

#[tokio::test]
async fn setup_update_calls_repos_changed_and_redirects() {
    let (restate_base, calls) = spawn_restate_recorder().await;
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user read:org"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/installations?per_page=100",
            serde_json::json!({
                "total_count": 1,
                "installations": [{
                    "id": 77,
                    "account": {"id": 9001, "login": "acme", "type": "Organization"},
                    "repository_selection": "selected",
                    "target_type": "Organization",
                    "target_id": 9001
                }]
            }),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user/installations/77/repositories?per_page=100",
            serde_json::json!({
                "total_count": 1,
                "repositories": [
                    {"id": 12, "full_name": "acme/new", "private": true}
                ]
            }),
        ),
    ]);
    let app = build_app_with(mock, &restate_base).await;
    let (app, cookie) = sign_in(app).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github?installation_id=77&setup_action=update")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/accounts/acme"
    );

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "77");
    assert_eq!(calls[0]["method"], "repos_changed");
    assert_eq!(calls[0]["body"]["installation_id"], 77);
    assert_eq!(calls[0]["body"]["selected_repos"], serde_json::json!([12]));
}
```

- [ ] **Step 2: Add a smoke test for missing query**

Append this test to `crates/web/tests/route_smoke.rs`:

```rust
#[tokio::test]
async fn setup_route_requires_installation_id() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/setup/github")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("installation_id"));
}
```

- [ ] **Step 3: Run the setup tests and fix compile issues**

Run: `cargo test -p web setup_ -- --nocapture`

Expected: all setup tests pass. If a compile error shows a missing import, add the exact import named in the compiler output and rerun the same command.

- [ ] **Step 4: Checkpoint**

Do not commit unless the user explicitly asks for commits. Record that Task 5 is complete before moving on.

## Task 6: Webhook Reconciliation and Stale Log Wording

**Files:**
- Modify: `crates/web/src/routes/webhook.rs`
- Modify: `crates/web/src/routes/oauth.rs`

- [ ] **Step 1: Add failing helper tests for repository webhook repo selection**

In `crates/web/src/routes/webhook.rs`, add pure helper tests that prove `installation_repositories` sends the same `SelectedRepos` wire format Restate expects:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use domain::SelectedRepos;

    #[test]
    fn repository_selection_webhook_all_maps_to_all() {
        let payload = serde_json::json!({
            "repository_selection": "all",
            "repositories_added": [],
            "repositories_removed": []
        });
        let current = SelectedRepos::Subset(vec![10, 11]);
        assert_eq!(
            selected_repos_from_repository_event(&payload, &current).unwrap(),
            SelectedRepos::All
        );
    }

    #[test]
    fn repository_selection_webhook_applies_selected_delta() {
        let payload = serde_json::json!({
            "repository_selection": "selected",
            "repositories_added": [{"id": 12}],
            "repositories_removed": [{"id": 10}]
        });
        let current = SelectedRepos::Subset(vec![10, 11]);
        assert_eq!(
            selected_repos_from_repository_event(&payload, &current).unwrap(),
            SelectedRepos::Subset(vec![11, 12])
        );
    }
}
```

- [ ] **Step 2: Run the webhook helper tests and verify they fail**

Run: `cargo test -p web repository_selection_webhook -- --nocapture`

Expected: compile failure because `selected_repos_from_repository_event` does not exist.

- [ ] **Step 3: Add the webhook reconciliation helper**

In `crates/web/src/routes/webhook.rs`, import `domain::SelectedRepos` and `std::collections::BTreeSet`, then add this helper near `handle_installation_repositories`:

```rust
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
    current: &SelectedRepos,
) -> Result<SelectedRepos, String> {
    match payload["repository_selection"].as_str().unwrap_or("selected") {
        "all" => Ok(SelectedRepos::All),
        "selected" => {
            let mut ids: BTreeSet<u64> = match current {
                SelectedRepos::All => {
                    return Err(
                        "cannot derive selected repository subset from prior all-repos state"
                            .into(),
                    );
                }
                SelectedRepos::Subset(existing) => existing.iter().copied().collect(),
            };
            for id in repository_ids(&payload["repositories_removed"]) {
                ids.remove(&id);
            }
            for id in repository_ids(&payload["repositories_added"]) {
                ids.insert(id);
            }
            Ok(SelectedRepos::Subset(ids.into_iter().collect()))
        }
        other => Err(format!("unsupported repository_selection: {other}")),
    }
}
```

Then update `handle_installation_repositories` to read the current installation and send the full `SelectedRepos` shape:

```rust
let current = state
    .storage
    .get_installation(installation_id)
    .await
    .map_err(|e| format!("storage lookup: {e}"))?
    .ok_or_else(|| format!("installation_repositories: unknown installation {installation_id}"))?;
let selected_repos = selected_repos_from_repository_event(payload, &current.selected_repos)?;
let input = serde_json::json!({
    "installation_id": installation_id,
    "selected_repos": selected_repos,
});
```

- [ ] **Step 4: Update stale log wording**

In `crates/web/src/routes/oauth.rs`, replace the callback installation log with wording that does not imply webhook onboarding is expected:

```rust
tracing::info!(
    installation_id,
    "OAuth callback carried installation_id; setup URL handles verified onboarding"
);
```

In `crates/web/src/routes/webhook.rs`, replace the `installation.created` log comment/message with:

```rust
// Setup URL is the primary onboarding path; installation.created is only a reconciliation signal for now.
tracing::debug!(
    installation_id,
    delivery = delivery_id,
    "installation.created observed; setup URL handles verified onboarding"
);
```

- [ ] **Step 5: Run webhook and OAuth smoke tests**

Run these valid single-filter commands:

```bash
cargo test -p web repository_selection_webhook -- --nocapture
cargo test -p web signin_with_installation_id_logs_and_proceeds -- --nocapture
cargo test -p web webhook_valid_hmac_returns_ok -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Checkpoint**

Do not commit unless the user explicitly asks for commits. Record that Task 6 is complete before moving on.

## Task 7: Documentation Corrections

**Files:**
- Modify: `README.md`
- Modify: `docs/deploy.md`

- [ ] **Step 1: Update README GitHub App setup**

In `README.md`, replace lines 27-36 under `## GitHub App setup` with:

```markdown
1. Go to **GitHub → Settings → Developer settings → GitHub Apps → New GitHub App**
2. Fill in:
   - **Homepage URL**: `http://127.0.0.1:8787` (or your public URL)
   - **Callback URL**: `http://127.0.0.1:8787/oauth/callback`
   - **Setup URL**: `http://127.0.0.1:8787/setup/github` (or `{GHINVITE_BASE_URL}/setup/github` in production)
   - Enable **Redirect on update** so repository-selection changes return to ghinvite
   - **Webhook URL**: `http://<public-url>/webhooks/github` (use [ngrok](https://ngrok.com) for local dev)
   - **Webhook secret**: any random string — set it as `GHINVITE_WEBHOOK_SECRET`
3. **Repository permissions**: Administration → Read & write, Metadata → Read-only
4. **Subscribe to events**: `Member` and `Repository invitation` if available for your selected permissions. GitHub sends `installation` and `installation_repositories` to GitHub Apps by default; they are app-level events and may not appear as repository-level checkboxes.
5. After creating: note the **App ID** (`GHINVITE_GITHUB_APP_ID`) and generate a **private key** (`.pem` file, `GHINVITE_GITHUB_APP_PRIVATE_KEY_FILE`)
6. Under **OAuth** in the app settings: note the **Client ID** and generate a **Client secret** (`GHINVITE_GITHUB_CLIENT_ID` / `GHINVITE_GITHUB_CLIENT_SECRET`)
7. Install the app on your org or personal account and note the install URL: `https://github.com/apps/<your-app-name>/installations/new`
```

- [ ] **Step 2: Update deploy docs GitHub App setup**

In `docs/deploy.md`, replace the `## GitHub App Setup` numbered list with:

```markdown
1. Create a GitHub App at https://github.com/settings/apps/new
2. Set **Homepage URL** to your `GHINVITE_BASE_URL`
3. Set **Callback URL** to `{GHINVITE_BASE_URL}/oauth/callback`
4. Set **Setup URL** to `{GHINVITE_BASE_URL}/setup/github`
5. Enable **Redirect on update**
6. Set **Webhook URL** to `{GHINVITE_BASE_URL}/webhooks/github`
7. Set repository permissions: **Administration** → **Read & write**, **Metadata** → **Read-only**
8. Subscribe to `Member` and `Repository invitation` if GitHub shows them for the selected permissions. `installation` and `installation_repositories` are app-level GitHub App events that GitHub sends by default.
9. Generate and download a private key (used for `GHINVITE_GITHUB_APP_PRIVATE_KEY`)
10. Note the **App ID** (used for `GHINVITE_GITHUB_APP_ID`)
11. Note the **Client ID** and generate a **Client Secret** (used for OAuth vars)
12. Generate a **Webhook Secret** (used for `GHINVITE_WEBHOOK_SECRET`)
```

- [ ] **Step 3: Add setup troubleshooting docs**

Add this section to `README.md` after the GitHub App setup list, and add the production URL variants to `docs/deploy.md` after `## GitHub App Setup`:

```markdown
### Troubleshooting GitHub App setup

- **No redirect after install**: confirm the GitHub App **Setup URL** is `{GHINVITE_BASE_URL}/setup/github` and **Redirect on update** is enabled.
- **`missing installation_id`**: GitHub did not return through the Setup URL. Recheck the Setup URL and install the app from the app installation URL again.
- **`installation is not visible to signed-in user`**: sign out of ghinvite, sign in with the GitHub user that installed or can administer the app installation, then retry the GitHub App install/update.
- **`Restate error` or setup returns 502**: make sure Restate is running, the `Installation` service is registered, and `GHINVITE_RESTATE_INGRESS` points at the active Restate ingress.
- **Repository picker is empty after a selected-repository install**: reopen the GitHub App installation settings, verify repository access, and use **Update** so GitHub redirects back to ghinvite with `setup_action=update`.
- **Permission failures when inviting collaborators**: confirm repository permissions are **Administration: Read & write** and **Metadata: Read-only**.
```

- [ ] **Step 4: Check docs for stale permission text**

Run: `rg "Members →|Members ->|Repository permissions.*Members|installation settings page" README.md docs/deploy.md docs/superpowers/specs/2026-05-05-github-app-installation-onboarding-design.md`

Expected: no stale `Members` repository-permission instruction remains.

- [ ] **Step 5: Checkpoint**

Do not commit unless the user explicitly asks for commits. Record that Task 7 is complete before moving on.

## Task 8: Final Verification

**Files:**
- No source edits unless verification exposes a bug in files touched by earlier tasks.

- [ ] **Step 1: Format Rust**

Run: `cargo fmt --all --check`

Expected: PASS. If it fails, run `cargo fmt --all`, then rerun `cargo fmt --all --check`.

- [ ] **Step 2: Run focused GitHub tests**

Run:

```bash
cargo test -p github user_installation -- --nocapture
cargo test -p github list_user_installations -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Run focused web tests**

Run:

```bash
cargo test -p web setup_ -- --nocapture
cargo test -p web return_to_validation -- --nocapture
cargo test -p web repository_selection_webhook -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Run broader workspace tests**

Run: `cargo test --workspace --exclude github-stub`

Expected: PASS.

- [ ] **Step 5: Manual install-flow check**

Run the app locally with real GitHub App config, install the app on an org, and confirm GitHub redirects to `/setup/github?installation_id=...&setup_action=install`. After setup completes, the browser should land on `/accounts/{org}` and the New Link form should show repositories.

Then change the GitHub App repository selection, confirm GitHub redirects to `/setup/github?installation_id=...&setup_action=update`, and confirm the New Link repository picker reflects the changed repository set.

- [ ] **Step 6: Checkpoint any verification fixes**

If verification required changes, do not commit unless the user explicitly asks for commits. Record the changed files and verification commands that passed.

## GSTACK REVIEW REPORT

| Phase | Reviewer | Status | Key Findings Applied |
| --- | --- | --- | --- |
| CEO | Claude subagent | DONE_WITH_CONCERNS | Fixed `setup_action=update` no-op risk; added personal-account coverage; kept pagination and webhook-created backup as out of scope. |
| Design | Claude subagent | DONE_WITH_CONCERNS | Documented deferred non-admin post-install page; added setup troubleshooting docs instead of raw-only recovery guidance. |
| Engineering | Claude subagent | DONE_WITH_CONCERNS | Fixed invalid Cargo commands; added webhook `SelectedRepos` reconciliation task; removed task-level commit instructions. |
| DX | Claude subagent | DONE_WITH_CONCERNS | Added troubleshooting docs, update-flow manual verification, and stale OAuth/webhook log wording updates. |
| Codex | CLI preflight | UNAVAILABLE | Codex binary was not installed; autoplan proceeded with Claude subagents only. |

## Out of Scope

- Full pagination for `GET /user/installations` and repository lists. Existing code already uses page-1-only behavior for installation repository lists.
- App-JWT reconciliation using `GET /app/installations`. This is a good follow-up for missed webhooks and stale installs.
- Making `installation.created` a full backup onboarding path. The Setup URL path is primary and verified against the signed-in user.
- A branded post-install page for users who can see an org installation but are not GitHub org admins. Current dashboard authorization still controls access after setup.
