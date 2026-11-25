//! The GitHub base-URL seam (GE-09a) end to end: ghinvite-web's local
//! development configuration (`WebConfig::for_local_dev`, which
//! `ghinvite-web-server --local-dev` runs) reads `GITHUB_API_BASE`,
//! `GITHUB_WEB_BASE` and `GITHUB_AVATAR_BASE` from the process environment,
//! and every GitHub client it builds follows them to an embedded `octoemu`
//! emulator. No test-only constructor or field assignment points anything
//! at the emulator:
//!
//! - sign-in (`/login` → the emulator's authorize page → `/oauth/callback`):
//!   the web base, then the API base for `/user` and the token revoke, and
//!   the avatar base for the stored avatar URL;
//! - the installed repository App: an installation token minted at the API
//!   base and an installation read with it;
//! - avatar bytes from the avatar base.
//!
//! This binary holds one test: it sets process environment variables.
#![cfg(not(target_arch = "wasm32"))]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ghinvite_core::storage::RecordStorage;
use ghinvite_github::endpoints::{self, Repository};
use ghinvite_github::profiles::REPO_READ;
use ghinvite_github::{AppSlot, CallError};
use ghinvite_web::{AppState, WebConfig, WebGithub};
use octoemu::Emulator;
use octoemu::seed::{Access, App, Email, EmailVisibility, Installation, Repo, Seed, User};
use std::sync::Arc;
use tower::ServiceExt;
mod common;
use common::sign_in::session_cookie;

const PRIVATE_KEY: &str =
    include_str!("../../../libs/octoemu/crates/octoemu/tests/fixtures/rsa2048.pem");
const PUBLIC_KEY: &str =
    include_str!("../../../libs/octoemu/crates/octoemu/tests/fixtures/rsa2048.pub.pem");
const SIGNIN_CLIENT_ID: &str = "Iv1.seam-signin";
const SIGNIN_SECRET: &str = "seam-signin-secret";
const REPO_CLIENT_ID: &str = "Iv1.seam-repo";
const BASE_URL: &str = "https://p1.test";
const INSTALLATION: u64 = 77;

/// octocat (42) with the repository App (123) installed on `octocat/api`,
/// and the sign-in App calling back to [`BASE_URL`].
fn seed() -> Seed {
    Seed::new()
        .user(
            User::new(42, "octocat")
                .email_address(Email::new("octocat@example.com").primary(EmailVisibility::Public)),
        )
        .repo(Repo::new(10, "octocat", "api").private())
        .app(
            App::new(7, "p1-signin", SIGNIN_CLIENT_ID, SIGNIN_SECRET)
                .public_key(PUBLIC_KEY)
                .callback_url(format!("{BASE_URL}/oauth/callback")),
        )
        .app(
            App::new(123, "p1-repo", REPO_CLIENT_ID, "seam-repo-secret")
                .public_key(PUBLIC_KEY)
                .permission("administration", Access::Write)
                .permission("metadata", Access::Read),
        )
        .installation(Installation::new(INSTALLATION, 123, "octocat").selected(["octocat/api"]))
}

/// The local-development environment, GitHub's bases pointing at `emulator`.
fn configure(emulator: &Emulator) {
    let keys = format!(
        "seam:{}",
        base64::engine::general_purpose::STANDARD.encode(PRIVATE_KEY)
    );
    let variables = [
        (
            "P1_KEYRING",
            "kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7".to_owned(),
        ),
        ("GHINVITE_BASE_URL", BASE_URL.to_owned()),
        ("GH_SIGNIN_CLIENT_ID", SIGNIN_CLIENT_ID.to_owned()),
        ("GH_SIGNIN_CLIENT_SECRET", SIGNIN_SECRET.to_owned()),
        ("GH_REPO_APP_ID", "123".to_owned()),
        ("GH_REPO_CLIENT_ID", REPO_CLIENT_ID.to_owned()),
        ("GH_REPO_PRIVATE_KEYS", keys),
        ("GH_REPO_WEBHOOK_SECRETS", "seam-webhook-secret".to_owned()),
        ("GHINVITE_RESTATE_AUTH", "local-unauthenticated".to_owned()),
        // As an operator would write them: with and without a trailing slash.
        ("GITHUB_API_BASE", emulator.api_base().to_owned()),
        ("GITHUB_WEB_BASE", format!("{}/", emulator.web_base())),
        ("GITHUB_AVATAR_BASE", emulator.avatar_base().to_owned()),
    ];
    // SAFETY: this test binary runs a single test, and nothing else reads or
    // writes the environment while it is being set.
    unsafe {
        for name in [
            "P1_APP_STAGE",
            "GH_ORG_DELIVERY_ENABLED",
            "GH_REISSUE_ENABLED",
            "GH_BULK_ENABLED",
            "GH_ACCESS_REMOVAL_ENABLED",
            "GH_ORG_APP_INSTALL_URL",
            "P1_OPERATOR_IDS",
            "GHINVITE_RESTATE_API_KEY",
        ] {
            std::env::remove_var(name);
        }
        for (name, value) in variables {
            std::env::set_var(name, value);
        }
    }
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

#[tokio::test]
async fn local_dev_follows_the_github_base_variables_to_the_emulator() {
    let emulator = Emulator::start(seed()).await.unwrap();
    configure(&emulator);

    // The real configuration path, then the clients the binary builds from it.
    let config = WebConfig::for_local_dev().unwrap();
    assert_eq!(config.github.api(), format!("{}/", emulator.api_base()));
    assert_eq!(config.github.web(), format!("{}/", emulator.web_base()));
    assert_eq!(config.github.avatar(), emulator.avatar_base());
    let github = WebGithub::new(
        Arc::new(ghinvite_github::transport::ReqwestTransport::new().unwrap()),
        &config.github,
        config.github_apps.clone(),
    )
    .unwrap();
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let state = AppState::new(
        storage.clone(),
        github,
        common::sign_in::unreachable_restate(),
        config,
    );
    let app = common::build_app(state.clone(), tower_sessions::MemoryStore::default());

    // Sign-in: the browser is sent to the emulator's authorize page.
    let started = get(&app, "/login", None).await;
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
    let consented = browser
        .get(format!("{authorize}&login=octocat"))
        .send()
        .await
        .unwrap();
    assert_eq!(consented.status(), reqwest::StatusCode::FOUND);
    let back = consented.headers()["location"].to_str().unwrap();
    let callback = back
        .strip_prefix(BASE_URL)
        .unwrap_or_else(|| panic!("redirected back to {back}"))
        .to_owned();
    let finished = get(&app, &callback, Some(&cookie)).await;
    assert_eq!(finished.status(), StatusCode::SEE_OTHER);
    assert_eq!(finished.headers()["location"], "/");
    // The stored avatar URL is at the configured avatar base.
    let user = storage.get_user(42).await.unwrap().unwrap();
    assert_eq!(user.login, "octocat");
    assert_eq!(
        user.avatar_url.as_deref(),
        Some(format!("{}/u/42?s=80&v=4", emulator.avatar_base()).as_str())
    );

    // The installed repository App mints its token at the API base.
    let repo = state
        .github_apps
        .app(AppSlot::Repo)
        .unwrap()
        .read(INSTALLATION, &REPO_READ, |handle| async move {
            endpoints::get_repository(
                &handle.rest(),
                Repository {
                    owner: "octocat",
                    name: "api",
                },
            )
            .await
            .map_err(CallError::<std::convert::Infallible>::from)
        })
        .await
        .unwrap();
    assert_eq!((repo.id, repo.full_name.as_str()), (10, "octocat/api"));

    // Avatar bytes come from the avatar base.
    let avatar = ghinvite_github::avatar::get_at(
        state.github_transport.as_ref(),
        state.config.github.avatar(),
        ghinvite_github::ids::UserId::new(42),
    )
    .await
    .unwrap();
    assert_eq!(avatar.content_type, "image/png");

    // Every request reached the emulator: the OAuth pages, the API and the
    // avatar CDN, each with the credential GitHub would see.
    let routes: Vec<String> = emulator
        .calls()
        .into_iter()
        .filter_map(|call| call.route)
        .collect();
    assert_eq!(
        routes,
        [
            "GET /login/oauth/authorize",
            "POST /login/oauth/access_token",
            "GET /user",
            "DELETE /applications/{client_id}/token",
            "POST /app/installations/{installation_id}/access_tokens",
            "GET /repos/{owner}/{repo}",
            "GET /avatars/u/{id}",
        ]
    );
}
