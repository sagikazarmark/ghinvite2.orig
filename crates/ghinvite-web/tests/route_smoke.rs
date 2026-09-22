//! Route-existence smoke tests. Each route from spec §11 must be registered
//! and respond with a non-500. Some return 200/302; auth-required ones return
//! 302→/login.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghinvite_web::{AppState, WebConfig, build_app};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn encoded_return_to(path: &str) -> String {
    url::form_urlencoded::byte_serialize(path.as_bytes()).collect()
}

/// Every `<script>` in rendered HTML must be the shared static bundle; inline
/// script bodies are blocked by the CSP.
fn assert_only_external_app_script(html: &str) {
    let external = "<script src=\"/static/app.js\"></script>";
    assert!(html.contains(external), "missing {external}");
    assert_eq!(
        html.matches("<script").count(),
        html.matches(external).count(),
        "rendered HTML contains an inline <script> block"
    );
}

fn csp_header(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get("content-security-policy")
        .map(|v| v.to_str().unwrap().to_string())
}

async fn build_test_app() -> axum::Router {
    build_test_app_with_webhook_secret(b"test-webhook-secret").await
}

async fn build_test_app_with_webhook_secret(webhook_secret: &[u8]) -> axum::Router {
    use ghinvite_github::mocks::MockTransport;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(vec![]));
    let config = WebConfig {
        webhook_secret: webhook_secret.to_vec(),
        ..WebConfig::for_local_dev_with_secret([7; 32])
    };
    let state = AppState::new(
        storage,
        transport,
        std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        config,
    );
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
    assert_eq!(
        csp_header(&resp),
        None,
        "non-HTML responses must not carry a CSP"
    );
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
async fn logout_get_is_not_a_mutation() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/logout?return_to=/console/accounts/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
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
    assert_eq!(
        csp_header(&resp),
        None,
        "static assets must not carry a CSP"
    );
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
    assert!(text.contains(".console-page .app-header"));
    assert!(text.contains("position:sticky"));
    assert!(text.contains("top:0"));
    assert!(text.contains("z-index:20"));
    assert!(!text.contains(".app-header{position:sticky"));
    assert!(text.contains(".theme-toggle"));
    assert!(text.contains(".console-frame"));
    assert!(text.contains("height:calc(100vh - 3rem)"));
    assert!(text.contains("overflow:hidden"));
    assert!(text.contains(".console-sidebar"));
    assert!(text.contains("height:100%"));
    assert!(text.contains(".console-main"));
    assert!(text.contains("overflow:auto"));
    assert!(text.contains(".mac-panel"));
    assert!(text.contains(".compact-table"));
    assert!(!text.contains(".home-hero"));
    assert!(!text.contains(".home-feature-item"));
    assert!(!text.contains(".home-feature-kicker"));
}

#[tokio::test]
async fn static_app_js_returns_javascript() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/static/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        csp_header(&resp),
        None,
        "static assets must not carry a CSP"
    );
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(ct, "text/javascript; charset=utf-8");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    // Theme sync.
    assert!(text.contains("'ghinvite-theme'"));
    assert!(text.contains("window.localStorage.getItem"));
    assert!(text.contains("data-theme-toggle"));
    // Invitation-code shortcut.
    assert!(text.contains("[data-open-invitation-code]"));
    assert!(text.contains("[data-invitation-code-input]"));
    assert!(text.contains("Enter an invitation code first."));
}

#[tokio::test]
async fn home_returns_html() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        csp_header(&resp).as_deref(),
        Some(ghinvite_web::middleware::csp::CONTENT_SECURITY_POLICY)
    );
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/html"));
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert_only_external_app_script(&text);
    assert!(text.contains("ghinvite"));
    assert!(text.contains("Sign in with GitHub"));
    assert!(text.contains("GitHub repository access"));
    assert!(!text.contains("console-page"));
    assert!(!text.contains("home-features"));
    assert!(!text.contains("home-feature-item"));
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
    assert_eq!(
        csp_header(&resp).as_deref(),
        Some(ghinvite_web::middleware::csp::CONTENT_SECURITY_POLICY),
        "HTML error pages carry the CSP too"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert_only_external_app_script(&text);
    assert!(text.contains("Page not found"));
    assert!(text.contains("The link may be incorrect or no longer available."));
    assert!(text.contains("Go home"));
    assert!(text.contains("app-header"));
    assert!(!text.contains("console-page"));
    assert!(!text.contains("console-frame"));
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
    assert_eq!(csp_header(&resp), None);
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
async fn invitation_landing_unauthenticated_redirects_to_login() {
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
        StatusCode::SEE_OTHER,
        "GET /i/AAAAAAAAAAAAAAAA"
    );
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to("/i/AAAAAAAAAAAAAAAA")
        )
    );
}

#[tokio::test]
async fn invitation_unknown_nested_route_unauthenticated_redirects_to_login() {
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

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to("/i/AAAAAAAAAAAAAAAA/anything")
        )
    );
}

#[tokio::test]
async fn invitation_unknown_nested_post_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/i/AAAAAAAAAAAAAAAA/anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to("/i/AAAAAAAAAAAAAAAA/anything")
        )
    );
}

#[tokio::test]
async fn invitation_request_form_unauthenticated_redirects_to_login() {
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
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to("/i/AAAAAAAAAAAAAAAA/request")
        )
    );
}

#[tokio::test]
async fn invitation_pending_unauthenticated_redirects_to_login() {
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
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to("/i/AAAAAAAAAAAAAAAA/pending/01ARZ3NDEKTSV4RRFFQ69G5FAV")
        )
    );
}

#[tokio::test]
async fn console_audit_route_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        "/login?return_to=%2Fconsole%2Faccounts%2Facme%2Faudit"
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
    // A well-formed but incorrect HMAC signature must be rejected with 401.
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .header(
                    "x-hub-signature-256",
                    "sha256=0000000000000000000000000000000000000000000000000000000000000000",
                )
                .body(Body::from(r#"{"action":"ping"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// A verified ping is acknowledged without contacting Restate.
#[tokio::test]
async fn webhook_valid_hmac_returns_no_content() {
    let app = build_test_app().await;
    let resp = app.oneshot(signed_webhook("ping", "{}")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

fn signed_webhook(event: &str, body: &'static str) -> Request<Body> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(b"test-webhook-secret").unwrap();
    mac.update(body.as_bytes());
    Request::builder()
        .method("POST")
        .uri("/webhooks/github")
        .header("content-type", "application/json")
        .header("x-github-event", event)
        .header("x-github-delivery", "test-delivery-id")
        .header(
            "x-hub-signature-256",
            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn webhook_requires_delivery_event_and_json_content_type() {
    let app = build_test_app().await;
    for header in ["x-github-delivery", "x-github-event", "content-type"] {
        let mut request = signed_webhook("ping", "{}");
        request.headers_mut().remove(header);
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{header}");
    }
    let mut request = signed_webhook("ping", "{}");
    request.headers_mut().insert(
        "content-type",
        "application/x-www-form-urlencoded".parse().unwrap(),
    );
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn webhook_rejects_malformed_signature() {
    let mut request = signed_webhook("ping", "{}");
    request
        .headers_mut()
        .insert("x-hub-signature-256", "sha256=invalid".parse().unwrap());
    let response = build_test_app().await.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_acknowledges_unhandled_events_and_actions() {
    let app = build_test_app().await;
    for (event, body) in [
        ("future_event", "{}"),
        ("installation", r#"{"action":"created"}"#),
    ] {
        let response = app
            .clone()
            .oneshot(signed_webhook(event, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn webhook_handler_failure_returns_bare_500() {
    // Unknown installation fails the storage lookup without a Restate call.
    let request = signed_webhook(
        "installation_repositories",
        r#"{"action":"added","installation":{"id":77},"repository_selection":"selected","repositories_added":[],"repositories_removed":[]}"#,
    );
    let response = build_test_app().await.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn webhook_without_configured_secret_is_unavailable() {
    let response = build_test_app_with_webhook_secret(b"")
        .await
        .oneshot(signed_webhook("ping", "{}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn webhook_rejects_body_over_two_mib() {
    let mut request = signed_webhook("ping", "{}");
    *request.body_mut() = Body::from(vec![b' '; 2 * 1024 * 1024 + 1]);
    let response = build_test_app().await.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// --- island assets (native dev only; Workers Static Assets serve these) -----

async fn build_test_app_with_assets(island_assets_dir: Option<std::path::PathBuf>) -> axum::Router {
    use ghinvite_github::mocks::MockTransport;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let transport: Arc<dyn ghinvite_github::HttpTransport> =
        Arc::new(MockTransport::scripted(vec![]));
    let config = WebConfig {
        island_assets_dir,
        ..WebConfig::for_local_dev_with_secret([7; 32])
    };
    let state = AppState::new(
        storage,
        transport,
        std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        config,
    );
    let session_store = tower_sessions::MemoryStore::default();
    // Mount `/assets` exactly as the native binary does, so this still covers
    // the real wiring now that `ServeDir` lives in `ghinvite-web-server`.
    let assets = ghinvite_web_server::island_assets(&state.config);
    ghinvite_web::build_app_with(state, session_store, assets)
}

/// A throwaway assets directory shaped like `dist/public/assets` after the
/// island build: the stable loader plus one hashed file of each kind.
fn island_assets_fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ghinvite-island-assets-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ghinvite-island.js"),
        "import \"/assets/island-dxh0123.js\";\n",
    )
    .unwrap();
    std::fs::write(dir.join("island-dxh0123.js"), "export {};\n").unwrap();
    std::fs::write(dir.join("island_bg-dxh0123.wasm"), b"\0asm\x01\0\0\0").unwrap();
    dir
}

#[tokio::test]
async fn island_assets_are_served_from_the_configured_directory() {
    let dir = island_assets_fixture();
    let app = build_test_app_with_assets(Some(dir.clone())).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/assets/ghinvite-island.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(csp_header(&resp), None, "assets must not carry a CSP");
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("javascript"), "content-type was {ct}");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), b"import \"/assets/island-dxh0123.js\";\n");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/assets/island_bg-dxh0123.wasm")
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
        .unwrap()
        .to_string();
    assert_eq!(ct, "application/wasm");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn missing_island_asset_is_a_plain_404() {
    // The module tag on the new-link page points here; before the bundle is
    // built the browser must get a 404 (and keep the plain form), not an HTML
    // page or an error.
    let dir = island_assets_fixture();
    let app = build_test_app_with_assets(Some(dir.clone())).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/assets/does-not-exist.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(csp_header(&resp), None);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn island_assets_route_is_absent_without_a_directory() {
    let app = build_test_app_with_assets(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/assets/ghinvite-island.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Falls through to the ordinary not-found handling.
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn local_dev_config_defaults_island_assets_to_dist_public_assets() {
    let cfg = WebConfig::for_local_dev_with_secret([7; 32]);
    assert_eq!(
        cfg.island_assets_dir.as_deref(),
        Some(std::path::Path::new("dist/public/assets"))
    );
}
