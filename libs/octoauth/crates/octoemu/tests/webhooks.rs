//! Webhook delivery and the deliveries API (GE-04), against a local axum
//! receiver: signatures and headers, payloads, auto and manual dispatch,
//! failures (status, stall) without retries, the deliveries API (cursor
//! pages, the status filter, redelivery, the hook configuration), the
//! installation events from every admin action and the suspension rule.
#![cfg(not(target_arch = "wasm32"))]

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as AxumStatus};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use octoemu::Emulator;
use octoemu::seed::{
    Access, App, Installation, Membership, Org, OrgRole, Repo, RepositoryInvitation, Role, Seed,
    User, Webhook,
};
use octoemu::webhooks::{Delivery, Dispatch};
use reqwest::{Client, Method, StatusCode};
use rsa::pkcs1v15::SigningKey as RsaSigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PRIVATE_KEY: &str = include_str!("fixtures/rsa2048.pem");
const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");
const SECRET: &str = "It's a Secret to Everybody";
const WAIT: Duration = Duration::from_secs(10);

// --- the receiver -------------------------------------------------------------

/// How the receiver answers.
#[derive(Clone, Copy, Debug)]
enum Answer {
    Ok,
    Status(u16),
    Stall(Duration),
}

/// One request the receiver got.
#[derive(Clone, Debug)]
struct Received {
    headers: HeaderMap,
    body: Bytes,
}

impl Received {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .map(|v| v.to_str().unwrap())
            .unwrap_or_else(|| panic!("no {name} header"))
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn event(&self) -> (String, String) {
        (
            self.header("x-github-event").to_owned(),
            self.json()["action"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }
}

#[derive(Clone)]
struct Receiver {
    received: Arc<Mutex<Vec<Received>>>,
    answer: Arc<Mutex<Answer>>,
    url: String,
}

impl Receiver {
    async fn start() -> Self {
        let receiver = Self {
            received: Arc::default(),
            answer: Arc::new(Mutex::new(Answer::Ok)),
            url: String::new(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hooks", listener.local_addr().unwrap());
        let router = axum::Router::new()
            .route("/hooks", axum::routing::post(receive))
            .with_state(receiver.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, ..receiver }
    }

    fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    fn events(&self) -> Vec<(String, String)> {
        self.received().iter().map(Received::event).collect()
    }
}

async fn receive(
    State(receiver): State<Receiver>,
    headers: HeaderMap,
    body: Bytes,
) -> (AxumStatus, &'static str) {
    receiver
        .received
        .lock()
        .unwrap()
        .push(Received { headers, body });
    let answer = *receiver.answer.lock().unwrap();
    match answer {
        Answer::Ok => (AxumStatus::ACCEPTED, "thanks"),
        Answer::Status(code) => (AxumStatus::from_u16(code).unwrap(), "nope"),
        Answer::Stall(how_long) => {
            tokio::time::sleep(how_long).await;
            (AxumStatus::OK, "late")
        }
    }
}

// --- the world ------------------------------------------------------------------

/// App 4 (`invite-bot`, subscribed to `member`, webhook to `url` signed with
/// [`SECRET`]) installed on acme (5, widgets and gadgets selected); acme's
/// owner is boss; invitations 31–39 for u1…u9 to acme/widgets. App 7
/// (`other-bot`, no webhook) exists for cross-App checks.
fn seed(url: &str) -> Seed {
    let mut seed = Seed::new()
        .start_time("2030-01-01T00:00:00Z".parse().unwrap())
        .user(User::new(1, "octocat"))
        .user(User::new(14, "boss"))
        .org(Org::new(2, "acme"))
        .membership(Membership::new("acme", "boss", OrgRole::Admin))
        .repo(Repo::new(101, "acme", "widgets").private())
        .repo(Repo::new(102, "acme", "gadgets").private())
        .repo(Repo::new(103, "acme", "gizmos"))
        .app(
            App::new(4, "invite-bot", "Iv23.fixture", "fixture-client-secret")
                .public_key(PUBLIC_KEY)
                .webhook(Webhook::new(url, SECRET))
                .permission("administration", Access::Write)
                .permission("metadata", Access::Read)
                .permission("members", Access::Write)
                .event("member"),
        )
        .app(
            App::new(7, "other-bot", "Iv23.other", "other-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("metadata", Access::Read),
        )
        .installation(Installation::new(5, 4, "acme").selected(["acme/widgets", "acme/gadgets"]));
    for n in 1..=9 {
        seed = seed
            .user(User::new(1000 + n, format!("u{n}")))
            .repository_invitation(
                RepositoryInvitation::new(30 + n, "acme/widgets", format!("u{n}"), Role::Push)
                    .inviter("boss"),
            );
    }
    seed
}

async fn start(receiver: &Receiver) -> Emulator {
    Emulator::start(seed(&receiver.url)).await.unwrap()
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

async fn send(method: Method, url: &str, auth: Option<&str>, body: Option<Value>) -> Reply {
    let mut request = Client::new().request(method, url);
    if let Some(auth) = auth {
        request = request.bearer_auth(auth);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Reply {
        status,
        headers,
        body,
    }
}

async fn admin(emulator: &Emulator, method: Method, path: &str, body: Option<Value>) -> Reply {
    send(
        method,
        &format!("{}{path}", emulator.admin_base()),
        None,
        body,
    )
    .await
}

/// u`n` accepts invitation 30+`n`: `member.added` on acme/widgets.
async fn accept(emulator: &Emulator, n: u64) {
    let reply = admin(
        emulator,
        Method::POST,
        &format!("/act/u{n}/repository_invitations/{}/accept", 30 + n),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
}

async fn settle(emulator: &Emulator) {
    assert!(emulator.wait_for_deliveries(WAIT).await, "deliveries idle");
}

/// An App JWT for App `app_id` on the emulator's clock.
fn jwt(emulator: &Emulator, app_id: u64) -> String {
    let key =
        RsaSigningKey::<Sha256>::new(rsa::RsaPrivateKey::from_pkcs8_pem(PRIVATE_KEY).unwrap());
    let now = emulator.now().timestamp();
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(json!({"alg": "RS256", "typ": "JWT"}).to_string()),
        URL_SAFE_NO_PAD
            .encode(json!({"iat": now - 60, "exp": now + 540, "iss": app_id}).to_string())
    );
    let signature = key.sign(input.as_bytes()).to_vec();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

async fn app_get(emulator: &Emulator, app_id: u64, path: &str) -> Reply {
    let url = if path.starts_with("http") {
        path.to_owned()
    } else {
        format!("{}{path}", emulator.api_base())
    };
    send(Method::GET, &url, Some(&jwt(emulator, app_id)), None).await
}

fn hmac_256(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex(&mac.finalize().into_bytes()))
}

fn hmac_1(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha1={}", hex(&mac.finalize().into_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- delivery -------------------------------------------------------------------

#[tokio::test]
async fn deliveries_carry_githubs_headers_signature_and_payload() {
    // The verification below is the one GitHub documents, checked against
    // the docs' test vector first.
    assert_eq!(
        hmac_256(SECRET, b"Hello, World!"),
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
    );
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;
    accept(&emulator, 1).await;
    settle(&emulator).await;

    let received = receiver.received();
    assert_eq!(received.len(), 1);
    let hit = &received[0];
    assert_eq!(hit.header("content-type"), "application/json");
    assert!(hit.header("user-agent").starts_with("GitHub-Hookshot/"));
    assert_eq!(hit.header("x-github-event"), "member");
    let guid = hit.header("x-github-delivery");
    assert_eq!(guid.len(), 36);
    assert_eq!(guid.matches('-').count(), 4);
    assert!(!hit.header("x-github-hook-id").is_empty());
    assert_eq!(
        hit.header("x-github-hook-installation-target-type"),
        "integration"
    );
    assert_eq!(hit.header("x-github-hook-installation-target-id"), "4");
    assert_eq!(
        hit.header("x-hub-signature-256"),
        hmac_256(SECRET, &hit.body)
    );
    assert_eq!(hit.header("x-hub-signature"), hmac_1(SECRET, &hit.body));

    let payload = hit.json();
    assert_eq!(payload["action"], "added");
    assert_eq!(payload["installation"]["id"], 5);
    assert!(payload["installation"]["node_id"].is_string());
    assert_eq!(payload["member"]["login"], "u1");
    assert_eq!(payload["sender"]["login"], "u1");
    assert_eq!(payload["repository"]["full_name"], "acme/widgets");
    assert_eq!(payload["organization"]["login"], "acme");

    // The attempt record matches what was sent and answered.
    let deliveries = emulator.deliveries();
    assert_eq!(deliveries.len(), 1);
    let attempt = &deliveries[0];
    assert_eq!(attempt.guid, guid);
    assert_eq!(attempt.status, "OK");
    assert_eq!(attempt.status_code, 202);
    assert!(!attempt.redelivery);
    assert_eq!(attempt.event, "member");
    assert_eq!(attempt.action.as_deref(), Some("added"));
    assert_eq!(attempt.installation_id, Some(5));
    assert_eq!(attempt.repository_id, Some(101));
    // On the virtual clock (which started in 2030).
    assert!(attempt.delivered_at <= emulator.now());
    assert!(
        attempt.delivered_at
            >= "2030-01-01T00:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
    );
    assert_eq!(attempt.request.payload, payload);
    assert_eq!(attempt.request.headers["X-GitHub-Event"], "member");
    assert_eq!(attempt.response.payload.as_deref(), Some("thanks"));
    assert_eq!(attempt.url, receiver.url);

    // The admin API serves the same records.
    let served = admin(&emulator, Method::GET, "/deliveries", None).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.body["deliveries"][0]["guid"], guid);
    assert_eq!(served.body["mode"], "auto");
}

#[tokio::test]
async fn unsigned_without_a_secret_and_nothing_without_an_active_webhook() {
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;
    assert!(emulator.set_webhook(
        4,
        Some(Webhook {
            url: receiver.url.clone(),
            secrets: Vec::new(),
            active: true,
        })
    ));
    accept(&emulator, 1).await;
    settle(&emulator).await;
    let received = receiver.received();
    assert_eq!(received.len(), 1);
    assert!(received[0].headers.get("x-hub-signature-256").is_none());
    assert!(received[0].headers.get("x-hub-signature").is_none());

    let mut inactive = Webhook::new(receiver.url.clone(), SECRET);
    inactive.active = false;
    let reply = admin(
        &emulator,
        Method::PUT,
        "/apps/4/webhook",
        Some(serde_json::to_value(&inactive).unwrap()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    accept(&emulator, 2).await;
    settle(&emulator).await;
    assert_eq!(receiver.received().len(), 1, "inactive: nothing sent");
    assert_eq!(emulator.deliveries().len(), 1, "and nothing recorded");
}

#[tokio::test]
async fn the_first_secret_signs_during_a_rotation() {
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;
    let rotating = Webhook::new(receiver.url.clone(), "new-secret").secret(SECRET);
    assert!(emulator.set_webhook(4, Some(rotating)));
    accept(&emulator, 1).await;
    settle(&emulator).await;
    let hit = &receiver.received()[0];
    assert_eq!(
        hit.header("x-hub-signature-256"),
        hmac_256("new-secret", &hit.body)
    );
}

#[tokio::test]
async fn manual_dispatch_holds_deliveries_until_dispatched() {
    let receiver = Receiver::start().await;
    let emulator = Emulator::builder(seed(&receiver.url))
        .dispatch(Dispatch::Manual)
        .start()
        .await
        .unwrap();
    accept(&emulator, 1).await;
    accept(&emulator, 2).await;
    settle(&emulator).await;
    assert!(receiver.received().is_empty(), "held");
    let state = admin(&emulator, Method::GET, "/deliveries", None).await;
    assert_eq!(state.body["mode"], "manual");
    assert_eq!(state.body["held"], 2);

    let dispatched = admin(&emulator, Method::POST, "/deliveries/dispatch", None).await;
    assert_eq!(dispatched.body["dispatched"], 2);
    let waited = admin(
        &emulator,
        Method::POST,
        "/deliveries/wait",
        Some(json!({"timeout_ms": 10_000})),
    )
    .await;
    assert_eq!(waited.status, StatusCode::OK);
    assert_eq!(waited.body["idle"], true);
    // In event order.
    let members: Vec<String> = receiver
        .received()
        .iter()
        .map(|r| r.json()["member"]["login"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(members, ["u1", "u2"]);

    // Back to auto: a new event goes out by itself.
    let mode = admin(
        &emulator,
        Method::POST,
        "/deliveries/mode",
        Some(json!({"mode": "auto"})),
    )
    .await;
    assert_eq!(mode.status, StatusCode::OK);
    accept(&emulator, 3).await;
    settle(&emulator).await;
    assert_eq!(receiver.received().len(), 3);
}

#[tokio::test]
async fn failures_are_recorded_and_never_retried() {
    let receiver = Receiver::start().await;
    let emulator = Emulator::builder(seed(&receiver.url))
        .webhook_timeout(Duration::from_millis(300))
        .start()
        .await
        .unwrap();
    receiver.answer(Answer::Status(500));
    accept(&emulator, 1).await;
    settle(&emulator).await;
    receiver.answer(Answer::Stall(Duration::from_secs(2)));
    accept(&emulator, 2).await;
    settle(&emulator).await;

    let deliveries = emulator.deliveries();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0].status_code, 500);
    assert_eq!(deliveries[0].status, "Invalid HTTP Response: 500");
    assert_eq!(deliveries[0].response.payload.as_deref(), Some("nope"));
    assert_eq!(deliveries[1].status_code, 0);
    assert_eq!(deliveries[1].status, "timed out");
    assert!(deliveries[1].duration >= 0.3 && deliveries[1].duration < 2.0);

    // No retry: one request per event, even after a while.
    receiver.answer(Answer::Ok);
    tokio::time::sleep(Duration::from_millis(500)).await;
    settle(&emulator).await;
    assert_eq!(receiver.received().len(), 2);
    assert_eq!(emulator.deliveries().len(), 2);

    // Nobody listening.
    assert!(emulator.set_webhook(4, Some(Webhook::new("http://127.0.0.1:9/hooks", SECRET))));
    accept(&emulator, 3).await;
    settle(&emulator).await;
    let refused = &emulator.deliveries()[2];
    assert_eq!(refused.status_code, 0);
    assert_eq!(refused.status, "Couldn't connect to server");
}

#[test]
fn the_default_timeout_is_ten_seconds() {
    assert_eq!(octoemu::webhooks::DEFAULT_TIMEOUT, Duration::from_secs(10));
}

#[tokio::test]
async fn refuse_assumed_mode_withholds_guessed_status_texts() {
    let receiver = Receiver::start().await;
    receiver.answer(Answer::Status(500));
    let emulator = Emulator::builder(seed(&receiver.url))
        .refuse_assumed()
        .start()
        .await
        .unwrap();
    accept(&emulator, 1).await;
    settle(&emulator).await;
    let deliveries = emulator.deliveries();
    assert_eq!(deliveries[0].status_code, 500);
    assert_eq!(deliveries[0].status, "octoemu: assumed message withheld");
}

// --- the deliveries API -----------------------------------------------------------

/// Every page of `GET /app/hook/deliveries` from `first`, following `next`.
async fn walk(emulator: &Emulator, first: &str) -> Vec<Vec<u64>> {
    let mut pages = Vec::new();
    let mut url = first.to_owned();
    loop {
        let reply = app_get(emulator, 4, &url).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        pages.push(
            reply
                .body
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["id"].as_u64().unwrap())
                .collect(),
        );
        let next = reply
            .headers
            .get("link")
            .map(|l| l.to_str().unwrap().to_owned())
            .and_then(|link| {
                link.split(", ")
                    .find(|rel| rel.ends_with("rel=\"next\""))
                    .map(|rel| rel[1..rel.find('>').unwrap()].to_owned())
            });
        match next {
            Some(next) => {
                assert!(next.contains("cursor="), "{next}");
                url = next;
            }
            None => return pages,
        }
    }
}

#[tokio::test]
async fn the_deliveries_api_pages_filters_and_redelivers() {
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;
    for n in 1..=5 {
        receiver.answer(if n % 2 == 0 {
            Answer::Status(500)
        } else {
            Answer::Ok
        });
        accept(&emulator, n).await;
        settle(&emulator).await;
    }
    receiver.answer(Answer::Ok);

    // Newest first, two per page, cursors in Link.
    let pages = walk(&emulator, "/app/hook/deliveries?per_page=2").await;
    assert_eq!(pages, vec![vec![5, 4], vec![3, 2], vec![1]]);
    let second = app_get(&emulator, 4, "/app/hook/deliveries?per_page=2&cursor=v1_3").await;
    let link = second.headers["link"].to_str().unwrap();
    assert!(link.contains("rel=\"prev\""), "{link}");
    let summary = &second.body[0];
    for field in [
        "id",
        "guid",
        "delivered_at",
        "redelivery",
        "duration",
        "status",
        "status_code",
        "event",
        "action",
        "installation_id",
        "repository_id",
        "throttled_at",
    ] {
        assert!(summary.get(field).is_some(), "{field}");
    }
    assert!(summary.get("request").is_none());

    // The status filter (documented: success is 200-399).
    let failures = walk(&emulator, "/app/hook/deliveries?status=failure").await;
    assert_eq!(failures, vec![vec![4, 2]]);
    let successes = walk(&emulator, "/app/hook/deliveries?status=success&per_page=1").await;
    assert_eq!(successes, vec![vec![5], vec![3], vec![1]]);
    let bad = app_get(&emulator, 4, "/app/hook/deliveries?status=maybe").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let bad = app_get(&emulator, 4, "/app/hook/deliveries?cursor=garbage").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    // One delivery, with request and response.
    let one = app_get(&emulator, 4, "/app/hook/deliveries/2").await;
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.body["status_code"], 500);
    assert_eq!(one.body["request"]["payload"]["member"]["login"], "u2");
    assert_eq!(one.body["request"]["headers"]["X-GitHub-Event"], "member");
    assert_eq!(one.body["response"]["payload"], "nope");
    assert_eq!(one.body["url"], receiver.url.as_str());
    // Another App sees none of them.
    assert_eq!(
        app_get(&emulator, 7, "/app/hook/deliveries/2").await.status,
        StatusCode::NOT_FOUND
    );
    let theirs = app_get(&emulator, 7, "/app/hook/deliveries").await;
    assert_eq!(theirs.body, json!([]));
    assert_eq!(
        app_get(&emulator, 4, "/app/hook/deliveries/99")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Installation tokens are the wrong credential.
    let token = admin(
        &emulator,
        Method::POST,
        "/tokens/installation",
        Some(json!({"installation_id": 5})),
    )
    .await;
    let with_token = send(
        Method::GET,
        &format!("{}/app/hook/deliveries", emulator.api_base()),
        Some(token.body["token"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(with_token.status, StatusCode::UNAUTHORIZED);

    // Redelivering the failed one: 202, the same body and GUID again.
    let redeliver = send(
        Method::POST,
        &format!("{}/app/hook/deliveries/2/attempts", emulator.api_base()),
        Some(&jwt(&emulator, 4)),
        None,
    )
    .await;
    assert_eq!(redeliver.status, StatusCode::ACCEPTED);
    assert_eq!(redeliver.body, json!({}));
    settle(&emulator).await;
    let received = receiver.received();
    assert_eq!(received.len(), 6);
    assert_eq!(received[5].body, received[1].body, "byte for byte");
    assert_eq!(
        received[5].header("x-github-delivery"),
        received[1].header("x-github-delivery")
    );
    let deliveries = emulator.deliveries();
    let again: &Delivery = &deliveries[5];
    assert!(again.redelivery);
    assert_eq!(again.guid, deliveries[1].guid);
    assert_eq!(again.status, "OK");
    let other_app = send(
        Method::POST,
        &format!("{}/app/hook/deliveries/2/attempts", emulator.api_base()),
        Some(&jwt(&emulator, 7)),
        None,
    )
    .await;
    assert_eq!(other_app.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_hook_configuration_reads_and_updates() {
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;
    let config = app_get(&emulator, 4, "/app/hook/config").await;
    assert_eq!(config.status, StatusCode::OK);
    assert_eq!(
        config.body,
        json!({"url": receiver.url, "content_type": "json", "secret": "********", "insecure_ssl": "0"})
    );
    assert_eq!(
        app_get(&emulator, 7, "/app/hook/config").await.status,
        StatusCode::NOT_FOUND
    );

    let patch = |body: Value| {
        let url = format!("{}/app/hook/config", emulator.api_base());
        let token = jwt(&emulator, 4);
        async move { send(Method::PATCH, &url, Some(&token), Some(body)).await }
    };
    let updated = patch(json!({"secret": "patched", "insecure_ssl": "1"})).await;
    assert_eq!(updated.status, StatusCode::OK);
    assert_eq!(updated.body["insecure_ssl"], "1");
    assert_eq!(updated.body["secret"], "********");
    accept(&emulator, 1).await;
    settle(&emulator).await;
    let hit = &receiver.received()[0];
    assert_eq!(
        hit.header("x-hub-signature-256"),
        hmac_256("patched", &hit.body)
    );
    assert_eq!(
        patch(json!({"content_type": "form"})).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        patch(json!({"url": "ftp://x"})).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

// --- installation events --------------------------------------------------------------

#[tokio::test]
async fn every_admin_action_announces_its_installation_event() {
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;

    // Installing (App 4 is not subscribed to installation events: GitHub
    // sends them to every App anyway).
    let installed = admin(
        &emulator,
        Method::POST,
        "/installations",
        Some(json!({
            "installation": {"id": 8, "app_id": 4, "account": "octocat"},
            "sender": "octocat",
        })),
    )
    .await;
    assert_eq!(installed.status, StatusCode::CREATED, "{}", installed.body);
    let duplicate = admin(
        &emulator,
        Method::POST,
        "/installations",
        Some(json!({"installation": {"id": 8, "app_id": 4, "account": "octocat"}})),
    )
    .await;
    assert_eq!(duplicate.status, StatusCode::UNPROCESSABLE_ENTITY);

    // New permissions, a new selection, suspension both ways (admin and
    // the App's own routes), uninstalling.
    let accepted = admin(
        &emulator,
        Method::POST,
        "/installations/5/permissions",
        Some(json!({"permissions": {"metadata": "read", "members": "write"}})),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT, "{}", accepted.body);
    let wider = admin(
        &emulator,
        Method::POST,
        "/installations/5/permissions",
        Some(json!({"permissions": {"contents": "write"}})),
    )
    .await;
    assert_eq!(wider.status, StatusCode::UNPROCESSABLE_ENTITY);
    let reselected = admin(
        &emulator,
        Method::POST,
        "/installations/5/repositories",
        Some(json!({"repositories": ["acme/widgets", "acme/gizmos"], "sender": "boss"})),
    )
    .await;
    assert_eq!(
        reselected.status,
        StatusCode::NO_CONTENT,
        "{}",
        reselected.body
    );
    assert_eq!(
        admin(&emulator, Method::POST, "/installations/5/suspend", None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin(&emulator, Method::POST, "/installations/5/unsuspend", None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let suspended_url = format!("{}/app/installations/5/suspended", emulator.api_base());
    let put = send(Method::PUT, &suspended_url, Some(&jwt(&emulator, 4)), None).await;
    assert_eq!(put.status, StatusCode::NO_CONTENT);
    let delete = send(
        Method::DELETE,
        &suspended_url,
        Some(&jwt(&emulator, 4)),
        None,
    )
    .await;
    assert_eq!(delete.status, StatusCode::NO_CONTENT);
    assert_eq!(
        admin(&emulator, Method::DELETE, "/installations/8", None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    settle(&emulator).await;

    let pairs = |events: &[(&str, &str)]| {
        events
            .iter()
            .map(|(e, a)| ((*e).to_owned(), (*a).to_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        receiver.events(),
        pairs(&[
            ("installation", "created"),
            ("installation", "new_permissions_accepted"),
            ("installation_repositories", "added"),
            ("installation_repositories", "removed"),
            ("installation", "suspend"),
            ("installation", "unsuspend"),
            ("installation", "suspend"),
            ("installation", "unsuspend"),
            ("installation", "deleted"),
        ])
    );
    let bodies: Vec<Value> = receiver.received().iter().map(Received::json).collect();
    for body in &bodies {
        assert!(body["installation"]["account"]["login"].is_string());
        assert!(body["installation"]["app_id"].is_number());
        assert!(body["installation"]["permissions"].is_object());
        assert!(body["sender"]["login"].is_string());
        assert!(body.get("requester").is_some());
        assert!(body.get("repository").is_none());
    }
    let created = &bodies[0];
    assert_eq!(created["installation"]["id"], 8);
    assert_eq!(created["sender"]["login"], "octocat");
    assert_eq!(created["repositories"], json!([]));
    assert_eq!(
        bodies[1]["installation"]["permissions"],
        json!({"metadata": "read", "members": "write"})
    );
    // Without a named sender: acme's owner.
    assert_eq!(bodies[1]["sender"]["login"], "boss");
    let added = &bodies[2];
    assert_eq!(added["repository_selection"], "selected");
    assert_eq!(
        added["repositories_added"],
        json!([{"id": 103, "node_id": added["repositories_added"][0]["node_id"], "name": "gizmos", "full_name": "acme/gizmos", "private": false}])
    );
    assert_eq!(added["repositories_removed"], json!([]));
    let removed = &bodies[3];
    assert_eq!(removed["repositories_added"], json!([]));
    assert_eq!(
        removed["repositories_removed"][0]["full_name"],
        "acme/gadgets"
    );
    assert_eq!(removed["repositories_removed"][0]["private"], true);
    assert!(bodies[4]["installation"]["suspended_at"].is_string());
    assert_eq!(bodies[4]["installation"]["suspended_by"]["login"], "boss");
    assert!(bodies[5]["installation"]["suspended_at"].is_null());
    // The App's own route: its bot is the sender.
    assert_eq!(bodies[6]["sender"]["login"], "invite-bot[bot]");
    assert_eq!(bodies[8]["installation"]["id"], 8);
    for (delivery, received) in emulator.deliveries().iter().zip(receiver.received()) {
        assert_eq!(delivery.guid, received.header("x-github-delivery"));
        assert_eq!(delivery.repository_id, None);
    }

    // The handle has the same actions.
    assert!(emulator.suspend_installation(5));
    emulator
        .install(Installation::new(9, 7, "octocat"))
        .unwrap();
    settle(&emulator).await;
    assert_eq!(receiver.received().len(), 10, "App 7 has no webhook");
    assert_eq!(
        emulator.events().last().unwrap().action.as_deref(),
        Some("created")
    );
}

#[tokio::test]
async fn suspended_installations_get_no_further_webhooks() {
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;
    accept(&emulator, 1).await;
    assert!(emulator.suspend_installation(5));
    accept(&emulator, 2).await;
    assert!(emulator.unsuspend_installation(5));
    accept(&emulator, 3).await;
    settle(&emulator).await;
    let members: Vec<String> = receiver
        .received()
        .iter()
        .map(|r| {
            let body = r.json();
            format!(
                "{}.{} {}",
                r.header("x-github-event"),
                body["action"].as_str().unwrap(),
                body["member"]["login"].as_str().unwrap_or("-")
            )
        })
        .collect();
    assert_eq!(
        members,
        [
            "member.added u1",
            "installation.suspend -",
            "installation.unsuspend -",
            "member.added u3",
        ]
    );
    // The member event of the suspended period has no target at all.
    let skipped = &emulator.events()[2];
    assert_eq!(skipped.payload["member"]["login"], "u2");
    assert!(skipped.targets.is_empty());
}

#[tokio::test]
async fn member_edits_and_removals_are_delivered_too() {
    let receiver = Receiver::start().await;
    let emulator = start(&receiver).await;
    accept(&emulator, 1).await;
    let token = admin(
        &emulator,
        Method::POST,
        "/tokens/installation",
        Some(json!({"installation_id": 5})),
    )
    .await;
    let token = token.body["token"].as_str().unwrap().to_owned();
    let edited = send(
        Method::PUT,
        &format!(
            "{}/repos/acme/widgets/collaborators/u1",
            emulator.api_base()
        ),
        Some(&token),
        Some(json!({"permission": "admin"})),
    )
    .await;
    assert_eq!(edited.status, StatusCode::NO_CONTENT);
    let removed = send(
        Method::DELETE,
        &format!(
            "{}/repos/acme/widgets/collaborators/u1",
            emulator.api_base()
        ),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    settle(&emulator).await;
    let pairs: Vec<(String, String)> = receiver.events();
    assert_eq!(
        pairs,
        [
            ("member".to_owned(), "added".to_owned()),
            ("member".to_owned(), "edited".to_owned()),
            ("member".to_owned(), "removed".to_owned()),
        ]
    );
    let edited = receiver.received()[1].json();
    assert_eq!(edited["sender"]["login"], "invite-bot[bot]");
    assert_eq!(edited["changes"]["permission"]["to"], "admin");
}
