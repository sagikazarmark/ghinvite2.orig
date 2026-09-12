//! End-to-end OAuth sign-in tests. Drives the full /login → /oauth/callback
//! flow through axum + the in-process MockTransport from Plan 2.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghinvite_core::{Account, AccountType, SelectedRepos};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::{AppState, RestateClient, RestateCommands, WebConfig, build_app};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tower::ServiceExt;
use tower_sessions::{
    SessionStore,
    session::{Id, Record},
    session_store,
};

async fn build_app_with_mock(mock: MockTransport) -> axum::Router {
    build_app_with_store(mock, tower_sessions::MemoryStore::default()).await
}

async fn build_app_with_store<S: SessionStore + Clone + 'static>(
    mock: MockTransport,
    session_store: S,
) -> axum::Router {
    let storage: Arc<dyn ghinvite_core::storage::Storage> = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransport> = Arc::new(mock);
    storage
        .insert_installation(&Account {
            installation_id: 77,
            account_id: 42,
            account_login: "octocat".into(),
            account_type: AccountType::User,
            installed_at: chrono::Utc::now(),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .await
        .unwrap();
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    build_app(state, session_store)
}

#[tokio::test]
async fn signin_happy_path() {
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#
                    .to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]);
    let app = build_app_with_mock(mock).await;

    // Step 1: hit /login, capture the cookie + state.
    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .expect("session cookie set")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state_param = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    // Step 2: hit /oauth/callback with the same cookie and the captured state.
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/oauth/callback?code=test-code&state={state_param}"
                ))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
    let dest = resp2.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(dest, "/");

    let new_cookie = session_cookie(&resp2);
    assert_ne!(
        cookie, new_cookie,
        "authentication must rotate the session ID"
    );
    let authorized = get(&app, "/console/accounts/octocat", &new_cookie).await;
    assert_eq!(authorized.status(), StatusCode::OK);
    let old_session = get(&app, "/console/accounts/octocat", &cookie).await;
    assert_eq!(old_session.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        old_session.headers()["location"],
        "/login?return_to=%2Fconsole%2Faccounts%2Foctocat"
    );
    let replay = get(
        &app,
        &format!("/oauth/callback?code=test-code&state={state_param}"),
        &new_cookie,
    )
    .await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
}

async fn get(app: &axum::Router, path: &str, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn session_cookie(response: &axum::response::Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn begin_login(app: &axum::Router, path: &str) -> (String, String) {
    let response = get(app, path, "").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    (session_cookie(&response), state)
}

fn signin_expectations() -> Vec<Expectation> {
    vec![
        Expectation::ok_json(
            Method::Post,
            "https://github.com/login/oauth/access_token",
            serde_json::json!({"access_token": "u_xxx", "token_type": "bearer", "scope": "read:user"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]
}

#[tokio::test]
async fn github_failure_consumes_state_without_authenticating_or_rotating() {
    for fail_user_lookup in [false, true] {
        let expectations = if fail_user_lookup {
            let mut expectations = signin_expectations();
            expectations[1] = Expectation::status(Method::Get, "https://api.github.com/user", 503);
            expectations
        } else {
            vec![Expectation::status(
                Method::Post,
                "https://github.com/login/oauth/access_token",
                503,
            )]
        };
        let mock = MockTransport::scripted(expectations);
        let app = build_app_with_mock(mock.clone()).await;
        let (cookie, state) = begin_login(&app, "/login").await;
        let callback = format!("/oauth/callback?code=test-code&state={state}");
        let response = get(&app, &callback, &cookie).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!response.headers().contains_key("set-cookie"));
        assert!(!response.headers().contains_key("location"));
        let protected = get(&app, "/console/accounts/octocat", &cookie).await;
        assert_eq!(protected.status(), StatusCode::SEE_OTHER);
        let replay = get(&app, &callback, &cookie).await;
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
        mock.assert_exhausted();
    }
}

#[derive(Clone, Debug, Default)]
struct FailingStore {
    inner: tower_sessions::MemoryStore,
    failure: Arc<AtomicU8>,
}

#[derive(Clone, Copy)]
enum StoreOperation {
    Save = 1,
    Delete = 2,
    Create = 3,
}

impl FailingStore {
    fn check(&self, operation: StoreOperation) -> session_store::Result<()> {
        if self.failure.load(Ordering::SeqCst) == operation as u8 {
            Err(session_store::Error::Backend(
                "injected session failure".into(),
            ))
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl SessionStore for FailingStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        self.check(StoreOperation::Create)?;
        self.inner.create(record).await
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        self.check(StoreOperation::Save)?;
        self.inner.save(record).await
    }

    async fn load(&self, id: &Id) -> session_store::Result<Option<Record>> {
        self.inner.load(id).await
    }

    async fn delete(&self, id: &Id) -> session_store::Result<()> {
        self.check(StoreOperation::Delete)?;
        self.inner.delete(id).await
    }
}

#[tokio::test]
async fn session_write_or_rotation_failure_never_reports_success_or_authenticates_old_cookie() {
    for operation in [
        StoreOperation::Save,
        StoreOperation::Delete,
        StoreOperation::Create,
    ] {
        let store = FailingStore::default();
        let expectations = if matches!(operation, StoreOperation::Save) {
            vec![] // Consuming state must be durable before exchanging credentials.
        } else {
            signin_expectations()
        };
        let mock = MockTransport::scripted(expectations);
        let app = build_app_with_store(mock.clone(), store.clone()).await;
        let (cookie, state) = begin_login(&app, "/login").await;
        store.failure.store(operation as u8, Ordering::SeqCst);
        let callback = format!("/oauth/callback?code=test-code&state={state}");
        let response = get(&app, &callback, &cookie).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!response.headers().contains_key("location"));
        assert!(!response.headers().contains_key("set-cookie"));
        store.failure.store(0, Ordering::SeqCst);
        let protected = get(&app, "/console/accounts/octocat", &cookie).await;
        assert_eq!(protected.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            protected.headers()["location"],
            "/login?return_to=%2Fconsole%2Faccounts%2Foctocat"
        );
        if !matches!(operation, StoreOperation::Save) {
            let replay = get(&app, &callback, &cookie).await;
            assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
        }
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn sqlite_rotation_preserves_only_valid_return_destinations() {
    for (return_to, destination) in [
        ("/i/AAAAAAAAAAAAAAAA/request", "/i/AAAAAAAAAAAAAAAA/request"),
        ("/console/accounts/octocat", "/console/accounts/octocat"),
        (
            "/setup/github?installation_id=77&setup_action=install",
            "/setup/github?installation_id=77&setup_action=install",
        ),
        ("https://evil.example", "/"),
        ("//evil.example/console", "/"),
    ] {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = tower_sessions_sqlx_store::SqliteStore::new(pool);
        store.migrate().await.unwrap();
        let app = build_app_with_store(MockTransport::scripted(signin_expectations()), store).await;
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("return_to", return_to)
            .finish();
        let (old_cookie, state) = begin_login(&app, &format!("/login?{query}")).await;
        let response = get(
            &app,
            &format!("/oauth/callback?code=test-code&state={state}"),
            &old_cookie,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], destination);
        let new_cookie = session_cookie(&response);
        assert_ne!(new_cookie, old_cookie);
        let authorized = get(&app, "/console/accounts/octocat", &new_cookie).await;
        assert_eq!(authorized.status(), StatusCode::OK);
        let unauthorized = get(&app, "/console/accounts/octocat", &old_cookie).await;
        assert_eq!(unauthorized.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            unauthorized.headers()["location"],
            "/login?return_to=%2Fconsole%2Faccounts%2Foctocat"
        );
    }
}

#[tokio::test]
async fn rejected_callback_preserves_state_and_does_not_authenticate_or_rotate() {
    let mock = MockTransport::scripted(signin_expectations());
    let app = build_app_with_mock(mock.clone()).await;
    let (cookie, state) = begin_login(&app, "/login").await;
    for callback in [
        "/oauth/callback?error=access_denied&error_description=Cancelled",
        "/oauth/callback?code=test-code",
        "/oauth/callback?state=anything",
        "/oauth/callback?code=test-code&state=wrong",
    ] {
        let response = get(&app, callback, &cookie).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!response.headers().contains_key("set-cookie"));
        let protected = get(&app, "/console/accounts/octocat", &cookie).await;
        assert_eq!(protected.status(), StatusCode::SEE_OTHER);
    }
    let response = get(
        &app,
        &format!("/oauth/callback?code=test-code&state={state}"),
        &cookie,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_ne!(session_cookie(&response), cookie);
    mock.assert_exhausted();
}

#[tokio::test]
async fn callback_csrf_mismatch_rejects() {
    let mock = MockTransport::scripted(vec![]); // no GitHub calls expected
    let app = build_app_with_mock(mock).await;
    // Hit /login first to create a session with a real CSRF.
    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    // Then send a wrong state.
    let resp2 = app
        .oneshot(
            Request::builder()
                .uri("/oauth/callback?code=any&state=wrong-csrf")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
    let body = resp2.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("CSRF"));
}

#[tokio::test]
async fn signin_with_installation_id_logs_and_proceeds() {
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#
                    .to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]);
    let app = build_app_with_mock(mock).await;

    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state_param = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let resp2 = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/oauth/callback?code=test-code&state={state_param}&installation_id=99&setup_action=install"
                ))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
}
