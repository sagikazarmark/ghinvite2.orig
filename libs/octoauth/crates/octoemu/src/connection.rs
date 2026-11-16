//! The accept loop. octoemu serves each connection itself (instead of
//! `axum::serve`) so a fault rule can close or reset the connection a
//! request arrived on without answering it.

use axum::Router;
use axum::body::Body;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use std::io;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, watch};
use tower::ServiceExt as _;

/// A request's connection, in the request's extensions: lets the fault
/// layer drop it.
#[derive(Debug, Default)]
pub(crate) struct Connection {
    killed: Notify,
    reset: AtomicBool,
}

impl Connection {
    /// Closes the connection without a response (`reset`: with a TCP RST).
    pub(crate) fn kill(&self, reset: bool) {
        self.reset.store(reset, Ordering::SeqCst);
        self.killed.notify_one();
    }
}

/// Serves `app` on `listener` until `stop` turns true, then lets open
/// connections finish their in-flight requests.
pub(crate) async fn serve(
    listener: TcpListener,
    app: Router,
    mut stop: watch::Receiver<bool>,
) -> io::Result<()> {
    // Every connection task holds a sender; `recv` ends once all are gone.
    let (open, mut all_closed) = mpsc::channel::<()>(1);
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            () = stopped(&mut stop) => break,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            // Per-connection errors (a reset during the handshake, out of
            // descriptors): keep accepting, as axum::serve does.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }
        };
        let app = app.clone();
        let stop = stop.clone();
        let open = open.clone();
        tokio::spawn(async move {
            connection(stream, app, stop).await;
            drop(open);
        });
    }
    drop(listener);
    drop(open);
    let _ = all_closed.recv().await;
    Ok(())
}

/// Resolves once `stop` turns true (or its sender is gone).
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

async fn connection(stream: TcpStream, app: Router, mut stop: watch::Receiver<bool>) {
    let _ = stream.set_nodelay(true);
    // A second descriptor of the socket, to set SO_LINGER on a reset after
    // hyper owns the stream.
    let Ok(std_stream) = stream.into_std() else {
        return;
    };
    let Ok(spare) = std_stream.try_clone() else {
        return;
    };
    let Ok(stream) = TcpStream::from_std(std_stream) else {
        return;
    };
    let control = Arc::new(Connection::default());
    let service = {
        let control = control.clone();
        hyper::service::service_fn(move |request: hyper::Request<Incoming>| {
            let mut request = request.map(Body::new);
            request.extensions_mut().insert(control.clone());
            app.clone().oneshot(request)
        })
    };
    let mut conn = pin!(
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .with_upgrades()
    );
    let mut stopping = false;
    loop {
        tokio::select! {
            _ = conn.as_mut() => break,
            () = stopped(&mut stop), if !stopping => {
                stopping = true;
                conn.as_mut().graceful_shutdown();
            }
            () = control.killed.notified() => {
                if control.reset.load(Ordering::SeqCst) {
                    let _ = socket2::SockRef::from(&spare).set_linger(Some(Duration::ZERO));
                }
                break;
            }
        }
    }
    // Dropping `conn` and `spare` closes the socket (a RST with zero linger).
}
