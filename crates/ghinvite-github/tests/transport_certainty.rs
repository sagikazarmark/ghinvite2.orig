use ghinvite_github::{
    HttpTransport,
    transport::TransportError,
    transport::{MAX_RESPONSE_BYTES, Method, Request, ReqwestTransport},
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn build_and_connection_failures_are_provably_not_sent() {
    let transport = ReqwestTransport::new().unwrap();
    assert_eq!(
        transport
            .send(Request::new(Method::Post, "not a URL"))
            .await
            .unwrap_err(),
        TransportError::NotSent
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/write", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        transport
            .send(Request::new(Method::Put, url))
            .await
            .unwrap_err(),
        TransportError::NotSent
    );
}

#[tokio::test]
async fn timeouts_after_dispatch_are_ambiguous_for_headers_and_body() {
    for body in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/write", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            if body {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                    .await
                    .unwrap();
            }
            std::future::pending::<()>().await;
        });
        let result = ReqwestTransport::new()
            .unwrap()
            .send(Request::new(Method::Patch, url).timeout(Duration::from_millis(100)))
            .await;
        server.abort();
        assert_eq!(
            result.unwrap_err(),
            TransportError::SentNoResponse { timed_out: true }
        );
    }
}

#[tokio::test]
async fn response_limit_applies_with_and_without_content_length() {
    for advertised in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/read", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            let header = if advertised {
                format!("Content-Length: {}\r\n", MAX_RESPONSE_BYTES + 1)
            } else {
                String::new()
            };
            socket
                .write_all(
                    format!("HTTP/1.1 200 OK\r\n{header}Connection: close\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
            if advertised {
                // Must reject immediately from headers, without waiting for a body.
                std::future::pending::<()>().await;
            } else {
                let _ = socket.write_all(&vec![b'x'; MAX_RESPONSE_BYTES + 1]).await;
            }
        });
        let result = ReqwestTransport::new()
            .unwrap()
            .send(Request::new(Method::Get, url).timeout(Duration::from_secs(2)))
            .await;
        server.abort();
        assert_eq!(result.unwrap_err(), TransportError::Oversize);
    }
}

/// Implementers provide only the typed async method. The production object-safe
/// bridge must preserve every dispatch classification, without retry or erasure.
#[tokio::test]
async fn boxed_transport_preserves_all_failure_classes_and_dispatches_once() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Failure {
        error: TransportError,
        calls: Arc<AtomicUsize>,
    }
    impl HttpTransport for Failure {
        async fn send(&self, _: Request) -> Result<ghinvite_github::Response, TransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(self.error)
        }
    }
    for expected in [
        TransportError::NotSent,
        TransportError::SentNoResponse { timed_out: false },
        TransportError::SentNoResponse { timed_out: true },
        TransportError::Oversize,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let transport: Arc<dyn ghinvite_github::HttpTransportDyn> = Arc::new(Failure {
            error: expected,
            calls: calls.clone(),
        });
        // Spawning also proves that the boxed bridge preserves its Send contract.
        let outcome = tokio::spawn(async move {
            transport
                .send(
                    Request::new(Method::Post, "https://api.github.test/write")
                        .header("authorization", "Bearer PRIVATE_TRANSPORT_MARKER"),
                )
                .await
        })
        .await
        .unwrap();
        assert_eq!(outcome.unwrap_err(), expected);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!format!("{expected:?} {expected}").contains("PRIVATE_TRANSPORT_MARKER"));
    }
}
