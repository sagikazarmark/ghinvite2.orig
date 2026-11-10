use ghinvite_github::{
    HttpTransport,
    transport::TransportError,
    transport::{Method, Request, ReqwestTransport},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn native_default_deadline_bounds_stalled_headers_and_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            connections.spawn(async move {
                let mut request = [0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                if String::from_utf8_lossy(&request[..n]).contains("/body") {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                        .await
                        .unwrap();
                }
                std::future::pending::<()>().await;
            });
        }
    });
    let transport = ReqwestTransport::new().unwrap();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_secs(35), async {
        tokio::join!(
            transport.send(Request::new(Method::Get, format!("{base}/headers"))),
            transport.send(Request::new(Method::Put, format!("{base}/body"))),
        )
    })
    .await;
    server.abort();
    let (headers, body) = result.expect("application deadline must precede watchdog");
    assert!(matches!(
        headers,
        Err(TransportError::SentNoResponse { timed_out: true })
    ));
    assert!(matches!(
        body,
        Err(TransportError::SentNoResponse { timed_out: true })
    ));
    assert!(started.elapsed() >= std::time::Duration::from_secs(29));
}

/// A same-origin pagination `Link` is accepted with its query intact, so a
/// credential a gateway puts there becomes part of the *next* request's URL.
/// `reqwest` renders the URL it was working on into its error text, which would
/// then reach logs and Restate terminal errors through `Error::Transport`.
#[tokio::test]
async fn a_transport_failure_never_renders_the_url_it_was_fetching() {
    // Reserve then release a port so connecting to it fails for real.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let secret = "token-test-only-not-a-credential";
    let url = format!("http://{address}/installation/repositories?page=2&access_token={secret}");
    let transport = ReqwestTransport::new().unwrap();

    let error = transport
        .send(Request::new(Method::Get, &url))
        .await
        .unwrap_err();

    let rendered = format!("{error} {error:?}");
    assert!(matches!(error, TransportError::NotSent), "{rendered}");
    assert!(!rendered.contains(secret), "{rendered}");
    assert!(!rendered.contains(&address.to_string()), "{rendered}");
    // The classification is what triage actually reads.
    assert!(rendered.contains("gh:transport:not_sent"), "{rendered}");
}

#[tokio::test]
async fn default_transport_never_repeats_a_write_after_a_redirect() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let destination = format!("{base}/redirected");
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 8192];
            assert!(socket.read(&mut bytes).await.unwrap() > 0);
            seen.fetch_add(1, Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let response = ReqwestTransport::new()
        .unwrap()
        .send(
            Request::new(Method::Put, format!("{base}/write"))
                .header("authorization", "Bearer ghs_test_marker"),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 307);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}
