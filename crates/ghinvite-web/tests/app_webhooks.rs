//! Signed app scope, operation identity and removal of obsolete member routing.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ghinvite_github::mocks::MockTransport;
use ghinvite_web::WebConfig;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
mod common;
use common::restate_recorder::RestateRecorder;
async fn app() -> (axum::Router, RestateRecorder) {
    app_with_secrets("repo-current;repo-previous").await
}
async fn app_with_secrets(repo_secrets: &str) -> (axum::Router, RestateRecorder) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let restate = RestateRecorder::start().await;
    let mut config = WebConfig::for_local_dev_with_secret([7; 32]);
    config.webhooks = Some(common::apps::webhooks(repo_secrets));
    (
        common::build_app(
            crate::common::sign_in::app_state(
                storage,
                Arc::new(MockTransport::scripted(vec![])),
                restate.client(),
                config,
            ),
            tower_sessions::MemoryStore::default(),
        ),
        restate,
    )
}
fn payload(action: &str, app_id: u64) -> Value {
    json!({"action":action,"installation":common::apps::installation(app_id,77,9001,"acme","Organization"),"sender":{"id":42,"login":"installer"}})
}
fn signed(path: &str, key: &str, event: &str, id: &str, body: &Value) -> Request<Body> {
    let body = serde_json::to_vec(body).unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(&body);
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-github-event", event)
        .header("x-github-delivery", id)
        .header(
            "x-hub-signature-256",
            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
        )
        .body(Body::from(body))
        .unwrap()
}
#[tokio::test]
async fn rotated_secrets_are_app_scoped_and_headers_cannot_change_command_identity() {
    let (app, restate) = app().await;
    let body = payload("created", 123);
    for (path, key, status) in [
        ("/webhooks/github", "repo-current", StatusCode::NOT_FOUND),
        ("/webhooks/signin", "repo-current", StatusCode::NOT_FOUND),
        ("/webhooks/repo", "org-current", StatusCode::UNAUTHORIZED),
        ("/webhooks/org", "repo-current", StatusCode::UNAUTHORIZED),
        ("/webhooks/repo", "repo-current", StatusCode::ACCEPTED),
        ("/webhooks/repo", "repo-previous", StatusCode::ACCEPTED),
    ] {
        let response = app
            .clone()
            .oneshot(signed(path, key, "installation", key, &body))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
    let calls = restate.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body, calls[1].body);
    assert_eq!(calls[0].key, "repo:77");
    assert_eq!(calls[0].method, "onboard_webhook");
    assert!(calls[0].send);
    assert_eq!(calls[0].body["sender_id"], 42);
    assert_eq!(calls[0].body["sender_login"], "installer");
    assert_eq!(
        calls[0].body["delivery_id"],
        hex::encode(Sha256::digest(serde_json::to_vec(&body).unwrap()))
    );
}
/// Rotation: every configured secret verifies, in any position; a secret
/// rotated out of the configuration no longer does.
#[tokio::test]
async fn a_rotated_out_secret_is_refused_while_old_and_new_are_accepted() {
    let (app, restate) = app_with_secrets("repo-next;repo-current").await;
    let body = payload("created", 123);
    for (key, status) in [
        ("repo-next", StatusCode::ACCEPTED),
        ("repo-current", StatusCode::ACCEPTED),
        ("repo-previous", StatusCode::UNAUTHORIZED),
    ] {
        let response = app
            .clone()
            .oneshot(signed("/webhooks/repo", key, "installation", key, &body))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{key}");
    }
    assert_eq!(restate.calls().len(), 2);
}
#[tokio::test]
async fn all_installation_actions_and_repository_changes_route_to_the_selected_app() {
    let (app, restate) = app().await;
    for name in ["repo", "org"] {
        let (app_id, secret) = if name == "repo" {
            (123, "repo-current")
        } else {
            (456, "org-current")
        };
        for action in [
            "created",
            "new_permissions_accepted",
            "deleted",
            "suspend",
            "unsuspend",
        ] {
            let response = app
                .clone()
                .oneshot(signed(
                    &format!("/webhooks/{name}"),
                    secret,
                    "installation",
                    action,
                    &payload(action, app_id),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }
        for action in ["added", "removed"] {
            let mut body = payload(action, app_id);
            body["repository_selection"] = json!("selected");
            body["repositories_added"] = json!([{"id":10,"name":"api","full_name":"acme/api"}]);
            body["repositories_removed"] = json!([]);
            let response = app
                .clone()
                .oneshot(signed(
                    &format!("/webhooks/{name}"),
                    secret,
                    "installation_repositories",
                    action,
                    &body,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }
    }
    let calls = restate.calls();
    assert_eq!(calls.len(), 14);
    for (index, call) in calls.iter().enumerate() {
        assert_eq!(call.key, if index < 7 { "repo:77" } else { "org:77" });
        assert!(call.send);
        assert_eq!(
            call.method,
            if index % 7 < 5 {
                "onboard_webhook"
            } else {
                "recheck_permissions"
            }
        );
    }
}
#[tokio::test]
async fn malformed_or_cross_app_signed_payloads_cannot_dispatch_and_legacy_events_are_ignored() {
    let (app, restate) = app().await;
    for event in ["member", "repository_invitation", "future_event"] {
        let response = app
            .clone()
            .oneshot(signed(
                "/webhooks/repo",
                "repo-current",
                event,
                "legacy",
                &json!({"action":"added"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    let response = app
        .clone()
        .oneshot(signed(
            "/webhooks/repo",
            "repo-current",
            "installation",
            "wrong-app",
            &payload("created", 456),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    for field in ["sender", "installation"] {
        let mut body = payload("created", 123);
        body.as_object_mut().unwrap().remove(field);
        let response = app
            .clone()
            .oneshot(signed(
                "/webhooks/repo",
                "repo-current",
                "installation",
                "malformed",
                &body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let mut body = payload("created", 123);
    body["member"] = json!({"id":42});
    let response = app
        .clone()
        .oneshot(signed(
            "/webhooks/repo",
            "repo-current",
            "installation",
            "changed-kind",
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(signed(
            "/webhooks/repo",
            "repo-current",
            "installation",
            "future",
            &json!({"action":"future_action"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(restate.calls().is_empty());
}

/// GM-00 (PD-9): the dedupe key is SHA-256 over the exact signed body bytes
/// (gh-toolkit `VerifiedDelivery::dedupe.payload_sha256`), sent hex-encoded as
/// `delivery_id`; ghinvite-workflows stores it as Restate state `webhook/{hex}`
/// (`availability.rs`). The expected digest was computed independently.
const GOLDEN_APP_BODY: &str = concat!(
    r#"{"action": "created","#,
    "\n",
    r#" "installation": {"id": 77, "app_id": 123, "created_at": "2026-05-20T11:50:00Z", "updated_at": "2026-05-20T11:50:00Z", "suspended_at": null, "repository_selection": "selected", "permissions": {"administration": "write", "metadata": "read"}, "account": {"id": 9001, "login": "acme", "type": "Organization"}},"#,
    "\n",
    r#" "sender": {"id": 42, "login": "installer"}}"#,
    "\n",
);
const GOLDEN_APP_DIGEST: &str = "a3b7c219529458618a9e4a7d802827494459ccd4dcaa6924c512c8b50089445c";
#[tokio::test]
async fn golden_webhook_dedupe_key() {
    let (app, restate) = app().await;
    for (key, hint) in [("repo-current", "first"), ("repo-previous", "second")] {
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(GOLDEN_APP_BODY.as_bytes());
        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/repo")
            .header("content-type", "application/json")
            .header("x-github-event", "installation")
            .header("x-github-delivery", hint)
            .header(
                "x-hub-signature-256",
                format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
            )
            .body(Body::from(GOLDEN_APP_BODY))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    let calls = restate.calls();
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        assert_eq!(call.method, "onboard_webhook");
        assert_eq!(call.body["delivery_id"], GOLDEN_APP_DIGEST);
    }
}
