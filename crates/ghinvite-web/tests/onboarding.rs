mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use common::sign_in::{self, ConsoleInstallationFixture, ConsoleTransport, OCTOCAT};
use ghinvite_core::{
    account::{App, Installation},
    storage::AppInstallationStorage,
};
use ghinvite_github::{HttpTransport, Method, Response as GhResponse, mocks::Expectation};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
struct Transport {
    rest: ConsoleTransport,
    responses: BTreeMap<(String, String), GhResponse>,
    requests: Mutex<Vec<(String, String)>>,
}
impl HttpTransport for Transport {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<GhResponse, ghinvite_github::TransportError> {
        if request.url.contains("/app/installation-requests") {
            assert_eq!(request.method, Method::Get);
            let bearer = request.headers.get("authorization").unwrap();
            let parts: Vec<_> = bearer.strip_prefix("Bearer ").unwrap().split('.').collect();
            assert_eq!(
                parts.len(),
                3,
                "installation requests require app JWT, never IAT or user token"
            );
            let claims: Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
            let issuer = claims["iss"].as_str().unwrap().to_owned();
            assert!(matches!(
                issuer.as_str(),
                "Iv.fixture-repo" | "Iv.fixture-org"
            ));
            self.requests
                .lock()
                .unwrap()
                .push((issuer.clone(), request.url.clone()));
            return Ok(self
                .responses
                .get(&(issuer, request.url))
                .expect("explicit app/page fixture")
                .clone());
        }
        self.rest.send(request).await
    }
}
fn response(value: Value) -> GhResponse {
    GhResponse {
        status: 200,
        headers: Default::default(),
        body: serde_json::to_vec(&value).unwrap(),
    }
}
fn pending(id: u64, account: u64, login: &str, requester: u64) -> Value {
    // GitHub's documented minimal requester omits login.
    json!({"id":id,"account":{"id":account,"login":login,"type":"Organization"},"requester":{"id":requester},"created_at":"2026-10-01T12:00:00Z"})
}
const FIRST: &str = "https://api.github.com/app/installation-requests?per_page=100";
async fn fixture(
    responses: Vec<(&str, &str, GhResponse)>,
    org_only: bool,
) -> (
    axum::Router,
    String,
    Arc<Transport>,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    fixture_with_membership(responses, org_only, "admin").await
}
async fn fixture_with_membership(
    responses: Vec<(&str, &str, GhResponse)>,
    org_only: bool,
    membership_role: &str,
) -> (
    axum::Router,
    String,
    Arc<Transport>,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    if !org_only {
        storage
            .seed_console_installation(&sign_in::acme_installation())
            .await
            .unwrap();
    }
    let mut org = Installation::repo(sign_in::acme_installation());
    org.app = App::Org;
    org.account.installation_id = 88;
    storage.put_app_installation(&org).await.unwrap();
    let mut expected = sign_in::oauth_expectations(OCTOCAT);
    expected.push(Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/memberships/octocat",json!({"user":{"id":42,"login":"octocat"},"organization":{"id":9001,"login":"acme"},"role":membership_role,"state":"active"})));
    if membership_role == "member" {
        storage
            .put_account_settings(&ghinvite_core::account::AccountSettings {
                account_id: 9001,
                revision: 1,
                repo_admin_delegation: true,
                changed_by: 7,
                changed_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        expected.push(Expectation::ok_json(
            Method::Get,
            "https://api.github.com/installation/repositories?per_page=100",
            json!({"total_count":0,"repositories":[]}),
        ));
    }
    let transport = Arc::new(Transport {
        rest: ConsoleTransport::scripted(expected),
        responses: responses
            .into_iter()
            .map(|(app, url, value)| ((format!("Iv.fixture-{app}"), url.to_owned()), value))
            .collect(),
        requests: Mutex::new(vec![]),
    });
    let mut config = sign_in::console_config();
    config.github_org_install_url =
        Some("https://github.com/apps/org-fixture/installations/new".into());
    let app = common::build_app(
        crate::common::sign_in::app_state(
            storage.clone(),
            transport.clone(),
            sign_in::unreachable_restate(),
            config,
        ),
        tower_sessions::MemoryStore::default(),
    );
    let cookie = sign_in::sign_in(&app).await;
    (app, cookie, transport, storage)
}
async fn get(app: &axum::Router, cookie: Option<&str>, path: &str) -> Response {
    let mut request = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
async fn text(response: Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}
#[tokio::test]
async fn chooser_requires_signin_and_shows_exact_separate_permissions() {
    let (app, cookie, transport, _) = fixture(vec![], true).await;
    let response = get(&app, None, "/onboarding").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with("/login?return_to=")
    );
    let response = get(&app, Some(&cookie), "/onboarding").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let html = text(response).await;
    for phrase in [
        "Invite people to repositories",
        "Invite people to an organization and its teams",
        "Administration — read and write",
        "Members — read and write",
        "Metadata — read",
        "No organization permissions",
        "/install/org",
    ] {
        assert!(html.contains(phrase), "{phrase}");
    }
    assert_eq!(html.matches("<head>").count(), 1);
    assert_eq!(html.matches("<body").count(), 1);
    assert!(transport.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn pending_requests_are_paginated_app_scoped_and_private_to_numeric_requester() {
    let next = "https://api.github.com/app/installation-requests?per_page=100&page=2";
    let mut first = response(json!([
        pending(1, 9001, "acme", 42),
        pending(2, 9002, "private-other-org", 99)
    ]));
    first
        .headers
        .insert("link".into(), format!("<{next}>; rel=\"next\""));
    let (app, cookie, transport, _) = fixture(
        vec![
            ("repo", FIRST, first),
            (
                "repo",
                next,
                response(json!([pending(3, 9003, "second-org", 42)])),
            ),
            (
                "org",
                FIRST,
                response(json!([pending(4, 9001, "acme", 42)])),
            ),
        ],
        true,
    )
    .await;
    let response = get(&app, Some(&cookie), "/onboarding/requests").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("second-org"));
    assert!(html.contains("Repo app"));
    assert!(html.contains("Org app"));
    assert!(!html.contains("private-other-org"));
    assert_eq!(
        html.matches("Request sent to the organization owners")
            .count(),
        3
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn request_callback_is_only_a_hint_and_never_fabricates_success() {
    let (app, cookie, transport, _) =
        fixture(vec![("org", FIRST, response(json!([])))], true).await;
    let response = get(&app, Some(&cookie), "/setup/org?setup_action=request").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/onboarding/requests?app=org"
    );
    assert!(transport.requests.lock().unwrap().is_empty());
    let response = get(&app, None, "/onboarding/requests?app=org").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response = get(&app, Some(&cookie), "/onboarding/requests?app=org").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("No pending installation request is currently confirmed"));
    assert!(!html.contains("Request sent to the organization owners"));
}
#[tokio::test]
async fn failed_later_page_and_invalid_identity_do_not_expose_partial_results() {
    let next = "https://api.github.com/app/installation-requests?per_page=100&page=2";
    for malformed in [false, true] {
        let mut first = response(json!([pending(
            if malformed { 0 } else { 1 },
            9001,
            "withheld-org",
            42
        )]));
        if !malformed {
            first
                .headers
                .insert("link".into(), format!("<{next}>; rel=\"next\""));
        }
        let mut failed = response(json!({"message":"upstream-private-marker"}));
        failed.status = 503;
        let (app, cookie, _, _) =
            fixture(vec![("repo", FIRST, first), ("repo", next, failed)], true).await;
        let response = get(&app, Some(&cookie), "/onboarding/requests?app=repo").await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let html = text(response).await;
        assert!(html.contains("status is unknown"));
        assert!(!html.contains("withheld-org"));
        assert!(!html.contains("upstream-private-marker"));
    }
}
#[tokio::test]
async fn console_discovers_org_only_accounts_and_deduplicates_both_apps() {
    for org_only in [true, false] {
        let (app, cookie, _, _) = fixture(vec![], org_only).await;
        let response = get(&app, Some(&cookie), "/console").await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], "/console/accounts/acme");
    }
}
#[tokio::test]
async fn account_installations_list_only_exact_account_pending_requests() {
    let (app, cookie, _, _) = fixture(
        vec![
            (
                "repo",
                FIRST,
                response(json!([
                    pending(1, 9001, "acme", 99),
                    pending(2, 9010, "private-other-org", 42)
                ])),
            ),
            ("org", FIRST, response(json!([]))),
        ],
        true,
    )
    .await;
    let response = get(&app, Some(&cookie), "/console/accounts/acme/installations").await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    assert!(html.contains("Not installed"));
    assert!(html.contains("Installed"));
    assert!(html.contains("acme — Repo app"));
    assert!(!html.contains("private-other-org"));
}

#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn onboarding_browser_server() {
    use axum::response::IntoResponse;
    let (app, cookie, _, _) = fixture(
        vec![
            (
                "repo",
                FIRST,
                response(json!([
                    pending(1, 9001, "acme", 42),
                    pending(2, 9002, "private-other-org", 99)
                ])),
            ),
            ("org", FIRST, response(json!([]))),
        ],
        true,
    )
    .await;
    let router = axum::Router::new()
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let cookie = cookie.clone();
                async move {
                    (
                        StatusCode::SEE_OTHER,
                        [
                            (
                                axum::http::header::SET_COOKIE,
                                format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                            ),
                            (axum::http::header::LOCATION, "/onboarding".to_owned()),
                        ],
                    )
                        .into_response()
                }
            }),
        )
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    axum::serve(
        tokio::net::TcpListener::bind("127.0.0.1:4199")
            .await
            .unwrap(),
        router,
    )
    .await
    .unwrap();
}

#[test]
fn onboarding_signin_return_paths_are_exact_and_cannot_carry_arbitrary_data() {
    for path in [
        "/onboarding",
        "/onboarding/requests",
        "/onboarding/requests?app=repo",
        "/onboarding/requests?app=org",
    ] {
        assert_eq!(
            ghinvite_web::session::validate_return_to(path).as_deref(),
            Some(path)
        );
    }
    for path in [
        "/onboarding/requests?app=signin",
        "/onboarding/requests?app=org&secret=private",
        "/onboarding?next=https://evil.test",
        "//evil.test/onboarding",
    ] {
        assert_eq!(ghinvite_web::session::validate_return_to(path), None);
    }
}

#[tokio::test]
async fn every_account_console_shell_receives_current_app_facts() {
    let (app, cookie, _, storage) = fixture(vec![], true).await;
    for path in [
        "/console/accounts/acme",
        "/console/accounts/acme/links",
        "/console/accounts/acme/requests",
        "/console/accounts/acme/audit",
    ] {
        let response = get(&app, Some(&cookie), path).await;
        let status = response.status();
        let page = text(response).await;
        assert_eq!(status, StatusCode::OK, "{path}: {page}");
        assert!(page.contains("Repo app: Not installed"), "{path}: {page}");
        assert!(page.contains("Org app: Installed"), "{path}: {page}");
        assert!(page.contains("Add repo app"), "{path}: {page}");
        assert!(page.contains("Manage apps"), "{path}: {page}");
    }
    let mut repository = Installation::repo(sign_in::acme_installation());
    repository.suspended_at = Some(chrono::Utc::now());
    storage.put_app_installation(&repository).await.unwrap();
    let response = get(&app, Some(&cookie), "/console/accounts/acme/links").await;
    let status = response.status();
    let page = text(response).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("Repo app: Suspended"), "{page}");
    assert!(page.contains("Org app: Installed"), "{page}");
    assert!(!page.contains("Add repo app"), "{page}");
}

#[tokio::test]
async fn delegated_header_does_not_reveal_org_app_state_or_owner_controls() {
    let (app, cookie, transport, _) = fixture_with_membership(vec![], false, "member").await;
    let response = get(&app, Some(&cookie), "/console/accounts/acme/links/new").await;
    let status = response.status();
    let page = text(response).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let header = page
        .split_once("aria-label=\"Installed GitHub apps\"")
        .unwrap()
        .1
        .split_once("</header>")
        .unwrap()
        .0;
    assert!(header.contains("Repo app: Installed"), "{header}");
    assert!(!header.contains("Org app:"), "{header}");
    assert!(!header.contains("Manage apps"), "{header}");
    assert!(!header.contains("Add org app"), "{header}");
    assert!(header.contains("Choose apps"), "{header}");
    assert!(!page.contains("href=\"/console/accounts/acme/installations\""));
    assert!(
        transport.requests.lock().unwrap().is_empty(),
        "header never reads app-wide requests"
    );
}
