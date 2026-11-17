//! Native actual-TCP execution of every synthetic HTTP pair through the
//! frozen synthetic fixture (`wire_router`).
use super::wire_contract::*;
use crate::{
    rest::{Rest, RestEnv},
    transport::{HttpTransport, HttpTransportDyn, Method, Request, ReqwestTransport},
};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::oneshot};

#[tokio::test]
async fn wire_router_serves_all_typed_pairs_and_header_status_families() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, super::wire_router::router())
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let fixture = fixtures();
    let native = ReqwestTransport::new().unwrap();
    let transport: Arc<dyn HttpTransportDyn> = Arc::new(native.clone());
    let environment = RestEnv::new(transport, &base, USER_AGENT).unwrap();
    let select = |body: serde_json::Value| {
        let native = native.clone();
        let base = base.clone();
        async move {
            let request = Request::new(Method::Post, format!("{base}/__fixture/select"))
                .json_body(&body)
                .unwrap();
            assert_eq!(
                HttpTransport::send(&native, request).await.unwrap().status,
                204
            );
        }
    };
    let mut expected_count = 0;
    for pair in &fixture.endpoints {
        for name in &pair.credentials {
            for (index, _) in pair.responses.iter().enumerate() {
                select(serde_json::json!({"endpoint":pair.endpoint,"response":index})).await;
                let credential = credential(pair, name);
                let result = dispatch(pair, &Rest::new(&environment, &*credential)).await;
                assert!(
                    result.is_ok(),
                    "reusable typed fixture failed for {}: {:?}",
                    pair.endpoint,
                    result.err()
                );
                expected_count += 1;
            }
        }
    }
    for row in &fixture.protocol_cases {
        select(serde_json::json!({"protocol_case":row["name"]})).await;
        let pair = fixture
            .endpoints
            .iter()
            .find(|p| p.endpoint == row["endpoint"])
            .unwrap();
        let credential = credential(pair, row["credential"].as_str().unwrap());
        let error = dispatch(pair, &Rest::new(&environment, &*credential))
            .await
            .unwrap_err();
        assert_eq!(error.code(), row["expected_error"].as_str().unwrap());
        expected_count += 1;
    }
    let summary = HttpTransport::send(
        &native,
        Request::new(Method::Get, format!("{base}/__fixture/summary")),
    )
    .await
    .unwrap();
    let summary: serde_json::Value = serde_json::from_slice(&summary.body).unwrap();
    assert_eq!(
        summary,
        serde_json::json!({"requests":expected_count,"matched":expected_count,"refused":0})
    );
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("reusable fixture shutdown deadline")
        .unwrap();
}

#[tokio::test]
async fn wire_router_refuses_mismatched_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, super::wire_router::router())
            .await
            .unwrap();
    });
    let fixture = fixtures();
    let pair = fixture
        .endpoints
        .iter()
        .find(|p| p.endpoint == "GetRepository")
        .unwrap();
    let environment = RestEnv::new(
        Arc::new(ReqwestTransport::new().unwrap()),
        &base,
        USER_AGENT,
    )
    .unwrap();
    let wrong = crate::mocks::installation_credential(123, 77, "ghs_not_the_reviewed_marker");
    let error = dispatch(pair, &Rest::new(&environment, &wrong))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "gh:api:status");
}
