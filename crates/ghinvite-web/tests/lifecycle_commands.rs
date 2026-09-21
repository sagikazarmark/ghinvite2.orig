use ghinvite_core::request_lifecycle::{DecideRequest, DecisionOutcome, RequestStatus};
use ghinvite_web::lifecycle::{RequestLifecycle, RestateRequestLifecycle};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

mod common;

use common::link_authority::LINK_SERVICE;

#[tokio::test]
async fn lifecycle_calls_link_authority_and_preserves_truthful_result() {
    let calls = Arc::new(Mutex::new(vec![]));
    let observed = calls.clone();
    let request = json!({"request_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAA",
        "account_id": 100, "requester_id": 11, "justification": null, "state": "expired",
        "admitted_at": "2026-01-01T00:00:00Z", "decision_deadline": "2026-01-08T00:00:00Z", "revision": 2});
    let response_request = request.clone();
    let app = axum::Router::new().route(
        "/{service}/{key}/{method}",
        axum::routing::post(
            move |axum::extract::Path(path): axum::extract::Path<(String, String, String)>,
                  axum::Json(body): axum::Json<Value>| {
                let calls = observed.clone();
                let request = response_request.clone();
                async move {
                    calls.lock().unwrap().push((path.clone(), body));
                    axum::Json(if path.2 == "decide" {
                        json!({"outcome": "incompatible", "request": request})
                    } else {
                        request
                    })
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = RestateRequestLifecycle::new(Arc::new(
        ghinvite_web::RestateClient::new(format!("http://{address}")).unwrap(),
    ));
    let command: DecideRequest = serde_json::from_value(json!({"version": 1,
        "link_id": request["link_id"], "request_id": request["request_id"],
        "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAB", "admin": {"account_id": 100, "user_id": 7},
        "action": {"kind": "approve"}}))
    .unwrap();
    let receipt = client.decide(command.clone()).await.unwrap();
    assert_eq!(receipt.outcome, DecisionOutcome::Incompatible);
    let status = client
        .status(RequestStatus {
            link_id: command.link_id,
            request_id: command.request_id,
            requester_id: 11,
        })
        .await
        .unwrap();
    assert_eq!(status.state, ghinvite_core::RequestState::Expired);
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls[0].0,
        (
            LINK_SERVICE.into(),
            command.link_id.to_string(),
            "decide".into()
        )
    );
    assert_eq!(calls[0].1, serde_json::to_value(command).unwrap());
    assert_eq!(calls[1].0.2, "request_status");
    server.abort();
}
