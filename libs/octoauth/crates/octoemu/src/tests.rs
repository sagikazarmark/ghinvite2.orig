//! The route registry, the manifest check and the refuse-assumed enforcement
//! points, exercised with a test-only route (no GitHub route exists yet).

use crate::api::{self, Api, BEHAVIOURS};
use crate::fidelity::Manifest;
use crate::response::{self, REQUEST_ID};
use crate::seed::Seed;
use crate::state::SharedState;
use crate::{Emulator, calls::CredentialKind};
use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use std::sync::Arc;

#[test]
fn every_registered_route_and_behaviour_has_a_manifest_entry() {
    let manifest = Manifest::embedded().unwrap();
    let api = api::github();
    manifest
        .check(
            api.routes().iter().map(String::as_str),
            BEHAVIOURS.iter().copied(),
        )
        .unwrap();
}

#[test]
fn the_check_catches_an_unlisted_route() {
    let manifest = Manifest::embedded().unwrap();
    let api = Api::new().route(Method::GET, "/meta", async || "{}");
    let error = manifest
        .check(
            api.routes().iter().map(String::as_str),
            BEHAVIOURS.iter().copied(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("\"GET /meta\""), "{error}");
}

/// A test manifest: `GET /meta` assumed, `GET /zen` documented, and the
/// embedded behaviours.
fn test_manifest() -> Arc<Manifest> {
    let mut manifest = Manifest::parse(
        r#"
        [routes."GET /meta"]
        label = "assumed"
        reason = "a test route"

        [routes."GET /zen"]
        label = "documented"
        doc = "https://docs.github.com/rest/meta/meta#get-the-zen-of-github"
        "#,
    )
    .unwrap();
    manifest.behaviours = Manifest::embedded().unwrap().behaviours.clone();
    Arc::new(manifest)
}

async fn guessed(State(shared): State<SharedState>) -> Response {
    if let Some(refusal) = shared.refuse("rate-limit-headers") {
        return refusal;
    }
    response::json(StatusCode::OK, &serde_json::json!({"guessed": true}))
}

fn test_api() -> Api {
    Api::new()
        .route(Method::GET, "/meta", async || {
            response::json(StatusCode::OK, &serde_json::json!({}))
        })
        .route(Method::GET, "/zen", async || "Keep it logically awesome.")
        .route(Method::GET, "/guessed", guessed)
}

async fn start(seed: Seed) -> Emulator {
    let mut manifest = (*test_manifest()).clone();
    manifest.routes.insert(
        "GET /guessed".into(),
        crate::fidelity::Fidelity::Documented {
            doc: "https://docs.github.com/rest".into(),
        },
    );
    Emulator::builder(seed)
        .start_with(test_api(), Arc::new(manifest))
        .await
        .unwrap()
}

#[tokio::test]
async fn registered_routes_are_logged_and_other_methods_answer_501() {
    let emulator = start(Seed::new()).await;
    let client = reqwest::Client::new();
    let base = emulator.api_base();

    let ok = client
        .get(format!("{base}/zen"))
        .bearer_auth("ghs_1_x")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let wrong_method = client.delete(format!("{base}/zen")).send().await.unwrap();
    assert_eq!(wrong_method.status(), 501);
    let body: serde_json::Value = wrong_method.json_value().await;
    assert_eq!(body["route"], "DELETE /zen");

    let calls = emulator.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].route.as_deref(), Some("GET /zen"));
    assert_eq!(calls[0].credential, CredentialKind::InstallationToken);
    assert_eq!(calls[1].route, None);
    assert_eq!(calls[1].status, 501);
}

#[tokio::test]
async fn refuse_assumed_refuses_assumed_routes_behaviours_and_headers() {
    // Off: everything answers, with the assumed headers.
    let relaxed = start(Seed::new()).await;
    let client = reqwest::Client::new();
    for path in ["/meta", "/zen", "/guessed"] {
        let response = client
            .get(format!("{}{path}", relaxed.api_base()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
        assert!(response.headers().contains_key("x-ratelimit-limit"));
        assert!(response.headers().contains_key(&REQUEST_ID));
    }

    // On: the assumed route and behaviour are refused, the documented route
    // answers without the assumed headers.
    let strict = start(Seed::new().refuse_assumed()).await;
    let meta = client
        .get(format!("{}/meta", strict.api_base()))
        .send()
        .await
        .unwrap();
    assert_eq!(meta.status(), 501);
    let body = meta.json_value().await;
    assert_eq!(body["message"], "octoemu: refusing assumed behaviour");
    assert_eq!(body["route"], "GET /meta");
    assert_eq!(body["reason"], "a test route");
    assert_eq!(strict.calls()[0].route.as_deref(), Some("GET /meta"));

    let guessed = client
        .get(format!("{}/guessed", strict.api_base()))
        .send()
        .await
        .unwrap();
    assert_eq!(guessed.status(), 501);
    assert_eq!(
        guessed.json_value().await["behaviour"],
        "rate-limit-headers"
    );

    let zen = client
        .get(format!("{}/zen", strict.api_base()))
        .send()
        .await
        .unwrap();
    assert_eq!(zen.status(), 200);
    assert!(!zen.headers().contains_key("x-ratelimit-limit"));
    assert!(!zen.headers().contains_key(&REQUEST_ID));
    assert!(
        zen.headers().contains_key("date"),
        "documented: always sent"
    );
}

#[tokio::test]
async fn the_builder_flag_turns_refuse_assumed_on() {
    let emulator = Emulator::builder(Seed::new())
        .refuse_assumed()
        .start_with(test_api(), test_manifest())
        .await
        .unwrap();
    assert_eq!(
        emulator.state()["world"]["settings"]["refuse_assumed"],
        true
    );
}

trait JsonValue {
    async fn json_value(self) -> serde_json::Value;
}

impl JsonValue for reqwest::Response {
    async fn json_value(self) -> serde_json::Value {
        serde_json::from_str(&self.text().await.unwrap()).unwrap()
    }
}
