//! An in-process Restate ingress that records every invocation it receives.
//!
//! It answers request-response calls (`/{service}/{key}/{method}`) and one-way
//! sends (`/{service}/{key}/{method}/send`) with an empty success, so tests run
//! the real web commands and assert on what reached the wire: the service, key
//! and handler, call versus send, and the JSON body.
//!
//! The server runs on the test's tokio runtime, so it outlives this handle.
#![allow(dead_code)]

use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Debug)]
pub struct RecordedCall {
    pub service: String,
    pub key: String,
    pub method: String,
    pub send: bool,
    pub body: Value,
}

#[derive(Clone)]
pub struct RestateRecorder {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    url: String,
}

impl RestateRecorder {
    pub async fn start() -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = axum::Router::new()
            .route("/{service}/{key}/{method}", axum::routing::post(call))
            .route("/{service}/{key}/{method}/send", axum::routing::post(send))
            .with_state(calls.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { calls, url }
    }

    pub fn client(&self) -> Arc<ghinvite_web::RestateClient> {
        Arc::new(ghinvite_web::RestateClient::new(self.url.clone()).unwrap())
    }

    /// Every invocation received so far, in arrival order.
    pub fn calls(&self) -> MutexGuard<'_, Vec<RecordedCall>> {
        self.calls.lock().unwrap()
    }
}

type Calls = Arc<Mutex<Vec<RecordedCall>>>;

async fn call(
    State(calls): State<Calls>,
    Path((service, key, method)): Path<(String, String, String)>,
    axum::Json(body): axum::Json<Value>,
) -> StatusCode {
    record(&calls, service, key, method, false, body)
}

async fn send(
    State(calls): State<Calls>,
    Path((service, key, method)): Path<(String, String, String)>,
    axum::Json(body): axum::Json<Value>,
) -> StatusCode {
    record(&calls, service, key, method, true, body)
}

fn record(
    calls: &Calls,
    service: String,
    key: String,
    method: String,
    send: bool,
    body: Value,
) -> StatusCode {
    calls.lock().unwrap().push(RecordedCall {
        service,
        key,
        method,
        send,
        body,
    });
    StatusCode::OK
}
