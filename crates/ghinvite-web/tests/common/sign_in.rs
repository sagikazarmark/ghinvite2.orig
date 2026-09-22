//! Signing a browser in through the real `/login` → `/oauth/callback` routes.
//!
//! Tests obtain a session the way a browser does, never by writing session
//! records: the scripted GitHub answers the token exchange and the `/user`
//! lookup, and the returned cookie carries whatever the routes stored.
#![allow(dead_code)] // Not every test binary that shares these helpers uses all of them.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use ghinvite_core::storage::InstallationStorage;
use ghinvite_core::{Account, AccountType, SelectedRepos};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::{AppState, RestateClient, WebConfig, build_app};
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

/// The GitHub account the scripted OAuth exchange signs in as.
#[derive(Clone, Copy, Debug)]
pub struct GithubUser<'a> {
    pub login: &'a str,
    pub id: u64,
    pub access_token: &'a str,
    /// An organization (login, id) whose active admin membership is scripted
    /// right after sign-in, for the first Console authorization check.
    pub admin_of: Option<(&'a str, u64)>,
}

impl<'a> GithubUser<'a> {
    pub const fn new(login: &'a str, id: u64) -> Self {
        Self {
            login,
            id,
            access_token: "u_xxx",
            admin_of: None,
        }
    }

    pub const fn with_access_token(self, access_token: &'a str) -> Self {
        Self {
            access_token,
            ..self
        }
    }

    pub const fn admin_of(self, organization: &'a str, organization_id: u64) -> Self {
        Self {
            admin_of: Some((organization, organization_id)),
            ..self
        }
    }
}

pub const OCTOCAT: GithubUser<'static> = GithubUser::new("octocat", 42);

/// `octocat`, an active admin of the `acme` organization.
pub const ACME_ADMIN: GithubUser<'static> = OCTOCAT.admin_of("acme", 9001);

/// The GitHub calls one sign-in makes, in order: the token exchange, the
/// `/user` lookup and, for an organization admin, one membership check.
pub fn oauth_expectations(user: GithubUser<'_>) -> Vec<Expectation> {
    let mut expectations = vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: format!(
                    r#"{{"access_token":"{}","token_type":"bearer","scope":"read:user read:org"}}"#,
                    user.access_token
                )
                .into_bytes(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": user.id, "login": user.login}),
        ),
    ];
    if let Some((organization, organization_id)) = user.admin_of {
        expectations.push(Expectation::ok_json(
            Method::Get,
            format!("https://api.github.com/user/memberships/orgs/{organization}"),
            serde_json::json!({"role": "admin", "state": "active", "organization": {"id": organization_id}}),
        ));
    }
    expectations
}

/// The session cookie a response sets, or `fallback` when it sets none.
pub fn session_cookie(response: &axum::response::Response, fallback: Option<String>) -> String {
    response
        .headers()
        .get("set-cookie")
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .or(fallback)
        .expect("session cookie available")
}

/// The OAuth `state` parameter of a GitHub authorize redirect.
pub fn state_from_location(location: &str) -> &str {
    location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
}

/// Drive `/login` → `/oauth/callback` and return the signed-in cookie.
pub async fn sign_in(app: &axum::Router) -> String {
    sign_in_with_cookie(app, None).await
}

/// Sign in again from a browser that already holds `cookie`.
pub async fn sign_in_with_cookie(app: &axum::Router, cookie: Option<String>) -> String {
    let mut request = Request::builder().uri("/login");
    if let Some(cookie) = &cookie {
        request = request.header("cookie", cookie);
    }
    let started = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::SEE_OTHER);
    let cookie = session_cookie(&started, cookie);
    let location = started.headers().get("location").unwrap().to_str().unwrap();
    let state = state_from_location(location);

    let finished = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(finished.status(), StatusCode::SEE_OTHER);
    session_cookie(&finished, Some(cookie))
}

/// The `acme` organization installation most Console tests administer.
pub fn acme_installation() -> Account {
    Account {
        installation_id: 77,
        account_id: 9001,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: Utc::now(),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}

/// A Restate ingress nothing listens on, for tests that never reach it.
pub fn unreachable_restate() -> Arc<RestateClient> {
    Arc::new(RestateClient::new("http://127.0.0.1:9").unwrap())
}

/// An app over in-memory storage holding `installations`, with GitHub
/// scripted by `expectations` (sign-in first, then whatever the test drives)
/// and Restate reached through `restate`, already signed in.
pub async fn signed_in_app(
    installations: &[Account],
    expectations: Vec<Expectation>,
    restate: Arc<RestateClient>,
) -> (axum::Router, String) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    for installation in installations {
        storage.insert_installation(installation).await.unwrap();
    }
    let state = AppState::new(
        storage,
        Arc::new(MockTransport::scripted(expectations)),
        restate,
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    (app, cookie)
}
