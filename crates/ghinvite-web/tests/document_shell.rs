//! Every full HTML response is one standards-mode document (#68).
//!
//! `ghinvite_ui::document` unit-tests the shell itself and `ghinvite_ui::layouts`
//! the viewport meta. These tests pin both to the real router, so a page that
//! stops rendering through `views::render`, or a layout that loses its `<head>`,
//! fails here rather than in a browser.
//!
//! What only a browser can decide — quirks vs. standards mode, and whether a
//! 390px phone lays the page out at the device width — is covered by
//! `tests/browser/document.spec.mjs`, which drives the same pages through
//! `document_browser_server` below.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use common::link_authority::FakeLinkAuthority;
use ghinvite_core::admission::RequesterPage;
use ghinvite_core::storage::Storage;
use ghinvite_core::storage::projection::RequestSnapshot;
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, InvitationRequest,
    Permission, RequestId, RequestState, SelectedRepos, Slug, User,
};
use ghinvite_github::transport::{HttpTransport, Method, Request as GithubRequest, Response};
use ghinvite_web::{AppState, RestateCommands, WebConfig, build_app};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

mod common;

/// The signed-in GitHub user, who also owns the personal account the Console
/// pages below belong to (`check_admin` derives personal authority from the
/// user ID, so those pages need no GitHub call).
const USER_ID: u64 = 42;
const USER_LOGIN: &str = "octocat";
const ORG_ID: u64 = 9001;
const ORG_LOGIN: &str = "acme";

/// An active invitation link with no invitation request from the signed-in
/// user, so `/i/{code}` renders the invitation request form.
const FORM_CODE: &str = "abcdEFGH01234567";
/// An active invitation link the signed-in user already has a pending
/// invitation request on, so `/i/{code}` renders its status, not the form.
const STATUS_CODE: &str = "bcdeFGHI12345678";
/// Fixed link IDs, so the Console detail and edit paths are the same here and
/// in `tests/browser/document.spec.mjs`.
const FORM_LINK_ID: &str = "01JQRFRM000000000000000000";
const STATUS_LINK_ID: &str = "01JQSTAT000000000000000000";

/// Every full HTML response ghinvite serves, with the status it answers with.
/// `/console/accounts/{login}/links/new` is left out on purpose: the island
/// form page has its own browser fixture (`tests/browser/link-form.spec.mjs`),
/// which renders through the same layout.
fn document_paths() -> Vec<(String, StatusCode)> {
    let console = format!("/console/accounts/{USER_LOGIN}");
    vec![
        ("/".into(), StatusCode::OK),
        ("/console".into(), StatusCode::OK),
        (console.clone(), StatusCode::OK),
        (format!("{console}/links"), StatusCode::OK),
        (format!("{console}/links/{FORM_LINK_ID}"), StatusCode::OK),
        (
            format!("{console}/links/{FORM_LINK_ID}/edit"),
            StatusCode::OK,
        ),
        (format!("{console}/requests"), StatusCode::OK),
        (format!("{console}/audit"), StatusCode::OK),
        (format!("{console}/settings"), StatusCode::OK),
        (format!("{console}/nowhere"), StatusCode::NOT_FOUND),
        (format!("/i/{FORM_CODE}"), StatusCode::OK),
        (format!("/i/{STATUS_CODE}"), StatusCode::OK),
        ("/i/not-a-code".into(), StatusCode::NOT_FOUND),
        ("/nowhere".into(), StatusCode::NOT_FOUND),
    ]
}

/// One document shell, opened by the standards-mode doctype, carrying an
/// explicit language and the mobile viewport.
fn assert_one_standards_mode_document(html: &str) {
    assert!(
        html.starts_with("<!DOCTYPE html><html lang=\"en\">"),
        "document does not open the standards-mode shell: {}",
        &html[..html.len().min(120)]
    );
    assert!(html.ends_with("</html>"), "document root is left open");
    for tag in ["<html", "</html>", "<head>", "</head>", "<body", "</body>"] {
        assert_eq!(html.matches(tag).count(), 1, "expected exactly one {tag}");
    }
    let head = html.split_once("</head>").expect("a <head>").0;
    assert!(
        head.contains("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>"),
        "no mobile viewport in <head>"
    );
    assert_eq!(html.matches("name=\"viewport\"").count(), 1);
}

#[tokio::test]
async fn every_signed_in_page_is_one_standards_mode_document() {
    let (app, cookie) = document_fixture().await;

    for (path, expected) in document_paths() {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&path)
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
        assert_one_standards_mode_document(&body_text(response).await);
    }
}

/// The public surface a signed-out visitor sees. `/console` and `/i/{code}`
/// redirect to sign-in, so only the home page renders a document here.
#[tokio::test]
async fn the_signed_out_home_page_is_one_standards_mode_document() {
    let (app, _) = document_fixture().await;

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert_one_standards_mode_document(&html);
    assert!(
        html.contains("Sign in"),
        "expected the signed-out home page"
    );
}

/// The two `/i/{code}` documents above are the same route; without this, a
/// fixture that stopped seeding the pending invitation request would serve two
/// form pages and every shape assertion would still pass. The pending status
/// page is also the only document here that reloads itself, so this is where
/// the layout's meta refresh has to survive the shared `<head>`.
#[tokio::test]
async fn the_two_invitation_request_documents_are_the_form_and_its_refreshing_status() {
    let (app, cookie) = document_fixture().await;

    let form = invitation_request_page(&app, &cookie, FORM_CODE).await;
    assert!(form.contains("Submit request"), "no submit control");
    assert!(!form.contains("http-equiv=\"refresh\""));

    let status = invitation_request_page(&app, &cookie, STATUS_CODE).await;
    assert!(status.contains("Awaiting review"), "not the status page");
    assert!(!status.contains("Submit request"));
    let head = status.split_once("</head>").expect("a <head>").0;
    assert!(
        head.contains("<meta http-equiv=\"refresh\""),
        "the pending status page stopped reloading itself"
    );
}

async fn invitation_request_page(app: &axum::Router, cookie: &str, code: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{code}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_text(response).await
}

/// Playwright drives this real router through `document.config.mjs`.
/// `/fixture-login` hands the browser the signed-in session; a context that
/// never visits it stays signed out.
#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn document_browser_server() {
    use axum::response::IntoResponse;

    let (app, cookie) = document_fixture().await;
    let served = app.clone();
    let server = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let cookie = cookie.clone();
                async move {
                    (
                        [(
                            "set-cookie",
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to("/"),
                    )
                        .into_response()
                }
            }),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = served.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4175")
        .await
        .unwrap();
    axum::serve(listener, server).await.unwrap();
}

/// GitHub reads the Console makes on every request. `MockTransport` scripts one
/// response per call, which a browser fixture exhausts on its second
/// navigation; this answers the same reads as often as they are made.
#[derive(Clone, Default)]
struct FixtureGithub;

#[async_trait::async_trait]
impl HttpTransport for FixtureGithub {
    async fn send(&self, request: GithubRequest) -> ghinvite_github::Result<Response> {
        let body = match (request.method, request.url.as_str()) {
            (Method::Post, "https://github.com/login/oauth/access_token") => serde_json::json!({
                "access_token": "u_xxx", "token_type": "bearer", "scope": "read:user read:org"
            }),
            (Method::Get, "https://api.github.com/user") => {
                serde_json::json!({"id": USER_ID, "login": USER_LOGIN})
            }
            (
                Method::Get,
                "https://api.github.com/user/installations/77/repositories?per_page=100",
            ) => {
                serde_json::json!({"total_count": 0, "repositories": []})
            }
            (Method::Get, "https://api.github.com/user/installations?per_page=100") => {
                serde_json::json!({"total_count": 2, "installations": [
                    {"id": 77, "account": {"id": USER_ID, "login": USER_LOGIN, "type": "User"},
                     "repository_selection": "all", "target_type": "User", "target_id": USER_ID},
                    {"id": 78, "account": {"id": ORG_ID, "login": ORG_LOGIN, "type": "Organization"},
                     "repository_selection": "all", "target_type": "Organization", "target_id": ORG_ID},
                ]})
            }
            (Method::Get, url)
                if url == format!("https://api.github.com/user/memberships/orgs/{ORG_LOGIN}") =>
            {
                serde_json::json!({"role": "admin", "state": "active",
                    "organization": {"id": ORG_ID, "login": ORG_LOGIN}})
            }
            (method, url) => panic!("unexpected GitHub call: {} {url}", method.as_str()),
        };
        Ok(Response {
            status: 200,
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
            body: serde_json::to_vec(&body).unwrap(),
        })
    }
}

/// A router over in-memory storage holding both Console accounts, the two
/// invitation links, and the signed-in user's pending invitation request,
/// paired with that user's session cookie. Requests that carry the cookie are
/// signed in; requests that leave it off are not.
async fn document_fixture() -> (axum::Router, String) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    for (installation_id, account_id, login, account_type) in [
        (77, USER_ID, USER_LOGIN, AccountType::User),
        (78, ORG_ID, ORG_LOGIN, AccountType::Organization),
    ] {
        storage
            .insert_installation(&Account {
                installation_id,
                account_id,
                account_login: login.into(),
                account_type,
                installed_at: Utc::now(),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
    }
    storage
        .upsert_user(&User {
            user_id: USER_ID,
            login: USER_LOGIN.into(),
            avatar_url: None,
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    // The authority answers link and invitation request reads; SQL holds the
    // projection the Console lists.
    let authority = FakeLinkAuthority::start().await;
    for (code, link_id) in [(FORM_CODE, FORM_LINK_ID), (STATUS_CODE, STATUS_LINK_ID)] {
        let link = active_link(code, link_id);
        storage.insert_invitation_link(&link).await.unwrap();
        authority.seed_link(&link);
        if code == STATUS_CODE {
            let request = RequestId::new();
            authority.set_requester_page(pending_page(&link, request));
            storage
                .insert_invitation_request_and_increment_uses(&InvitationRequest {
                    id: request,
                    invitation_link_id: link.id,
                    requester_id: USER_ID,
                    justification: None,
                    state: RequestState::Pending,
                    decided_by: None,
                    decided_at: None,
                    decline_reason: None,
                    created_at: Utc::now(),
                    decision_deadline: Some(Utc::now() - chrono::Duration::minutes(1)),
                })
                .await
                .unwrap();
        }
    }

    let storage: Arc<dyn Storage> = storage;
    let restate = authority.client();
    let state = AppState::new(
        storage,
        Arc::new(FixtureGithub),
        Arc::new(RestateCommands::new(restate.clone())),
        restate,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie)
}

fn active_link(code: &str, link_id: &str) -> InvitationLink {
    InvitationLink {
        id: link_id.parse::<InvitationLinkId>().unwrap(),
        slug: Slug::from_string(code.to_string()).unwrap(),
        installation_id: 77,
        account_id: USER_ID,
        created_by: USER_ID,
        created_at: Utc::now(),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: Permission::Pull,
        approval_required: true,
        description: "AI coding workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: format!("{USER_LOGIN}/{}", "r".repeat(60)),
        }],
    }
}

/// The requester page for a user whose invitation request on `link` awaits
/// review, so no fresh attempt is offered.
fn pending_page(link: &InvitationLink, request: RequestId) -> RequesterPage {
    RequesterPage {
        link_id: link.id,
        invitation_code: link.slug.as_str().into(),
        repos: link.repos.clone(),
        permission: link.permission,
        approval_required: link.approval_required,
        can_start_fresh: false,
        attempt: None,
        request: Some(RequestSnapshot {
            request_id: request,
            link_id: link.id,
            account_id: link.account_id,
            requester_id: USER_ID,
            justification: None,
            state: RequestState::Pending,
            admitted_at: Utc::now(),
            decision_deadline: Some(Utc::now() + chrono::Duration::days(7)),
            revision: 1,
            decision: None,
        }),
    }
}

/// Sign in through the real OAuth routes; session internals stay private.
async fn sign_in(app: &axum::Router) -> String {
    let start = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::SEE_OTHER);
    let cookie = session_cookie(&start, None);
    let location = start.headers().get("location").unwrap().to_str().unwrap();
    let oauth_state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let callback = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/oauth/callback?code=test-code&state={oauth_state}"
                ))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::SEE_OTHER);
    session_cookie(&callback, Some(cookie))
}

fn session_cookie(response: &axum::response::Response, fallback: Option<String>) -> String {
    response
        .headers()
        .get("set-cookie")
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .or(fallback)
        .expect("session cookie available")
}

async fn body_text(response: axum::response::Response) -> String {
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
