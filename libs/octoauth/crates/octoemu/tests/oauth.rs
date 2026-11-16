//! The OAuth web flow and user tokens (GE-05) over HTTP: the authorize page
//! (auto-consent, the HTML page and its form, errors), the token endpoint
//! (PKCE, codes, refresh, form and JSON answers, the error matrix), `/user`,
//! `/user/emails`, the `/applications` routes, the token caps, the admin
//! mint and the invitee's `/user/repository_invitations` routes.
#![cfg(not(target_arch = "wasm32"))]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use octoemu::Emulator;
use octoemu::seed::{
    Access, App, Email, EmailVisibility, Installation, Org, Repo, RepositoryInvitation, Role, Seed,
    User, Webhook,
};
use octoemu::webhooks::Dispatch;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");
const CLIENT_ID: &str = "Iv23.signin";
const SECRET: &str = "signin-client-secret";
const CALLBACK: &str = "https://app.test/oauth/callback";
const OTHER_CALLBACK: &str = "https://app.test/other/callback";

fn start_2030() -> DateTime<Utc> {
    "2030-01-01T00:00:00Z".parse().unwrap()
}

/// App 4 (sign-in, expiring tokens, email_addresses: read), App 5
/// (non-expiring, no user permission), App 6 (suspended). octocat has two
/// addresses (primary public); hubot a private primary; unverified an
/// unverified primary.
fn seed() -> Seed {
    Seed::new()
        .start_time(start_2030())
        .user(
            User::new(1, "octocat")
                .name("The Octocat")
                .email_address(Email::new("octocat@example.com").primary(EmailVisibility::Public))
                .email_address(Email::new("old@example.com").unverified())
                .noreply_email(),
        )
        .user(
            User::new(2, "hubot")
                .email_address(Email::new("hubot@example.com").primary(EmailVisibility::Private)),
        )
        .user(
            User::new(3, "unverified").email_address(
                Email::new("new@example.com")
                    .primary(EmailVisibility::Public)
                    .unverified(),
            ),
        )
        .user(User::new(9, "robot").bot())
        .org(Org::new(20, "acme"))
        .repo(Repo::new(101, "acme", "widgets").private())
        .app(
            App::new(4, "signin", CLIENT_ID, SECRET)
                .public_key(PUBLIC_KEY)
                .callback_url(CALLBACK)
                .callback_url(OTHER_CALLBACK)
                .callback_url("http://127.0.0.1:8080/cb")
                .permission("metadata", Access::Read)
                .permission("administration", Access::Write)
                .user_permission("email_addresses", Access::Read)
                .event("member"),
        )
        .app(
            App::new(5, "forever", "Iv23.forever", "forever-secret")
                .public_key(PUBLIC_KEY)
                .callback_url(CALLBACK)
                .non_expiring_user_tokens(),
        )
        .app(
            App::new(6, "frozen", "Iv23.frozen", "frozen-secret")
                .public_key(PUBLIC_KEY)
                .callback_url(CALLBACK)
                .suspended(),
        )
        .installation(Installation::new(7, 4, "acme"))
}

fn client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn verifier() -> String {
    "verifier-".repeat(6)
}

fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The query of a URL, decoded.
fn query(url: &str) -> BTreeMap<String, String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// `GET /login/oauth/authorize` with `params`; the response.
async fn authorize(emulator: &Emulator, params: &[(&str, &str)]) -> reqwest::Response {
    client()
        .get(format!("{}/login/oauth/authorize", emulator.web_base()))
        .query(params)
        .send()
        .await
        .unwrap()
}

/// Authorizes as `login` with PKCE and returns the code.
async fn code_for(emulator: &Emulator, client_id: &str, login: &str) -> String {
    let challenge = challenge_of(&verifier());
    let response = authorize(
        emulator,
        &[
            ("client_id", client_id),
            ("redirect_uri", CALLBACK),
            ("state", "st4te"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("login", login),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    assert!(location.starts_with(CALLBACK), "{location}");
    let pairs = query(&location);
    assert_eq!(pairs["state"], "st4te");
    pairs["code"].clone()
}

/// `POST /login/oauth/access_token` with a form body, asking for JSON.
async fn exchange(emulator: &Emulator, form: &[(&str, &str)]) -> Value {
    let response = client()
        .post(format!("{}/login/oauth/access_token", emulator.web_base()))
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_str(&response.text().await.unwrap()).unwrap()
}

async fn exchange_code(emulator: &Emulator, code: &str) -> Value {
    exchange(
        emulator,
        &[
            ("client_id", CLIENT_ID),
            ("client_secret", SECRET),
            ("code", code),
            ("redirect_uri", CALLBACK),
            ("code_verifier", &verifier()),
        ],
    )
    .await
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

async fn api(
    emulator: &Emulator,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut request = client().request(method, format!("{}{path}", emulator.api_base()));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).unwrap());
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap();
    Reply {
        status,
        headers,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
    }
}

/// `/applications/{client_id}/{what}` with the App's Basic credentials.
async fn applications(
    emulator: &Emulator,
    method: Method,
    what: &str,
    secret: &str,
    token: &str,
) -> Reply {
    let response = client()
        .request(
            method,
            format!("{}/applications/{CLIENT_ID}/{what}", emulator.api_base()),
        )
        .basic_auth(CLIENT_ID, Some(secret))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&json!({"access_token": token})).unwrap())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap();
    Reply {
        status,
        headers,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
    }
}

async fn admin(emulator: &Emulator, method: Method, path: &str, body: Option<Value>) -> Reply {
    let mut request = client().request(method, format!("{}{path}", emulator.admin_base()));
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).unwrap());
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    Reply {
        status,
        headers,
        body: serde_json::from_str(&response.text().await.unwrap()).unwrap_or(Value::Null),
    }
}

#[tokio::test]
async fn the_full_flow_with_pkce() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    assert_eq!(code.len(), 20);
    let answer = exchange_code(&emulator, &code).await;
    let token = answer["access_token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("ghu_") && token.len() == 40, "{answer}");
    assert_eq!(answer["expires_in"], 28800);
    assert!(
        answer["refresh_token"]
            .as_str()
            .unwrap()
            .starts_with("ghr_")
    );
    assert_eq!(answer["refresh_token_expires_in"], 15897600);
    assert_eq!(answer["scope"], "");
    assert_eq!(answer["token_type"], "bearer");

    let user = api(&emulator, Method::GET, "/user", Some(&token), None).await;
    assert_eq!(user.status, StatusCode::OK);
    assert_eq!(user.body["id"], 1);
    assert_eq!(user.body["login"], "octocat");
    assert_eq!(user.body["type"], "User");
    assert_eq!(user.body["name"], "The Octocat");
    assert_eq!(user.body["email"], "octocat@example.com");
    assert_eq!(user.body["created_at"], "2030-01-01T00:00:00Z");
    assert!(
        user.body["avatar_url"]
            .as_str()
            .unwrap()
            .ends_with("/avatars/u/1?v=4")
    );

    let emails = api(&emulator, Method::GET, "/user/emails", Some(&token), None).await;
    assert_eq!(emails.status, StatusCode::OK);
    assert_eq!(
        emails.body,
        json!([
            {"email": "octocat@example.com", "primary": true, "verified": true, "visibility": "public"},
            {"email": "old@example.com", "primary": false, "verified": false, "visibility": null},
            {"email": "1+octocat@users.noreply.github.com", "primary": false, "verified": true, "visibility": null},
        ])
    );

    // The call log knows who called, never the token.
    let calls = emulator.calls();
    let last = calls.last().unwrap();
    assert_eq!(last.route.as_deref(), Some("GET /user/emails"));
    let identity = serde_json::to_value(&last.identity).unwrap();
    assert_eq!(identity["kind"], "user_token");
    assert_eq!(identity["app_id"], 4);
    assert_eq!(identity["login"], "octocat");
    assert!(!serde_json::to_string(&calls).unwrap().contains(&token[4..]));

    let revoked = applications(&emulator, Method::DELETE, "token", SECRET, &token).await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let after = api(&emulator, Method::GET, "/user", Some(&token), None).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert_eq!(after.body["message"], "Bad credentials");
    let again = applications(&emulator, Method::DELETE, "token", SECRET, &token).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    // The refresh token died with it.
    let refreshed = exchange(
        &emulator,
        &[
            ("client_id", CLIENT_ID),
            ("client_secret", SECRET),
            ("grant_type", "refresh_token"),
            ("refresh_token", answer["refresh_token"].as_str().unwrap()),
        ],
    )
    .await;
    assert_eq!(refreshed["error"], "bad_refresh_token");
}

#[tokio::test]
async fn the_token_endpoint_error_matrix() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let bad_code = json!({
        "error": "bad_verification_code",
        "error_description": "The code passed is incorrect or expired.",
        "error_uri": "/apps/managing-oauth-apps/troubleshooting-oauth-app-access-token-request-errors/#bad-verification-code",
    });
    let v = verifier();

    // Reuse.
    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    assert!(exchange_code(&emulator, &code).await["access_token"].is_string());
    assert_eq!(exchange_code(&emulator, &code).await, bad_code);

    // Expiry after 10 minutes on the virtual clock.
    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    emulator.advance_clock(Duration::from_secs(600));
    assert_eq!(exchange_code(&emulator, &code).await, bad_code);

    // An unknown code, another App's code.
    assert_eq!(
        exchange_code(&emulator, "0123456789abcdefghij").await,
        bad_code
    );
    let forever_code = authorize(
        &emulator,
        &[("client_id", "Iv23.forever"), ("login", "octocat")],
    )
    .await;
    let forever_code = query(forever_code.headers()["location"].to_str().unwrap())["code"].clone();
    assert_eq!(exchange_code(&emulator, &forever_code).await, bad_code);

    // A redirect_uri other than the code's (registered or not).
    for other in [OTHER_CALLBACK, "https://evil.test/cb"] {
        let code = code_for(&emulator, CLIENT_ID, "octocat").await;
        let answer = exchange(
            &emulator,
            &[
                ("client_id", CLIENT_ID),
                ("client_secret", SECRET),
                ("code", &code),
                ("redirect_uri", other),
                ("code_verifier", &v),
            ],
        )
        .await;
        assert_eq!(answer["error"], "redirect_uri_mismatch", "{other}");
        assert_eq!(
            answer["error_description"],
            "The redirect_uri MUST match the registered callback URL for this application."
        );
    }

    // A wrong verifier, a missing verifier.
    for verifier in [Some("wrong-verifier-".repeat(3)), None] {
        let code = code_for(&emulator, CLIENT_ID, "octocat").await;
        let mut form = vec![
            ("client_id", CLIENT_ID.to_owned()),
            ("client_secret", SECRET.to_owned()),
            ("code", code),
        ];
        if let Some(verifier) = verifier {
            form.push(("code_verifier", verifier));
        }
        let form: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(exchange(&emulator, &form).await, bad_code);
    }

    // A verifier for a code issued without a challenge.
    let plain = authorize(&emulator, &[("client_id", CLIENT_ID), ("login", "octocat")]).await;
    let plain = query(plain.headers()["location"].to_str().unwrap())["code"].clone();
    let answer = exchange(
        &emulator,
        &[
            ("client_id", CLIENT_ID),
            ("client_secret", SECRET),
            ("code", &plain),
            ("code_verifier", &v),
        ],
    )
    .await;
    assert_eq!(answer, bad_code);

    // A wrong secret, an unknown client.
    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    for (id, secret) in [
        (CLIENT_ID, "wrong"),
        ("Iv23.nobody", SECRET),
        (CLIENT_ID, ""),
    ] {
        let answer = exchange(
            &emulator,
            &[
                ("client_id", id),
                ("client_secret", secret),
                ("code", &code),
                ("code_verifier", &v),
            ],
        )
        .await;
        assert_eq!(
            answer,
            json!({
                "error": "incorrect_client_credentials",
                "error_description": "The client_id and/or client_secret passed are incorrect.",
                "error_uri": "/apps/managing-oauth-apps/troubleshooting-oauth-app-access-token-request-errors/#incorrect-client-credentials",
            })
        );
    }
    // The wrong secret did not use the code up.
    assert!(exchange_code(&emulator, &code).await["access_token"].is_string());

    // An unverified primary address.
    let code = code_for(&emulator, CLIENT_ID, "unverified").await;
    let answer = exchange_code(&emulator, &code).await;
    assert_eq!(answer["error"], "unverified_user_email");
    assert_eq!(
        answer["error_description"],
        "The user must have a verified primary email."
    );

    // Another grant type.
    let answer = exchange(
        &emulator,
        &[
            ("client_id", CLIENT_ID),
            ("client_secret", SECRET),
            ("grant_type", "password"),
        ],
    )
    .await;
    assert_eq!(answer["error"], "unsupported_grant_type");
}

#[tokio::test]
async fn answers_in_form_encoding_unless_json_is_accepted() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    let response = client()
        .post(format!("{}/login/oauth/access_token", emulator.web_base()))
        .form(&[
            ("client_id", CLIENT_ID),
            ("client_secret", SECRET),
            ("code", &code),
            ("code_verifier", &verifier()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/x-www-form-urlencoded; charset=utf-8"
    );
    let text = response.text().await.unwrap();
    let pairs = query(&format!("http://x/?{text}"));
    assert!(pairs["access_token"].starts_with("ghu_"));
    assert_eq!(pairs["expires_in"], "28800");
    assert_eq!(pairs["token_type"], "bearer");
    assert_eq!(pairs["scope"], "");

    // A JSON body works too; errors come form-encoded without Accept.
    let response = client()
        .post(format!("{}/login/oauth/access_token", emulator.web_base()))
        .header("content-type", "application/json")
        .body(
            serde_json::to_vec(
                &json!({"client_id": CLIENT_ID, "client_secret": "nope", "code": "x"}),
            )
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(
        text.starts_with("error=incorrect_client_credentials&error_description="),
        "{text}"
    );

    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    let response = client()
        .post(format!("{}/login/oauth/access_token", emulator.web_base()))
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .body(
            serde_json::to_vec(&json!({
                "client_id": CLIENT_ID,
                "client_secret": SECRET,
                "code": code,
                "code_verifier": verifier(),
            }))
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    let body: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert!(body["access_token"].as_str().unwrap().starts_with("ghu_"));
}

#[tokio::test]
async fn refresh_rotates_and_kills_the_old_pair() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    let first = exchange_code(&emulator, &code).await;
    let refresh = |token: String| {
        let emulator = &emulator;
        async move {
            exchange(
                emulator,
                &[
                    ("client_id", CLIENT_ID),
                    ("client_secret", SECRET),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &token),
                ],
            )
            .await
        }
    };
    let second = refresh(first["refresh_token"].as_str().unwrap().to_owned()).await;
    let new_token = second["access_token"].as_str().unwrap();
    assert!(new_token.starts_with("ghu_"));
    assert_ne!(second["refresh_token"], first["refresh_token"]);
    assert_eq!(second["expires_in"], 28800);

    let old = api(
        &emulator,
        Method::GET,
        "/user",
        first["access_token"].as_str(),
        None,
    )
    .await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    let new = api(&emulator, Method::GET, "/user", Some(new_token), None).await;
    assert_eq!(new.status, StatusCode::OK);

    let reused = refresh(first["refresh_token"].as_str().unwrap().to_owned()).await;
    assert_eq!(reused["error"], "bad_refresh_token");
    assert_eq!(
        reused["error_description"],
        "The refresh token passed is incorrect or expired."
    );

    // Access tokens expire after 8 hours; refresh tokens outlive them.
    emulator.advance_clock(Duration::from_secs(28_800));
    let expired = api(&emulator, Method::GET, "/user", Some(new_token), None).await;
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    let third = refresh(second["refresh_token"].as_str().unwrap().to_owned()).await;
    assert!(third["access_token"].as_str().unwrap().starts_with("ghu_"));
}

#[tokio::test]
async fn an_app_without_expiring_tokens_gets_no_refresh_token() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let response = authorize(
        &emulator,
        &[("client_id", "Iv23.forever"), ("login", "hubot")],
    )
    .await;
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    let pairs = query(&location);
    assert!(!pairs.contains_key("state"), "no state sent, none returned");
    let answer = exchange(
        &emulator,
        &[
            ("client_id", "Iv23.forever"),
            ("client_secret", "forever-secret"),
            ("code", &pairs["code"]),
        ],
    )
    .await;
    let token = answer["access_token"].as_str().unwrap();
    assert_eq!(
        answer.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["access_token", "scope", "token_type"]
    );
    emulator.advance_clock(Duration::from_secs(365 * 24 * 3600));
    let user = api(&emulator, Method::GET, "/user", Some(token), None).await;
    assert_eq!(user.status, StatusCode::OK);
    assert_eq!(user.body["login"], "hubot");
    // A private primary address is not on the profile.
    assert_eq!(user.body["email"], Value::Null);
    // Without email_addresses: read, no emails.
    let emails = api(&emulator, Method::GET, "/user/emails", Some(token), None).await;
    assert_eq!(emails.status, StatusCode::FORBIDDEN);
    assert_eq!(
        emails.body["message"],
        "Resource not accessible by integration"
    );
    assert_eq!(
        emails.headers["x-accepted-github-permissions"],
        "email_addresses=read"
    );
}

#[tokio::test]
async fn authorize_errors_redirect_as_documented() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let error_uri = "/apps/building-integrations/setting-up-and-registering-oauth-apps/troubleshooting-authorization-request-errors/";

    // An unregistered redirect_uri: to the first registered callback.
    let response = authorize(
        &emulator,
        &[
            ("client_id", CLIENT_ID),
            ("redirect_uri", "https://evil.test/cb"),
            ("state", "s1"),
            ("login", "octocat"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response.headers()["location"].to_str().unwrap();
    assert!(location.starts_with(&format!("{CALLBACK}?")), "{location}");
    let pairs = query(location);
    assert_eq!(pairs["error"], "redirect_uri_mismatch");
    assert_eq!(
        pairs["error_description"],
        "The redirect_uri MUST match the registered callback URL for this application."
    );
    assert_eq!(
        pairs["error_uri"],
        format!("{error_uri}#redirect-uri-mismatch")
    );
    assert_eq!(pairs["state"], "s1");
    assert!(!pairs.contains_key("code"));

    // A loopback callback matches on any port.
    let response = authorize(
        &emulator,
        &[
            ("client_id", CLIENT_ID),
            ("redirect_uri", "http://127.0.0.1:43210/cb"),
            ("login", "octocat"),
        ],
    )
    .await;
    let location = response.headers()["location"].to_str().unwrap();
    assert!(
        location.starts_with("http://127.0.0.1:43210/cb?code="),
        "{location}"
    );

    // Denied: everyone, then one user.
    let denied = admin(&emulator, Method::POST, "/oauth/deny", None).await;
    assert_eq!(denied.body["deny_all"], true);
    let response = authorize(
        &emulator,
        &[
            ("client_id", CLIENT_ID),
            ("state", "s2"),
            ("login", "octocat"),
        ],
    )
    .await;
    let pairs = query(response.headers()["location"].to_str().unwrap());
    assert_eq!(pairs["error"], "access_denied");
    assert_eq!(
        pairs["error_description"],
        "The user has denied your application access."
    );
    assert_eq!(pairs["error_uri"], format!("{error_uri}#access-denied"));
    assert_eq!(pairs["state"], "s2");
    admin(&emulator, Method::DELETE, "/oauth/deny", None).await;
    admin(
        &emulator,
        Method::POST,
        "/oauth/deny",
        Some(json!({"login": "hubot"})),
    )
    .await;
    let hubot = authorize(&emulator, &[("client_id", CLIENT_ID), ("login", "hubot")]).await;
    assert_eq!(
        query(hubot.headers()["location"].to_str().unwrap())["error"],
        "access_denied"
    );
    let octocat = authorize(&emulator, &[("client_id", CLIENT_ID), ("login", "octocat")]).await;
    assert!(query(octocat.headers()["location"].to_str().unwrap()).contains_key("code"));
    let unknown = admin(
        &emulator,
        Method::POST,
        "/oauth/deny",
        Some(json!({"login": "nobody"})),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);

    // A suspended App.
    let response = authorize(
        &emulator,
        &[
            ("client_id", "Iv23.frozen"),
            ("state", "s3"),
            ("login", "octocat"),
        ],
    )
    .await;
    let pairs = query(response.headers()["location"].to_str().unwrap());
    assert_eq!(pairs["error"], "application_suspended");
    assert_eq!(
        pairs["error_description"],
        "Your application has been suspended. Contact support@github.com."
    );
    assert_eq!(pairs["state"], "s3");

    // Shown, not redirected: an unknown client, bad PKCE parameters.
    let unknown = authorize(&emulator, &[("client_id", "Iv23.nobody")]).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    for pkce in [
        vec![("code_challenge", "x".repeat(43))],
        vec![
            ("code_challenge", "x".repeat(43)),
            ("code_challenge_method", "plain".to_owned()),
        ],
        vec![
            ("code_challenge", "short".to_owned()),
            ("code_challenge_method", "S256".to_owned()),
        ],
    ] {
        let mut params = vec![
            ("client_id", CLIENT_ID.to_owned()),
            ("login", "octocat".to_owned()),
        ];
        params.extend(pkce);
        let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let response = authorize(&emulator, &params).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{params:?}");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
    }
}

#[tokio::test]
async fn the_page_lists_users_and_its_form_authorizes() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let challenge = challenge_of(&verifier());
    let params = [
        ("client_id", CLIENT_ID),
        ("redirect_uri", CALLBACK),
        ("state", "pick<me>"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    let page = authorize(&emulator, &params).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()["content-type"], "text/html; charset=utf-8");
    let html = page.text().await.unwrap();
    for login in ["octocat", "hubot", "unverified"] {
        assert!(html.contains(&format!("Authorize as {login}")), "{login}");
    }
    assert!(!html.contains("robot"), "bots cannot sign in");
    assert!(html.contains("pick&lt;me&gt;"), "escaped");
    assert!(html.contains("method=\"post\" action=\"/login/oauth/authorize\""));

    // `login=` with select_account still shows the page.
    let mut with_prompt = params.to_vec();
    with_prompt.extend([("login", "octocat"), ("prompt", "select_account")]);
    assert_eq!(
        authorize(&emulator, &with_prompt).await.status(),
        StatusCode::OK
    );
    // As does login= with auto-consent off.
    emulator.set_auto_consent(false);
    let mut with_login = params.to_vec();
    with_login.push(("login", "octocat"));
    assert_eq!(
        authorize(&emulator, &with_login).await.status(),
        StatusCode::OK
    );

    // The form's choice.
    let mut form = params.to_vec();
    form.extend([("login", "hubot"), ("authorize", "1")]);
    let chosen = client()
        .post(format!("{}/login/oauth/authorize", emulator.web_base()))
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(chosen.status(), StatusCode::FOUND);
    let pairs = query(chosen.headers()["location"].to_str().unwrap());
    assert_eq!(pairs["state"], "pick<me>");
    let answer = exchange_code(&emulator, &pairs["code"]).await;
    let user = api(
        &emulator,
        Method::GET,
        "/user",
        answer["access_token"].as_str(),
        None,
    )
    .await;
    assert_eq!(user.body["login"], "hubot");

    // Cancel.
    let mut form = params.to_vec();
    form.push(("cancel", "1"));
    let cancelled = client()
        .post(format!("{}/login/oauth/authorize", emulator.web_base()))
        .form(&form)
        .send()
        .await
        .unwrap();
    let pairs = query(cancelled.headers()["location"].to_str().unwrap());
    assert_eq!(pairs["error"], "access_denied");
}

#[tokio::test]
async fn ten_tokens_per_user_and_an_hourly_limit() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let mut tokens = Vec::new();
    for _ in 0..11 {
        let code = code_for(&emulator, CLIENT_ID, "octocat").await;
        tokens.push(
            exchange_code(&emulator, &code).await["access_token"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        // Stay under the hourly limit.
        emulator.advance_clock(Duration::from_secs(400));
    }
    let oldest = api(&emulator, Method::GET, "/user", Some(&tokens[0]), None).await;
    assert_eq!(
        oldest.status,
        StatusCode::UNAUTHORIZED,
        "the 11th revoked the 1st"
    );
    for token in &tokens[1..] {
        let user = api(&emulator, Method::GET, "/user", Some(token), None).await;
        assert_eq!(user.status, StatusCode::OK);
    }

    let limit = admin(
        &emulator,
        Method::POST,
        "/oauth",
        Some(json!({"user_tokens_per_hour": 2})),
    )
    .await;
    assert_eq!(limit.body["user_tokens_per_hour"], 2);
    emulator.advance_clock(Duration::from_secs(3600));
    // Two new tokens within the hour redirect straight back; the third
    // authorization asks to authorize again.
    for _ in 0..2 {
        let code = code_for(&emulator, CLIENT_ID, "hubot").await;
        assert!(exchange_code(&emulator, &code).await["access_token"].is_string());
    }
    let third = authorize(&emulator, &[("client_id", CLIENT_ID), ("login", "hubot")]).await;
    assert_eq!(third.status(), StatusCode::OK);
    let html = third.text().await.unwrap();
    assert!(html.contains("authorize again"), "{html}");
    assert!(html.contains("Authorize as hubot"));
    // An hour later it redirects again.
    emulator.advance_clock(Duration::from_secs(3600));
    let later = authorize(&emulator, &[("client_id", CLIENT_ID), ("login", "hubot")]).await;
    assert_eq!(later.status(), StatusCode::FOUND);
}

#[tokio::test]
async fn the_applications_routes() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let token = emulator.mint_user_token("octocat", 4).unwrap();

    let checked = applications(&emulator, Method::POST, "token", SECRET, &token).await;
    assert_eq!(checked.status, StatusCode::OK);
    assert_eq!(checked.body["token"], token);
    assert_eq!(checked.body["token_last_eight"], token[token.len() - 8..]);
    assert_eq!(checked.body["app"]["client_id"], CLIENT_ID);
    assert_eq!(checked.body["user"]["login"], "octocat");
    assert_eq!(checked.body["expires_at"], "2030-01-01T08:00:00Z");
    assert_eq!(checked.body["scopes"], json!([]));

    // Wrong secret: 401; another App's path: 404; missing token: 422.
    let wrong = applications(&emulator, Method::POST, "token", "nope", &token).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let other = client()
        .post(format!(
            "{}/applications/Iv23.forever/token",
            emulator.api_base()
        ))
        .basic_auth(CLIENT_ID, Some(SECRET))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&json!({"access_token": token})).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::NOT_FOUND);
    let missing = client()
        .delete(format!(
            "{}/applications/{CLIENT_ID}/token",
            emulator.api_base()
        ))
        .basic_auth(CLIENT_ID, Some(SECRET))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&json!({})).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // A bearer token is not the App's credential.
    let bearer = api(
        &emulator,
        Method::DELETE,
        &format!("/applications/{CLIENT_ID}/token"),
        Some(&token),
        Some(json!({"access_token": token})),
    )
    .await;
    assert_eq!(bearer.status, StatusCode::UNAUTHORIZED);

    // Reset: the old token dies, the new one works.
    let reset = applications(&emulator, Method::PATCH, "token", SECRET, &token).await;
    assert_eq!(reset.status, StatusCode::OK);
    let new_token = reset.body["token"].as_str().unwrap().to_owned();
    assert_ne!(new_token, token);
    let old = api(&emulator, Method::GET, "/user", Some(&token), None).await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    let new = api(&emulator, Method::GET, "/user", Some(&new_token), None).await;
    assert_eq!(new.status, StatusCode::OK);

    // The grant: every token of the App for that user.
    let another = emulator.mint_user_token("octocat", 4).unwrap();
    let elsewhere = emulator.mint_user_token("octocat", 5).unwrap();
    let hubot = emulator.mint_user_token("hubot", 4).unwrap();
    let grant = applications(&emulator, Method::DELETE, "grant", SECRET, &another).await;
    assert_eq!(grant.status, StatusCode::NO_CONTENT);
    for (token, status) in [
        (&new_token, StatusCode::UNAUTHORIZED),
        (&another, StatusCode::UNAUTHORIZED),
        (&elsewhere, StatusCode::OK),
        (&hubot, StatusCode::OK),
    ] {
        let user = api(&emulator, Method::GET, "/user", Some(token), None).await;
        assert_eq!(user.status, status);
    }
    let client_calls: Vec<_> = emulator
        .calls()
        .into_iter()
        .filter(|c| c.route.as_deref() == Some("DELETE /applications/{client_id}/grant"))
        .collect();
    assert_eq!(
        serde_json::to_value(&client_calls[0].identity).unwrap(),
        json!({"kind": "client", "app_id": 4})
    );
}

#[tokio::test]
async fn admin_minted_tokens_and_the_state_dump() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let minted = admin(
        &emulator,
        Method::POST,
        "/tokens/user",
        Some(json!({"login": "octocat", "app_id": 4})),
    )
    .await;
    assert_eq!(minted.status, StatusCode::CREATED);
    let token = minted.body["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("ghu_"));
    assert!(
        minted.body["refresh_token"]
            .as_str()
            .unwrap()
            .starts_with("ghr_")
    );
    assert_eq!(minted.body["expires_at"], "2030-01-01T08:00:00Z");

    let pat = admin(
        &emulator,
        Method::POST,
        "/tokens/user",
        Some(json!({"login": "hubot", "kind": "pat", "scopes": ["repo"]})),
    )
    .await;
    assert_eq!(pat.status, StatusCode::CREATED);
    let pat = pat.body["token"].as_str().unwrap().to_owned();
    assert!(pat.starts_with("ghp_"));
    let user = api(&emulator, Method::GET, "/user", Some(&pat), None).await;
    assert_eq!(user.body["login"], "hubot");
    // Without user or user:email, no emails.
    let emails = api(&emulator, Method::GET, "/user/emails", Some(&pat), None).await;
    assert_eq!(emails.status, StatusCode::FORBIDDEN);
    assert_eq!(
        emails.headers["x-accepted-oauth-scopes"],
        "user, user:email"
    );
    let scoped = emulator
        .mint_personal_access_token("hubot", &["user:email"])
        .unwrap();
    let emails = api(&emulator, Method::GET, "/user/emails", Some(&scoped), None).await;
    assert_eq!(emails.status, StatusCode::OK);
    assert_eq!(emails.body[0]["visibility"], "private");

    for bad in [
        json!({"login": "nobody", "app_id": 4}),
        json!({"login": "octocat", "app_id": 99}),
        json!({"login": "octocat"}),
        json!({"login": "octocat", "kind": "pat", "app_id": 4}),
    ] {
        let rejected = admin(&emulator, Method::POST, "/tokens/user", Some(bad.clone())).await;
        assert_eq!(rejected.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }

    let state = emulator.state();
    let text = state.to_string();
    for secret in [&token[4..], &pat[4..], &scoped[4..]] {
        assert!(!text.contains(secret));
    }
    let records = state["user_tokens"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["kind"], "user_token");
    assert_eq!(records[0]["app_id"], 4);
    assert_eq!(records[0]["login"], "octocat");
    assert_eq!(records[1]["kind"], "personal_access_token");
    assert_eq!(records[1]["scopes"], json!(["repo"]));
    assert_eq!(records[1]["expires_at"], Value::Null);
    assert_eq!(state["refresh_tokens"].as_array().unwrap().len(), 1);
    assert_eq!(state["oauth"]["auto_consent"], true);
    assert_eq!(state["oauth"]["user_tokens_per_hour"], 10);
}

#[tokio::test]
async fn user_tokens_reach_what_user_and_app_both_can() {
    let emulator = Emulator::start(seed().user(User::new(30, "member")).collaborator(
        octoemu::seed::Collaborator::new("acme/widgets", "member", Role::Push),
    ))
    .await
    .unwrap();
    let member = emulator.mint_user_token("member", 4).unwrap();
    let outsider = emulator.mint_user_token("octocat", 4).unwrap();
    let elsewhere = emulator.mint_user_token("member", 5).unwrap();
    let get = |token: String| {
        let emulator = &emulator;
        async move {
            api(
                emulator,
                Method::GET,
                "/repos/acme/widgets",
                Some(&token),
                None,
            )
            .await
            .status
        }
    };
    assert_eq!(get(member).await, StatusCode::OK);
    assert_eq!(get(outsider).await, StatusCode::NOT_FOUND);
    // App 5 lacks metadata: read.
    assert_eq!(get(elsewhere).await, StatusCode::FORBIDDEN);
    // Installation-only routes refuse user tokens.
    let token = emulator.mint_user_token("member", 4).unwrap();
    let installation = api(
        &emulator,
        Method::GET,
        "/installation/repositories",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(installation.status, StatusCode::UNAUTHORIZED);
    // A refresh token is no credential.
    let unknown = api(&emulator, Method::GET, "/user", Some("ghr_nothing"), None).await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
}

/// Webhook bodies a local receiver got.
#[derive(Clone, Default)]
struct Received(Arc<Mutex<Vec<(String, Value)>>>);

async fn receiver() -> (String, Received) {
    let received = Received::default();
    let sink = received.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let sink = sink.clone();
                async move {
                    let event = headers["x-github-event"].to_str().unwrap().to_owned();
                    sink.0
                        .lock()
                        .unwrap()
                        .push((event, serde_json::from_slice(&body).unwrap()));
                    axum::http::StatusCode::OK
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, received)
}

#[tokio::test]
async fn the_invitee_accepts_and_declines_with_a_user_token() {
    let (url, received) = receiver().await;
    let created = start_2030();
    let mut seed = seed()
        .user(User::new(31, "invitee"))
        .repository_invitation(
            RepositoryInvitation::new(41, "acme/widgets", "invitee", Role::Push)
                .created_at(created),
        )
        .repository_invitation(
            RepositoryInvitation::new(42, "acme/widgets", "hubot", Role::Pull).created_at(created),
        );
    seed.apps[0].webhook = Some(Webhook::new(url, "hook-secret"));
    let emulator = Emulator::builder(seed)
        .dispatch(Dispatch::Manual)
        .start()
        .await
        .unwrap();
    let invitee = emulator.mint_user_token("invitee", 4).unwrap();
    let hubot = emulator
        .mint_personal_access_token("hubot", &["repo"])
        .unwrap();

    let mine = api(
        &emulator,
        Method::GET,
        "/user/repository_invitations",
        Some(&invitee),
        None,
    )
    .await;
    assert_eq!(mine.status, StatusCode::OK);
    assert_eq!(mine.body.as_array().unwrap().len(), 1);
    assert_eq!(mine.body[0]["id"], 41);
    assert_eq!(mine.body[0]["repository"]["full_name"], "acme/widgets");

    // Not theirs: 404, nothing happens.
    let theirs = api(
        &emulator,
        Method::PATCH,
        "/user/repository_invitations/42",
        Some(&invitee),
        None,
    )
    .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);

    let accepted = api(
        &emulator,
        Method::PATCH,
        "/user/repository_invitations/41",
        Some(&invitee),
        None,
    )
    .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT);
    let declined = api(
        &emulator,
        Method::DELETE,
        "/user/repository_invitations/42",
        Some(&hubot),
        None,
    )
    .await;
    assert_eq!(declined.status, StatusCode::NO_CONTENT);
    let gone = api(
        &emulator,
        Method::PATCH,
        "/user/repository_invitations/41",
        Some(&invitee),
        None,
    )
    .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);

    // member.added, held until dispatched, then delivered.
    assert!(received.0.lock().unwrap().is_empty());
    assert_eq!(emulator.dispatch_deliveries(), 1);
    assert!(emulator.wait_for_deliveries(Duration::from_secs(10)).await);
    let hits = received.0.lock().unwrap().clone();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "member");
    assert_eq!(hits[0].1["action"], "added");
    assert_eq!(hits[0].1["member"]["login"], "invitee");
    assert_eq!(hits[0].1["sender"]["login"], "invitee");
    assert_eq!(hits[0].1["installation"]["id"], 7);

    // The invitee is a collaborator now; their token reaches the repository.
    let repo = api(
        &emulator,
        Method::GET,
        "/repos/acme/widgets",
        Some(&invitee),
        None,
    )
    .await;
    assert_eq!(repo.status, StatusCode::OK);
}

#[tokio::test]
async fn refuse_assumed_mode_refuses_the_guesses() {
    let emulator = Emulator::builder(seed())
        .refuse_assumed()
        .start()
        .await
        .unwrap();
    // The documented flow works.
    let code = code_for(&emulator, CLIENT_ID, "octocat").await;
    let token = exchange_code(&emulator, &code).await["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let user = api(&emulator, Method::GET, "/user", Some(&token), None).await;
    assert_eq!(user.status, StatusCode::OK);
    // An error's HTTP 200 is a guess.
    let response = client()
        .post(format!("{}/login/oauth/access_token", emulator.web_base()))
        .form(&[("client_id", CLIENT_ID), ("client_secret", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let body: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(body["behaviour"], "oauth-error-status");
    // So are the consent form and an App token on invitations.
    let form = client()
        .post(format!("{}/login/oauth/authorize", emulator.web_base()))
        .form(&[("client_id", CLIENT_ID), ("login", "octocat")])
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), StatusCode::NOT_IMPLEMENTED);
    let invitations = api(
        &emulator,
        Method::GET,
        "/user/repository_invitations",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(invitations.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(invitations.body["behaviour"], "user-token-invitations");
}
