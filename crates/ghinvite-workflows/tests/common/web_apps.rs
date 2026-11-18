#![allow(dead_code)]
use ghinvite_web::config::apps::SignInApp;
use std::sync::Arc;

/// The sign-in app's client ID: tokens are revoked at
/// `DELETE {base}/applications/Iv1.signin/token`.
pub const SIGNIN_CLIENT_ID: &str = "Iv1.signin";
/// The sign-in app's client secret.
pub const SIGNIN_CLIENT_SECRET: &str = "signin-test-secret";
/// The sign-in app's callback; GitHub redirects the browser back to it.
pub const SIGNIN_CALLBACK: &str = "https://p1.test/oauth/callback";

/// Sign-in against `base`, a local GitHub stand-in serving both the web
/// flow (`/login/oauth/access_token`) and the API (`/user`, revocation).
pub fn signin_app(base: &str) -> Arc<SignInApp> {
    let mut app = SignInApp::new(
        SIGNIN_CLIENT_ID,
        SIGNIN_CLIENT_SECRET,
        SIGNIN_CALLBACK.parse().unwrap(),
    );
    app.web_base_url = Some(base.to_owned());
    app.api_base_url = Some(base.to_owned());
    Arc::new(app)
}

/// The sign-in App (789) for the GitHub emulator, with [`signin_app`]'s
/// client ID, secret and callback, so the emulator's OAuth web flow signs a
/// seeded user in (`/login/oauth/authorize?…&login=<user>`).
pub fn signin_seed() -> octoemu::seed::Seed {
    octoemu::seed::Seed::new().app(
        octoemu::seed::App::new(789, "p1-signin", SIGNIN_CLIENT_ID, SIGNIN_CLIENT_SECRET)
            .public_key(ghinvite_e2e::github::PUBLIC_KEY)
            .callback_url(SIGNIN_CALLBACK),
    )
}

/// Sign-in against a local stand-in that forwards each request to `github`
/// with GitHub's own URL (`https://github.com/login/...`, otherwise
/// `https://api.github.com/...`) and answers with `github`'s answer; a
/// transport error is a 502. It serves for the rest of the runtime.
pub async fn signin_bridge(github: Arc<dyn ghinvite_github::HttpTransportDyn>) -> Arc<SignInApp> {
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let forward = move |request: axum::extract::Request| {
        let github = github.clone();
        async move {
            let (parts, body) = request.into_parts();
            let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
            let method = match parts.method.as_str() {
                "GET" => ghinvite_github::Method::Get,
                "POST" => ghinvite_github::Method::Post,
                "DELETE" => ghinvite_github::Method::Delete,
                other => panic!("sign-in bridge: unexpected method {other}"),
            };
            let target = parts.uri.path_and_query().unwrap().as_str();
            let origin = if target.starts_with("/login/") {
                "https://github.com"
            } else {
                "https://api.github.com"
            };
            let mut forwarded = ghinvite_github::Request::new(method, format!("{origin}{target}"));
            for (name, value) in &parts.headers {
                if !matches!(
                    name.as_str(),
                    "host" | "content-length" | "connection" | "accept-encoding"
                ) {
                    forwarded = forwarded.header(name.as_str(), value.to_str().unwrap());
                }
            }
            forwarded.body = (!body.is_empty()).then(|| body.to_vec());
            match github.send(forwarded).await {
                Ok(answer) => (
                    axum::http::StatusCode::from_u16(answer.status).unwrap(),
                    answer.body,
                )
                    .into_response(),
                Err(_) => axum::http::StatusCode::BAD_GATEWAY.into_response(),
            }
        }
    };
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(forward))
            .await
            .unwrap();
    });
    signin_app(&base)
}

/// The webhooks of the `repo` app (App ID 123, secrets `repo_secrets`) and
/// the `org` app (456, `org-current;org-previous`), the App IDs of
/// `ghinvite_e2e::installed_apps`.
pub fn webhooks(repo_secrets: &str) -> Arc<ghinvite_web::config::apps::InstalledWebhooks> {
    use ghinvite_web::config::apps::{
        InstalledSlot, InstalledWebhook, InstalledWebhooks, WebhookSecrets,
    };
    Arc::new(InstalledWebhooks::new([
        InstalledWebhook {
            slot: InstalledSlot::Repo,
            app_id: 123,
            secrets: WebhookSecrets::parse(repo_secrets).unwrap(),
        },
        InstalledWebhook {
            slot: InstalledSlot::Org,
            app_id: 456,
            secrets: WebhookSecrets::parse("org-current;org-previous").unwrap(),
        },
    ]))
}

pub fn installation(
    app_id: u64,
    id: u64,
    account_id: u64,
    login: &str,
    kind: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": id, "app_id": app_id, "account": {"id": account_id, "login": login, "type": kind},
        "created_at": "2026-05-20T11:50:00Z", "updated_at": "2026-05-20T11:50:00Z", "suspended_at": null,
        "repository_selection": "selected", "permissions": {"administration":"write","metadata":"read"}
    })
}
