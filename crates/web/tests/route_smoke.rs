//! Route-existence smoke tests. Each route from spec §11 must be registered
//! and respond with a non-500. Some return 501 (Plans 5–6 fill them in);
//! some return 200/302; auth-required ones return 302→/login.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use web::{AppState, RestateClient, RestateCommands, WebConfig, build_app};

async fn build_test_app() -> axum::Router {
    use github::mocks::MockTransport;
    let storage: Arc<dyn storage::Storage> =
        Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(vec![]));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let commands = Arc::new(RestateCommands::new(restate));
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

#[tokio::test]
async fn health_returns_ok() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn login_redirects_to_github_authorize() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("https://github.com/login/oauth/authorize?"));
    assert!(location.contains("client_id="));
    assert!(location.contains("state="));
}

#[tokio::test]
async fn install_redirects_to_install_url() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/install")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("https://github.com/apps/"));
}

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

#[tokio::test]
async fn logout_clears_session_and_redirects_home() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/");
}

#[tokio::test]
async fn static_styles_returns_css() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/static/styles.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/css"));
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ghinvite"));
    assert!(text.contains("--color-primary"));
    assert!(text.contains("oklch("));
    assert!(text.contains("ghinvite-dark"));
    assert!(text.contains("color-scheme:dark"));
    assert!(text.contains(".app-header"));
    assert!(text.contains(".theme-toggle"));
    assert!(text.contains(".home-hero"));
    assert!(text.contains(".home-feature-item"));
    assert!(text.contains(".home-feature-kicker"));
    assert!(text.contains(".dashboard-sidebar"));
    assert!(text.contains(".mac-panel"));
    assert!(text.contains(".compact-table"));
}

#[tokio::test]
async fn home_returns_html() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/html"));
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ghinvite"));
    assert!(text.contains("home-hero"));
    assert!(text.contains("home-hero-action"));
    assert!(text.contains("Sign in with GitHub"));
    assert!(text.contains("Controlled GitHub invitations without access guesswork"));
    assert!(text.contains("home-features"));
    assert!(text.contains("Controlled share links"));
    assert!(text.contains("Review requests before invitations"));
    assert!(text.contains("Operational history"));
    assert!(!text.contains(concat!("class=\"he", "ro")));
}

#[tokio::test]
async fn public_unknown_get_returns_html_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/missing-page")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("The link may be incorrect or no longer available."));
    assert!(text.contains("Go home"));
    assert!(text.contains("app-header"));
    assert!(!text.contains("dashboard-frame"));
}

#[tokio::test]
async fn missing_static_asset_returns_plain_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/static/missing.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Not Found");
}

#[tokio::test]
async fn favicon_returns_plain_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/favicon.ico")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Not Found");
}

#[tokio::test]
async fn dashboard_routes_return_501() {
    let app = build_test_app().await;
    for path in ["/accounts/acme/audit"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED, "path = {path}");
    }
}

// NOTE: The requester_id ownership guard in the `pending` handler is a security-critical
// check. It is exercised only at the unit level (handler code review) in Plan 6 and will
// get an integration test in Plan 8 (tests/invitation_ownership.rs).

#[tokio::test]
async fn invitation_landing_unknown_slug_returns_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "GET /i/AAAAAAAAAAAAAAAA"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("The link may be incorrect or no longer available."));
    assert!(text.contains("Go home"));
    assert!(text.contains("mac-panel"));
    assert!(!text.contains("expired"));
    assert!(!text.contains("revoked"));
}

#[tokio::test]
async fn invitation_unknown_nested_route_returns_recipient_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA/anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("The link may be incorrect or no longer available."));
    assert!(text.contains("Go home"));
    assert!(text.contains("mac-panel"));
    assert!(!text.contains("home-hero"));
}

#[tokio::test]
async fn invitation_request_form_unauthenticated_redirects_to_login() {
    // GET /i/{slug}/request is now implemented:
    // unauthenticated request → 303 to /login with return_to.
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA/request")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "GET /i/.../request unauthenticated should redirect to /login"
    );
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/login"),
        "expected redirect to /login, got {location}"
    );
    assert!(
        location.contains("return_to="),
        "expected return_to in redirect, got {location}"
    );
}

#[tokio::test]
async fn invitation_pending_unauthenticated_redirects_to_login() {
    // GET /i/{slug}/pending/{request_id} is now implemented:
    // unauthenticated (any ULID, valid or not) → 303 to /login first.
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA/pending/01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "GET /i/.../pending/... unauthenticated should redirect to /login"
    );
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(
        location.contains("/login"),
        "expected redirect to /login, got {location}"
    );
    assert!(
        location.contains("return_to="),
        "expected return_to in redirect location, got {location}"
    );
    assert!(
        location.contains("/i/AAAAAAAAAAAAAAAA/pending/01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        "expected return_to to point to the pending URL, got {location}"
    );
}

#[tokio::test]
async fn webhook_route_rejects_missing_signature() {
    // A POST without an X-Hub-Signature-256 header must be rejected with 401.
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_bad_hmac_returns_401() {
    // A POST with a malformed/wrong HMAC signature must be rejected with 401.
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .header(
                    "x-hub-signature-256",
                    "sha256=deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                )
                .body(Body::from(r#"{"action":"ping"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Verifies that a correctly-signed webhook payload returns 200.
/// Uses empty secret key — matches `for_local_dev` default where
/// `GHINVITE_WEBHOOK_SECRET` env var is unset.
#[tokio::test]
async fn webhook_valid_hmac_returns_ok() {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let body = r#"{"action":"ping"}"#;
    let mut mac = Hmac::<Sha256>::new_from_slice(b"").unwrap();
    mac.update(body.as_bytes());
    let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .header("x-github-event", "ping")
                .header("x-github-delivery", "test-delivery-id")
                .header("x-hub-signature-256", &sig)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
