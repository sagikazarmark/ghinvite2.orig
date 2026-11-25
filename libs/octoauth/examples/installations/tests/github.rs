//! The example against a local GitHub stand-in (wiremock): paging, a
//! suspended installation and a revoked token.
//!
//! `fixtures/rsa2048.pem` is the toolkit's test-only key (the same as
//! octoauth's and octotoken's).
#![cfg(not(target_arch = "wasm32"))]

use example_installations::{Error, Installation, Lister, Repository};
use octotoken::{AppTokenProvider, InstallationId, octoauth};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PRIVATE_KEY: &str = include_str!("fixtures/rsa2048.pem");

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

fn installation(id: u64, login: &str, kind: &str, selection: &str) -> Value {
    json!({
        "id": id, "app_id": 4, "account": account(login, id * 10, kind),
        "repository_selection": selection, "permissions": {"metadata": "read"}, "events": []
    })
}

fn repository(id: u64, full_name: &str, private: bool) -> Value {
    let name = full_name.split('/').nth(1).unwrap();
    json!({
        "id": id, "name": name, "full_name": full_name, "private": private,
        "url": format!("https://api.github.com/repos/{full_name}")
    })
}

fn created(token: &str) -> ResponseTemplate {
    ResponseTemplate::new(201).set_body_json(json!({
        "token": token, "expires_at": "2099-01-01T00:00:00Z",
        "permissions": {"metadata": "read"}, "repository_selection": "all"
    }))
}

/// Mount `GET route?page=<page>` (pages of 2) answering `body`.
async fn page(server: &MockServer, route: &str, page: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(route))
        .and(query_param("per_page", "2"))
        .and(query_param("page", page))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// App 4 installed on acme (5; three repositories, two pages) and octocat
/// (6; one selected repository). Installation 5 gets tokens `ghs_5_<n>`.
async fn github() -> MockServer {
    let server = MockServer::start().await;
    let installations = "/app/installations";
    page(
        &server,
        installations,
        "1",
        json!([
            installation(5, "acme", "Organization", "all"),
            installation(6, "octocat", "User", "selected"),
        ]),
    )
    .await;
    page(&server, installations, "2", json!([])).await;

    let repositories = "/installation/repositories";
    for token in ["ghs_5_1", "ghs_5_2"] {
        let bearer = format!("Bearer {token}");
        for (n, body) in [
            (
                "1",
                json!([
                    repository(101, "acme/api", false),
                    repository(102, "acme/web", true)
                ]),
            ),
            ("2", json!([repository(103, "acme/docs", false)])),
        ] {
            Mock::given(method("GET"))
                .and(path(repositories))
                .and(header("authorization", bearer.as_str()))
                .and(query_param("page", n))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"total_count": 3, "repository_selection": "all", "repositories": body}),
                ))
                .mount(&server)
                .await;
        }
    }
    Mock::given(method("GET"))
        .and(path(repositories))
        .and(header("authorization", "Bearer ghs_6_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 1, "repository_selection": "selected",
            "repositories": [repository(111, "octocat/dotfiles", false)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/5/access_tokens"))
        .respond_with(created("ghs_5_1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/5/access_tokens"))
        .respond_with(created("ghs_5_2"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/6/access_tokens"))
        .respond_with(created("ghs_6_1"))
        .mount(&server)
        .await;
    server
}

fn lister(server: &MockServer) -> Lister {
    let app = octoauth::App::builder(4, octoauth::SigningKey::from_pem(PRIVATE_KEY).unwrap())
        .base_url(server.uri())
        .build()
        .unwrap();
    Lister::new(AppTokenProvider::new(app)).unwrap().per_page(2)
}

fn repo(full_name: &str, private: bool) -> Repository {
    Repository {
        full_name: full_name.into(),
        private,
    }
}

async fn token_creations(server: &MockServer, installation: u64) -> usize {
    let path = format!("/app/installations/{installation}/access_tokens");
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path() == path)
        .count()
}

#[tokio::test]
async fn lists_every_installation_and_its_repositories_across_pages() {
    let server = github().await;
    let lister = lister(&server);

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
        ]
    );
    // App-level requests carry the App's credential, a JWT.
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/app/installations")
            .all(|r| r.headers["authorization"]
                .to_str()
                .unwrap()
                .starts_with("Bearer eyJ"))
    );

    let acme = lister.repositories(InstallationId::new(5)).await.unwrap();
    assert_eq!(
        acme,
        [
            repo("acme/api", false),
            repo("acme/web", true),
            repo("acme/docs", false),
        ]
    );
    // Two pages, one token.
    assert_eq!(token_creations(&server, 5).await, 1);

    let octocat = lister.repositories(InstallationId::new(6)).await.unwrap();
    assert_eq!(octocat, [repo("octocat/dotfiles", false)]);
}

#[tokio::test]
async fn a_suspended_installation_fails_with_a_fixed_code() {
    let server = MockServer::start().await;
    // GitHub refuses the token with 403; octoauth then reads the
    // installation and finds it suspended.
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "message": "This installation has been suspended"
        })))
        .mount(&server)
        .await;
    let mut suspended = installation(7, "globex", "Organization", "all");
    suspended["suspended_at"] = json!("2026-11-01T00:00:00Z");
    suspended["suspended_by"] = account("hubot", 3, "User");
    Mock::given(method("GET"))
        .and(path("/app/installations/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(suspended))
        .mount(&server)
        .await;

    let error = lister(&server)
        .repositories(InstallationId::new(7))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Token(_)), "{error:?}");
    assert_eq!(error.to_string(), "gh:installation_suspended");
}

#[tokio::test]
async fn a_revoked_token_is_replaced_once() {
    let server = github().await;
    // `ghs_5_1` is revoked: GitHub answers 401 to it (a higher priority
    // than the 200s above).
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .and(header("authorization", "Bearer ghs_5_1"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"message": "Bad credentials"})),
        )
        .with_priority(1)
        .mount(&server)
        .await;

    let repositories = lister(&server)
        .repositories(InstallationId::new(5))
        .await
        .unwrap();
    assert_eq!(repositories.len(), 3);
    // The token got a 401 and exactly one replacement.
    assert_eq!(token_creations(&server, 5).await, 2);
}
