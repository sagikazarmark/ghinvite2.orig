#![allow(dead_code)]
use ghinvite_github::{TokenSource, mocks::FixedTokens};
use ghinvite_web::config::apps::{AppStage, SignInApp, parse_installed_github_apps};
use std::sync::Arc;

/// The installed apps `repo` (App ID 123, client ID `Iv.fixture-repo`) and
/// `org` (456, `Iv.fixture-org`), signing with the fixture key, calling
/// `api.github.com` through `transport`. Installation tokens are fixed
/// (`Bearer ghs_fixed_{app}_{installation}_{generation}`, see
/// `FixedTokens`); app-authenticated requests carry a real app JWT.
pub fn github(transport: Arc<dyn ghinvite_github::HttpTransportDyn>) -> ghinvite_web::WebGithub {
    use base64::Engine as _;
    let key = format!(
        "fixture:{}",
        base64::engine::general_purpose::STANDARD
            .encode(include_str!("../../../../test-fixtures/github-app-rsa.pem"))
    );
    let installed = parse_installed_github_apps(
        AppStage::RepoAndOrg,
        ghinvite_github::mocks::UNREACHABLE_APP_BASE,
        |field| {
            Some(match field {
                "GH_REPO_APP_ID" => "123".into(),
                "GH_ORG_APP_ID" => "456".into(),
                "GH_REPO_CLIENT_ID" => "Iv.fixture-repo".into(),
                "GH_ORG_CLIENT_ID" => "Iv.fixture-org".into(),
                "GH_REPO_PRIVATE_KEYS" | "GH_ORG_PRIVATE_KEYS" => key.clone(),
                _ => return None,
            })
        },
    )
    .unwrap();
    let env = ghinvite_github::rest::RestEnv::new(
        transport.clone(),
        ghinvite_github::rest::DEFAULT_API_BASE,
        "p1",
    )
    .unwrap();
    let apps = installed.into_iter().map(|app| {
        let tokens = FixedTokens::new(app.app.id().get());
        (app, Arc::new(tokens) as Arc<dyn TokenSource>)
    });
    ghinvite_web::WebGithub {
        transport,
        apps: ghinvite_github::GithubApps::with_sources(env, apps).unwrap(),
    }
}

/// The webhooks of the `repo` app (App ID 123, secrets `repo_secrets`) and
/// the `org` app (456, `org-current;org-previous`), matching the installed
/// apps of `github`.
pub fn webhooks(repo_secrets: &str) -> Arc<ghinvite_config::apps::InstalledWebhooks> {
    use ghinvite_config::apps::{
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

/// Typed sign-in against `github`, a local GitHub stand-in.
pub fn signin_app(github: &wiremock::MockServer) -> Arc<SignInApp> {
    let mut app = SignInApp::new(
        "Iv1.signin",
        "signin-test-secret",
        "https://p1.test/oauth/callback".parse().unwrap(),
    );
    app.web_base_url = Some(github.uri());
    app.api_base_url = Some(github.uri());
    Arc::new(app)
}
