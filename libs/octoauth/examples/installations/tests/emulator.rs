//! The example against an embedded octoemu: the real App JWT and
//! installation-token checks, paging, a suspended installation and a
//! revoked token.
//!
//! `fixtures/rsa2048.pem` is the toolkit's test-only key (the same as
//! octotoken's and octoemu's); `rsa2048.pub.pem` is its public half.
#![cfg(not(target_arch = "wasm32"))]

use example_installations::{Error, Installation, Lister, Repository, repository_scope};
use octoemu::Emulator;
use octoemu::seed::{Access, App, Installation as Install, Org, Repo, Seed, User};
use octotoken::{AppTokenProvider, InstallationId, InstallationTokenProvider, octoauth};
use std::time::Duration;

const PRIVATE_KEY: &str = include_str!("fixtures/rsa2048.pem");
const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");

/// App 4 installed on acme (5; all three repositories), on octocat (6; one
/// of two, selected) and on globex (7; suspended).
fn seed() -> Seed {
    Seed::new()
        .user(User::new(1, "octocat"))
        .org(Org::new(2, "acme"))
        .org(Org::new(3, "globex"))
        .repo(Repo::new(101, "acme", "api"))
        .repo(Repo::new(102, "acme", "web").private())
        .repo(Repo::new(103, "acme", "docs"))
        .repo(Repo::new(111, "octocat", "dotfiles"))
        .repo(Repo::new(112, "octocat", "secret").private())
        .repo(Repo::new(121, "globex", "gadgets"))
        .app(
            App::new(4, "lister", "Iv23.lister", "lister-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("metadata", Access::Read),
        )
        .installation(Install::new(5, 4, "acme"))
        .installation(Install::new(6, 4, "octocat").selected(["octocat/dotfiles"]))
        .installation(Install::new(7, 4, "globex").suspended_at(chrono::Utc::now()))
}

/// The provider (a clone shares its cache) and a lister on pages of 2.
fn lister(emulator: &Emulator) -> (AppTokenProvider, Lister) {
    let app = octoauth::App::builder(4, octoauth::SigningKey::from_pem(PRIVATE_KEY).unwrap())
        .base_url(emulator.api_base())
        .build()
        .unwrap();
    let tokens = AppTokenProvider::new(app);
    (tokens.clone(), Lister::new(tokens).unwrap().per_page(2))
}

fn repository(full_name: &str, private: bool) -> Repository {
    Repository {
        full_name: full_name.into(),
        private,
    }
}

fn token_creations(emulator: &Emulator, installation: u64) -> usize {
    let path = format!("/app/installations/{installation}/access_tokens");
    emulator
        .calls()
        .iter()
        .filter(|call| call.method == "POST" && call.path == path)
        .count()
}

#[tokio::test]
async fn lists_every_installation_and_its_repositories_across_pages() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let (_, lister) = lister(&emulator);

    let installations = lister.installations().await.unwrap();
    let summary: Vec<_> = installations
        .iter()
        .map(
            |Installation {
                 id,
                 account,
                 kind,
                 selection,
             }| {
                (
                    id.get(),
                    account.as_str(),
                    kind.as_str(),
                    selection.as_deref(),
                )
            },
        )
        .collect();
    assert_eq!(
        summary,
        [
            (5, "acme", "Organization", Some("all")),
            (6, "octocat", "User", Some("selected")),
            (7, "globex", "Organization", Some("all")),
        ]
    );

    let mut acme = lister.repositories(InstallationId::new(5)).await.unwrap();
    acme.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    assert_eq!(
        acme,
        [
            repository("acme/api", false),
            repository("acme/docs", false),
            repository("acme/web", true),
        ]
    );
    // Two pages, one token.
    assert_eq!(token_creations(&emulator, 5), 1);

    let octocat = lister.repositories(InstallationId::new(6)).await.unwrap();
    assert_eq!(octocat, [repository("octocat/dotfiles", false)]);
}

#[tokio::test]
async fn a_suspended_installation_fails_with_a_fixed_code() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let (_, lister) = lister(&emulator);

    let error = lister
        .repositories(InstallationId::new(7))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Token(_)), "{error:?}");
    assert_eq!(error.to_string(), "gh:installation_suspended");
}

#[tokio::test]
async fn a_revoked_token_is_replaced_once() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let (tokens, lister) = lister(&emulator);
    let installation = InstallationId::new(5);

    // Cache a token, then revoke it behind the provider's back.
    let token = tokens
        .token(installation, &repository_scope())
        .await
        .unwrap();
    let revoked = reqwest::Client::new()
        .delete(format!("{}/installation/token", emulator.api_base()))
        .bearer_auth(token.expose_secret())
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "example-installations-test")
        .header("x-github-api-version", octoauth::DEFAULT_API_VERSION)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 204);
    emulator.advance_clock(Duration::from_secs(1));

    let repositories = lister.repositories(installation).await.unwrap();
    assert_eq!(repositories.len(), 3);
    // The cached token got a 401 and exactly one replacement.
    assert_eq!(token_creations(&emulator, 5), 2);
}
