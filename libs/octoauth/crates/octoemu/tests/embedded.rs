//! The embedded mode end to end over HTTP: startup, the 501 fallback, the
//! virtual clock in `Date`, the admin API, the call log and stopping.
#![cfg(not(target_arch = "wasm32"))]

use chrono::{DateTime, TimeDelta, Utc};
use octoemu::Emulator;
use octoemu::calls::{CredentialKind, Surface};
use octoemu::seed::{Access, App, Installation, Org, Repo, Seed, User, Webhook};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");

fn start_2030() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .to_utc()
}

fn seed() -> Seed {
    Seed::new()
        .start_time(start_2030())
        .user(User::new(1, "octocat"))
        .org(Org::new(2, "acme"))
        .repo(Repo::new(3, "acme", "widgets"))
        .app(
            App::new(4, "invite-bot", "Iv23.fixture", "fixture-client-secret")
                .public_key(PUBLIC_KEY)
                .webhook(Webhook::new(
                    "http://127.0.0.1:9/hooks",
                    "fixture-webhook-secret",
                ))
                .permission("members", Access::Write),
        )
        .installation(Installation::new(5, 4, "acme"))
}

async fn json_of(response: reqwest::Response) -> Value {
    serde_json::from_str(&response.text().await.unwrap()).unwrap()
}

fn date_of(response: &reqwest::Response) -> DateTime<Utc> {
    let date = response.headers()["date"].to_str().unwrap();
    assert!(date.ends_with(" GMT"), "{date}");
    DateTime::parse_from_rfc2822(date).unwrap().to_utc()
}

#[tokio::test]
async fn starts_and_reports_its_bases() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let root = format!("http://127.0.0.1:{}", emulator.addr().port());
    assert_eq!(emulator.api_base(), root);
    assert_eq!(emulator.web_base(), root);
    assert_eq!(emulator.avatar_base(), format!("{root}/avatars"));
    assert_eq!(emulator.admin_base(), format!("{root}/__octoemu"));

    let health = reqwest::get(format!("{}/health", emulator.admin_base()))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(
        health.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    assert_eq!(json_of(health).await["status"], "ok");
}

#[tokio::test]
async fn unbuilt_routes_answer_501_never_404() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let client = Client::new();
    let base = emulator.api_base();
    let cases = [
        (Method::GET, "/"),
        (Method::GET, "/repos/acme/widgets/branches"),
        (Method::DELETE, "/app/installations/5"),
        (Method::DELETE, "/app/hook/deliveries/1"),
        (Method::PUT, "/orgs/acme/memberships/octocat"),
        (Method::PATCH, "/user/memberships/orgs/acme"),
        (Method::POST, "/installation/token"),
        (Method::GET, "/totally/made/up"),
        (Method::POST, "/login/device/code"),
        (Method::GET, "/login/oauth/unknown"),
        (Method::GET, "/avatars/x/1"),
    ];
    for (method, path) in cases {
        let response = client
            .request(method.clone(), format!("{base}{path}?per_page=5"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "{method} {path}"
        );
        assert_eq!(
            response.headers()["content-type"],
            "application/json; charset=utf-8"
        );
        let body = json_of(response).await;
        assert_eq!(body["message"], "octoemu: not implemented");
        assert_eq!(body["route"], format!("{method} {path}"), "no query");
        assert!(
            body["documentation_url"]
                .as_str()
                .unwrap()
                .starts_with("https://")
        );
    }
    // HEAD gets the status without a body.
    let head = client
        .head(format!("{base}/orgs/acme"))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn api_responses_carry_githubs_common_headers() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let client = Client::new();
    let first = client
        .get(format!("{}/rate_limit", emulator.api_base()))
        .send()
        .await
        .unwrap();
    let second = client
        .get(format!("{}/rate_limit", emulator.api_base()))
        .send()
        .await
        .unwrap();
    for header in [
        "x-ratelimit-limit",
        "x-ratelimit-remaining",
        "x-ratelimit-used",
        "x-ratelimit-reset",
        "x-ratelimit-resource",
        "x-github-request-id",
        "date",
    ] {
        assert!(first.headers().contains_key(header), "{header}");
    }
    assert_ne!(
        first.headers()["x-github-request-id"],
        second.headers()["x-github-request-id"]
    );
    // Web pages and the admin API are not rate limited.
    let web = client
        .get(format!("{}/login/oauth/authorize", emulator.web_base()))
        .send()
        .await
        .unwrap();
    assert!(!web.headers().contains_key("x-ratelimit-limit"));
    assert!(web.headers().contains_key("date"));
}

#[tokio::test]
async fn date_follows_the_virtual_clock() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let client = Client::new();
    let get = || client.get(format!("{}/meta", emulator.api_base())).send();

    let before = date_of(&get().await.unwrap());
    assert!(before >= start_2030());
    assert!(before < start_2030() + TimeDelta::seconds(10));

    let advanced = client
        .post(format!("{}/clock/advance", emulator.admin_base()))
        .body(json!({"seconds": 3600}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(advanced.status(), StatusCode::OK);
    let advanced = json_of(advanced).await;
    assert_eq!(advanced["advanced_seconds"], 3600);

    let after = date_of(&get().await.unwrap());
    assert!(after >= before + TimeDelta::hours(1), "{before} → {after}");
    assert!(after < before + TimeDelta::hours(1) + TimeDelta::seconds(10));

    // The handle moves the same clock; admin responses carry it too.
    emulator.advance_clock(Duration::from_secs(86_400));
    let clock = client
        .get(format!("{}/clock", emulator.admin_base()))
        .send()
        .await
        .unwrap();
    assert!(date_of(&clock) >= before + TimeDelta::hours(25));
    let now: DateTime<Utc> = json_of(clock).await["now"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(now >= before + TimeDelta::hours(25));
    assert!(emulator.now() >= now);

    for bad in [json!({"seconds": -1}), json!({"secs": 1}), json!({})] {
        let response = client
            .post(format!("{}/clock/advance", emulator.admin_base()))
            .body(bad.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let too_far = client
        .post(format!("{}/clock/advance", emulator.admin_base()))
        .body(json!({"seconds": u64::MAX}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(too_far.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn without_a_start_time_the_clock_starts_at_real_time() {
    let emulator = Emulator::start(Seed::new()).await.unwrap();
    let skew = (emulator.now() - Utc::now()).num_seconds().abs();
    assert!(skew < 5, "{skew}");
}

#[tokio::test]
async fn state_dumps_the_world_without_secrets_and_seed_merges() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let client = Client::new();
    let state = json_of(
        client
            .get(format!("{}/state", emulator.admin_base()))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(state["world"]["users"][0]["login"], "octocat");
    assert_eq!(state["world"]["installations"][0]["account"], "acme");
    let text = state.to_string();
    assert!(!text.contains("fixture-client-secret"), "{text}");
    assert!(!text.contains("fixture-webhook-secret"), "{text}");
    assert_eq!(state["world"]["apps"][0]["client_secret"], "<redacted>");

    // A valid addition is merged.
    let merged = client
        .post(format!("{}/seed", emulator.admin_base()))
        .body(
            json!({
                "users": [{"id": 10, "login": "hubot"}],
                "memberships": [{"org": "acme", "user": "hubot", "role": "admin"}],
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(merged.status(), StatusCode::OK);
    assert_eq!(json_of(merged).await["world"]["users"][1]["login"], "hubot");

    // An invalid one is rejected whole, with every problem.
    let rejected = client
        .post(format!("{}/seed", emulator.admin_base()))
        .body(
            json!({
                "users": [{"id": 11, "login": "monalisa"}],
                "installations": [{"id": 12, "app_id": 99, "account": "ghost"}],
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let rejected = json_of(rejected).await;
    assert_eq!(rejected["message"], "octoemu: seed rejected");
    let paths: Vec<&str> = rejected["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["installations[1].app_id", "installations[1].account"]
    );
    let users = emulator.state()["world"]["users"].clone();
    assert_eq!(users.as_array().unwrap().len(), 2, "nothing of it merged");

    // Not JSON, or an unknown field: 400.
    for body in ["{", r#"{"usres": []}"#] {
        let response = client
            .post(format!("{}/seed", emulator.admin_base()))
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }

    // The handle merges too, and refuses to move the clock.
    emulator
        .seed(Seed::new().repo(Repo::new(20, "hubot", "dotfiles")))
        .unwrap();
    let error = emulator
        .seed(Seed::new().start_time(Utc::now()))
        .unwrap_err();
    assert_eq!(error.problems()[0].path, "settings.start_time");
}

#[tokio::test]
async fn the_call_log_records_requests_without_secrets() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let client = Client::new();
    let installation_token = format!("ghs_4_{}", "eyJhbGciOiJSUzI1NiJ9.e30.c2ln");
    client
        .get(format!(
            "{}/installation/repositories?page=2",
            emulator.api_base()
        ))
        .bearer_auth(&installation_token)
        .send()
        .await
        .unwrap();
    client
        .post(format!(
            "{}/login/oauth/access_token?client_id=Iv23.fixture&client_secret=fixture-client-secret&code=one-time-code",
            emulator.web_base()
        ))
        .basic_auth("Iv23.fixture", Some("fixture-client-secret"))
        .body("client_secret=fixture-client-secret&refresh_token=ghr_body")
        .send()
        .await
        .unwrap();
    client
        .get(format!("{}/avatars/u/1?s=40", emulator.api_base()))
        .send()
        .await
        .unwrap();
    // Admin requests are not recorded.
    client
        .get(format!("{}/state", emulator.admin_base()))
        .send()
        .await
        .unwrap();

    let response = client
        .get(format!("{}/calls", emulator.admin_base()))
        .send()
        .await
        .unwrap();
    let log = json_of(response).await;
    let text = log.to_string();
    for secret in [
        installation_token.as_str(),
        "fixture-client-secret",
        "one-time-code",
        "ghr_body",
        "SXYyMy5maXh0dXJl", // base64 of the Basic credential's start
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    assert_eq!(log["dropped"], 0);
    let calls = log["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0]["method"], "GET");
    assert_eq!(calls[0]["path"], "/installation/repositories?page=2");
    assert_eq!(calls[0]["surface"], "api");
    assert_eq!(calls[0]["route"], "GET /installation/repositories");
    assert_eq!(calls[0]["credential"], "installation_token");
    // Not a token this emulator minted.
    assert_eq!(calls[0]["identity"], Value::Null);
    assert_eq!(calls[0]["status"], 401);
    assert!(calls[0]["request_id"].as_str().unwrap().contains(':'));
    assert!(
        calls[0]["at"]
            .as_str()
            .unwrap()
            .parse::<DateTime<Utc>>()
            .unwrap()
            >= start_2030()
    );
    assert_eq!(
        calls[1]["path"],
        "/login/oauth/access_token?client_id=Iv23.fixture&client_secret=<redacted>&code=<redacted>"
    );
    assert_eq!(calls[1]["surface"], "web");
    assert_eq!(calls[1]["credential"], "basic");
    assert_eq!(calls[2]["surface"], "avatars");
    assert_eq!(calls[2]["credential"], "none");

    // The handle sees the same log, typed.
    let typed = emulator.calls();
    assert_eq!(typed[0].credential, CredentialKind::InstallationToken);
    assert_eq!(typed[1].surface, Surface::Web);
    assert!(typed[0].seq < typed[1].seq);

    let cleared = client
        .delete(format!("{}/calls", emulator.admin_base()))
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
    assert!(emulator.calls().is_empty());
    client
        .get(format!("{}/user", emulator.api_base()))
        .send()
        .await
        .unwrap();
    assert_eq!(emulator.calls().len(), 1);
    emulator.clear_calls();
    assert!(emulator.calls().is_empty());
}

#[tokio::test]
async fn unknown_admin_routes_are_404_and_fidelity_is_served() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let missing = reqwest::get(format!("{}/nope", emulator.admin_base()))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_of(missing).await["message"],
        "octoemu: no such admin route"
    );
    let fidelity = json_of(
        reqwest::get(format!("{}/fidelity", emulator.admin_base()))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(fidelity["behaviours"]["date-header"]["label"], "documented");
}

#[tokio::test]
async fn an_invalid_seed_does_not_start() {
    let error = Emulator::start(Seed::new().installation(Installation::new(1, 2, "ghost")))
        .await
        .unwrap_err();
    assert!(matches!(error, octoemu::StartError::Seed(_)), "{error}");
    assert!(error.to_string().contains("installations[0].app_id"));
}

async fn refused(addr: std::net::SocketAddr) -> bool {
    for _ in 0..250 {
        if tokio::net::TcpStream::connect(addr).await.is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn stops_on_drop_and_on_shutdown() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let addr = emulator.addr();
    reqwest::get(format!("{}/health", emulator.admin_base()))
        .await
        .unwrap();
    drop(emulator);
    assert!(refused(addr).await, "still listening after drop");

    let emulator = Emulator::start(seed()).await.unwrap();
    let addr = emulator.addr();
    emulator.shutdown().await.unwrap();
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn stops_after_the_idle_timeout() {
    let emulator = Emulator::builder(seed())
        .idle_timeout(Duration::from_millis(400))
        .start()
        .await
        .unwrap();
    let addr = emulator.addr();
    // Requests keep it alive past the timeout.
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let response = reqwest::get(format!("{}/health", emulator.admin_base()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    tokio::time::timeout(Duration::from_secs(5), emulator.stopped())
        .await
        .expect("stopped once idle");
    assert!(refused(addr).await);
}
