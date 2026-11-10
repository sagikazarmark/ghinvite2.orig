//! Authenticated webhook routing uses numeric members, never sender or invitation IDs.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use ghinvite_core::{org_delivery::*, storage::OrgDeliveryStorage};
use ghinvite_github::mocks::MockTransport;
use ghinvite_storage_sqlx::SqlxStorage;
use ghinvite_web::WebConfig;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::Arc;
use tower::ServiceExt;
mod common;
use common::restate_recorder::RestateRecorder;

async fn seeded() -> (Arc<SqlxStorage>, OrganizationDeliverySnapshot) {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    // Reuse the adapter-independent fixture: one current pending owner remains
    // after stale-generation and ambiguity controls, with a pending team step.
    ghinvite_core::storage::test_suite::scenario_organization_webhook_bindings((*storage).clone())
        .await;
    use ghinvite_core::storage::org_webhooks::{OrgWebhookStorage, OrganizationWebhookEvent};
    let wake = storage
        .bind_organization_webhook(
            &OrganizationWebhookEvent {
                app: ghinvite_core::account::App::Org,
                account_id: 9001,
                installation_id: 1,
                requester_id: 701,
                team_id: None,
                source:
                    ghinvite_core::org_webhook::OrganizationWebhookSource::OrganizationMemberAdded,
                payload_sha256: [99; 32],
            },
            Utc::now(),
            true,
        )
        .await
        .unwrap()
        .unwrap();
    let id = format!(
        "org:{}:{}:{}",
        wake.identity.request_id, wake.identity.target_id, wake.identity.delivery_generation
    );
    let snapshot = storage
        .get_organization_delivery(&id)
        .await
        .unwrap()
        .unwrap();
    (storage, snapshot)
}
fn app(
    storage: Arc<SqlxStorage>,
    client: Arc<ghinvite_web::RestateClient>,
    enabled: bool,
) -> axum::Router {
    let mut config = WebConfig::for_local_dev_with_secret([7; 32]);
    config.webhooks = Some(common::apps::webhooks("repo-current;repo-previous"));
    config.organization_delivery_enabled = enabled;
    common::build_app(
        crate::common::sign_in::app_state(
            storage,
            Arc::new(MockTransport::scripted(vec![])),
            client,
            config,
        ),
        tower_sessions::MemoryStore::default(),
    )
}
fn organization() -> Value {
    json!({"action":"member_added","installation":{"id":1},"organization":{"id":9001},"membership":{"user":{"id":701}},"sender":{"id":990},"invitation":{"id":888}})
}
fn membership() -> Value {
    json!({"action":"added","scope":"team","installation":{"id":1},"organization":{"id":9001},"team":{"id":5},"member":{"id":701},"sender":{"id":990}})
}
fn signed(path: &str, key: &str, event: &str, hint: &str, body: &Value) -> Request<Body> {
    let body = serde_json::to_vec(body).unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(&body);
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-github-event", event)
        .header("x-github-delivery", hint)
        .header(
            "x-hub-signature-256",
            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
        )
        .body(Body::from(body))
        .unwrap()
}
#[tokio::test]
async fn signed_numeric_org_identity_and_exact_retry_are_app_scoped() {
    let (storage, snapshot) = seeded().await;
    let rpc = RestateRecorder::start().await;
    let app = app(storage, rpc.client(), true);
    let payload = organization();
    for (path, key, status) in [
        ("/webhooks/org", "repo-current", StatusCode::UNAUTHORIZED),
        ("/webhooks/repo", "repo-current", StatusCode::ACCEPTED),
        ("/webhooks/org", "org-current", StatusCode::ACCEPTED),
        ("/webhooks/org", "org-previous", StatusCode::ACCEPTED),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(signed(path, key, "organization", key, &payload))
                .await
                .unwrap()
                .status(),
            status
        );
    }
    let calls = rpc.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body, calls[1].body);
    assert_eq!(calls[0].service, "OrganizationDelivery");
    assert_eq!(calls[0].method, "on_webhook");
    assert!(calls[0].send);
    assert_eq!(calls[0].key, snapshot.create.command.delivery_key());
    assert_eq!(calls[0].body["requester_id"], 701);
    assert_eq!(calls[0].body["identity"]["kind"], "organization");
    assert!(calls[0].body["team_id"].is_null());
    assert_eq!(calls[0].body["source"], "organization_member_added");
}
#[tokio::test]
async fn team_events_bind_numeric_team_and_member_without_org_invitation_guessing() {
    let (storage, _) = seeded().await;
    let rpc = RestateRecorder::start().await;
    let app = app(storage, rpc.client(), true);
    let body = membership();
    assert_eq!(
        app.oneshot(signed(
            "/webhooks/org",
            "org-current",
            "membership",
            "team",
            &body
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::ACCEPTED
    );
    let calls = rpc.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["team_id"], 5);
    assert_eq!(calls[0].body["requester_id"], 701);
    assert_eq!(calls[0].body["source"], "team_membership_added");
}
#[tokio::test]
async fn unsupported_composition_and_no_matches_never_rebind_on_retry() {
    let (storage, _) = seeded().await;
    let rpc = RestateRecorder::start().await;
    let body = organization();
    assert_eq!(
        app(storage.clone(), rpc.client(), false)
            .oneshot(signed(
                "/webhooks/org",
                "org-current",
                "organization",
                "off",
                &body
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        app(storage.clone(), rpc.client(), true)
            .oneshot(signed(
                "/webhooks/org",
                "org-current",
                "organization",
                "on",
                &body
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert!(rpc.calls().is_empty());
    let mut wrong = organization();
    wrong["membership"]["user"]["id"] = json!(990);
    assert_eq!(
        app(storage, rpc.client(), true)
            .oneshot(signed(
                "/webhooks/org",
                "org-current",
                "organization",
                "invite-id",
                &wrong
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert!(rpc.calls().is_empty());
}
#[tokio::test]
async fn malformed_identity_and_invited_event_cannot_dispatch() {
    let (storage, _) = seeded().await;
    let rpc = RestateRecorder::start().await;
    let app = app(storage, rpc.client(), true);
    let mut cases = Vec::new();
    let mut body = organization();
    body.as_object_mut().unwrap().remove("membership");
    cases.push((body, "organization", StatusCode::BAD_REQUEST));
    let mut body = organization();
    body["installation"]["app_id"] = json!(123);
    cases.push((body, "organization", StatusCode::BAD_REQUEST));
    let mut body = organization();
    body["membership"]["user"]["id"] = json!(9007199254740992u64);
    cases.push((body, "organization", StatusCode::BAD_REQUEST));
    let mut body = membership();
    body["scope"] = json!("organization");
    cases.push((body, "membership", StatusCode::BAD_REQUEST));
    let mut body = organization();
    body["action"] = json!("member_invited");
    cases.push((body, "organization", StatusCode::ACCEPTED));
    for (body, event, status) in cases {
        assert_eq!(
            app.clone()
                .oneshot(signed("/webhooks/org", "org-current", event, "hint", &body))
                .await
                .unwrap()
                .status(),
            status
        );
    }
    assert!(rpc.calls().is_empty());
}
#[tokio::test]
async fn uncertain_ingress_ack_and_concurrent_duplicates_resend_the_same_bound_wake() {
    use axum::{Json, extract::State, routing::post};
    use std::sync::Mutex;
    async fn receive(
        State(calls): State<Arc<Mutex<Vec<Value>>>>,
        Json(body): Json<Value>,
    ) -> StatusCode {
        let mut calls = calls.lock().unwrap();
        calls.push(body);
        if calls.len() == 1 {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        }
    }
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/OrganizationDelivery/{key}/on_webhook/send", post(receive))
        .with_state(calls.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Arc::new(ghinvite_web::RestateClient::new(url).unwrap());
    let (storage, _) = seeded().await;
    let app = app(storage.clone(), client.clone(), true);
    let body = organization();
    let first = app
        .clone()
        .oneshot(signed(
            "/webhooks/org",
            "org-current",
            "organization",
            "lost",
            &body,
        ))
        .await
        .unwrap();
    assert!(first.status().is_server_error());
    let (a, b) = tokio::join!(
        app.clone().oneshot(signed(
            "/webhooks/org",
            "org-current",
            "organization",
            "retry-a",
            &body
        )),
        app.clone().oneshot(signed(
            "/webhooks/org",
            "org-current",
            "organization",
            "retry-b",
            &body
        ))
    );
    assert_eq!(a.unwrap().status(), StatusCode::ACCEPTED);
    assert_eq!(b.unwrap().status(), StatusCode::ACCEPTED);
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert!(
        calls
            .lock()
            .unwrap()
            .windows(2)
            .all(|pair| pair[0] == pair[1])
    );
    let disabled = crate::app(storage, client, false);
    assert_eq!(
        disabled
            .oneshot(signed(
                "/webhooks/org",
                "org-current",
                "organization",
                "disabled",
                &body
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        calls.lock().unwrap().len(),
        3,
        "disabled deployment must not dispatch a previously matched receipt"
    );
    server.abort();
}

#[tokio::test]
async fn member_removed_is_an_authenticated_account_hint_not_a_requester_delivery_binding() {
    let (storage, _) = seeded().await;
    let rpc = RestateRecorder::start().await;
    let app = app(storage, rpc.client(), true);
    let body = json!({"action":"member_removed","installation":{"id":1},"organization":{"id":9001},"membership":{"user":{"id":999},"organization":{"id":9001}},"sender":{"id":7}});
    for secret in ["org-current", "org-previous"] {
        assert_eq!(
            app.clone()
                .oneshot(signed(
                    "/webhooks/org",
                    secret,
                    "organization",
                    secret,
                    &body
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }
    {
        let calls = rpc.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].body, calls[1].body);
        assert_eq!(calls[0].service, "OrganizationEligibility");
        assert_eq!(calls[0].key, "org:9001");
        assert_eq!(calls[0].method, "member_removed");
        assert!(calls[0].send);
        assert_eq!(calls[0].body["removed_user_id"], 999);
        assert!(calls[0].body.get("requester_id").is_none());
        assert!(calls[0].body.get("identity").is_none());
    }
    let mut invalid = body.clone();
    invalid["membership"]["organization"]["id"] = json!(9002);
    assert_eq!(
        app.clone()
            .oneshot(signed(
                "/webhooks/org",
                "org-current",
                "organization",
                "mismatch",
                &invalid
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.oneshot(signed(
            "/webhooks/org",
            "repo-current",
            "organization",
            "wrong-key",
            &body
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(rpc.calls().len(), 2);
}

/// GM-00 (PD-9): organization webhooks key their dedupe by SHA-256 of the
/// exact signed body (gh-toolkit `dedupe.payload_sha256`): hex in
/// `organization_webhook_receipts.payload_sha256`, bytes in the bound wake, and
/// `MemberRemoved::event_key()` in OrganizationEligibility's receipts. The
/// expected digests were computed independently.
fn signed_bytes(key: &str, body: &str) -> Request<Body> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    Request::builder()
        .method("POST")
        .uri("/webhooks/org")
        .header("content-type", "application/json")
        .header("x-github-event", "organization")
        .header("x-github-delivery", "golden")
        .header(
            "x-hub-signature-256",
            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
        )
        .body(Body::from(body.to_owned()))
        .unwrap()
}
fn digest_bytes(hex_digest: &str) -> Value {
    json!(hex::decode(hex_digest).unwrap())
}
const GOLDEN_ADDED_BODY: &str = concat!(
    r#"{"action": "member_added", "installation": {"id": 1}, "organization": {"id": 9001},"#,
    "\n",
    r#" "membership": {"user": {"id": 701}}, "sender": {"id": 990}, "invitation": {"id": 888}}"#,
    "\n",
);
const GOLDEN_ADDED_DIGEST: &str =
    "7bb73861c848116f37b454a338b9d44cd100889ed8517c61e65e2ba2cc97ab38";
const GOLDEN_REMOVED_BODY: &str = concat!(
    r#"{"action": "member_removed", "installation": {"id": 1}, "organization": {"id": 9001},"#,
    "\n",
    r#" "membership": {"user": {"id": 999}, "organization": {"id": 9001}}, "sender": {"id": 7}}"#,
    "\n",
);
const GOLDEN_REMOVED_DIGEST: &str =
    "d808a8d46bf2e065b087a10b2cc268df6d19f8e7cffad72a65a8e2664a06dd26";
#[tokio::test]
async fn golden_organization_webhook_dedupe_keys() {
    let database = std::env::temp_dir().join(format!(
        "ghinvite-golden-org-webhooks-{}.db",
        ghinvite_core::RequestId::new()
    ));
    let storage = Arc::new(SqlxStorage::at_path(&database).await.unwrap());
    ghinvite_core::storage::test_suite::scenario_organization_webhook_bindings((*storage).clone())
        .await;
    let rpc = RestateRecorder::start().await;
    let app = app(storage, rpc.client(), true);
    for body in [GOLDEN_ADDED_BODY, GOLDEN_REMOVED_BODY] {
        assert_eq!(
            app.clone()
                .oneshot(signed_bytes("org-current", body))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }
    {
        let calls = rpc.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].method, "on_webhook");
        assert_eq!(
            calls[0].body["payload_sha256"],
            digest_bytes(GOLDEN_ADDED_DIGEST)
        );
        assert_eq!(calls[1].method, "member_removed");
        assert_eq!(
            calls[1].body["payload_sha256"],
            digest_bytes(GOLDEN_REMOVED_DIGEST)
        );
        let removed: ghinvite_core::org_eligibility::MemberRemoved =
            serde_json::from_value(calls[1].body.clone()).unwrap();
        assert_eq!(removed.event_key(), GOLDEN_REMOVED_DIGEST);
    }
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(&database),
    )
    .await
    .unwrap();
    let receipts: Vec<String> = sqlx::query_scalar(
        "SELECT payload_sha256 FROM organization_webhook_receipts WHERE app='org'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        receipts.contains(&GOLDEN_ADDED_DIGEST.to_owned()),
        "{receipts:?}"
    );
    pool.close().await;
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", database.display()));
    }
}
