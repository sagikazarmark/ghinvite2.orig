//! The router: GitHub routes, the admin API, the 501 fallback, and the
//! middleware every response passes through (`Date`, request ID, rate-limit
//! headers, call log, idle tracking).

use crate::api::Api;
use crate::calls::{Call, CredentialKind, Surface, redacted_path};
use crate::clock::http_date;
use crate::response::{self, OCTOEMU_DOCS, REQUEST_ID, RateLimit};
use crate::state::SharedState;
use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header::DATE};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;

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

/// Runs before each registered GitHub route: refuse-assumed enforcement and
/// the route name for the call log.
pub(crate) async fn route_context(
    State(shared): State<SharedState>,
    request: Request,
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
        .filter(|route| shared.is_registered(route));
    let Some(route) = route else {
        // A registered path with an unregistered method: the 501 fallback.
        return next.run(request).await;
    };
    let mut response = match shared.refuse_route(&route) {
        Some(refusal) => refusal,
        None => next.run(request).await,
    };
    response.extensions_mut().insert(MatchedRoute(route));
    response
}

/// Wraps every request.
async fn observe(State(shared): State<SharedState>, request: Request, next: Next) -> Response {
    let _active = shared.activity.enter();
    let sequence = shared.next_sequence();
    let request_id = response::request_id(shared.port, sequence);
    let method = request.method().to_string();
    let surface = Surface::of(request.uri().path());
    let credential = CredentialKind::of(request.headers());
    let path = redacted_path(request.uri());

    let mut response = next.run(request).await;

    let now = shared.now();
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
        shared.record(Call {
            seq: sequence,
            at: now,
            method,
            path,
            surface,
            route,
            credential,
            status: response.status().as_u16(),
            request_id,
        });
    }
    response
}
