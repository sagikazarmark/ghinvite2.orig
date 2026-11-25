//! ghinvite-web's real sign-in (`/login` → GitHub → `/oauth/callback`)
//! against the GitHub emulator (`octoemu`, GE-05) instead of wiremock or the
//! scripted sign-in bridge: the start redirect goes to the emulator's
//! authorize page, which (with `login=` and auto-consent) redirects back to
//! the callback with a code; the callback exchanges it with PKCE, reads
//! `/user` and revokes the token, all over HTTP.
//!
//! The scripted tests (`oauth_flow.rs`, `src/signin/tests.rs`) stay until
//! GE-09 migrates them.
#![cfg(not(target_arch = "wasm32"))]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghinvite_core::{Account, AccountType, SelectedRepos};
use ghinvite_github::mocks::MockTransport;
use ghinvite_web::config::apps::SignInApp;
use http_body_util::BodyExt;
use octoemu::Emulator;
use octoemu::seed::{App, Email, EmailVisibility, Seed, User};
use std::sync::Arc;
use tower::ServiceExt;
mod common;
use common::sign_in::{ConsoleInstallationFixture, SIGNIN_CLIENT_ID, session_cookie};

const PUBLIC_KEY: &str =
    include_str!("../../../libs/octoemu/crates/octoemu/tests/fixtures/rsa2048.pub.pem");
const SECRET: &str = "signin-test-secret";
const CALLBACK: &str = "https://p1.test/oauth/callback";

/// octocat (42), who installed the repo App on their account, and the
/// sign-in App with the web app's callback.
fn seed() -> Seed {
    Seed::new()
        .user(
            User::new(42, "octocat")
                .email_address(Email::new("octocat@example.com").primary(EmailVisibility::Public)),
        )
        .app(
            App::new(7, "p1-signin", SIGNIN_CLIENT_ID, SECRET)
                .public_key(PUBLIC_KEY)
                .callback_url(CALLBACK),
        )
}

/// ghinvite-web over in-memory storage holding octocat's installation,
/// signing in against `emulator`; nothing else reaches GitHub.
async fn app(emulator: &Emulator) -> axum::Router {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .seed_console_installation(&Account {
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
    let mut state = common::sign_in::app_state(
        storage,
        Arc::new(MockTransport::scripted(vec![])),
        common::sign_in::unreachable_restate(),
        common::sign_in::console_config(),
    );
    let mut signin = SignInApp::new(SIGNIN_CLIENT_ID, SECRET, CALLBACK.parse().unwrap());
    signin.web_base_url = Some(emulator.web_base().to_owned());
    signin.api_base_url = Some(emulator.api_base().to_owned());
    state.config.signin_apps = Some(Arc::new(signin));
    common::build_app(state, tower_sessions::MemoryStore::default())
}

async fn get(app: &axum::Router, path: &str, cookie: Option<&str>) -> axum::response::Response {
    let mut request = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// `/login`, then the browser's visit to GitHub (with `login=octocat`): the
/// anonymous session cookie and the callback path (`/oauth/callback?…`)
/// GitHub redirected back to.
async fn start(app: &axum::Router, emulator: &Emulator) -> (String, String) {
    let started = get(app, "/login", None).await;
    assert_eq!(started.status(), StatusCode::SEE_OTHER);
    let cookie = session_cookie(&started, None);
    let authorize = started.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        authorize.starts_with(&format!("{}/login/oauth/authorize?", emulator.web_base())),
        "{authorize}"
    );
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let github = browser
        .get(format!("{authorize}&login=octocat"))
        .send()
        .await
        .unwrap();
    assert_eq!(github.status(), reqwest::StatusCode::FOUND);
    let back = github.headers()["location"].to_str().unwrap();
    let callback = back
        .strip_prefix("https://p1.test")
        .unwrap_or_else(|| panic!("redirected back to {back}"))
        .to_owned();
    assert!(callback.starts_with("/oauth/callback?"), "{callback}");
    (cookie, callback)
}

#[tokio::test]
async fn signs_in_through_the_emulator_and_revokes_the_token() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = app(&emulator).await;
    let (cookie, callback) = start(&app, &emulator).await;

    let finished = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(finished.status(), StatusCode::SEE_OTHER);
    assert_eq!(finished.headers()["location"], "/");
    let signed_in = session_cookie(&finished, None);
    assert_ne!(cookie, signed_in, "authentication rotates the session ID");

    let console = get(&app, "/console/accounts/octocat", Some(&signed_in)).await;
    assert_eq!(console.status(), StatusCode::OK);
    let anonymous = get(&app, "/console/accounts/octocat", Some(&cookie)).await;
    assert_eq!(anonymous.status(), StatusCode::SEE_OTHER);
    // The callback works once.
    let replay = get(&app, &callback, Some(&signed_in)).await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

    // GitHub saw the exchange, the identity read with the new user token,
    // and the App revoke it with its client credentials.
    let calls: Vec<(String, u16, serde_json::Value)> = emulator
        .calls()
        .into_iter()
        .filter_map(|call| {
            Some((
                call.route?,
                call.status,
                serde_json::to_value(&call.identity).unwrap(),
            ))
        })
        .collect();
    let routes: Vec<&str> = calls.iter().map(|(route, ..)| route.as_str()).collect();
    assert_eq!(
        routes,
        [
            "GET /login/oauth/authorize",
            "POST /login/oauth/access_token",
            "GET /user",
            "DELETE /applications/{client_id}/token",
        ]
    );
    assert!(
        calls
            .iter()
            .all(|(_, status, _)| [200, 204, 302].contains(status))
    );
    assert_eq!(calls[2].2["kind"], "user_token");
    assert_eq!(calls[2].2["login"], "octocat");
    assert_eq!(calls[3].1, 204);
    assert_eq!(
        calls[3].2,
        serde_json::json!({"kind": "client", "app_id": 7})
    );
    let state = emulator.state();
    let tokens = state["user_tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0]["login"], "octocat");
    assert_eq!(tokens[0]["revoked"], true);
}

#[tokio::test]
async fn a_declined_authorization_signs_nobody_in() {
    let emulator = Emulator::start(seed()).await.unwrap();
    emulator.deny_authorization(None);
    let app = app(&emulator).await;
    let (cookie, callback) = start(&app, &emulator).await;
    assert!(callback.contains("error=access_denied"), "{callback}");

    let declined = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(declined.status(), StatusCode::BAD_REQUEST);
    assert!(!declined.headers().contains_key("set-cookie"));
    let body = declined.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Sign-in was not completed"), "{text}");
    assert!(
        !text.contains("denied your application"),
        "no upstream prose"
    );
    let console = get(&app, "/console/accounts/octocat", Some(&cookie)).await;
    assert_eq!(console.status(), StatusCode::SEE_OTHER);
    // Nothing was exchanged.
    assert!(
        emulator
            .calls()
            .iter()
            .all(|call| call.route.as_deref() == Some("GET /login/oauth/authorize"))
    );
}
