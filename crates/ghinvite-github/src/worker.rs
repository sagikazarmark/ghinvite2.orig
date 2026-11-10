//! Cloudflare Workers transport. Every fetch uses manual redirects, its
//! lifetime is bounded through the final response byte, and dropping the
//! future aborts the host fetch. No JS promise or controller is cached.
//!
//! Host futures hold JavaScript values and are not `Send`. A Worker isolate is
//! single-threaded, so [`send_wrapper::SendWrapper`] adapts them to the `Send`
//! contract of [`HttpTransport`]; its thread check never fires there.

use crate::transport::{
    HttpTransport, MAX_RESPONSE_BYTES, MAX_TIMEOUT, Method, Request, Response, TransportError,
};
use ::worker::{AbortController, Fetch, Headers, RequestInit, RequestRedirect};
use futures_util::{
    TryStreamExt,
    future::{Either, select},
    task::{ArcWake, AtomicWaker, waker_ref},
};
use send_wrapper::SendWrapper;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

/// `fetch`-backed [`HttpTransport`] for Cloudflare Workers.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerFetchTransport;

impl WorkerFetchTransport {
    /// The transport has no state: build one wherever it is needed.
    pub fn new() -> Self {
        Self
    }

    async fn execute(request: Request) -> Result<Response, TransportError> {
        let headers = Headers::new();
        for (name, value) in &request.headers {
            headers
                .set(name, value)
                .map_err(|_| TransportError::NotSent)?;
        }
        let mut init = RequestInit::new();
        init.with_method(match request.method {
            Method::Get => ::worker::Method::Get,
            Method::Post => ::worker::Method::Post,
            Method::Put => ::worker::Method::Put,
            Method::Patch => ::worker::Method::Patch,
            Method::Delete => ::worker::Method::Delete,
        })
        .with_headers(headers)
        .with_redirect(RequestRedirect::Manual);
        if let Some(body) = request.body_for_transport() {
            init.with_body(Some(js_sys::Uint8Array::from(body).into()));
        }
        let outgoing = ::worker::Request::new_with_init(&request.url, &init)
            .map_err(|_| TransportError::NotSent)?;
        let guard = AbortOnDrop(Some(AbortController::default()));
        let signal = guard.0.as_ref().expect("live abort controller").signal();
        let read = async {
            let mut response = Fetch::Request(outgoing)
                .send_with_signal(&signal)
                .await
                .map_err(|_| TransportError::SentNoResponse { timed_out: false })?;
            let status = response.status_code();
            let headers: std::collections::BTreeMap<_, _> = response
                .headers()
                .entries()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect();
            if headers
                .get("content-length")
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
            {
                return Err(TransportError::Oversize);
            }
            let mut body = Vec::new();
            match response.stream() {
                Ok(mut stream) => {
                    while let Some(chunk) = stream
                        .try_next()
                        .await
                        .map_err(|_| TransportError::SentNoResponse { timed_out: false })?
                    {
                        if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                            return Err(TransportError::Oversize);
                        }
                        body.extend_from_slice(&chunk);
                    }
                }
                // Fetch's only non-stream body is Empty. Also bound a buffered
                // body should a future worker-rs release return one here.
                Err(_) => {
                    body = response
                        .bytes()
                        .await
                        .map_err(|_| TransportError::SentNoResponse { timed_out: false })?;
                    if body.len() > MAX_RESPONSE_BYTES {
                        return Err(TransportError::Oversize);
                    }
                }
            }
            Ok(Response {
                status,
                headers,
                body,
            })
        };
        let timer = Deadline::new(request.timeout.min(MAX_TIMEOUT));
        match select(Box::pin(read), Box::pin(timer)).await {
            Either::Left((result, _timer)) => result,
            Either::Right(((), _fetch)) => Err(TransportError::SentNoResponse { timed_out: true }),
        }
        // `guard` aborts on return, timeout, error, or cancellation by our caller.
    }
}

/// worker 0.8's `Delay` keeps its first polling waker. An executor can replace
/// that waker while the HTTP future is pending, so give `Delay` a stable
/// forwarding waker and register the current task on every poll.
struct Deadline {
    delay: Pin<Box<::worker::Delay>>,
    wake: Arc<DeadlineWake>,
}

struct DeadlineWake(AtomicWaker);

impl ArcWake for DeadlineWake {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.wake();
    }
}

impl Deadline {
    fn new(duration: Duration) -> Self {
        Self {
            delay: Box::pin(::worker::Delay::from(duration)),
            wake: Arc::new(DeadlineWake(AtomicWaker::new())),
        }
    }
}

impl Future for Deadline {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        this.wake.0.register(cx.waker());
        let waker = waker_ref(&this.wake);
        this.delay.as_mut().poll(&mut Context::from_waker(&waker))
    }
}

struct AbortOnDrop(Option<AbortController>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            controller.abort();
        }
    }
}

impl HttpTransport for WorkerFetchTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        SendWrapper::new(Self::execute(request)).await
    }
}
