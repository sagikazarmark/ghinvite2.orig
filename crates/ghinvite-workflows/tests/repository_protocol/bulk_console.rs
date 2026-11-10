//! Joined native Console, toolkit reads, real Restate coordinator/request/budget/
//! delivery, SQLite and the ordinary GitHub write stub. No response-model owner.
use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use ghinvite_core::{
    account::{App, Installation, InstallationPermission},
    storage::{AppInstallationReadStorage, AppInstallationStorage},
};
use std::sync::atomic::AtomicU64;
use tower::ServiceExt;

#[path = "../common/web_apps.rs"]
mod web_apps;

struct ReadonlyBrowserGithub {
    actor: AtomicU64,
}
impl ReadonlyBrowserGithub {
    /// One scripted answer; sign-in and the installed apps' reads share it.
    fn respond(&self, method: &str, url: &str) -> (u16, Value) {
        match (method, url) {
            ("DELETE", "https://api.github.com/applications/Iv1.signin/token") => {
                (204, Value::Null)
            }
            ("POST", "https://github.com/login/oauth/access_token") => (
                200,
                json!({"access_token":"ghu_JOINED_BULK_PRIVATE","token_type":"bearer","scope":"read:user"}),
            ),
            ("GET", "https://api.github.com/user") => {
                let id = self.actor.load(Ordering::SeqCst);
                (
                    200,
                    json!({"id":id,"login":if id==7{"creator"}else{"submitting-admin"}}),
                )
            }
            ("GET", "https://api.github.com/orgs/acme/memberships/creator") => (
                200,
                json!({"user":{"id":7,"login":"creator"},"organization":{"id":100,"login":"acme"},"role":"admin","state":"active"}),
            ),
            ("GET", "https://api.github.com/orgs/acme/memberships/submitting-admin") => (
                200,
                json!({"user":{"id":9,"login":"submitting-admin"},"organization":{"id":100,"login":"acme"},"role":"admin","state":"active"}),
            ),
            ("GET", "https://api.github.com/installation/repositories?per_page=100") => (
                200,
                json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","owner":{"id":100,"login":"acme"}}]}),
            ),
            ("GET", "https://api.github.com/repos/acme/api") => (
                200,
                json!({"id":10,"full_name":"acme/api","owner":{"id":100,"login":"acme"}}),
            ),
            (
                "GET",
                "https://api.github.com/users/alice" | "https://api.github.com/users/alias",
            ) => (200, json!({"id":8,"login":"alice"})),
            ("GET", "https://api.github.com/users/missing") => (404, json!({})),
            ("GET", "https://api.github.com/repos/acme/api/collaborators/alice/permission") => (
                200,
                json!({"user":{"id":8,"login":"alice"},"role_name":"none"}),
            ),
            _ => panic!("unexpected browser GitHub operation: {method} {url}"),
        }
    }
}
/// The installed apps' reads, with fixed read-only installation tokens, and
/// sign-in (through `web_apps::signin_bridge`) with its own credentials.
impl ghinvite_github::HttpTransport for ReadonlyBrowserGithub {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<ghinvite_github::Response, ghinvite_github::TransportError> {
        let signin = request.url.starts_with("https://github.com/login/")
            || request.url == "https://api.github.com/user"
            || request
                .url
                .starts_with("https://api.github.com/applications/");
        assert!(
            signin
                || request
                    .headers
                    .get("authorization")
                    .is_some_and(|value| value.starts_with("Bearer ghs_fixed_")),
            "browser reads use installation tokens"
        );
        let (status, value) = self.respond(request.method.as_str(), &request.url);
        Ok(ghinvite_github::Response {
            status,
            headers: Default::default(),
            body: serde_json::to_vec(&value).unwrap(),
        })
    }
}
/// The web's installed apps (`repo` 123, `org` 456, as `web_apps::webhooks`)
/// reading through `github` with fixed tokens.
fn web_github(github: Arc<ReadonlyBrowserGithub>) -> ghinvite_web::WebGithub {
    use ghinvite_github::{AppSlot, InstalledApp, TokenSource, mocks};
    let transport: Arc<dyn ghinvite_github::HttpTransportDyn> = github;
    let env = ghinvite_github::rest::RestEnv::new(
        transport.clone(),
        ghinvite_github::rest::DEFAULT_API_BASE,
        "p1",
    )
    .unwrap();
    let apps = [(AppSlot::Repo, 123), (AppSlot::Org, 456)].map(|(slot, id)| {
        let installed = InstalledApp {
            slot,
            app: mocks::app(id, mocks::UNREACHABLE_APP_BASE),
            user_agent: format!("p1/{slot}"),
        };
        let tokens: Arc<dyn TokenSource> = Arc::new(mocks::FixedTokens::new(id));
        (installed, tokens)
    });
    ghinvite_web::WebGithub {
        transport,
        apps: ghinvite_github::GithubApps::with_sources(env, apps).unwrap(),
    }
}
async fn request(app: &axum::Router, cookie: &str, path: &str, body: Option<&str>) -> Response {
    let mut builder = Request::builder().uri(path).header("cookie", cookie);
    if body.is_some() {
        builder = builder
            .method("POST")
            .header("content-type", "application/x-www-form-urlencoded");
    }
    app.clone()
        .oneshot(
            builder
                .body(Body::from(body.unwrap_or_default().to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
fn cookie(response: &Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}
async fn html(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}
async fn sign_in(app: &axum::Router) -> String {
    let login = request(app, "", "/login", None).await;
    let csrf_state = login.headers()["location"]
        .to_str()
        .unwrap()
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let callback = request(
        app,
        &cookie(&login),
        &format!("/oauth/callback?code=fixture&state={csrf_state}"),
        None,
    )
    .await;
    assert_eq!(callback.status(), StatusCode::SEE_OTHER);
    cookie(&callback)
}
async fn csrf(app: &axum::Router, cookie: &str) -> String {
    html(request(app, cookie, "/", None).await)
        .await
        .split("name=\"csrf_token\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn native_console_review_to_real_delivery_retains_current_admin_and_one_write() {
    let f = BulkFixture::new().await;
    let repo = f
        .storage
        .app_installation(App::Repo, 1)
        .await
        .unwrap()
        .unwrap();
    let mut org = Installation::repo(repo.account);
    org.app = App::Org;
    org.account.installation_id = 2;
    org.permissions
        .insert("members".into(), InstallationPermission::Read);
    f.storage.put_app_installation(&org).await.unwrap();
    let github = Arc::new(ReadonlyBrowserGithub {
        actor: AtomicU64::new(7),
    });
    let mut config = ghinvite_web::WebConfig::for_local_dev_with_secret([7; 32]);
    config.bulk_enabled = true;
    config.signin_apps = Some(web_apps::signin_bridge(github.clone()).await);
    let state = ghinvite_web::AppState::new(
        f.storage.clone(),
        web_github(github.clone()),
        Arc::new(ghinvite_web::RestateClient::new(f.h.restate.ingress_url().to_owned()).unwrap()),
        config,
    )
    .with_authority_cache(Arc::new(
        ghinvite_web::authority_cache::MemoryAuthorityCache::default(),
    ))
    .with_edge_limits(Arc::new(
        ghinvite_web::rate_limit::MemoryRateLimiter::default(),
    ));
    let app = ghinvite_web::build_app(state, tower_sessions::MemoryStore::default()).layer(
        axum::Extension(ghinvite_web::rate_limit::TrustedClientIp::new(
            std::net::Ipv4Addr::LOCALHOST.into(),
        )),
    );
    let creator = sign_in(&app).await;
    let token = csrf(&app, &creator).await;
    let batch = BulkBatchId::new();
    let base = format!("/console/accounts/acme/bulk/{batch}");
    let response = request(
        &app,
        &creator,
        &format!("{base}/resolve"),
        Some(&format!(
            "csrf_token={token}&entries=alice%0Aalias%0Amissing&repo_ids=10&repo_grants=10%3Apush"
        )),
    )
    .await;
    let status = response.status();
    let review_html = html(response).await;
    assert_eq!(status, StatusCode::OK, "{review_html}");
    assert!(review_html.contains("Duplicate of row 1"));
    assert!(review_html.contains("Not found"));
    assert!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .all(|trace| trace.method != "PUT")
    );
    github.actor.store(9, Ordering::SeqCst);
    let submitter = sign_in(&app).await;
    let token = csrf(&app, &submitter).await;
    let response = request(
        &app,
        &submitter,
        &format!("{base}/rows/remove"),
        Some(&format!(
            "csrf_token={token}&operation_id={}&expected_revision=1&remove_rows=3",
            RequestId::new()
        )),
    )
    .await;
    let status = response.status();
    let edited = html(response).await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    let operation = RequestId::new();
    let body = format!("csrf_token={token}&operation_id={operation}&expected_revision=2");
    let response = request(&app, &submitter, &format!("{base}/submit"), Some(&body)).await;
    let status = response.status();
    let submitted = html(response).await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let q = BatchQuery {
        batch_id: batch,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 9,
        },
    };
    let progress = f.wait_delivery(&q).await;
    assert_eq!(progress.review.manifest.admin.user_id, 7);
    let submission = progress.review.submitted.as_ref().unwrap();
    assert_eq!(submission.submitted_by, 9);
    assert_eq!(submission.requests.len(), 1);
    let row = &submission.requests[0];
    assert_eq!(row.source.row, 1);
    assert_eq!(row.requester_id, 8);
    let stored = f
        .storage
        .get_invitation_request(row.request_id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.invitation_link_id.is_none());
    assert_eq!(stored.batch, Some(row.source));
    assert_eq!(stored.decided_by, Some(9));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM invitation_links")
            .fetch_one(&f.github.sql)
            .await
            .unwrap(),
        0
    );
    let response = request(&app, &submitter, &format!("{base}/submit"), Some(&body)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|trace| trace.method == "PUT")
            .count(),
        1
    );
    let changed = body.replace("expected_revision=2", "expected_revision=1");
    assert_eq!(
        request(&app, &submitter, &format!("{base}/submit"), Some(&changed))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.github
            .scenario
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|trace| trace.method == "PUT")
            .count(),
        1
    );
    f.finish().await;
}
