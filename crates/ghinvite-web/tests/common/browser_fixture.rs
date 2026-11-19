//! Serving a Playwright fixture server (a `webServer` in `tests/browser`).
//!
//! The server logs the app's own `tracing` events (an internal error's cause,
//! a refused throttle) and one line per completed request with its status, so
//! a CI failure shows the server side of what the browser saw. Playwright
//! prints a web server's stderr prefixed `[WebServer]`; with
//! `GHINVITE_FIXTURE_LOG_DIR` set (CI sets it), the log also goes to
//! `<dir>/<fixture test name>.log`, which CI uploads with the browser test
//! results. `RUST_LOG` overrides the default `info` filter.
#![allow(dead_code)]

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriterExt};

/// Serves `router` on `address` until the process ends, with request logging.
pub async fn serve(address: &str, router: axum::Router) {
    // A test binary may run several fixtures; the first subscriber wins.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(writer())
        .with_ansi(false)
        .try_init();
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    tracing::info!(target: "browser_fixture", %address, "listening");
    let router = router.layer(axum::middleware::from_fn(log_request));
    axum::serve(listener, router).await.unwrap();
}

/// Stderr, and the log file when `GHINVITE_FIXTURE_LOG_DIR` names a directory.
fn writer() -> BoxMakeWriter {
    let Some(dir) = std::env::var_os("GHINVITE_FIXTURE_LOG_DIR") else {
        return BoxMakeWriter::new(std::io::stderr);
    };
    // A `#[tokio::test]` runs on a thread named after the test.
    let name = std::thread::current()
        .name()
        .unwrap_or("fixture")
        .replace("::", ".");
    std::fs::create_dir_all(&dir).unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::Path::new(&dir).join(format!("{name}.log")))
        .unwrap();
    BoxMakeWriter::new(std::io::stderr.and(Arc::new(file)))
}

async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if response.status().is_server_error() {
        tracing::warn!(target: "browser_fixture", %method, %path, status, elapsed_ms, "request");
    } else {
        tracing::info!(target: "browser_fixture", %method, %path, status, elapsed_ms, "request");
    }
    response
}
