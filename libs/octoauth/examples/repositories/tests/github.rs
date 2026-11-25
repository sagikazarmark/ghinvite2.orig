//! The web app against a local GitHub stand-in (wiremock), driven through
//! its router: sign-in, page reloads, filtering, sign-out, the setup
//! redirect and the ways a callback is refused.
//!
//! `fixtures/rsa2048.pem` is the toolkit's test-only key (the same as
//! octoauth's and octotoken's).
#![cfg(not(target_arch = "wasm32"))]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use example_repositories::WebApp;
use octotoken::{AppTokenProvider, octoauth};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use wiremock::matchers::{body_string_contains, header as has_header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PRIVATE_KEY: &str = include_str!("fixtures/rsa2048.pem");
const CLIENT_ID: &str = "Iv23example";

/// A user or organization as GitHub embeds it (every field octocrab
/// requires).
fn account(login: &str, id: u64, kind: &str) -> Value {
    let url = |rest: &str| format!("https://api.github.com/users/{login}{rest}");
    json!({
        "login": login, "id": id, "node_id": format!("N_{id}"), "type": kind,
        "avatar_url": format!("https://avatars.githubusercontent.com/u/{id}"),
        "gravatar_id": "", "url": url(""), "html_url": format!("https://github.com/{login}"),
        "followers_url": url("/followers"), "following_url": url("/following{/other_user}"),
        "gists_url": url("/gists{/gist_id}"), "starred_url": url("/starred{/owner}{/repo}"),
        "subscriptions_url": url("/subscriptions"), "organizations_url": url("/orgs"),
        "repos_url": url("/repos"), "events_url": url("/events{/privacy}"),
        "received_events_url": url("/received_events"), "site_admin": false
    })
}

fn installation(id: u64, login: &str, kind: &str) -> Value {
    json!({
        "id": id, "app_id": 4, "account": account(login, id * 10, kind),
        "repository_selection": "all", "permissions": {"metadata": "read"}, "events": []
    })
}

fn repository(id: u64, full_name: &str, private: bool) -> Value {
    let name = full_name.split('/').nth(1).unwrap();
    json!({
        "id": id, "name": name, "full_name": full_name, "private": private,
        "url": format!("https://api.github.com/repos/{full_name}"),
        "html_url": format!("https://github.com/{full_name}")
    })
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

async fn mount(server: &MockServer, verb: &str, route: &str, response: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(response)
        .mount(server)
        .await;
}

/// App 4 ("Repo Lens"), installed on acme (5: api, web, secret) and octocat
/// (6: dotfiles). The user octocat may see acme/api, acme/web (not
/// acme/secret) and octocat/dotfiles. Lists come in pages of 2.
async fn github() -> MockServer {
    let server = MockServer::start().await;

    // As the App, at startup.
    mount(
        &server,
        "GET",
        "/app",
        ok(json!({
            "id": 4, "slug": "repo-lens", "node_id": "A_4", "owner": account("acme", 50, "Organization"),
            "name": "Repo Lens", "external_url": "https://example.com",
            "html_url": "https://github.com/apps/repo-lens",
            "permissions": {"metadata": "read"}, "events": []
        })),
    )
    .await;

    // Sign-in.
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains("code=the-code"))
        .respond_with(ok(
            json!({"access_token": "ghu_user", "token_type": "bearer"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(has_header("authorization", "Bearer ghu_user"))
        .respond_with(ok(json!({
            "id": 1, "login": "octocat",
            "avatar_url": "https://avatars.githubusercontent.com/u/1"
        })))
        .mount(&server)
        .await;
    for (page, installations) in [
        (
            "1",
            json!([
                installation(5, "acme", "Organization"),
                installation(6, "octocat", "User")
            ]),
        ),
        ("2", json!([])),
    ] {
        Mock::given(method("GET"))
            .and(path("/user/installations"))
            .and(has_header("authorization", "Bearer ghu_user"))
            .and(query_param("page", page))
            .respond_with(ok(
                json!({"total_count": 2, "installations": installations}),
            ))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/user/installations/5/repositories"))
        .and(has_header("authorization", "Bearer ghu_user"))
        .respond_with(ok(json!({"total_count": 2, "repositories": [
            repository(101, "acme/api", false),
            repository(102, "acme/web", true)
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/installations/6/repositories"))
        .and(has_header("authorization", "Bearer ghu_user"))
        .respond_with(ok(json!({"total_count": 1, "repositories": [
            repository(111, "octocat/dotfiles", false)
        ]})))
        .mount(&server)
        .await;
    mount(
        &server,
        "DELETE",
        &format!("/applications/{CLIENT_ID}/token"),
        ResponseTemplate::new(204),
    )
    .await;

    // As each installation, on every page view.
    for installation in [5, 6] {
        mount(
            &server,
            "POST",
            &format!("/app/installations/{installation}/access_tokens"),
            ResponseTemplate::new(201).set_body_json(json!({
                "token": format!("ghs_{installation}"), "expires_at": "2099-01-01T00:00:00Z",
                "permissions": {"metadata": "read"}, "repository_selection": "all"
            })),
        )
        .await;
    }
    for (page, repositories) in [
        (
            "1",
            json!([
                repository(101, "acme/api", false),
                repository(102, "acme/web", true)
            ]),
        ),
        ("2", json!([repository(103, "acme/secret", true)])),
    ] {
        Mock::given(method("GET"))
            .and(path("/installation/repositories"))
            .and(has_header("authorization", "Bearer ghs_5"))
            .and(query_param("page", page))
            .respond_with(ok(json!({"total_count": 3, "repositories": repositories})))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .and(has_header("authorization", "Bearer ghs_6"))
        .respond_with(ok(json!({"total_count": 1, "repositories": [
            repository(111, "octocat/dotfiles", false)
        ]})))
        .mount(&server)
        .await;
    server
}

async fn web(server: &MockServer) -> Router {
    let app = octoauth::App::builder(4, octoauth::SigningKey::from_pem(PRIVATE_KEY).unwrap())
        .base_url(server.uri())
        .build()
        .unwrap();
    let sign_in = octologin::SignIn::builder(CLIENT_ID, "secret", "http://localhost:3000/callback")
        .web_base_url(server.uri())
        .api_base_url(server.uri())
        .build()
        .unwrap();
    WebApp::new(AppTokenProvider::new(app), sign_in)
        .await
        .unwrap()
        .secure_cookies(false)
        .per_page(2)
        .router()
}

/// `GET`/`POST` `uri` with `cookies` (`name=value` pairs).
async fn send(router: &Router, verb: &str, uri: &str, cookies: &[&str]) -> Response {
    let mut request = Request::builder().method(verb).uri(uri);
    if !cookies.is_empty() {
        request = request.header(header::COOKIE, cookies.join("; "));
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn text(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn location(response: &Response) -> &str {
    response.headers()[header::LOCATION].to_str().unwrap()
}

/// The `name=value` the response sets for cookie `name` (empty when it
/// clears it).
fn set_cookie(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .find(|value| value.starts_with(&format!("{name}=")))
        .map(|value| value.split(';').next().unwrap().to_owned())
}

fn state_of(authorize: &str) -> String {
    let query = authorize.split_once('?').unwrap().1;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("state="))
        .unwrap()
        .to_owned()
}

async fn count(server: &MockServer, verb: &str, route: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == route)
        .count()
}

/// `/login`, then GitHub's redirect back with `code` and the right state:
/// the session cookie.
async fn sign_in(router: &Router) -> String {
    let login = send(router, "GET", "/login", &[]).await;
    let login_cookie = set_cookie(&login, "login").unwrap();
    let state = state_of(location(&login));
    let callback = send(
        router,
        "GET",
        &format!("/callback?code=the-code&state={state}"),
        &[&login_cookie],
    )
    .await;
    assert_eq!(callback.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&callback), "/");
    assert_eq!(set_cookie(&callback, "login").unwrap(), "login=");
    set_cookie(&callback, "session").unwrap()
}

#[tokio::test]
async fn signed_out_home_links_to_sign_in() {
    let server = github().await;
    let router = web(&server).await;
    let home = send(&router, "GET", "/", &[]).await;
    assert_eq!(home.status(), StatusCode::OK);
    assert_eq!(home.headers()[header::CACHE_CONTROL], "no-store");
    let page = text(home).await;
    assert!(page.contains("<h1>Repo Lens</h1>"), "{page}");
    assert!(page.contains("href=\"/login\""));
}

#[tokio::test]
async fn login_redirects_to_github_with_state_and_pkce() {
    let server = github().await;
    let router = web(&server).await;
    let login = send(&router, "GET", "/login", &[]).await;
    assert_eq!(login.status(), StatusCode::SEE_OTHER);
    let url = location(&login);
    assert!(url.starts_with(&format!("{}/login/oauth/authorize?", server.uri())));
    assert!(url.contains(&format!("client_id={CLIENT_ID}")));
    assert!(url.contains("code_challenge_method=S256"));
    let cookie = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .next()
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Max-Age=600"));
}

#[tokio::test]
async fn signs_in_revokes_the_token_and_shows_only_what_the_user_may_see() {
    let server = github().await;
    let router = web(&server).await;
    let session = sign_in(&router).await;
    // The user token was used for the snapshot, then revoked.
    let revoked = format!("/applications/{CLIENT_ID}/token");
    assert_eq!(count(&server, "DELETE", &revoked).await, 1);

    let page = text(send(&router, "GET", "/", &[&session]).await).await;
    assert!(
        page.contains("Signed in as <strong>octocat</strong>"),
        "{page}"
    );
    assert!(page.contains("<h2>acme</h2>") && page.contains("<h2>octocat</h2>"));
    assert!(page.contains(">acme/api</a>"));
    assert!(page.contains(">acme/web</a> <small>private</small>"));
    assert!(page.contains(">octocat/dotfiles</a>"));
    // The App reaches acme/secret; the user may not see it.
    assert!(!page.contains("acme/secret"), "{page}");
    assert!(page.contains("https://github.com/apps/repo-lens/installations/new"));
}

#[tokio::test]
async fn a_reload_reads_the_repositories_again_with_installation_tokens() {
    let server = github().await;
    let router = web(&server).await;
    let session = sign_in(&router).await;
    for _ in 0..2 {
        let home = send(&router, "GET", "/", &[&session]).await;
        assert!(text(home).await.contains("acme/api"));
    }
    // Two pages for acme and one for octocat, per load; one token each,
    // cached across loads. No user token after sign-in.
    assert_eq!(count(&server, "GET", "/installation/repositories").await, 6);
    assert_eq!(
        count(&server, "POST", "/app/installations/5/access_tokens").await,
        1
    );
    let user_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| {
            r.headers
                .get("authorization")
                .is_some_and(|v| v == "Bearer ghu_user")
        })
        .count();
    assert_eq!(user_calls, 5); // /user, 2 × /user/installations, 2 × repositories
}

#[tokio::test]
async fn signing_in_again_replaces_the_session() {
    let server = github().await;
    let router = web(&server).await;
    let first = sign_in(&router).await;
    // "Re-check access" with the old session still in the browser.
    let login = send(&router, "GET", "/login", &[&first]).await;
    let login_cookie = set_cookie(&login, "login").unwrap();
    let state = state_of(location(&login));
    let callback = send(
        &router,
        "GET",
        &format!("/callback?code=the-code&state={state}"),
        &[&first, &login_cookie],
    )
    .await;
    let second = set_cookie(&callback, "session").unwrap();
    assert_ne!(first, second);
    let old = text(send(&router, "GET", "/", &[&first]).await).await;
    assert!(
        old.contains("Sign in with GitHub"),
        "the old session is gone"
    );
    let new = text(send(&router, "GET", "/", &[&second]).await).await;
    assert!(new.contains("Signed in as"));
}

#[tokio::test]
async fn logout_ends_the_session() {
    let server = github().await;
    let router = web(&server).await;
    let session = sign_in(&router).await;
    let logout = send(&router, "POST", "/logout", &[&session]).await;
    assert_eq!(logout.status(), StatusCode::SEE_OTHER);
    assert_eq!(set_cookie(&logout, "session").unwrap(), "session=");
    let page = text(send(&router, "GET", "/", &[&session]).await).await;
    assert!(page.contains("Sign in with GitHub"));
}

#[tokio::test]
async fn a_wrong_state_is_refused_without_exchanging_the_code() {
    let server = github().await;
    let router = web(&server).await;
    let login = send(&router, "GET", "/login", &[]).await;
    let login_cookie = set_cookie(&login, "login").unwrap();
    let forged = "A".repeat(43);
    let callback = send(
        &router,
        "GET",
        &format!("/callback?code=the-code&state={forged}"),
        &[&login_cookie],
    )
    .await;
    assert_eq!(callback.status(), StatusCode::BAD_REQUEST);
    assert!(text(callback).await.contains("gh:signin:state_mismatch"));
    assert_eq!(count(&server, "POST", "/login/oauth/access_token").await, 0);
    // The pending sign-in was used up by the attempt.
    let state = state_of(location(&login));
    let retry = send(
        &router,
        "GET",
        &format!("/callback?code=the-code&state={state}"),
        &[&login_cookie],
    )
    .await;
    assert!(text(retry).await.contains("example:sign_in_expired"));
}

#[tokio::test]
async fn a_callback_without_a_pending_sign_in_is_refused() {
    let server = github().await;
    let router = web(&server).await;
    let state = "A".repeat(43);
    let callback = send(
        &router,
        "GET",
        &format!("/callback?code=the-code&state={state}"),
        &[],
    )
    .await;
    assert_eq!(callback.status(), StatusCode::BAD_REQUEST);
    assert!(text(callback).await.contains("example:sign_in_expired"));
    assert_eq!(count(&server, "POST", "/login/oauth/access_token").await, 0);
}

#[tokio::test]
async fn a_declined_consent_shows_a_message() {
    let server = github().await;
    let router = web(&server).await;
    let login = send(&router, "GET", "/login", &[]).await;
    let login_cookie = set_cookie(&login, "login").unwrap();
    let state = state_of(location(&login));
    let callback = send(
        &router,
        "GET",
        &format!("/callback?error=access_denied&state={state}"),
        &[&login_cookie],
    )
    .await;
    assert_eq!(callback.status(), StatusCode::OK);
    assert!(
        text(callback)
            .await
            .contains("You did not authorize the App.")
    );
    assert_eq!(count(&server, "POST", "/login/oauth/access_token").await, 0);
}

#[tokio::test]
async fn after_an_installation_access_is_read_again_by_signing_in() {
    let server = github().await;
    let router = web(&server).await;
    for uri in [
        "/setup?installation_id=5&setup_action=install",
        "/setup?installation_id=5&setup_action=update",
        // "Request user authorization (OAuth) during installation": a code
        // without state. Never exchanged.
        "/callback?code=the-code&installation_id=5&setup_action=install",
    ] {
        let response = send(&router, "GET", uri, &[]).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(location(&response), "/login", "{uri}");
    }
    assert_eq!(count(&server, "POST", "/login/oauth/access_token").await, 0);

    let requested = send(&router, "GET", "/setup?setup_action=request", &[]).await;
    assert!(text(requested).await.contains("Installation requested"));
}

#[tokio::test]
async fn one_failing_installation_does_not_hide_the_others() {
    let server = github().await;
    // Installation 6 is suspended after sign-in; mounted first, so it wins.
    Mock::given(method("POST"))
        .and(path("/app/installations/6/access_tokens"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "message": "This installation has been suspended"
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    // octoauth then reads the installation and finds it suspended.
    let mut suspended = installation(6, "octocat", "User");
    suspended["suspended_at"] = json!("2026-11-01T00:00:00Z");
    suspended["suspended_by"] = account("hubot", 3, "User");
    mount(&server, "GET", "/app/installations/6", ok(suspended)).await;
    let router = web(&server).await;
    let session = sign_in(&router).await;
    let page = text(send(&router, "GET", "/", &[&session]).await).await;
    assert!(page.contains("acme/api"), "{page}");
    assert!(
        page.contains("Cannot list repositories: <code>gh:installation_suspended</code>"),
        "{page}"
    );
}
