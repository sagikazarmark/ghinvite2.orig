//! The route registry: every GitHub route octoemu implements goes through
//! [`Api::route`], which records it so the fidelity manifest can be checked
//! against it, and wraps it in the refuse-assumed enforcement point.

use crate::state::SharedState;
use axum::Router;
use axum::handler::Handler;
use axum::http::Method;
use axum::routing::{MethodFilter, on};

/// Cross-cutting behaviours with a `[behaviours.<id>]` entry in
/// `fidelity.toml`. Handlers check one with `Shared::refuse` (or
/// `Shared::allows`) before relying on it.
pub(crate) const BEHAVIOURS: &[&str] = &[
    "date-header",
    "error-body",
    "json-content-type",
    "rate-limit-headers",
    "request-id-header",
    "validation-errors",
];

/// GitHub routes (API and web) under construction.
pub(crate) struct Api {
    router: Router<SharedState>,
    routes: Vec<String>,
}

impl Api {
    pub(crate) fn new() -> Self {
        Self {
            router: Router::new(),
            routes: Vec::new(),
        }
    }

    /// Registers `handler` for `method` on `path` (axum's `{param}` syntax).
    /// The route needs a `[routes."METHOD /path"]` entry in `fidelity.toml`.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the first route arrives in GE-02")
    )]
    pub(crate) fn route<H, T>(mut self, method: Method, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, SharedState>,
        T: 'static,
    {
        let filter = MethodFilter::try_from(method.clone()).expect("a standard HTTP method");
        self.router = self.router.route(path, on(filter, handler));
        self.routes.push(format!("{method} {path}"));
        self
    }

    /// `METHOD /pattern` of every registered route.
    pub(crate) fn routes(&self) -> &[String] {
        &self.routes
    }

    /// The router, with the route-level middleware on every registered route
    /// and a 501 for unregistered methods on registered paths.
    pub(crate) fn into_router(self, shared: SharedState) -> Router<SharedState> {
        if self.routes.is_empty() {
            // axum refuses a route layer on a router without routes.
            return self.router;
        }
        self.router
            .method_not_allowed_fallback(crate::server::fallback)
            .route_layer(axum::middleware::from_fn_with_state(
                shared,
                crate::server::route_context,
            ))
    }
}

/// Every GitHub route octoemu implements. Empty until GE-02; everything
/// answers 501 meanwhile.
pub(crate) fn github() -> Api {
    Api::new()
}
