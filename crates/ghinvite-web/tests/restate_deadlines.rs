use ghinvite_web::{LinkAuthority, RestateClient, WebError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn native_restate_deadline_preserves_unknown_mutation_outcomes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = RestateClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let authority = LinkAuthority::new(std::sync::Arc::new(client.clone()));
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            connections.spawn(async move {
                let mut request = [0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                if String::from_utf8_lossy(&request[..n]).contains("/requester_page") {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                        .await
                        .unwrap();
                }
                std::future::pending::<()>().await;
            });
        }
    });
    let input = serde_json::json!({"operation_id": "original-attempt"});
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::join!(
            client.call::<_, serde_json::Value>("Fixture", "key", "headers", &input),
            async {
                authority
                    .requester_page("01ARZ3NDEKTSV4RRFFQ69G5FAV", 2, None)
                    .await
                    .map_err(WebError::from)
            },
            client.send("Fixture", "key", "headers", &input),
        )
    })
    .await;
    server.abort();
    let (headers, body, send) = result.expect("application deadline must precede watchdog");
    for error in [headers.unwrap_err(), body.unwrap_err(), send.unwrap_err()] {
        assert!(matches!(&error, WebError::Restate(_)));
        assert!(error.to_string().to_lowercase().contains("outcome unknown"));
    }
    assert!(started.elapsed() >= std::time::Duration::from_secs(14));
}
