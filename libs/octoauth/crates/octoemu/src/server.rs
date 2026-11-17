//! The router: GitHub routes, the admin API, the 501 fallback, and the
//! middleware every request passes through (fault rules, `Date`, request
//! ID, rate-limit headers, call log, idle tracking).

use crate::api::Api;
use crate::auth;
use crate::calls::{self, Call, CallFault, CredentialKind, Identity, Surface, redacted_path};
use crate::clock::http_date;
use crate::connection::Connection;
use crate::faults::{DelayClock, FaultAction, Fired, Gate, Probe, Reply, StallPhase};
use crate::response::{self, OCTOEMU_DOCS, REQUEST_ID, RateLimit};
use crate::state::SharedState;
use axum::Router;
use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::header::{CONTENT_TYPE, DATE};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use std::time::Duration;

/// The matched GitHub route (`METHOD /pattern`), attached to the response
/// for the call log.
#[derive(Clone, Debug)]
struct MatchedRoute(String);

/// The whole application.
pub(crate) fn router(shared: SharedState, api: Api) -> Router {
    api.into_router(shared.clone())
        .merge(crate::admin::router())
        .fallback(fallback)
        .layer(from_fn_with_state(shared.clone(), observe))
        .with_state(shared)
}

/// Every unmatched request: 501 for the GitHub surfaces (never 404, which a
/// client could take for GitHub's answer), 404 for an unknown admin route.
pub(crate) async fn fallback(method: Method, uri: Uri) -> Response {
    if Surface::of(uri.path()) == Surface::Admin {
        return response::error(
            StatusCode::NOT_FOUND,
            "octoemu: no such admin route",
            OCTOEMU_DOCS,
        );
    }
    response::not_implemented(method.as_str(), uri.path())
}

/// Runs before each registered GitHub route: refuse-assumed enforcement,
/// the route's [`Auth`](crate::auth::Auth) (credential, kind, permission),
/// and the route name and verified identity for the call log.
pub(crate) async fn route_context(
    State(shared): State<SharedState>,
    mut request: Request,
    next: Next,
) -> Response {
    // axum answers HEAD with the GET handler.
    let method = match *request.method() {
        Method::HEAD => Method::GET,
        ref method => method.clone(),
    };
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| format!("{method} {}", path.as_str()))
        .and_then(|route| shared.auth_of(&route).map(|auth| (route, auth)));
    let Some((route, auth)) = route else {
        // A registered path with an unregistered method: the 501 fallback.
        return next.run(request).await;
    };
    let mut identity = None;
    let mut response = if let Some(refusal) = shared.refuse_route(&route) {
        refusal
    } else {
        match auth::authenticate(&shared, request.headers()).and_then(|caller| {
            auth::admit(&shared, auth, &caller)?;
            Ok(caller)
        }) {
            Err(rejection) => rejection,
            Ok(caller) => {
                identity = caller.identity();
                request.extensions_mut().insert(caller);
                next.run(request).await
            }
        }
    };
    response.extensions_mut().insert(MatchedRoute(route));
    if let Some(identity) = identity {
        response.extensions_mut().insert(identity);
    }
    response
}

/// The largest request body octoemu reads (GitHub's own limits are far
/// lower for the routes it implements).
const BODY_LIMIT: usize = 16 * 1024 * 1024;

/// What became of a request.
enum Outcome {
    /// Answered.
    Response(Response),
    /// A [`FaultAction::Stall`] in the headers phase: logged, then nothing
    /// until the gate opens (or the client goes away), then the connection
    /// is dropped.
    Stalled {
        gate: Arc<Gate>,
        identity: Option<Identity>,
    },
    /// The connection is to be dropped without an answer.
    Dropped {
        reset: bool,
        route: Option<MatchedRoute>,
        identity: Option<Identity>,
    },
}

/// Wraps every request: fault rules (GitHub surfaces only), then the
/// response headers and the call log.
async fn observe(State(shared): State<SharedState>, request: Request, next: Next) -> Response {
    let _active = shared.activity.enter();
    let sequence = shared.next_sequence();
    let request_id = response::request_id(shared.port, sequence);
    let method = request.method().to_string();
    let surface = Surface::of(request.uri().path());
    let credential = CredentialKind::of(request.headers());
    let path = redacted_path(request.uri());
    let connection = request.extensions().get::<Arc<Connection>>().cloned();

    let mut body_fields = None;
    let mut mark = None;
    let outcome = if surface == Surface::Admin {
        Outcome::Response(next.run(request).await)
    } else {
        let (parts, body) = request.into_parts();
        match axum::body::to_bytes(body, BODY_LIMIT).await {
            Err(_) => Outcome::Response(response::error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "octoemu: request body too large",
                OCTOEMU_DOCS,
            )),
            Ok(bytes) => {
                body_fields = calls::body_fields(&bytes);
                let fired = shared.state().faults.decide(&Probe {
                    method: parts.method.as_str(),
                    path: parts.uri.path(),
                    query: parts.uri.query().unwrap_or_default(),
                    credential,
                    body: &bytes,
                });
                let request = Request::from_parts(parts, Body::from(bytes));
                match fired {
                    None => Outcome::Response(next.run(request).await),
                    Some(fired) => {
                        let mut fault = CallFault {
                            rule: fired.id,
                            action: fired.action.kind().to_owned(),
                            handler_status: None,
                        };
                        let outcome = inject(&shared, fired, request, next, &mut fault).await;
                        mark = Some(fault);
                        outcome
                    }
                }
            }
        }
    };

    let now = shared.now();
    let mut stalled = None;
    let (mut response, dropped) = match outcome {
        Outcome::Response(response) => (response, None),
        Outcome::Stalled { gate, identity } => {
            let mut response = Response::new(Body::empty());
            if let Some(identity) = identity {
                response.extensions_mut().insert(identity);
            }
            stalled = Some(gate);
            (response, Some(false))
        }
        Outcome::Dropped {
            reset,
            route,
            identity,
        } => {
            let mut response = Response::new(Body::empty());
            if let Some(route) = route {
                response.extensions_mut().insert(route);
            }
            if let Some(identity) = identity {
                response.extensions_mut().insert(identity);
            }
            (response, Some(reset))
        }
    };
    let headers = response.headers_mut();
    if let Ok(date) = HeaderValue::from_str(&http_date(now)) {
        headers.insert(DATE, date);
    }
    if shared.allows("request-id-header")
        && let Ok(id) = HeaderValue::from_str(&request_id)
    {
        headers.insert(REQUEST_ID.clone(), id);
    }
    if surface == Surface::Api
        && !headers.contains_key("x-ratelimit-limit")
        && shared.allows("rate-limit-headers")
    {
        RateLimit::stub(now).apply(headers);
    }
    if surface != Surface::Admin {
        let route = response
            .extensions()
            .get::<MatchedRoute>()
            .map(|route| route.0.clone());
        let identity = response.extensions().get::<Identity>().cloned();
        shared.record(Call {
            seq: sequence,
            at: now,
            method,
            path,
            surface,
            route,
            credential,
            identity,
            status: if dropped.is_some() {
                0
            } else {
                response.status().as_u16()
            },
            request_id,
            body_fields,
            fault: mark,
        });
    }
    if let Some(gate) = stalled {
        // Logged above; now the upstream says nothing until released.
        gate.pass().await;
    }
    if let Some(reset) = dropped {
        match connection {
            Some(connection) => {
                connection.kill(reset);
                // The connection task drops this request with the socket.
                std::future::pending::<()>().await;
            }
            None => {
                return response::error(
                    StatusCode::BAD_GATEWAY,
                    "octoemu: this connection cannot be dropped (fault rule)",
                    OCTOEMU_DOCS,
                );
            }
        }
    }
    response
}

/// Runs a fired rule's action around (or instead of) the handler.
async fn inject(
    shared: &SharedState,
    fired: Fired,
    request: Request,
    next: Next,
    mark: &mut CallFault,
) -> Outcome {
    // Who the credential belongs to, for the call log, when the handler
    // (and so the route's own check) does not run.
    let caller = || {
        auth::authenticate(shared, request.headers())
            .ok()
            .and_then(|caller| caller.identity())
    };
    let answered = |mut response: Response, identity: Option<Identity>| {
        if let Some(identity) = identity {
            response.extensions_mut().insert(identity);
        }
        Outcome::Response(response)
    };
    match fired.action {
        FaultAction::Respond(reply) => answered(reply_response(&reply), caller()),
        FaultAction::Custom(responder) => {
            answered(reply_response(&responder.reply(&fired.request)), caller())
        }
        FaultAction::Close => Outcome::Dropped {
            reset: false,
            route: None,
            identity: caller(),
        },
        FaultAction::Reset => Outcome::Dropped {
            reset: true,
            route: None,
            identity: caller(),
        },
        FaultAction::Delay { ms, clock } => {
            match clock {
                DelayClock::Real => tokio::time::sleep(Duration::from_millis(ms)).await,
                DelayClock::Virtual => shared.state().clock.advance_millis(ms),
            }
            let response = next.run(request).await;
            mark.handler_status = Some(response.status().as_u16());
            Outcome::Response(response)
        }
        FaultAction::Hold => {
            fired.gate.pass().await;
            let response = next.run(request).await;
            mark.handler_status = Some(response.status().as_u16());
            Outcome::Response(response)
        }
        FaultAction::HoldResponse => {
            let response = next.run(request).await;
            mark.handler_status = Some(response.status().as_u16());
            fired.gate.pass().await;
            Outcome::Response(response)
        }
        FaultAction::Stall {
            phase: StallPhase::Headers,
            ..
        } => Outcome::Stalled {
            gate: fired.gate,
            identity: caller(),
        },
        FaultAction::Stall {
            phase: StallPhase::Body,
            status,
        } => {
            let status = status
                .and_then(|status| StatusCode::from_u16(status).ok())
                .unwrap_or(StatusCode::OK);
            answered(
                (
                    status,
                    [(CONTENT_TYPE, "application/json; charset=utf-8")],
                    Body::new(StalledBody::new(fired.gate)),
                )
                    .into_response(),
                caller(),
            )
        }
        FaultAction::ApplyThenLose { status } => {
            let handled = next.run(request).await;
            mark.handler_status = Some(handled.status().as_u16());
            let route = handled.extensions().get::<MatchedRoute>().cloned();
            let identity = handled.extensions().get::<Identity>().cloned();
            drop(handled);
            match status.and_then(|status| StatusCode::from_u16(status).ok()) {
                None => Outcome::Dropped {
                    reset: false,
                    route,
                    identity,
                },
                Some(status) => {
                    let mut response = (
                        status,
                        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                        format!("octoemu: response lost (fault rule {})", fired.id),
                    )
                        .into_response();
                    if let Some(route) = route {
                        response.extensions_mut().insert(route);
                    }
                    if let Some(identity) = identity {
                        response.extensions_mut().insert(identity);
                    }
                    Outcome::Response(response)
                }
            }
        }
    }
}

/// A [`FaultAction::Stall`] body: the first bytes of a JSON document, then
/// nothing until the gate opens, then an error that aborts the connection
/// with the body unfinished.
struct StalledBody {
    first: Option<axum::body::Bytes>,
    wait: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}

impl StalledBody {
    fn new(gate: Arc<Gate>) -> Self {
        Self {
            first: Some(axum::body::Bytes::from_static(b"{\"partial\":")),
            wait: Box::pin(async move { gate.pass().await }),
        }
    }
}

impl hyper::body::Body for StalledBody {
    type Data = axum::body::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        if let Some(first) = self.first.take() {
            return std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(first))));
        }
        self.wait.as_mut().poll(cx).map(|()| {
            Some(Err(std::io::Error::other(
                "octoemu: stalled body ended (fault rule)",
            )))
        })
    }
}

/// A canned [`Reply`] as a response.
fn reply_response(reply: &Reply) -> Response {
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = match (&reply.body, &reply.text) {
        (_, Some(text)) => (status, text.clone()).into_response(),
        (Some(body), None) => response::json(status, body),
        (None, None) => status.into_response(),
    };
    for (name, value) in &reply.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}
