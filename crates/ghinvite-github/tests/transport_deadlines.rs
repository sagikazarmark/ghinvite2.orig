use ghinvite_github::{
    Error, HttpTransport,
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
    assert!(matches!(headers, Err(Error::Transport(_))));
    assert!(matches!(body, Err(Error::Transport(_))));
    assert!(started.elapsed() >= std::time::Duration::from_secs(29));
}
