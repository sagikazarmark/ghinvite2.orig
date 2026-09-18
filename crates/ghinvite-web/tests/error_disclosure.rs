//! What a browser is allowed to see when something upstream fails.
//!
//! Each test feeds a sensitive-looking payload into a failure path — a
//! browser-controlled callback parameter, a GitHub response, a Restate ingress
//! response — and asserts the value reaches neither the response body nor the
//! logs, while the page still offers somewhere to go.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::{AppState, RestateClient, RestateCommands, WebConfig, build_app};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use wiremock::matchers::method as wm_method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const USER_TOKEN: &str = "gho_16C7e42F292c6912E7710c838347Ae178B4a";
const INGRESS_KEY: &str = "ingress-key-do-not-expose";

/// A `tracing` sink that keeps everything written to it.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// `#[tokio::test]` runs on a current-thread runtime, so a thread-local
/// default subscriber sees everything the awaited work emits.
fn capture_logs() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    (logs.clone(), tracing::subscriber::set_default(subscriber))
}

async fn build_app_with(mock: MockTransport, ingress: &str) -> axum::Router {
    let storage: Arc<dyn ghinvite_core::storage::Storage> = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransport> = Arc::new(mock);
    let restate = Arc::new(RestateClient::new(ingress).unwrap());
    let state = AppState::new(
        storage,
        transport,
        Arc::new(RestateCommands::new(restate)),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    build_app(state, tower_sessions::MemoryStore::default())
}

fn cookie_of(resp: &axum::response::Response, fallback: Option<String>) -> String {
    resp.headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .or(fallback)
        .expect("session cookie available")
}

async fn get(app: &axum::Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder().uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn oauth_expectations() -> Vec<Expectation> {
    vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: format!(
                    r#"{{"access_token":"{USER_TOKEN}","token_type":"bearer","scope":"read:user read:org"}}"#
                )
                .into_bytes(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]
}

/// Drive /login → /oauth/callback and return the signed-in cookie.
async fn sign_in(app: &axum::Router) -> String {
    let started = get(app, "/login", None).await;
    assert_eq!(started.status(), StatusCode::SEE_OTHER);
    let cookie = cookie_of(&started, None);
    let location = started.headers()["location"].to_str().unwrap().to_owned();
    let state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let finished = get(
        app,
        &format!("/oauth/callback?code=test-code&state={state}"),
        Some(&cookie),
    )
    .await;
    assert_eq!(finished.status(), StatusCode::SEE_OTHER);
    cookie_of(&finished, Some(cookie))
}

fn assert_absent(haystack: &str, needles: &[&str], what: &str) {
    for needle in needles {
        assert!(
            !haystack.contains(needle),
            "{what} disclosed {needle:?}:\n{haystack}"
        );
    }
}

/// `error` and `error_description` arrive on a redirect the browser controls.
/// Reflecting them put arbitrary text — including anything an attacker could
/// talk a visitor into clicking — straight into ghinvite's response.
#[tokio::test]
async fn oauth_callback_error_parameters_are_not_reflected() {
    let (logs, _guard) = capture_logs();
    let app = build_app_with(MockTransport::scripted(vec![]), "http://127.0.0.1:8080").await;

    let response = get(
        &app,
        "/oauth/callback?error=contact%20support%20at%20evil.test&\
         error_description=your%20token%20gho_16C7e42F292c6912E7710c838347Ae178B4a%20expired",
        None,
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_text(response).await;
    let injected = ["evil.test", USER_TOKEN, "contact support"];
    assert_absent(&body, &injected, "oauth callback response");
    assert_absent(&logs.text(), &injected, "logs");
    // The visitor is still offered a way forward.
    assert!(body.contains("href=\"/login\""), "{body}");
}

/// A declined authorization and a provider error read differently to the
/// visitor, and both stay safe.
#[tokio::test]
async fn oauth_callback_distinguishes_a_decline_from_a_provider_error() {
    let app = build_app_with(MockTransport::scripted(vec![]), "http://127.0.0.1:8080").await;

    let declined = body_text(get(&app, "/oauth/callback?error=access_denied", None).await).await;
    let failed =
        body_text(get(&app, "/oauth/callback?error=bad_verification_code", None).await).await;

    assert!(declined.contains("did not authorize"), "{declined}");
    assert!(failed.contains("could not complete"), "{failed}");
    assert!(!failed.contains("bad_verification_code"), "{failed}");
}

/// A gateway sitting in front of GitHub answers a setup-return read with a body
/// that quotes the user token we sent.
#[tokio::test]
async fn github_gateway_body_never_reaches_the_browser() {
    let (logs, _guard) = capture_logs();
    let mut expectations = oauth_expectations();
    expectations.push(Expectation {
        method: Method::Get,
        url: "https://api.github.com/user/installations?per_page=100".into(),
        required_headers: BTreeMap::new(),
        expected_body: None,
        response: Response {
            status: 502,
            headers: BTreeMap::new(),
            body: format!(
                "<html>proxy failure; upstream request had Authorization: Bearer {USER_TOKEN}</html>"
            )
            .into_bytes(),
        },
    });
    let app = build_app_with(
        MockTransport::scripted(expectations),
        "http://127.0.0.1:8080",
    )
    .await;
    let cookie = sign_in(&app).await;

    let response = get(
        &app,
        "/setup/github?installation_id=77&setup_action=install",
        Some(&cookie),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = body_text(response).await;
    let leaked = [USER_TOKEN, "proxy failure", "Authorization"];
    assert_absent(&body, &leaked, "setup response");
    assert_absent(&logs.text(), &leaked, "logs");
    // The status is still there for whoever is on call.
    assert!(logs.text().contains("502"), "{}", logs.text());
    assert!(body.contains("href=\"/\""), "{body}");
}

/// The Restate ingress answers a setup-return command with a diagnostic that
/// quotes the ingress API key.
#[tokio::test]
async fn restate_ingress_body_never_reaches_the_browser() {
    let (logs, _guard) = capture_logs();
    let ingress = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string(format!(
            r#"{{"message":"ingress rejected authorization: Bearer {INGRESS_KEY}"}}"#
        )))
        .mount(&ingress)
        .await;

    let mut expectations = oauth_expectations();
    expectations.push(Expectation::ok_json(
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
    ));
    let app = build_app_with(MockTransport::scripted(expectations), &ingress.uri()).await;
    let cookie = sign_in(&app).await;

    let response = get(
        &app,
        "/setup/github?installation_id=77&setup_action=install",
        Some(&cookie),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = body_text(response).await;
    let leaked = [INGRESS_KEY, "ingress rejected", &ingress.uri()];
    assert_absent(&body, &leaked, "setup response");
    assert_absent(&logs.text(), &leaked, "logs");
    assert!(body.contains("href=\"/\""), "{body}");
    // The status is still there for whoever is on call.
    assert!(logs.text().contains("500"), "{}", logs.text());
}
