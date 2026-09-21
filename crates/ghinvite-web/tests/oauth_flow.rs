//! End-to-end OAuth sign-in tests. Drives the full /login → /oauth/callback
//! flow through axum + the in-process MockTransport.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghinvite_core::storage::InstallationStorage;
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
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = ghinvite_web::session_store::SqliteBackend::new(pool);
    backend.migrate().await.unwrap();
    build_app_with_store(
        mock,
        ghinvite_web::session_store::ProtectedStore::new(backend, [7; 32]),
    )
    .await
}

async fn build_app_with_store<S: SessionStore + Clone + 'static>(
    mock: MockTransport,
    session_store: S,
) -> axum::Router {
    let storage = Arc::new(
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
    let state = AppState::new(
        storage,
        transport,
        commands,
        std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
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

#[tokio::test]
async fn key_rotation_rejects_old_cookies_and_allows_fresh_login_without_destructive_overlap() {
    use ghinvite_web::session_store::{ProtectedStore, SqliteBackend};
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool.clone());
    backend.migrate().await.unwrap();
    let old_app = build_app_with_store(
        MockTransport::scripted(signin_expectations()),
        ProtectedStore::new(backend.clone(), [7; 32]),
    )
    .await;
    let (anonymous, state) = begin_login(&old_app, "/login").await;
    let response = get(
        &old_app,
        &format!("/oauth/callback?code=test&state={state}"),
        &anonymous,
    )
    .await;
    let old_cookie = session_cookie(&response);
    assert_eq!(
        get(&old_app, "/console/accounts/octocat", &old_cookie)
            .await
            .status(),
        StatusCode::OK
    );

    let new_app = build_app_with_store(
        MockTransport::scripted(signin_expectations()),
        ProtectedStore::new(backend, [8; 32]),
    )
    .await;
    assert_eq!(
        get(&new_app, "/console/accounts/octocat", &old_cookie)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let (anonymous, state) = begin_login(&new_app, "/login").await;
    let response = get(
        &new_app,
        &format!("/oauth/callback?code=test&state={state}"),
        &anonymous,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    // Successful OAuth must replace the anonymous 30-minute cookie deadline.
    let parsed =
        tower_sessions::cookie::Cookie::parse(response.headers()["set-cookie"].to_str().unwrap())
            .unwrap();
    assert!(parsed.max_age().unwrap().whole_seconds() > 29 * 24 * 60 * 60);
    let new_cookie = session_cookie(&response);
    assert_eq!(
        get(&new_app, "/console/accounts/octocat", &new_cookie)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&old_app, "/console/accounts/octocat", &new_cookie)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        get(&new_app, "/console/accounts/octocat", &new_cookie)
            .await
            .status(),
        StatusCode::OK
    );
    // Demonstrates why the runbook requires draining old-key deployments.
    assert_eq!(
        get(&old_app, "/console/accounts/octocat", &old_cookie)
            .await
            .status(),
        StatusCode::OK
    );
    let records: Vec<Vec<u8>> = sqlx::query_scalar("SELECT payload FROM protected_sessions")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        records
            .iter()
            .all(|raw| !raw.windows(5).any(|part| part == b"u_xxx"))
    );
}

#[tokio::test]
async fn failed_logout_clears_cookie_but_reports_unconfirmed_server_signout() {
    for operation in [StoreOperation::Delete, StoreOperation::Load] {
        let store = FailingStore::default();
        let app = build_app_with_store(
            MockTransport::scripted(signin_expectations()),
            store.clone(),
        )
        .await;
        let (anonymous, state) = begin_login(&app, "/login").await;
        let response = get(
            &app,
            &format!("/oauth/callback?code=test&state={state}"),
            &anonymous,
        )
        .await;
        let cookie = session_cookie(&response);
        let token = common::csrf_token(&app, &cookie).await;
        store.failure.store(operation as u8, Ordering::SeqCst);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/logout")
                    .header("cookie", &cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf_token={token}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        assert!(!response.headers().contains_key("location"));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("server sign-out could not be confirmed"));
        assert!(!text.contains("injected session failure"));
        store.failure.store(0, Ordering::SeqCst);
        // No incidental empty-record save is allowed to masquerade as revocation.
        assert_eq!(
            get(&app, "/console/accounts/octocat", &cookie)
                .await
                .status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn unchanged_console_reads_do_not_write_or_refresh_the_session() {
    let store = FailingStore::default();
    let app = build_app_with_store(
        MockTransport::scripted(signin_expectations()),
        store.clone(),
    )
    .await;
    let (anonymous, state) = begin_login(&app, "/login").await;
    let response = get(
        &app,
        &format!("/oauth/callback?code=test&state={state}"),
        &anonymous,
    )
    .await;
    let cookie = session_cookie(&response);
    store
        .failure
        .store(StoreOperation::Save as u8, Ordering::SeqCst);
    for _ in 0..3 {
        let response = get(&app, "/console/accounts/octocat", &cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key("set-cookie"));
    }
}

#[tokio::test]
async fn aged_authenticated_deadlines_survive_requests_but_fresh_signin_renews_them() {
    use ghinvite_web::session_store::{ProtectedStore, SqliteBackend};
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool);
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend, [7; 32]);
    let app = build_app_with_store(
        MockTransport::scripted(signin_expectations().into_iter().cycle().take(4).collect()),
        store.clone(),
    )
    .await;
    let (anonymous, state) = begin_login(&app, "/login").await;
    let response = get(
        &app,
        &format!("/oauth/callback?code=test&state={state}"),
        &anonymous,
    )
    .await;
    let cookie = session_cookie(&response);
    let id: Id = cookie.strip_prefix("id=").unwrap().parse().unwrap();
    let mut record = store.load(&id).await.unwrap().unwrap();
    // Seed an authentic 29-day-old record through the trusted store interface.
    let issued = chrono::Utc::now().timestamp() - 29 * 86400;
    let deadline = issued + 30 * 86400;
    record.data.insert(
        "ghinvite_lifetime".into(),
        serde_json::json!({
            "issued_at": issued, "deadline": deadline, "authenticated": true
        }),
    );
    store.save(&record).await.unwrap();
    assert_eq!(
        get(&app, "/console/accounts/octocat", &cookie)
            .await
            .status(),
        StatusCode::OK
    );
    let login = get(&app, "/login", &cookie).await;
    assert_eq!(login.status(), StatusCode::SEE_OTHER);
    let unchanged = store.load(&id).await.unwrap().unwrap();
    assert_eq!(unchanged.expiry_date.unix_timestamp(), deadline);
    assert_eq!(unchanged.data["ghinvite_lifetime"]["deadline"], deadline);
    let location = url::Url::parse(login.headers()["location"].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let response = get(
        &app,
        &format!("/oauth/callback?code=test&state={state}"),
        &cookie,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let new_cookie = session_cookie(&response);
    let new_id: Id = new_cookie.strip_prefix("id=").unwrap().parse().unwrap();
    assert_ne!(id, new_id);
    let renewed = store.load(&new_id).await.unwrap().unwrap();
    assert!(renewed.expiry_date.unix_timestamp() > chrono::Utc::now().timestamp() + 29 * 86400);
    assert!(store.load(&id).await.unwrap().is_none());
}

#[tokio::test]
async fn unavailable_session_storage_is_a_generic_error_not_anonymous_success() {
    let store = FailingStore::default();
    let app = build_app_with_store(MockTransport::scripted(vec![]), store.clone()).await;
    let (cookie, _) = begin_login(&app, "/login").await;
    store
        .failure
        .store(StoreOperation::Load as u8, Ordering::SeqCst);
    for path in [
        "/",
        "/login",
        "/console",
        "/i/AAAAAAAAAAAAAAAA",
        "/missing-page",
    ] {
        let response = get(&app, path, &cookie).await;
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{path}"
        );
        assert!(!response.headers().contains_key("location"));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&body).contains("injected session failure"));
    }
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
async fn logout_requires_a_session_bound_native_form_token() {
    let app = build_app_with_mock(MockTransport::scripted(signin_expectations())).await;
    let (cookie, state) = begin_login(&app, "/login").await;
    let signed_in = get(
        &app,
        &format!("/oauth/callback?code=test&state={state}"),
        &cookie,
    )
    .await;
    let cookie = session_cookie(&signed_in);

    // Even a same-site sibling can send the victim's Lax cookie on a POST.
    for body in ["", "csrf_token=incorrect"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/logout")
                    .header("cookie", &cookie)
                    .header("origin", "https://evil.ghinvite.test")
                    .header("sec-fetch-site", "same-site")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            get(&app, "/console/accounts/octocat", &cookie)
                .await
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        get(&app, "/logout", &cookie).await.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );

    let home = get(&app, "/", &cookie).await;
    assert_eq!(home.headers()["cache-control"], "private, no-store");
    let html = String::from_utf8(
        home.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("action=\"/logout\""));
    let token = html
        .split("name=\"csrf_token\" value=\"")
        .nth(1)
        .expect("native CSRF field")
        .split('"')
        .next()
        .unwrap();
    assert!(token.len() >= 32);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/logout")
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={token}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/");
    assert_eq!(
        get(&app, "/console/accounts/octocat", &cookie)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn browser_tokens_are_stable_session_separated_and_replaced_on_signin() {
    let app = build_app_with_mock(MockTransport::scripted(
        signin_expectations().into_iter().cycle().take(6).collect(),
    ))
    .await;
    let mut cookies = Vec::new();
    for _ in 0..2 {
        let (cookie, state) = begin_login(&app, "/login").await;
        let response = get(
            &app,
            &format!("/oauth/callback?code=test&state={state}"),
            &cookie,
        )
        .await;
        cookies.push(session_cookie(&response));
    }
    let first = common::csrf_token(&app, &cookies[0]).await;
    assert_eq!(first, common::csrf_token(&app, &cookies[0]).await);
    assert_ne!(first, common::csrf_token(&app, &cookies[1]).await);
    let login = get(&app, "/login", &cookies[0]).await;
    let location = url::Url::parse(login.headers()["location"].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let response = get(
        &app,
        &format!("/oauth/callback?code=test&state={state}"),
        &cookies[0],
    )
    .await;
    let rotated = session_cookie(&response);
    assert_ne!(first, common::csrf_token(&app, &rotated).await);
    for cookie in [&cookies[1], &rotated] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/logout")
                    .header("cookie", cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf_token={first}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            get(&app, "/console/accounts/octocat", cookie)
                .await
                .status(),
            StatusCode::OK
        );
    }
}

mod common;

#[tokio::test]
async fn authenticated_session_without_csrf_authority_confers_no_identity() {
    // A stored authenticated record must carry browser mutation authority;
    // without it the record is malformed and treated as signed out.
    let store = tower_sessions::MemoryStore::default();
    let session = tower_sessions::Session::new(None, Arc::new(store.clone()), None);
    session
        .insert(
            "ghinvite",
            serde_json::json!({
                "user_id": 42, "login": "octocat", "access_token": "u_xxx",
                "oauth_csrf": null, "csrf_token": null, "admin_checks": {}, "return_to": null
            }),
        )
        .await
        .unwrap();
    session.save().await.unwrap();
    let cookie = format!("id={}", session.id().unwrap());
    let app = build_app_with_store(MockTransport::scripted(vec![]), store).await;
    let response = get(&app, "/console/accounts/octocat", &cookie).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with("/login?return_to=")
    );
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
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
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
    Load = 4,
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
        self.check(StoreOperation::Load)?;
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
        let backend = ghinvite_web::session_store::SqliteBackend::new(pool);
        backend.migrate().await.unwrap();
        let store = ghinvite_web::session_store::ProtectedStore::new(backend, [7; 32]);
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
    // The visitor is told the link is stale and sent back to sign in; naming
    // the CSRF check is an internal detail they cannot act on.
    assert!(text.contains("no longer valid"), "{text}");
    assert!(text.contains("href=\"/login\""), "{text}");
    assert!(!text.contains("CSRF"), "{text}");
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
