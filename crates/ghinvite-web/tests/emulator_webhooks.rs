//! ghinvite-web's real webhook intake (`POST /webhooks/repo`) fed by the
//! GitHub emulator (`octoemu`, GE-04): the emulator delivers signed
//! `installation` and `installation_repositories` events over HTTP to the
//! web app served on a loopback port, and the web side verifies, routes and
//! dispatches them to a recording Restate ingress.
//!
//! - Signature: every accepted delivery carries the emulator's
//!   `X-Hub-Signature-256` over the exact body.
//! - Rotation: the web side holds `[repo-new, repo-old]`; deliveries signed
//!   with either are accepted, one signed with a retired secret is a 401.
//! - Dedupe: a redelivery (same body, same GUID) reaches Restate with the
//!   same payload-SHA-256 `delivery_id` as the original.
//! - Organizations (GE-06): `POST /webhooks/org` accepts the emulator's
//!   documented `organization` and `membership` payloads.
//!
//! Keys: octoemu's test key pair (public test material).
#![cfg(not(target_arch = "wasm32"))]

use ghinvite_github::mocks::MockTransport;
use ghinvite_web::WebConfig;
use octoemu::Emulator;
use octoemu::seed::{Access, App, Membership, Org, OrgRole, Repo, Seed, User, Webhook};
use octoemu::webhooks::Delivery;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
mod common;
use common::restate_recorder::{RecordedCall, RestateRecorder};

const PRIVATE_KEY: &str =
    include_str!("../../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pem");
const PUBLIC_KEY: &str =
    include_str!("../../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pub.pem");

/// The repo App's ID in `common::apps` (`GH_REPO_APP_ID`).
const REPO_APP: u64 = 123;
const INSTALLATION: u64 = 77;
const WAIT: Duration = Duration::from_secs(10);

/// ghinvite-web with the repo App's webhook secrets `[repo-new, repo-old]`,
/// served on a loopback port; its base URL and the Restate recorder.
async fn web() -> (String, RestateRecorder) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let restate = RestateRecorder::start().await;
    let mut config = WebConfig::for_local_dev_with_secret([7; 32]);
    config.webhooks = Some(common::apps::webhooks("repo-new;repo-old"));
    let router = common::build_app(
        common::sign_in::app_state(
            storage,
            Arc::new(MockTransport::scripted(vec![])),
            restate.client(),
            config,
        ),
        tower_sessions::MemoryStore::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (base, restate)
}

/// acme (9001, owned by installer 42) with repositories api and web, and
/// the repo App (123) delivering to `url`, signing with `repo-old`. Not
/// installed yet.
fn seed(url: &str) -> Seed {
    Seed::new()
        .user(User::new(42, "installer"))
        .org(Org::new(9001, "acme"))
        .membership(Membership::new("acme", "installer", OrgRole::Admin))
        .repo(Repo::new(10, "acme", "api").private())
        .repo(Repo::new(11, "acme", "web").private())
        .app(
            App::new(
                REPO_APP,
                "ghinvite-repo",
                "Iv.fixture-repo",
                "fixture-secret",
            )
            .public_key(PUBLIC_KEY)
            .webhook(Webhook::new(url, "repo-old"))
            .permission("administration", Access::Write)
            .permission("metadata", Access::Read),
        )
}

async fn admin(emulator: &Emulator, path: &str, body: Value) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!("{}{path}", emulator.admin_base()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
        .status()
}

async fn settle(emulator: &Emulator) -> Vec<Delivery> {
    assert!(emulator.wait_for_deliveries(WAIT).await, "deliveries idle");
    emulator.deliveries()
}

/// The dedupe identity ghinvite derives: hex SHA-256 of the signed body.
fn payload_sha256(delivery: &Delivery) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(&delivery.request.payload).unwrap(),
    ))
}

fn calls(restate: &RestateRecorder) -> Vec<RecordedCall> {
    restate.calls().clone()
}

/// Redelivers attempt `id` through the deliveries API with the App's JWT
/// (minted by octoauth).
async fn redeliver(emulator: &Emulator, id: u64) {
    let app = octoauth::App::builder(
        REPO_APP,
        octoauth::SigningKey::from_pem(PRIVATE_KEY).unwrap(),
    )
    .base_url(emulator.api_base())
    .build()
    .unwrap();
    let jwt = app.credential().unwrap();
    let response = reqwest::Client::new()
        .post(format!(
            "{}/app/hook/deliveries/{id}/attempts",
            emulator.api_base()
        ))
        .bearer_auth(jwt.expose_secret())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
}

fn rotate(emulator: &Emulator, url: &str, secret: &str) {
    assert!(emulator.set_webhook(REPO_APP, Some(Webhook::new(url, secret))));
}

#[tokio::test]
async fn the_web_intake_verifies_rotates_and_dedupes_emulator_deliveries() {
    let (base, restate) = web().await;
    let url = format!("{base}/webhooks/repo");
    let emulator = Emulator::start(seed(&url)).await.unwrap();

    // installation.created, signed with repo-old (the web side's second
    // secret): accepted and onboarded.
    let created = admin(
        &emulator,
        "/installations",
        json!({
            "installation": {"id": INSTALLATION, "app_id": REPO_APP, "account": "acme",
                             "repository_selection": "selected", "repositories": ["acme/api"]},
            "sender": "installer",
        }),
    )
    .await;
    assert_eq!(created, reqwest::StatusCode::CREATED);
    let deliveries = settle(&emulator).await;
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].event, "installation");
    assert_eq!(deliveries[0].status_code, 202, "{}", deliveries[0].status);
    let recorded = calls(&restate);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].key, "repo:77");
    assert_eq!(recorded[0].method, "onboard_webhook");
    assert_eq!(recorded[0].body["sender_login"], "installer");
    assert_eq!(recorded[0].body["sender_id"], 42);
    // The command identity is the body's SHA-256, never the GUID header.
    assert_eq!(
        recorded[0].body["delivery_id"],
        payload_sha256(&deliveries[0])
    );
    assert_ne!(recorded[0].body["delivery_id"], deliveries[0].guid.as_str());

    // installation_repositories.added: the installation is rechecked.
    let added = admin(
        &emulator,
        &format!("/installations/{INSTALLATION}/repositories"),
        json!({"repositories": ["acme/api", "acme/web"]}),
    )
    .await;
    assert_eq!(added, reqwest::StatusCode::NO_CONTENT);
    // installation.suspend.
    let suspended = admin(
        &emulator,
        &format!("/installations/{INSTALLATION}/suspend"),
        json!({"sender": "installer"}),
    )
    .await;
    assert_eq!(suspended, reqwest::StatusCode::NO_CONTENT);
    let deliveries = settle(&emulator).await;
    let summary: Vec<(&str, Option<&str>, u16)> = deliveries
        .iter()
        .map(|d| (d.event.as_str(), d.action.as_deref(), d.status_code))
        .collect();
    assert_eq!(
        summary,
        [
            ("installation", Some("created"), 202),
            ("installation_repositories", Some("added"), 202),
            ("installation", Some("suspend"), 202),
        ]
    );
    let recorded = calls(&restate);
    assert_eq!(recorded.len(), 3);
    assert_eq!(recorded[1].method, "recheck_permissions");
    assert_eq!(recorded[1].key, "repo:77");
    assert_eq!(recorded[2].method, "onboard_webhook");
    assert_eq!(recorded[2].body["action"], "suspend");

    // Rotation: the emulator now signs with repo-new (the web side's first
    // secret): accepted.
    rotate(&emulator, &url, "repo-new");
    assert!(emulator.unsuspend_installation(INSTALLATION));
    let deliveries = settle(&emulator).await;
    assert_eq!(deliveries[3].action.as_deref(), Some("unsuspend"));
    assert_eq!(deliveries[3].status_code, 202);
    assert_eq!(calls(&restate).len(), 4);

    // A secret the web side never had (or has retired): 401, nothing
    // dispatched.
    rotate(&emulator, &url, "repo-retired");
    let removed = admin(
        &emulator,
        &format!("/installations/{INSTALLATION}/repositories"),
        json!({"repositories": ["acme/api"]}),
    )
    .await;
    assert_eq!(removed, reqwest::StatusCode::NO_CONTENT);
    let deliveries = settle(&emulator).await;
    let refused = &deliveries[4];
    assert_eq!(refused.action.as_deref(), Some("removed"));
    assert_eq!(refused.status_code, 401);
    assert_eq!(refused.status, "Invalid HTTP Response: 401");
    assert_eq!(calls(&restate).len(), 4);

    // Back on a valid secret: redelivering installation.created sends the
    // same body under the same GUID, and ghinvite derives the same
    // delivery_id, so the workflow side dedupes it.
    rotate(&emulator, &url, "repo-new");
    redeliver(&emulator, deliveries[0].id).await;
    let deliveries = settle(&emulator).await;
    let again = &deliveries[5];
    assert!(again.redelivery);
    assert_eq!(again.guid, deliveries[0].guid);
    assert_eq!(again.request.payload, deliveries[0].request.payload);
    assert_eq!(again.status_code, 202);
    let recorded = calls(&restate);
    assert_eq!(recorded.len(), 5);
    assert_eq!(recorded[4].method, "onboard_webhook");
    assert_eq!(recorded[4].body, recorded[0].body);
    assert_eq!(
        recorded[4].body["delivery_id"],
        recorded[0].body["delivery_id"]
    );

    // The refused one, redelivered with the right secret, goes through.
    redeliver(&emulator, refused.id).await;
    let deliveries = settle(&emulator).await;
    assert_eq!(deliveries[6].guid, refused.guid);
    assert_eq!(deliveries[6].status_code, 202);
    let recorded = calls(&restate);
    assert_eq!(recorded.len(), 6);
    assert_eq!(recorded[5].method, "recheck_permissions");
}

/// The org App's ID in `common::apps` (`GH_ORG_APP_ID`).
const ORG_APP: u64 = 456;

/// ghinvite-web's organization intake (`POST /webhooks/org`) parses the
/// emulator's documented `organization` and `membership` payloads (GE-06):
/// `member_invited` is ignored, `member_added`, `membership.added` and
/// `member_removed` are accepted (202), never refused as malformed (400).
#[tokio::test]
async fn the_web_intake_parses_emulator_organization_events() {
    let (base, _restate) = web().await;
    let url = format!("{base}/webhooks/org");
    let seed = Seed::new()
        .user(User::new(42, "installer"))
        .user(User::new(8, "alice"))
        .org(Org::new(9001, "acme"))
        .membership(Membership::new("acme", "installer", OrgRole::Admin))
        .team(octoemu::seed::Team::new(50, "acme", "devs"))
        .app(
            App::new(ORG_APP, "ghinvite-org", "Iv.fixture-org", "fixture-secret")
                .public_key(PUBLIC_KEY)
                .webhook(Webhook::new(&url, "org-current"))
                .permission("members", Access::Write)
                .permission("metadata", Access::Read)
                .event("organization")
                .event("membership"),
        )
        .installation(octoemu::seed::Installation::new(78, ORG_APP, "acme"));
    let emulator = Emulator::start(seed).await.unwrap();
    let token = {
        let response = reqwest::Client::new()
            .post(format!("{}/tokens/installation", emulator.admin_base()))
            .header("content-type", "application/json")
            .body(json!({"installation_id": 78}).to_string())
            .send()
            .await
            .unwrap();
        response.json::<Value>().await.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let api = |method: reqwest::Method, path: &str, body: Value| {
        reqwest::Client::new()
            .request(method, format!("{}{path}", emulator.api_base()))
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
    };
    let invited = api(
        reqwest::Method::POST,
        "/orgs/acme/invitations",
        json!({"invitee_id": 8, "team_ids": [50]}),
    )
    .await
    .unwrap();
    assert_eq!(invited.status(), reqwest::StatusCode::CREATED);
    let accepted = admin(&emulator, "/act/alice/orgs/acme/accept", json!({})).await;
    assert_eq!(accepted, reqwest::StatusCode::NO_CONTENT);
    let removed = api(
        reqwest::Method::DELETE,
        "/orgs/acme/memberships/alice",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::NO_CONTENT);
    let deliveries = settle(&emulator).await;
    let summary: Vec<(&str, Option<&str>, u16)> = deliveries
        .iter()
        .map(|d| (d.event.as_str(), d.action.as_deref(), d.status_code))
        .collect();
    assert_eq!(
        summary,
        [
            ("organization", Some("member_invited"), 202),
            ("organization", Some("member_added"), 202),
            ("membership", Some("added"), 202),
            ("organization", Some("member_removed"), 202),
        ]
    );
}
