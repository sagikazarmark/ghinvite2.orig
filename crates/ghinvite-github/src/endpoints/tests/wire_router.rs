//! The frozen synthetic HTTP fixture for every typed endpoint, served over
//! real TCP for `http_contract` (test-only; never deploy it).
//!
//! Requests must match the corpus's literal method, target, authentication,
//! headers and serialized body. The default serves the first reviewed success
//! pair. `POST /__fixture/select` accepts `{ "endpoint": "PutCollaborator",
//! "response": 1 }` to select another listed success response, or
//! `{ "protocol_case": "sso-partial" }` for a named header/status family.
//! `GET /__fixture/summary` returns only request/match/refusal counts. All inputs
//! and responses are synthetic fixtures; they do not establish live GitHub
//! behavior, permissions, or unresolved spike results.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    response::Response,
    routing::any,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Counters {
    requests: u64,
    matched: u64,
    refused: u64,
    replies: BTreeMap<String, usize>,
    protocol: Option<String>,
}
#[derive(Clone)]
struct Fixture {
    corpus: Arc<Value>,
    counts: Arc<Mutex<Counters>>,
}
pub(super) fn router() -> Router {
    let corpus = serde_json::from_str(include_str!("../../../fixtures/http-fixtures.json"))
        .expect("reviewed wire fixture JSON");
    Router::new().fallback(any(serve)).with_state(Fixture {
        corpus: Arc::new(corpus),
        counts: Arc::default(),
    })
}
fn response(status: u16, headers: &Value, body: Option<&Value>) -> Response {
    let mut result = Response::builder().status(status);
    if let Some(headers) = headers.as_object() {
        for (name, value) in headers {
            result = result.header(name, value.as_str().expect("fixture header text"));
        }
    }
    result
        .body(Body::from(
            body.map(|v| serde_json::to_vec(v).expect("fixture body JSON"))
                .unwrap_or_default(),
        ))
        .expect("reviewed fixture response")
}
fn refused(state: &Fixture) -> Response {
    state.counts.lock().unwrap().refused += 1;
    response(
        400,
        &json!({"content-type":"application/json"}),
        Some(&json!({"error":"wire_fixture_mismatch"})),
    )
}
async fn serve(State(state): State<Fixture>, request: Request) -> Response {
    let method = request.method().as_str().to_owned();
    let target = request
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    if method == "GET" && target == "/__fixture/summary" {
        let counts = state.counts.lock().unwrap();
        return response(
            200,
            &json!({"content-type":"application/json"}),
            Some(
                &json!({"requests":counts.requests,"matched":counts.matched,"refused":counts.refused}),
            ),
        );
    }
    let headers = request.headers().clone();
    let Ok(body) = to_bytes(request.into_body(), 1024 * 1024).await else {
        return refused(&state);
    };
    let body = zeroize::Zeroizing::new(body.to_vec());
    if method == "POST" && target == "/__fixture/select" {
        let Ok(selection) = serde_json::from_slice::<Value>(&body) else {
            return refused(&state);
        };
        let mut counts = state.counts.lock().unwrap();
        if let Some(name) = selection.get("protocol_case").and_then(Value::as_str) {
            if !state.corpus["protocol_cases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["name"] == name)
            {
                drop(counts);
                return refused(&state);
            }
            counts.protocol = Some(name.to_owned());
        } else if let Some(endpoint) = selection.get("endpoint").and_then(Value::as_str) {
            let index = selection
                .get("response")
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok());
            if !state.corpus["endpoints"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| {
                    r["endpoint"] == endpoint
                        && index.is_some_and(|i| i < r["responses"].as_array().unwrap().len())
                })
            {
                drop(counts);
                return refused(&state);
            }
            counts.protocol = None;
            counts.replies.insert(endpoint.to_owned(), index.unwrap());
        } else {
            drop(counts);
            return refused(&state);
        }
        return response(204, &Value::Null, None);
    }
    state.counts.lock().unwrap().requests += 1;
    for pair in state.corpus["endpoints"].as_array().unwrap() {
        if !crate::endpoints::Endpoint::all()
            .iter()
            .any(|e| format!("{e:?}") == pair["endpoint"].as_str().unwrap())
        {
            continue;
        }
        if pair["method"] != method || (pair["target"] != target && pair["helper_target"] != target)
        {
            continue;
        }
        let actual_auth = headers.get("authorization").and_then(|v| v.to_str().ok());
        let valid_auth = pair["authorization"]
            .as_object()
            .unwrap()
            .values()
            .any(|v| v.as_str() == actual_auth);
        let valid_headers = pair["request_headers"]
            .as_object()
            .unwrap()
            .iter()
            .all(|(name, value)| headers.get(name).and_then(|v| v.to_str().ok()) == value.as_str());
        if !valid_auth
            || !valid_headers
            || body.as_slice() != pair["request_body_wire"].as_str().unwrap().as_bytes()
        {
            return refused(&state);
        }
        let mut counts = state.counts.lock().unwrap();
        counts.matched += 1;
        if let Some(name) = &counts.protocol
            && let Some(protocol) = state.corpus["protocol_cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["name"] == name.as_str() && r["endpoint"] == pair["endpoint"])
        {
            return response(
                protocol["status"].as_u64().unwrap() as u16,
                &protocol["headers"],
                Some(&protocol["body"]),
            );
        }
        let index = counts
            .replies
            .get(pair["endpoint"].as_str().unwrap())
            .copied()
            .unwrap_or_default();
        let reply = &pair["responses"][index];
        let status = reply["status"].as_u64().unwrap() as u16;
        return response(
            status,
            &pair["response_headers"],
            (status != 204).then_some(&reply["body"]),
        );
    }
    refused(&state)
}
