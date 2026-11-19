//! The browser sign-in fixture for `tests/browser/emulator-signin.spec.mjs`
//! (GE-09b): `ghinvite-web-server --local-dev`'s configuration
//! (`WebConfig::for_local_dev`) with GitHub's bases pointing at an embedded
//! `octoemu` emulator, served on `127.0.0.1:4176`. A real browser signs in on
//! the emulator's authorize page, and the Content-Security-Policy's
//! `form-action` names the emulator's origin, so the expired-session form
//! POST bounce (`303 → /login → 303 → {GITHUB_WEB_BASE}/login/oauth/authorize`)
//! is not blocked.
//!
//! octocat (42) owns `octocat/api` (10), with the repository App installed as
//! 77; the Console reads its authority from the emulator. Restate is
//! unreachable: nothing here submits a command.
//!
//! This binary holds one (ignored) test: it sets process environment
//! variables. Run it with `-- --ignored`, as the Playwright config does.
#![cfg(not(target_arch = "wasm32"))]

use base64::Engine as _;
use chrono::Utc;
use ghinvite_core::{Account, AccountType, SelectedRepos};
use ghinvite_web::{AppState, WebConfig, WebGithub};
use octoemu::Emulator;
use octoemu::seed::{Access, App, Email, EmailVisibility, Installation, Repo, Seed, User};
use std::sync::Arc;
mod common;
use common::sign_in::ConsoleInstallationFixture as _;

const PRIVATE_KEY: &str =
    include_str!("../../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pem");
const PUBLIC_KEY: &str =
    include_str!("../../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pub.pem");
const ADDRESS: &str = "127.0.0.1:4176";
const BASE_URL: &str = "http://127.0.0.1:4176";
const SIGNIN_CLIENT_ID: &str = "Iv1.browser-signin";
const SIGNIN_SECRET: &str = "browser-signin-secret";
const REPO_CLIENT_ID: &str = "Iv1.browser-repo";
const INSTALLATION: u64 = 77;

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
            App::new(123, "p1-repo", REPO_CLIENT_ID, "browser-repo-secret")
                .public_key(PUBLIC_KEY)
                .permission("administration", Access::Write)
                .permission("metadata", Access::Read),
        )
        .installation(Installation::new(INSTALLATION, 123, "octocat").selected(["octocat/api"]))
}

/// The local-development environment, GitHub's bases pointing at `emulator`.
fn configure(emulator: &Emulator) {
    let keys = format!(
        "browser:{}",
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
        ("GH_REPO_WEBHOOK_SECRETS", "browser-webhook-secret".to_owned()),
        ("GHINVITE_RESTATE_AUTH", "local-unauthenticated".to_owned()),
        ("GITHUB_API_BASE", emulator.api_base().to_owned()),
        ("GITHUB_WEB_BASE", emulator.web_base().to_owned()),
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

#[tokio::test]
#[ignore = "Playwright fixture server: tests/browser/emulator-signin.config.mjs"]
async fn emulator_signin_browser_server() {
    let emulator = Emulator::start(seed()).await.unwrap();
    configure(&emulator);
    let config = WebConfig::for_local_dev().unwrap();
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
    storage
        .seed_console_installation(&Account {
            installation_id: INSTALLATION,
            account_id: 42,
            account_login: "octocat".into(),
            account_type: AccountType::User,
            installed_at: Utc::now(),
            uninstalled_at: None,
            selected_repos: SelectedRepos::Subset(vec![10]),
        })
        .await
        .unwrap();
    let state = AppState::new(
        storage,
        github,
        common::sign_in::unreachable_restate(),
        config,
    );
    let app = common::build_app(state, tower_sessions::MemoryStore::default())
        // Tells the browser test where the emulator's pages are.
        .route(
            "/fixture-github",
            axum::routing::get({
                let web_base = emulator.web_base().to_owned();
                move || async move { web_base }
            }),
        );
    common::browser_fixture::serve(ADDRESS, app).await;
}
