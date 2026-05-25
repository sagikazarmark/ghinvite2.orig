# ghinvite — Plan 4: Web Binary Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `crates/web` — the axum-based web server skeleton: tower-sessions, OAuth login + install flow (consuming Plan 2's `github::oauth`), Restate ingress client, three Dioxus 0.7 layouts (Home / Dashboard / Invitation) with Tailwind v4 + DaisyUI styling, the home page, route-existence skeleton for the rest of spec §11, and an auth middleware that validates sessions and re-checks admin status. Plans 5 and 6 fill in the dashboard and recipient routes; Plan 7 wraps the library in a Workers `#[event(fetch)]` + D1 store.

**Architecture:** Library crate (`crates/web/src/lib.rs`) defines `build_app(state) -> axum::Router`, target-agnostic so Plan 7 can call it from `#[event(fetch)]`. A native binary (`crates/web/src/main.rs`, `cfg(not(target_arch = "wasm32"))`) runs locally for `cargo run -p web` against a sqlite session store. OAuth is delegated to Plan 2's `github::UserApiClient` + `AuthorizeUrl::build` + `exchange_code`. Restate invocations go through a small `RestateClient` HTTP wrapper (~50 lines, reqwest-backed; no `restate-sdk` dep on the web crate). Dioxus 0.7 SSR renders three layouts to HTML strings; no client-side hydration in Plan 4 (interactive dashboard ships in Plan 5+).

**Tech Stack:** Rust 2024 (workspace), `axum 0.8` (HTTP), `tower-sessions 0.13` (session store) + `tower-sessions-sqlx-store 0.14` (sqlite backend), `tower-http 0.6` (CORS, trace), `cookie 0.18`, `reqwest 0.12` (Restate ingress), `dioxus 0.7` (SSR), Tailwind CSS v4 + DaisyUI v4 (npm-driven build), existing workspace crates (`domain`, `audit`, `storage`, `github`). Dev: `tokio` runtime, axum test helpers via `tower::ServiceExt::oneshot`, `MockTransport` from `crates/github`.

---

## Spec coverage

This plan implements **§3 (tech stack: axum, tower-sessions, Dioxus, Tailwind+DaisyUI)**, **§10.1 / §10.2** (OAuth sign-in + install flows end-to-end), **§10.3** (auth middleware + admin recheck cache), **§10.4** (logout), **§11** in full at the routing-skeleton level (every route from the table is declared; ones owned by Plans 5–6 return `501 Not Implemented` with a clear hand-off message), **§12** (three Dioxus layouts shipped as visual chrome; home page is the only fully-rendered page in Plan 4), **§16** (HTTP error mapping at the framework boundary — server-function-internal `Result` mapping is Plan 5+), and **§18** security items relevant to this surface (Secure / HttpOnly / SameSite=Lax cookies, CSP header on dashboard responses, generic 404 for protected resources).

It does NOT implement:
- §11 dashboard routes (`/accounts/:login/...`) — Plan 5 fills them in (this plan ships 501 stubs).
- §11 recipient routes (`/i/:slug/...`) and `/webhooks/github` — Plan 6.
- §14 GitHub API surface — already shipped by Plan 2.
- §15 audit emission — Plan 3 owns it; the web binary's only DB write is OAuth/session state.
- D1Storage and Workers `#[event(fetch)]` entry — Plan 7.
- E2E tests — Plan 8.

## File structure

| Path | Responsibility |
|---|---|
| `crates/web/Cargo.toml` | Crate manifest (rlib + bin) |
| `crates/web/src/lib.rs` | `build_app(state) -> axum::Router` + module decls + re-exports |
| `crates/web/src/main.rs` | Native binary entry (`cfg(not(target_arch = "wasm32"))`) |
| `crates/web/src/state.rs` | `AppState { storage, github, restate, oauth_config, web_config }` shared container |
| `crates/web/src/config.rs` | `WebConfig { base_url, session_secret, csp_policy }` parsed from env |
| `crates/web/src/error.rs` | `WebError` enum + `IntoResponse` (HTTP status mapping) |
| `crates/web/src/session.rs` | `Session` struct (user_id, login, csrf_token, last_admin_check) + tower-sessions wiring |
| `crates/web/src/middleware/mod.rs` | Module entry |
| `crates/web/src/middleware/auth.rs` | `require_session` extractor + admin recheck cache |
| `crates/web/src/middleware/csp.rs` | CSP header layer for dashboard responses |
| `crates/web/src/restate_client.rs` | `RestateClient` HTTP wrapper for ingress invocations |
| `crates/web/src/routes/mod.rs` | Route registration |
| `crates/web/src/routes/health.rs` | `GET /health` |
| `crates/web/src/routes/home.rs` | `GET /` (home page) |
| `crates/web/src/routes/oauth.rs` | `GET /login`, `GET /oauth/callback`, `GET /install`, `GET /logout` |
| `crates/web/src/routes/dashboard.rs` | All `/accounts/:login/...` routes — 501 stubs (Plan 5) |
| `crates/web/src/routes/invitation.rs` | All `/i/:slug...` routes — 501 stubs (Plan 6) |
| `crates/web/src/routes/webhook.rs` | `POST /webhooks/github` — 501 stub (Plan 6) |
| `crates/web/src/views/mod.rs` | Dioxus components module |
| `crates/web/src/views/layouts.rs` | `HomeLayout`, `DashboardLayout`, `InvitationLayout` Dioxus components |
| `crates/web/src/views/home.rs` | `HomePage` Dioxus component |
| `crates/web/src/views/components.rs` | Shared `Nav`, `Footer`, `AccountSwitcher` components |
| `crates/web/src/views/render.rs` | `render(component, props) -> String` SSR helper |
| `crates/web/assets/styles.css` | Tailwind input |
| `crates/web/tailwind.config.js` | Tailwind config |
| `crates/web/package.json` | npm deps for the Tailwind build |
| `crates/web/.gitignore` | Ignore `node_modules/` and built CSS |
| `crates/web/tests/oauth_flow.rs` | OAuth integration tests using `MockTransport` |
| `crates/web/tests/route_smoke.rs` | Smoke test: every route from §11 returns a non-500 |
| `crates/web/README.md` | Local-dev guide |

---

## Task ordering

Tasks 1–4 establish the workspace member, error type, config, and shared `AppState`. Tasks 5–7 build the session and Restate-ingress plumbing. Tasks 8–13 ship the OAuth flow end-to-end. Tasks 14–15 wire route registration + a generic auth middleware. Tasks 16–18 set up the frontend toolchain (npm + Tailwind + DaisyUI) and CSS build. Tasks 19–22 ship Dioxus 0.7 layouts + the home page. Tasks 23–25 add the 501 placeholder routes (Plans 5–6 fill them in) and the native binary main. Tasks 26–28 are tests, README, and final polish.

A pure-async-fn-with-fake-state test pattern carries through. Each route handler that touches Restate or GitHub takes `State<AppState>` so tests can inject `MockTransport` + `SqlxStorage::in_memory` + a stubbed `RestateClient` (the test scaffolding ships in Task 5).

---

### Task 1: Crate skeleton

**Files:**
- Create: `crates/web/Cargo.toml`
- Create: `crates/web/src/lib.rs`
- Create: `crates/web/src/main.rs` (stub)
- Modify: workspace `Cargo.toml` (add member + new shared deps)

- [ ] **Step 1: Add workspace deps**

In the workspace root `Cargo.toml`, add `"crates/web"` to `members` and add to `[workspace.dependencies]` (alphabetical):

```toml
axum = { version = "0.8", default-features = false, features = ["http1", "json", "tokio", "tracing", "matched-path", "query"] }
cookie = { version = "0.18", default-features = false }
dioxus = { version = "0.7", default-features = false, features = ["html"] }
dioxus-ssr = { version = "0.7" }
tower = { version = "0.5", default-features = false, features = ["util"] }
tower-http = { version = "0.6", default-features = false, features = ["trace", "set-header"] }
tower-sessions = { version = "0.13", default-features = false, features = ["axum-core"] }
tower-sessions-sqlx-store = { version = "0.14", default-features = false, features = ["sqlite"] }
```

Plus the new path member:

```toml
web = { path = "crates/web" }
```

- [ ] **Step 2: Crate manifest**

`crates/web/Cargo.toml`:

```toml
[package]
name = "web"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[lib]
path = "src/lib.rs"

[[bin]]
name = "web"
path = "src/main.rs"

[dependencies]
async-trait.workspace = true
audit.workspace = true
axum.workspace = true
chrono.workspace = true
cookie.workspace = true
dioxus.workspace = true
dioxus-ssr.workspace = true
domain.workspace = true
github.workspace = true
parking_lot.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
storage.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "sync"] }
tower.workspace = true
tower-http.workspace = true
tower-sessions.workspace = true
tower-sessions-sqlx-store.workspace = true
tracing.workspace = true
url.workspace = true

[dev-dependencies]
github = { workspace = true, features = ["test-mock"] }
storage = { workspace = true, features = ["test-suite"] }
http-body-util = "0.1"
sqlx.workspace = true
```

- [ ] **Step 3: `crates/web/src/lib.rs` skeleton**

```rust
//! Web binary core. axum app builder + tower-sessions + OAuth flow + Dioxus
//! layouts. Plan 7 wraps `build_app()` in a Workers `#[event(fetch)]`; Plans 5
//! and 6 fill in the dashboard and recipient routes.

pub mod config;
pub mod error;
pub mod middleware;
pub mod restate_client;
pub mod routes;
pub mod session;
pub mod state;
pub mod views;

// Re-exports filled in as types appear:
// pub use config::WebConfig;
// pub use error::{Result, WebError};
// pub use restate_client::RestateClient;
// pub use state::AppState;
```

- [ ] **Step 4: Stub module files**

Create one-line stubs (`// filled in by a later task`) for: `config.rs`, `error.rs`, `middleware/mod.rs`, `restate_client.rs`, `routes/mod.rs`, `session.rs`, `state.rs`, `views/mod.rs`. Then `crates/web/src/middleware/auth.rs`, `middleware/csp.rs`, `routes/health.rs`, `routes/home.rs`, `routes/oauth.rs`, `routes/dashboard.rs`, `routes/invitation.rs`, `routes/webhook.rs`, `views/components.rs`, `views/home.rs`, `views/layouts.rs`, `views/render.rs`.

The `mod.rs` files declare their submodules:

`crates/web/src/middleware/mod.rs`:
```rust
pub mod auth;
pub mod csp;
```

`crates/web/src/routes/mod.rs`:
```rust
pub mod dashboard;
pub mod health;
pub mod home;
pub mod invitation;
pub mod oauth;
pub mod webhook;
```

`crates/web/src/views/mod.rs`:
```rust
pub mod components;
pub mod home;
pub mod layouts;
pub mod render;
```

Other top-level stub files contain only `// filled in by a later task`.

- [ ] **Step 5: Stub `main.rs`**

```rust
//! Native binary entry. Wraps `web::build_app(state)` in a hyper server.
//! Plan 7 swaps this for a Workers `#[event(fetch)]`.
//!
//! Filled in by Task 25.

fn main() {
    eprintln!("crates/web: native binary skeleton (filled in by Task 25)");
}
```

- [ ] **Step 6: Verify**

```bash
cargo check -p web
```

Expected: clean (warnings about empty modules / unused mod decls are OK).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/web
git commit -m "chore: scaffold crates/web skeleton"
```

---

### Task 2: `WebConfig`

Configuration parsed from environment variables. Production reads from Workers secrets (Plan 7 wires that); local dev reads from a `.env` file or a default `WebConfig::for_local_dev()` constructor.

**Files:**
- Modify: `crates/web/src/config.rs`
- Modify: `crates/web/src/lib.rs` (un-comment `WebConfig` re-export)

- [ ] **Step 1: Write `WebConfig`**

```rust
//! Web binary runtime configuration.

use github::oauth::OAuthConfig;
use serde::{Deserialize, Serialize};

/// Configuration parsed at server boot. Production sources fields from
/// Workers secrets (Plan 7); local dev uses [`WebConfig::for_local_dev`].
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// Base URL of THIS web binary (e.g. `https://ghinvite.example`). Used to
    /// build OAuth redirect URIs and absolute URLs in the home page.
    pub base_url: String,
    /// 32-byte symmetric key for tower-sessions cookie encryption.
    /// Production rotates this via Workers secrets.
    pub session_secret: [u8; 32],
    /// Restate ingress URL (e.g. `http://127.0.0.1:8080` for local dev).
    pub restate_ingress: String,
    /// OAuth credentials (client_id, client_secret, redirect_uri).
    pub oauth: OAuthConfig,
    /// GitHub App install URL — `https://github.com/apps/<app-name>/installations/new`.
    pub github_install_url: String,
    /// Whether to set the `Secure` attribute on session cookies.
    /// Off for local-dev http; on for production https.
    pub cookie_secure: bool,
}

impl WebConfig {
    /// Build a config suitable for `cargo run -p web` local dev. Reads from
    /// environment variables; falls back to safe defaults that won't actually
    /// authenticate against real GitHub.
    pub fn for_local_dev() -> Self {
        let secret = match std::env::var("GHINVITE_SESSION_SECRET") {
            Ok(s) if s.len() >= 32 => {
                let mut b = [0u8; 32];
                b.copy_from_slice(&s.as_bytes()[..32]);
                b
            }
            _ => *b"insecure-local-dev-session-key!!", // 32 bytes
        };
        Self {
            base_url: std::env::var("GHINVITE_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8787".into()),
            session_secret: secret,
            restate_ingress: std::env::var("GHINVITE_RESTATE_INGRESS")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".into()),
            oauth: OAuthConfig {
                client_id: std::env::var("GHINVITE_GITHUB_CLIENT_ID")
                    .unwrap_or_else(|_| "Iv1.local-dev-client-id".into()),
                client_secret: std::env::var("GHINVITE_GITHUB_CLIENT_SECRET")
                    .unwrap_or_else(|_| "local-dev-client-secret".into()),
                redirect_uri: format!(
                    "{}/oauth/callback",
                    std::env::var("GHINVITE_BASE_URL")
                        .unwrap_or_else(|_| "http://127.0.0.1:8787".into())
                ),
            },
            github_install_url: std::env::var("GHINVITE_GITHUB_INSTALL_URL")
                .unwrap_or_else(|_| "https://github.com/apps/ghinvite-local/installations/new".into()),
            cookie_secure: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_dev_default_does_not_panic() {
        // SAFETY: tests should not depend on the env, but for_local_dev reads
        // it; we rely on the unwrap_or fallbacks. Cleaner would be to inject
        // env, but acceptable for this smoke.
        let cfg = WebConfig::for_local_dev();
        assert!(cfg.base_url.starts_with("http"));
        assert_eq!(cfg.session_secret.len(), 32);
        assert!(cfg.oauth.redirect_uri.ends_with("/oauth/callback"));
    }
}
```

- [ ] **Step 2: Run test**

```bash
cargo test -p web --lib config
```

Expected: 1 test passes.

- [ ] **Step 3: Un-comment re-export**

In `lib.rs`, change `// pub use config::WebConfig;` to `pub use config::WebConfig;`.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/config.rs crates/web/src/lib.rs
git commit -m "feat(web): WebConfig with for_local_dev"
```

---

### Task 3: `WebError` and HTTP error mapping

Per spec §16: framework-level errors map to clean HTTP status codes. Server-function-internal errors map happens in Plans 5+.

**Files:**
- Modify: `crates/web/src/error.rs`
- Modify: `crates/web/src/lib.rs` (un-comment `WebError`/`Result` re-exports)

- [ ] **Step 1: Write `WebError`**

```rust
//! Web-binary error type with axum `IntoResponse` mapping.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, WebError>;

#[derive(Debug, Error)]
pub enum WebError {
    #[error("storage error: {0}")]
    Storage(#[from] storage::Error),

    #[error("github api error: {0}")]
    Github(#[from] github::Error),

    #[error("session error: {0}")]
    Session(String),

    #[error("oauth error: {0}")]
    OAuth(String),

    #[error("restate ingress error: {0}")]
    Restate(String),

    /// Resource not found / not authorized — surfaced as a generic 404 so we
    /// don't leak whether the resource exists. Used for invitation-link routes,
    /// account routes the user isn't an admin of, etc.
    #[error("not found")]
    NotFound,

    /// Caller is not authenticated. Surfaces as a 302 redirect to /login.
    #[error("unauthenticated")]
    Unauthenticated,

    /// Caller is authenticated but lacks permission for the resource.
    /// Per spec §10.3: surfaces as 404 to avoid leaking existence.
    #[error("forbidden")]
    Forbidden,

    /// Some input from the client was malformed (e.g. missing query param).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Catch-all for unexpected internal errors.
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            WebError::NotFound | WebError::Forbidden => (StatusCode::NOT_FOUND, "Not Found".to_string()),
            WebError::Unauthenticated => {
                // Redirect to /login. axum's redirect helper makes this clean.
                return axum::response::Redirect::to("/login").into_response();
            }
            WebError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            WebError::OAuth(msg) => (StatusCode::BAD_REQUEST, format!("OAuth error: {msg}")),
            WebError::Session(msg) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Session error: {msg}")),
            WebError::Restate(msg) => (StatusCode::BAD_GATEWAY, format!("Restate error: {msg}")),
            WebError::Storage(_) | WebError::Github(_) | WebError::Internal(_) => {
                tracing::error!(error = ?self, "internal error rendering response");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error".to_string())
            }
        };
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;

    async fn body_text(resp: Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn not_found_renders_404() {
        let resp = WebError::NotFound.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn forbidden_renders_404() {
        // Spec §10.3: forbidden → 404 to avoid information leak.
        let resp = WebError::Forbidden.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unauthenticated_redirects_to_login() {
        let resp = WebError::Unauthenticated.into_response();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp.headers().get("location").unwrap();
        assert_eq!(location.to_str().unwrap(), "/login");
    }

    #[tokio::test]
    async fn bad_request_passes_message() {
        let resp = WebError::BadRequest("missing 'code' param".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_text(resp).await;
        assert!(body.contains("missing 'code' param"));
    }

    #[tokio::test]
    async fn restate_error_renders_502() {
        let resp = WebError::Restate("timeout".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
```

Note the unused `axum::body::Body` import — drop if rustc complains. The `http_body_util::BodyExt` is the dev-dep we added in Task 1.

- [ ] **Step 2: Run tests**

```bash
cargo test -p web --lib error
```

Expected: 5 tests pass.

- [ ] **Step 3: Un-comment re-exports**

In `lib.rs`:
```rust
pub use error::{Result, WebError};
```

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/error.rs crates/web/src/lib.rs
git commit -m "feat(web): WebError with axum IntoResponse mapping"
```

---

### Task 4: `AppState`

Shared state container threaded through every route handler.

**Files:**
- Modify: `crates/web/src/state.rs`
- Modify: `crates/web/src/lib.rs` (un-comment `AppState` re-export)

- [ ] **Step 1: Write `AppState`**

```rust
//! Shared dependency container threaded through every route handler.

use crate::config::WebConfig;
use crate::restate_client::RestateClient;
use github::HttpTransport;
use std::sync::Arc;
use storage::Storage;

/// Cloneable state. Constructed once at startup and held by the axum app.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn Storage>,
    pub github_transport: Arc<dyn HttpTransport>,
    pub restate: Arc<RestateClient>,
    pub config: WebConfig,
}

impl AppState {
    pub fn new(
        storage: Arc<dyn Storage>,
        github_transport: Arc<dyn HttpTransport>,
        restate: Arc<RestateClient>,
        config: WebConfig,
    ) -> Self {
        Self {
            storage,
            github_transport,
            restate,
            config,
        }
    }
}
```

Note: the web binary uses `github::HttpTransport` (the trait from Plan 2) directly rather than holding a pre-built `UserApiClient`, because each request needs to construct a `UserApiClient` with the current user's access token from the session. A pre-built `InstallationClient` is NOT held by the web binary — that's the Restate worker's concern; the web binary only ever calls Restate, never GitHub-as-the-app.

- [ ] **Step 2: Verify**

`cargo check -p web` — expected to fail until Task 5 ships `RestateClient`. That's OK — leave the file in place.

- [ ] **Step 3: Skip the commit** until Task 5 ships `RestateClient`. The two land together.

---

### Task 5: `RestateClient`

Small HTTP wrapper around Restate's ingress API.

**Files:**
- Modify: `crates/web/src/restate_client.rs`
- Modify: `crates/web/src/lib.rs` (un-comment `RestateClient` re-export)

- [ ] **Step 1: Write `RestateClient`**

```rust
//! HTTP client for Restate's ingress API.
//!
//! Restate exposes a simple HTTP surface for invoking services from outside
//! the Restate runtime: `POST <ingress>/<ServiceName>/<Key>/<Method>` with a
//! JSON body that matches the handler's `input` parameter. Send-style
//! invocations (fire-and-forget) use a `?op=send` query parameter or a
//! distinct endpoint; this wrapper exposes both `call` (request-response)
//! and `send` (fire-and-forget) variants.
//!
//! The web binary uses `send` for every state-changing handler call so the
//! HTTP request returns quickly (Restate handles retries / durability).
//! `call` is reserved for the rare cases where the web wants the handler's
//! return value before responding to the user.

use crate::error::{Result, WebError};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;

/// Cloneable. The inner `Client` is itself cheap to clone (Arc internally).
#[derive(Clone, Debug)]
pub struct RestateClient {
    client: Client,
    ingress_base: String,
}

impl RestateClient {
    pub fn new(ingress_base: impl Into<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| WebError::Restate(format!("building reqwest client: {e}")))?;
        Ok(Self {
            client,
            ingress_base: ingress_base.into(),
        })
    }

    /// Fire-and-forget invocation: send the input to a Virtual Object / Service
    /// method and return immediately without waiting for the handler to finish.
    /// Restate persists the request and runs the handler durably.
    ///
    /// `key` is the Virtual Object key (or workflow id). For non-keyed Services
    /// pass an empty string.
    pub async fn send<I: Serialize>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<()> {
        let url = if key.is_empty() {
            format!("{}/{}/send/{}", self.ingress_base, service, method)
        } else {
            format!(
                "{}/{}/{}/{}/send",
                self.ingress_base, service, key, method
            )
        };
        let resp = self
            .client
            .post(&url)
            .json(input)
            .send()
            .await
            .map_err(|e| WebError::Restate(format!("send {service}/{method}: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(WebError::Restate(format!(
                "send {service}/{method} -> {status}: {body}"
            )));
        }
        Ok(())
    }

    /// Request-response invocation. Use sparingly — most state changes use
    /// `send`. Returns the handler's deserialized output type.
    pub async fn call<I: Serialize, O: DeserializeOwned>(
        &self,
        service: &str,
        key: &str,
        method: &str,
        input: &I,
    ) -> Result<O> {
        let url = if key.is_empty() {
            format!("{}/{}/{}", self.ingress_base, service, method)
        } else {
            format!("{}/{}/{}/{}", self.ingress_base, service, key, method)
        };
        let resp = self
            .client
            .post(&url)
            .json(input)
            .send()
            .await
            .map_err(|e| WebError::Restate(format!("call {service}/{method}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(WebError::Restate(format!(
                "call {service}/{method} -> {}: {body}",
                status.as_u16()
            )));
        }
        resp.json::<O>()
            .await
            .map_err(|e| WebError::Restate(format!("decoding {service}/{method} response: {e}")))
    }
}

/// `Arc<RestateClient>` shorthand for `AppState`.
pub type SharedRestateClient = Arc<RestateClient>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_shape_for_keyed_send() {
        // We don't actually hit a server — just sanity check the URL builder
        // shape via a small introspection. The send method is async and
        // does network I/O; deeper tests in Task 9+ use a wiremock server.
        let c = RestateClient::new("http://127.0.0.1:8080").unwrap();
        // No public URL builder; we verified the test by inspection of the
        // method body. Construction and field check:
        assert_eq!(c.ingress_base, "http://127.0.0.1:8080");
    }
}
```

The Restate ingress URL shape varies slightly across Restate versions — the patterns above match Restate platform 1.x. If `cargo run` against a real Restate platform produces 404s, the URL builder may need tweaking; the implementer should iterate against the actual platform response (Plan 7's smoke test will catch this end-to-end).

- [ ] **Step 2: Run tests**

```bash
cargo test -p web --lib restate_client
```

Expected: 1 test passes.

- [ ] **Step 3: Un-comment re-exports for state + restate_client**

In `lib.rs`:
```rust
pub use restate_client::RestateClient;
pub use state::AppState;
```

- [ ] **Step 4: Verify the crate compiles**

```bash
cargo check -p web
```

Expected: clean.

- [ ] **Step 5: Commit (covering Tasks 4 + 5)**

```bash
git add crates/web/src/state.rs crates/web/src/restate_client.rs crates/web/src/lib.rs
git commit -m "feat(web): AppState + RestateClient ingress wrapper"
```

---

### Task 6: `Session` struct + tower-sessions wiring

Per spec §10.1: session contains `user_id`, `login`, encrypted user access token, CSRF token, last admin-check timestamp per account.

**Files:**
- Modify: `crates/web/src/session.rs`

- [ ] **Step 1: Write `Session`**

```rust
//! Session struct + tower-sessions integration.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tower_sessions::Session as TowerSession;

/// Per spec §10.1: session contents.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Session {
    pub user_id: u64,
    pub login: String,
    /// User access token from GitHub OAuth. Stored encrypted at rest by
    /// tower-sessions's cookie-encryption layer (see [`crate::config::WebConfig::session_secret`]).
    pub access_token: String,
    /// CSRF token for the OAuth state round-trip. Set on `/login`, verified on
    /// `/oauth/callback`.
    pub oauth_csrf: Option<String>,
    /// Last admin-check timestamp per `account_login`. Used by the auth
    /// middleware to throttle GitHub `/user/memberships/orgs/{login}` calls
    /// (60-second cache per spec §10.3).
    pub admin_checks: HashMap<String, AdminCheck>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdminCheck {
    pub is_admin: bool,
    pub checked_at: DateTime<Utc>,
}

impl Session {
    pub fn is_authenticated(&self) -> bool {
        !self.login.is_empty()
    }
}

const SESSION_KEY: &str = "ghinvite";

/// Read the session from a tower-sessions handle. Returns the default empty
/// session if none is set.
pub async fn load(tower: &TowerSession) -> Result<Session, tower_sessions::session::Error> {
    Ok(tower.get(SESSION_KEY).await?.unwrap_or_default())
}

/// Persist the session.
pub async fn save(tower: &TowerSession, session: &Session) -> Result<(), tower_sessions::session::Error> {
    tower.insert(SESSION_KEY, session).await
}

/// Clear the session entirely.
pub async fn clear(tower: &TowerSession) {
    tower.flush().await.ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn default_session_is_unauthenticated() {
        let s = Session::default();
        assert!(!s.is_authenticated());
    }

    #[test]
    fn populated_session_is_authenticated() {
        let s = Session {
            user_id: 7,
            login: "octocat".into(),
            access_token: "u_xxx".into(),
            oauth_csrf: None,
            admin_checks: HashMap::new(),
        };
        assert!(s.is_authenticated());
    }

    #[test]
    fn admin_check_round_trips_via_serde() {
        let mut s = Session::default();
        s.admin_checks.insert(
            "acme".into(),
            AdminCheck {
                is_admin: true,
                checked_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            },
        );
        let json = serde_json::to_string(&s).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.admin_checks.len(), 1);
        assert!(parsed.admin_checks.get("acme").unwrap().is_admin);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p web --lib session
```

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/web/src/session.rs
git commit -m "feat(web): Session struct + tower-sessions helpers"
```

---

### Task 7: axum app builder

The central function `build_app(state) -> axum::Router` that registers every route. Most routes return 501 stubs at this point; OAuth + home come in later tasks.

**Files:**
- Modify: `crates/web/src/lib.rs`

- [ ] **Step 1: Add `build_app`**

Append to `crates/web/src/lib.rs` (after the `pub use` block):

```rust
use axum::Router;

/// Build the axum app with all routes registered. The session store is
/// supplied externally so Plan 7 can swap in a D1-backed store; for native
/// dev pass `tower_sessions_sqlx_store::SqliteStore`.
///
/// `state` carries storage, github transport, restate client, and config.
pub fn build_app<S>(state: AppState, session_store: S) -> Router
where
    S: tower_sessions::SessionStore + Clone + 'static,
{
    use tower_sessions::{Expiry, SessionManagerLayer};

    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(state.config.cookie_secure)
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(time::Duration::days(30)));

    Router::new()
        .merge(routes::health::router())
        .merge(routes::home::router())
        .merge(routes::oauth::router())
        .merge(routes::dashboard::router())
        .merge(routes::invitation::router())
        .merge(routes::webhook::router())
        .layer(session_layer)
        .with_state(state)
}
```

The `time::Duration` reference may need an import. `tower-sessions` re-exports `time` under its `cookie::time` path.

If the SessionManagerLayer / SameSite paths differ for `tower-sessions 0.13`, adjust to whatever the version exposes (e.g. `tower_sessions::SessionManagerLayer::new(store)` and `.with_same_site(SameSite::Lax)` are the canonical idioms; check the docs).

- [ ] **Step 2: Add stub route modules**

Each `routes/<name>.rs` needs a `pub fn router() -> Router<AppState>` (or `Router` parameterized appropriately) that registers its routes. For now, every stub `routes/<name>.rs` exposes:

```rust
use axum::Router;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new() // populated by Tasks 8–13 + 19–22 + 23–25
}
```

Replace the `// filled in by a later task` placeholder in `routes/{health,home,oauth,dashboard,invitation,webhook}.rs` with the above.

- [ ] **Step 3: Verify**

```bash
cargo check -p web
```

Expected: clean. The 501 placeholders ship in later tasks — for Task 7, an empty Router is fine.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/lib.rs crates/web/src/routes
git commit -m "feat(web): axum app builder + tower-sessions wiring"
```

---

### Task 8: `/health` route

Tiny route used by Plan 7's deployment smoke + uptime checks.

**Files:**
- Modify: `crates/web/src/routes/health.rs`

- [ ] **Step 1: Write the route**

```rust
//! `GET /health` — uptime check.

use crate::state::AppState;
use axum::routing::get;
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health() -> &'static str {
    "ok"
}
```

- [ ] **Step 2: Add a smoke test**

`crates/web/tests/route_smoke.rs`:

```rust
//! Route-existence smoke tests. Each route from spec §11 must be registered
//! and respond with a non-500. Some return 501 (Plans 5–6 fill them in);
//! some return 200/302; auth-required ones return 302→/login.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use web::{AppState, RestateClient, WebConfig, build_app};

async fn build_test_app() -> axum::Router {
    use github::mocks::MockTransport;
    let storage: Arc<dyn storage::Storage> = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(vec![]));
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let state = AppState::new(storage, transport, restate, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

#[tokio::test]
async fn health_returns_ok() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}
```

The integration test depends on `tower-sessions::MemoryStore` (in-memory store for tests). If the version we picked doesn't expose `MemoryStore` directly, use `tower_sessions::MokSessionStore` or pull in `tower-sessions-memory-store`. Adapt the import path as needed.

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test route_smoke
```

Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/health.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): /health route + smoke test"
```

---

### Task 9: `/login` route

Builds the OAuth authorize URL via `github::oauth::AuthorizeUrl::build`, stashes the CSRF state in the session, redirects.

**Files:**
- Modify: `crates/web/src/routes/oauth.rs`

- [ ] **Step 1: Add the route**

```rust
//! OAuth flow routes: /login, /oauth/callback, /install, /logout.

use crate::error::{Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use github::oauth::AuthorizeUrl;
use rand::RngCore;
use rand::rngs::OsRng;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/logout", get(logout))
        .route("/install", get(install))
        // Callback handler comes in Tasks 10 + 11.
}

/// Generate a CSRF state, stash it in the session, redirect to GitHub's
/// authorize endpoint.
async fn login(
    State(state): State<AppState>,
    tower: TowerSession,
) -> Result<impl IntoResponse> {
    let csrf = generate_csrf_token();
    let mut session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    session.oauth_csrf = Some(csrf.clone());
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    let authorize = AuthorizeUrl::build(&state.config.oauth, csrf, &[])
        .map_err(WebError::Github)?;

    Ok(Redirect::to(&authorize.url))
}

async fn logout(tower: TowerSession) -> impl IntoResponse {
    session::clear(&tower).await;
    Redirect::to("/")
}

async fn install(State(state): State<AppState>) -> impl IntoResponse {
    Redirect::to(&state.config.github_install_url)
}

fn generate_csrf_token() -> String {
    use base64::Engine;
    let mut buf = [0u8; 24]; // 24 bytes -> 32 base64url chars
    OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}
```

The `base64` crate is already a workspace dep (Plan 2's JWT signer added it).

- [ ] **Step 2: Add tests**

Append to `crates/web/tests/route_smoke.rs`:

```rust
#[tokio::test]
async fn login_redirects_to_github_authorize() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("https://github.com/login/oauth/authorize?"));
    assert!(location.contains("client_id="));
    assert!(location.contains("state="));
}

#[tokio::test]
async fn install_redirects_to_install_url() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/install").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("https://github.com/apps/"));
}

#[tokio::test]
async fn logout_clears_session_and_redirects_home() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/logout").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/");
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test route_smoke
```

Expected: 4 tests pass (1 from Task 8 + 3 new).

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/oauth.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): /login, /install, /logout routes"
```

---

### Task 10: `/oauth/callback` (sign-in path)

The callback route handles two cases:
1. **Sign-in only**: `?code=...&state=...` — verify state, exchange code, fetch `/user`, upsert user row, set session.
2. **Post-install**: same as above plus `?installation_id=...&setup_action=install` — also call `Installation::onboard.send` via Restate.

This task implements case 1; Task 11 adds case 2.

**Files:**
- Modify: `crates/web/src/routes/oauth.rs`

- [ ] **Step 1: Add the callback route**

In `crates/web/src/routes/oauth.rs`, add to the imports:

```rust
use chrono::Utc;
use github::HttpTransport;
use github::oauth::{exchange_code, UserApiClient};
use serde::Deserialize;
```

Add to the `router()` function:

```rust
        .route("/oauth/callback", get(oauth_callback))
```

Add the handler:

```rust
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    installation_id: Option<u64>,
    setup_action: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn oauth_callback(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Query(q): axum::extract::Query<CallbackQuery>,
) -> Result<impl IntoResponse> {
    // GitHub may redirect with `?error=access_denied` if the user clicked
    // cancel. Surface as a friendly message.
    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or_default();
        return Err(WebError::OAuth(format!("{err}: {desc}")));
    }

    let code = q.code.ok_or_else(|| WebError::BadRequest("missing 'code'".into()))?;
    let supplied_state = q.state.ok_or_else(|| WebError::BadRequest("missing 'state'".into()))?;

    let mut session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    let expected_state = session
        .oauth_csrf
        .clone()
        .ok_or_else(|| WebError::OAuth("no CSRF state in session".into()))?;
    // Constant-time-ish comparison (the strings are short and not secret in
    // the timing-attack sense, but defensive).
    if expected_state.len() != supplied_state.len()
        || expected_state
            .as_bytes()
            .iter()
            .zip(supplied_state.as_bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return Err(WebError::OAuth("CSRF state mismatch".into()));
    }
    // Consume the CSRF token so it can't be replayed.
    session.oauth_csrf = None;

    // Exchange the code for a user access token.
    let token = exchange_code(state.github_transport.as_ref(), &state.config.oauth, &code).await?;

    // Fetch /user via the user-token client.
    let user_api = UserApiClient::new(state.github_transport.clone(), token.access_token.clone());
    let gh_user = user_api.get_user().await?;

    // Upsert into our `users` table (Plan 1's storage).
    let user_row = domain::User {
        user_id: gh_user.id,
        login: gh_user.login.clone(),
        avatar_url: gh_user.avatar_url.clone(),
        last_seen_at: Utc::now(),
    };
    state.storage.upsert_user(&user_row).await?;

    // Populate the session.
    session.user_id = gh_user.id;
    session.login = gh_user.login.clone();
    session.access_token = token.access_token;
    session::save(&tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;

    // Task 11 adds the installation_id branch. For Task 10, ignore it.
    let _ = q.installation_id;
    let _ = q.setup_action;

    Ok(Redirect::to("/"))
}
```

- [ ] **Step 2: Add OAuth integration test**

`crates/web/tests/oauth_flow.rs`:

```rust
//! End-to-end OAuth sign-in tests. Drives the full /login → /oauth/callback
//! flow through axum + the in-process MockTransport from Plan 2.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use github::mocks::{Expectation, MockTransport};
use github::transport::{Method, Response};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;
use web::{AppState, RestateClient, WebConfig, build_app};

fn build_app_with_mock(mock: MockTransport) -> axum::Router {
    let rt = tokio::runtime::Handle::try_current();
    if rt.is_err() {
        panic!("must be called from within a tokio runtime");
    }
    let _ = rt; // suppress unused
    let storage_fut = storage::SqlxStorage::in_memory();
    let storage: Arc<dyn storage::Storage> = Arc::new(
        tokio::runtime::Handle::current()
            .block_on(storage_fut)
            .unwrap(),
    );
    let transport: Arc<dyn github::HttpTransport> = Arc::new(mock);
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let state = AppState::new(storage, transport, restate, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}

// (Tests append in following steps.)
```

The `block_on` inside an async test won't work — adjust to make `build_app_with_mock` async:

```rust
async fn build_app_with_mock(mock: MockTransport) -> axum::Router {
    let storage: Arc<dyn storage::Storage> = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let transport: Arc<dyn github::HttpTransport> = Arc::new(mock);
    let restate = Arc::new(RestateClient::new("http://127.0.0.1:8080").unwrap());
    let state = AppState::new(storage, transport, restate, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    build_app(state, session_store)
}
```

Now add the actual sign-in test. The full flow needs to:

1. Hit `/login` → get a redirect with `state=<csrf>` and a session cookie.
2. Hit `/oauth/callback?code=test-code&state=<csrf>` reusing the cookie → MockTransport scripts the OAuth-token-exchange + /user response.
3. Verify the response is a redirect to `/` AND the session cookie now has the user populated.

Append:

```rust
#[tokio::test]
async fn signin_happy_path() {
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]);
    let app = build_app_with_mock(mock).await;

    // Step 1: hit /login, capture the cookie + state.
    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .expect("session cookie set")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state_param = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    // Step 2: hit /oauth/callback with the same cookie and the captured state.
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state_param}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
    let dest = resp2.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(dest, "/");
}

#[tokio::test]
async fn callback_csrf_mismatch_rejects() {
    let mock = MockTransport::scripted(vec![]); // no GitHub calls expected
    let app = build_app_with_mock(mock).await;
    // Hit /login first to create a session with a real CSRF.
    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    // Then send a wrong state.
    let resp2 = app
        .oneshot(
            Request::builder()
                .uri("/oauth/callback?code=any&state=wrong-csrf")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
    let body = resp2.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("CSRF"));
}
```

The MockTransport's URL paths are `https://api.github.com/...` (the production base) because `UserApiClient::new` defaults to that. The transport doesn't need a base override here.

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test oauth_flow
```

Expected: 2 tests pass.

If the cookie roundtrip doesn't work because tower-sessions's MemoryStore uses a different cookie name, adjust the cookie capture / replay shape. The session cookie is named `id` by default; `set-cookie` will say something like `id=...`.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/oauth.rs crates/web/tests/oauth_flow.rs
git commit -m "feat(web): /oauth/callback sign-in path"
```

---

### Task 11: `/oauth/callback` post-install path

When `installation_id` is present in the callback query, also call `Installation::onboard` via Restate.

**Files:**
- Modify: `crates/web/src/routes/oauth.rs`

- [ ] **Step 1: Extend the callback handler**

After the session save in `oauth_callback`, but before the redirect, add:

```rust
    // Post-install path: if installation_id is present, fire-and-forget
    // Installation::onboard via Restate. We don't await the handler — it
    // runs durably and the user gets immediate navigation back to /.
    if let Some(installation_id) = q.installation_id {
        // Per spec §10.1 step 4: call Installation::onboard.send(installation_id).
        // We need account_id, account_login, account_type, selected_repos,
        // installed_at — but the OAuth callback doesn't carry them. v1
        // simplification: pass what we know (installation_id + actor_user_id)
        // and let Restate's onboard handler short-circuit if the row already
        // exists from a prior webhook.
        //
        // For Plan 4: send a minimal payload that the handler accepts. The
        // restate-svc OnboardInput requires more fields; this is a known
        // mismatch — Plan 6's webhook receiver carries the full payload.
        // Here we just trigger the workflow; the actual onboard happens via
        // the GitHub `installation.created` webhook in Plan 6.
        //
        // Concrete v1 behavior: log + skip. Re-visit when Plan 6 lands.
        tracing::info!(
            installation_id,
            "OAuth callback carried installation_id; awaiting webhook onboarding"
        );
    }
```

This is a deliberate Plan-4-only no-op: spec §10.1 step 4 and §10.2 say "call Installation::onboard.send(installation_id)" but the webhook from `installation.created` (Plan 6) is the canonical source — it carries the full account metadata. Plan 4's OAuth callback only logs that the install happened; the webhook does the real work.

If Plan 6 review pushes back, this can be revisited; in v1 the install webhook is reliable enough that the OAuth callback path is a hint, not a write.

- [ ] **Step 2: Add a test for the post-install branch**

Append to `crates/web/tests/oauth_flow.rs`:

```rust
#[tokio::test]
async fn signin_with_installation_id_logs_and_proceeds() {
    let mock = MockTransport::scripted(vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]);
    let app = build_app_with_mock(mock).await;

    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = resp1
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state_param = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let resp2 = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/oauth/callback?code=test-code&state={state_param}&installation_id=99&setup_action=install"
                ))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test oauth_flow
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/oauth.rs crates/web/tests/oauth_flow.rs
git commit -m "feat(web): /oauth/callback post-install path (logs; webhook does real work)"
```

---

### Task 12: Auth middleware

A tower layer that:
1. Loads the session.
2. If the session is unauthenticated, returns `WebError::Unauthenticated` (which redirects to /login).
3. Otherwise inserts the session into request extensions for downstream handlers.

Plus an admin-recheck cache helper that throttles `/user/memberships/orgs/{login}` calls to once per 60 seconds per session per login.

**Files:**
- Modify: `crates/web/src/middleware/auth.rs`

- [ ] **Step 1: Write the middleware**

```rust
//! Session-validation tower layer + admin recheck cache.

use crate::error::WebError;
use crate::session::{self, AdminCheck, Session};
use crate::state::AppState;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::response::Response;
use chrono::{Duration, Utc};
use github::oauth::UserApiClient;
use std::sync::Arc;

const ADMIN_CACHE_TTL_SECS: i64 = 60;

/// Extractor: returns the Session from request extensions or a 302→/login.
/// Routes that require authentication take this as a parameter.
pub struct RequireSession(pub Session);

impl<S: Send + Sync> FromRequestParts<S> for RequireSession {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let tower: tower_sessions::Session = parts
            .extensions
            .get::<tower_sessions::Session>()
            .cloned()
            .ok_or_else(|| WebError::Session("no tower session in request extensions".into()))?;
        let session = session::load(&tower)
            .await
            .map_err(|e| WebError::Session(e.to_string()))?;
        if !session.is_authenticated() {
            return Err(WebError::Unauthenticated);
        }
        Ok(RequireSession(session))
    }
}

/// Re-check whether the signed-in user is an admin of `account_login`.
/// Hits GitHub's `/user/memberships/orgs/{login}` once per 60s per login,
/// caching the result in the session.
///
/// Returns `Ok(true)` if admin, `Ok(false)` if not, `Err` on transport
/// failure.
pub async fn check_admin(
    state: &AppState,
    session: &mut Session,
    account_login: &str,
) -> Result<bool, WebError> {
    if let Some(cached) = session.admin_checks.get(account_login) {
        if Utc::now() - cached.checked_at < Duration::seconds(ADMIN_CACHE_TTL_SECS) {
            return Ok(cached.is_admin);
        }
    }
    let user_api = UserApiClient::new(state.github_transport.clone(), session.access_token.clone());
    let is_admin = match user_api.get_org_membership(account_login).await {
        Ok(m) => m.role == "admin" && m.state == "active",
        Err(github::Error::Status { status: 404, .. }) => false,
        Err(e) => return Err(WebError::Github(e)),
    };
    session.admin_checks.insert(
        account_login.to_string(),
        AdminCheck {
            is_admin,
            checked_at: Utc::now(),
        },
    );
    Ok(is_admin)
}
```

The `AsyncTrait` requirement of `FromRequestParts` may need a different signature in axum 0.8. The pattern is: `impl<S: Send + Sync> FromRequestParts<S> for RequireSession { type Rejection = WebError; async fn from_request_parts(...) }`. If axum 0.8 mandates the `axum::async_trait` macro, add it.

- [ ] **Step 2: Verify**

```bash
cargo check -p web
```

Expected: clean. (`AsyncTrait` issues — adapt as needed.)

- [ ] **Step 3: Commit**

```bash
git add crates/web/src/middleware/auth.rs
git commit -m "feat(web): RequireSession extractor + admin recheck cache"
```

---

### Task 13: CSP header middleware

Per spec §18: CSP set on dashboard responses. Apply a strict policy via a tower layer.

**Files:**
- Modify: `crates/web/src/middleware/csp.rs`

- [ ] **Step 1: Write the layer**

```rust
//! Content-Security-Policy header layer.
//!
//! Applied to dashboard responses (set by Plan 5+). For Plan 4 we ship the
//! constant + a helper layer; the home page (Task 22) sets a more permissive
//! policy because the marketing page may load external assets.

use axum::http::HeaderValue;
use tower_http::set_header::SetResponseHeaderLayer;

/// Strict CSP for dashboard pages. Adjust if Plan 5 introduces external
/// dependencies (CDN scripts, third-party fonts).
pub const DASHBOARD_CSP: &str =
    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' https://avatars.githubusercontent.com data:; \
     connect-src 'self'; \
     font-src 'self'; \
     frame-ancestors 'none'; \
     form-action 'self' https://github.com; \
     base-uri 'self'";

pub fn dashboard_csp_layer() -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(
        axum::http::header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DASHBOARD_CSP),
    )
}
```

- [ ] **Step 2: Verify**

```bash
cargo check -p web
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/web/src/middleware/csp.rs
git commit -m "feat(web): CSP header layer for dashboard responses"
```

---

### Task 14: Frontend toolchain (Tailwind v4 + DaisyUI v4)

`npm`-driven CSS build. The output (`crates/web/assets/styles.built.css`) is served as a static asset by axum.

**Files:**
- Create: `crates/web/package.json`
- Create: `crates/web/tailwind.config.js`
- Create: `crates/web/assets/styles.css`
- Create: `crates/web/.gitignore`
- Modify: workspace `.gitignore` (no, scope to crate-local)

- [ ] **Step 1: `package.json`**

```json
{
  "name": "ghinvite-web-frontend",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "build:css": "tailwindcss -i ./assets/styles.css -o ./assets/styles.built.css --minify",
    "watch:css": "tailwindcss -i ./assets/styles.css -o ./assets/styles.built.css --watch"
  },
  "devDependencies": {
    "tailwindcss": "^4.0.0",
    "@tailwindcss/cli": "^4.0.0",
    "daisyui": "^4.12.0"
  }
}
```

- [ ] **Step 2: `tailwind.config.js`**

```javascript
/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./src/**/*.rs",
  ],
  theme: {
    extend: {},
  },
  plugins: [require("daisyui")],
  daisyui: {
    themes: ["light", "dark"],
  },
};
```

- [ ] **Step 3: `assets/styles.css` (Tailwind input)**

```css
@import "tailwindcss";
@plugin "daisyui";
```

(Tailwind v4 uses CSS-first imports; the `@plugin` directive replaces the JS-config plugin entry. If your toolchain ends up on v3 instead, fall back to the classic `@tailwind base; @tailwind components; @tailwind utilities;` form.)

- [ ] **Step 4: `crates/web/.gitignore`**

```
node_modules/
assets/styles.built.css
```

- [ ] **Step 5: Verify the build runs**

```bash
cd crates/web
npm install --no-audit --prefer-offline 2>&1 | tail -5
npx tailwindcss -i ./assets/styles.css -o ./assets/styles.built.css --minify 2>&1 | tail -5
ls -la assets/styles.built.css
```

Expected: `styles.built.css` exists, non-empty.

If `npm` is not in the dev environment, add it to `devenv.nix`:

```nix
languages.javascript = {
  enable = true;
  package = pkgs.nodejs_20;
};
```

(Modify the relevant `devenv.nix`; commit if devenv config changes. Otherwise skip — the implementer decides whether to thread npm through devenv or use a system-installed npm.)

- [ ] **Step 6: Commit**

```bash
git add crates/web/package.json crates/web/tailwind.config.js crates/web/assets/styles.css crates/web/.gitignore
git commit -m "build(web): Tailwind v4 + DaisyUI v4 npm-driven CSS build"
```

---

### Task 15: Static asset serving

axum serves `assets/styles.built.css` at `/static/styles.css`.

**Files:**
- Modify: `crates/web/src/lib.rs`

- [ ] **Step 1: Add the static route**

In `crates/web/src/lib.rs`'s `build_app`, before `.layer(session_layer)`, add:

```rust
        .route(
            "/static/styles.css",
            axum::routing::get(serve_styles_css),
        )
```

Then add a free function:

```rust
async fn serve_styles_css() -> impl axum::response::IntoResponse {
    use axum::http::header;
    use axum::response::IntoResponse;
    let css = include_str!("../assets/styles.built.css");
    (
        [(header::CONTENT_TYPE, "text/css")],
        css,
    )
        .into_response()
}
```

`include_str!` embeds the built CSS at compile time. The implementer must run `npm run build:css` (Task 14's command) before `cargo build -p web`. Document this in Task 26's README.

If `assets/styles.built.css` doesn't exist at compile time, `cargo build` will fail with a clear error. That's acceptable for v1 — Plan 7's deployment workflow will run the npm build before `wrangler deploy`.

- [ ] **Step 2: Add a smoke test**

Append to `crates/web/tests/route_smoke.rs`:

```rust
#[tokio::test]
async fn static_styles_returns_css() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/static/styles.css").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/css"));
}
```

- [ ] **Step 3: Run tests**

```bash
# Build CSS first.
cd crates/web && npx tailwindcss -i ./assets/styles.css -o ./assets/styles.built.css --minify && cd ../..

cargo test -p web --test route_smoke
```

Expected: 5 tests pass (4 prior + 1 new).

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/lib.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): /static/styles.css route (embedded built CSS)"
```

---

### Task 16: Dioxus 0.7 SSR helper

`crates/web/src/views/render.rs` exposes `render(component) -> String` that runs Dioxus 0.7 server-side rendering and returns the HTML string.

**Files:**
- Modify: `crates/web/src/views/render.rs`

- [ ] **Step 1: Write the helper**

```rust
//! Dioxus 0.7 SSR renderer.

use dioxus::prelude::*;

/// Render a Dioxus component to a complete HTML string. Uses dioxus-ssr's
/// pre-rendering — no client-side hydration is wired in Plan 4.
///
/// The component receives no props; pass shared state through the closure.
pub fn render<F>(component: F) -> String
where
    F: 'static + Clone + Fn() -> Element + Send,
{
    let mut vdom = VirtualDom::new(move || component());
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}
```

The exact Dioxus 0.7 API may differ slightly from what's shown — `dioxus::prelude::*` and `dioxus_ssr::render` are the canonical paths in 0.6/0.7. If 0.7 has renamed `render()` to `render_to_string()` or similar, adapt. The closure-based `VirtualDom::new` shape is stable across recent versions.

- [ ] **Step 2: Add a smoke test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Element {
        rsx! { div { "hello world" } }
    }

    #[test]
    fn render_produces_html() {
        let html = render(hello);
        assert!(html.contains("<div"));
        assert!(html.contains("hello world"));
    }
}
```

- [ ] **Step 3: Run test**

```bash
cargo test -p web --lib views::render
```

Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/views/render.rs
git commit -m "feat(web): Dioxus SSR render helper"
```

---

### Task 17: Three Dioxus layouts

Per spec §12: HomeLayout / DashboardLayout / InvitationLayout. Plan 4 ships visual chrome only — no interactive components. Plans 5+ fill in the interior.

**Files:**
- Modify: `crates/web/src/views/layouts.rs`
- Modify: `crates/web/src/views/components.rs`

- [ ] **Step 1: Write the components**

`crates/web/src/views/components.rs`:

```rust
//! Shared Dioxus components used across layouts.

use dioxus::prelude::*;

#[component]
pub fn Nav(cx: NavProps) -> Element {
    rsx! {
        nav {
            class: "navbar bg-base-100",
            div {
                class: "flex-1",
                a {
                    class: "btn btn-ghost text-xl",
                    href: "/",
                    "ghinvite"
                }
            }
            div {
                class: "flex-none",
                {match cx.signed_in_login.as_deref() {
                    Some(login) => rsx! {
                        span { class: "px-2", "Signed in as @{login}" }
                        a { class: "btn btn-ghost", href: "/logout", "Sign out" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary", href: "/login", "Sign in with GitHub" }
                    }
                }}
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct NavProps {
    /// `Some(login)` if the user is signed in, `None` otherwise.
    pub signed_in_login: Option<String>,
}

#[component]
pub fn Footer() -> Element {
    rsx! {
        footer {
            class: "footer footer-center p-4 bg-base-200 text-base-content",
            aside {
                p {
                    "ghinvite — GitHub repo collaborator invitations made easy"
                }
            }
        }
    }
}
```

`crates/web/src/views/layouts.rs`:

```rust
//! Three Dioxus layouts: HomeLayout, DashboardLayout, InvitationLayout.
//! Each wraps page content in zone-specific chrome (per spec §12).

use crate::views::components::{Footer, Nav};
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct LayoutProps {
    pub signed_in_login: Option<String>,
    pub title: String,
    /// The page content rendered inside the layout.
    pub children: Element,
}

#[component]
pub fn HomeLayout(cx: LayoutProps) -> Element {
    rsx! {
        Document {
            title: "{cx.title}",
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-100 flex flex-col",
            "data-theme": "light",
            Nav { signed_in_login: cx.signed_in_login.clone() }
            main { class: "flex-1 container mx-auto px-4 py-8", {cx.children} }
            Footer {}
        }
    }
}

#[component]
pub fn DashboardLayout(cx: LayoutProps) -> Element {
    rsx! {
        Document {
            title: "{cx.title}",
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200",
            "data-theme": "light",
            Nav { signed_in_login: cx.signed_in_login.clone() }
            main { class: "container mx-auto px-4 py-8", {cx.children} }
        }
    }
}

#[component]
pub fn InvitationLayout(cx: LayoutProps) -> Element {
    rsx! {
        Document {
            title: "{cx.title}",
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-100 flex items-center justify-center",
            "data-theme": "light",
            div {
                class: "card w-full max-w-md bg-base-100 shadow-xl",
                div { class: "card-body", {cx.children} }
            }
        }
    }
}
```

The `Document` component for setting `<head>` content is from Dioxus 0.7+ (it provides `Document`, `Head`, `Title` etc.). If 0.7's exact API differs, the structure is the same — wrap content in the right element to express "this goes in head". Worst case, hand-write the HTML scaffolding via `dangerous_inner_html` and skip `Document`.

- [ ] **Step 2: Verify**

```bash
cargo check -p web
```

Expected: clean. The `cx: NavProps` shape may need tweaking — Dioxus 0.7 takes props as `(props: NavProps)`. Iterate against the actual API.

- [ ] **Step 3: Commit**

```bash
git add crates/web/src/views/components.rs crates/web/src/views/layouts.rs
git commit -m "feat(web): three Dioxus layouts (Home / Dashboard / Invitation)"
```

---

### Task 18: Home page Dioxus component

The `/` page when no one is signed in: marketing copy + "Install on GitHub" CTA. When signed in: redirect to last-used account dashboard (or fall back to `/install` if none).

**Files:**
- Modify: `crates/web/src/views/home.rs`

- [ ] **Step 1: Write the component**

```rust
//! Home page: logged-out marketing or signed-in redirect.

use crate::views::layouts::HomeLayout;
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct HomePageProps {
    pub signed_in_login: Option<String>,
}

#[component]
pub fn HomePage(cx: HomePageProps) -> Element {
    rsx! {
        HomeLayout {
            signed_in_login: cx.signed_in_login.clone(),
            title: "ghinvite — invite collaborators with one shareable link".to_string(),
            children: rsx! {
                div {
                    class: "hero",
                    div {
                        class: "hero-content text-center max-w-2xl",
                        div {
                            h1 { class: "text-5xl font-bold", "Invite collaborators with one link." }
                            p {
                                class: "py-6",
                                "ghinvite turns ad-hoc \"can you add this person to our repo?\" \
                                 messages into shareable links — with optional approval, expiration, \
                                 and audit trail."
                            }
                            {match cx.signed_in_login.as_deref() {
                                Some(_) => rsx! {
                                    a {
                                        class: "btn btn-primary btn-lg",
                                        href: "/install",
                                        "Install on a new account"
                                    }
                                },
                                None => rsx! {
                                    a {
                                        class: "btn btn-primary btn-lg",
                                        href: "/login",
                                        "Sign in with GitHub"
                                    }
                                }
                            }}
                        }
                    }
                }
            },
        }
    }
}
```

- [ ] **Step 2: Verify**

```bash
cargo check -p web
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/web/src/views/home.rs
git commit -m "feat(web): HomePage Dioxus component"
```

---

### Task 19: `GET /` route handler

Wire the home page Dioxus component to the axum route.

**Files:**
- Modify: `crates/web/src/routes/home.rs`

- [ ] **Step 1: Write the route**

```rust
//! `GET /` — home page (logged-out marketing or signed-in redirect).

use crate::session;
use crate::state::AppState;
use crate::views::home::{HomePage, HomePageProps};
use crate::views::render::render;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use axum::Router;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(home))
}

async fn home(State(_state): State<AppState>, tower: TowerSession) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();

    // If signed in: per spec §11, "logged-in redirect to last account".
    // Plan 4 doesn't yet track the "last account" cookie — for now we let
    // the user pick from the install URL (until Plan 5 adds account-list
    // routing).
    if session.is_authenticated() {
        // Fall through — render the page with Sign Out + Install CTA.
    }

    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };

    let html = render(move || {
        HomePage(HomePageProps {
            signed_in_login: signed_in_login.clone(),
        })
    });
    Html(html).into_response()
}

// Suppress unused if Redirect isn't reached in Plan 4:
#[allow(dead_code)]
fn _suppress_unused(r: Redirect) -> Redirect {
    r
}
```

- [ ] **Step 2: Add route smoke test**

Append to `crates/web/tests/route_smoke.rs`:

```rust
#[tokio::test]
async fn home_returns_html() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/html"));
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ghinvite"));
    assert!(text.contains("Sign in with GitHub"));
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test route_smoke
```

Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/home.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): GET / home page (Dioxus rendered)"
```

---

### Task 20: 501 placeholders for `/accounts/...` (Plan 5 fills in)

Per spec §11: 5 routes under `/accounts/:login`. Plan 4 ships them as `501 Not Implemented` so the route surface is complete (Plan 5 swaps in real handlers).

**Files:**
- Modify: `crates/web/src/routes/dashboard.rs`

- [ ] **Step 1: Write 501 placeholders**

```rust
//! `/accounts/:login/...` routes. **Plan 4: 501 placeholders.**
//! Plan 5 fills in the dashboard, link CRUD, approval queue, and settings.

use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/:login", get(stub))
        .route("/accounts/:login/links/new", get(stub))
        .route("/accounts/:login/links/:link_id", get(stub))
        .route("/accounts/:login/requests", get(stub))
        .route("/accounts/:login/audit", get(stub))
        .route("/accounts/:login/settings", get(stub))
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 5 will fill in the dashboard routes.",
    )
}
```

- [ ] **Step 2: Add smoke test**

Append to `crates/web/tests/route_smoke.rs`:

```rust
#[tokio::test]
async fn dashboard_routes_return_501() {
    let app = build_test_app().await;
    for path in [
        "/accounts/acme",
        "/accounts/acme/links/new",
        "/accounts/acme/links/01HFOOBAR",
        "/accounts/acme/requests",
        "/accounts/acme/audit",
        "/accounts/acme/settings",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED, "path = {path}");
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test route_smoke
```

Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/dashboard.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): 501 placeholders for /accounts/* routes (Plan 5)"
```

---

### Task 21: 501 placeholders for `/i/:slug` (Plan 6 fills in)

**Files:**
- Modify: `crates/web/src/routes/invitation.rs`

- [ ] **Step 1: Write the placeholders**

```rust
//! `/i/:slug...` routes. **Plan 4: 501 placeholders.**
//! Plan 6 fills in the recipient flow.

use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/:slug", get(stub))
        .route("/i/:slug/request", post(stub))
        .route("/i/:slug/pending/:request_id", get(stub))
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 6 will fill in the recipient routes.",
    )
}
```

- [ ] **Step 2: Add smoke test**

```rust
#[tokio::test]
async fn invitation_routes_return_501() {
    let app = build_test_app().await;
    for (method, path) in [
        ("GET", "/i/AAAAAAAAAAAAAAAA"),
        // /i/:slug/request is POST; oneshot needs the right method.
        ("GET", "/i/AAAAAAAAAAAAAAAA/pending/01HFREQ1"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED, "{method} {path}");
    }

    // Test the POST separately.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/i/AAAAAAAAAAAAAAAA/request")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test route_smoke
```

Expected: 8 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/invitation.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): 501 placeholders for /i/* routes (Plan 6)"
```

---

### Task 22: 501 placeholder for `/webhooks/github`

**Files:**
- Modify: `crates/web/src/routes/webhook.rs`

- [ ] **Step 1: Write the placeholder**

```rust
//! `POST /webhooks/github`. **Plan 4: 501 placeholder.**
//! Plan 6 fills in the HMAC-verified webhook receiver.

use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/github", post(stub))
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 6 will fill in the GitHub webhook receiver.",
    )
}
```

- [ ] **Step 2: Add smoke test**

```rust
#[tokio::test]
async fn webhook_route_returns_501() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p web --test route_smoke
```

Expected: 9 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/routes/webhook.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): 501 placeholder for /webhooks/github (Plan 6)"
```

---

### Task 23: Native `main.rs`

The native binary that runs the axum server locally. Plan 7 will swap this for a Workers `#[event(fetch)]`.

**Files:**
- Modify: `crates/web/src/main.rs`

- [ ] **Step 1: Replace the stub with a real binary**

```rust
//! Native binary: runs the axum app on a local hyper server.
//! Plan 7 wraps `web::build_app()` in a Workers `#[event(fetch)]` instead.

use std::sync::Arc;
use tokio::net::TcpListener;
use tower_sessions_sqlx_store::SqliteStore;
use web::{AppState, RestateClient, WebConfig, build_app};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = WebConfig::for_local_dev();

    // Storage: in-memory sqlite (per-process). Plan 7 swaps in D1.
    let storage: Arc<dyn storage::Storage> =
        Arc::new(storage::SqlxStorage::in_memory().await?);

    // GitHub transport: real reqwest. Local-dev OAuth only works if you
    // configure GHINVITE_GITHUB_CLIENT_ID / SECRET to point at a real GitHub
    // App; otherwise /login redirects to a 404.
    let transport: Arc<dyn github::HttpTransport> =
        Arc::new(github::transport::ReqwestTransport::new()?);

    // Restate client: ingress at config.restate_ingress (defaults to
    // 127.0.0.1:8080 from `docker compose up -d restate`).
    let restate = Arc::new(RestateClient::new(&config.restate_ingress)?);

    // Session store: a local sqlite database.
    let session_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await?;
    sqlx::migrate!("./migrations").run(&session_pool).await.ok();
    let session_store = SqliteStore::new(session_pool);
    session_store.migrate().await?;

    let state = AppState::new(storage, transport, restate, config);
    let app = build_app(state, session_store);

    let addr = "127.0.0.1:8787".parse::<std::net::SocketAddr>()?;
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
```

The `migrations/` directory at `crates/web/migrations/` is created by tower-sessions-sqlx-store. If the version we're using auto-migrates on `migrate()`, no migrations dir is needed. Otherwise, copy the migrations from the upstream crate.

If `tracing_subscriber` isn't a workspace dep yet, add it:

```toml
# Cargo.toml workspace.dependencies
tracing-subscriber = { version = "0.3", features = ["fmt", "env-filter"] }
```

And to `crates/web/Cargo.toml` `[dependencies]`:

```toml
tracing-subscriber.workspace = true
```

- [ ] **Step 2: Verify the binary builds**

```bash
cargo build -p web --bin web
```

Expected: clean build. Don't run it (no real GitHub credentials available).

- [ ] **Step 3: Commit**

```bash
git add crates/web/src/main.rs crates/web/Cargo.toml Cargo.toml
git commit -m "feat(web): native binary for cargo run -p web"
```

---

### Task 24: README — local dev guide

**Files:**
- Modify: `crates/web/README.md`

- [ ] **Step 1: Write the README**

`crates/web/README.md`:

```markdown
# crates/web

axum-based web server for ghinvite. Three Dioxus layouts (Home / Dashboard /
Invitation), OAuth login + install flow consuming `crates/github`, Restate
ingress client for state-changing actions.

This crate is **library + native binary in Plan 4**. Plan 5 fills in dashboard
routes, Plan 6 the recipient flow + webhook receiver, Plan 7 the Workers
`#[event(fetch)]` entry + D1 storage + secrets management.

## Local dev

### One-time setup

\`\`\`bash
# Install npm deps for the Tailwind build.
cd crates/web
npm install
\`\`\`

### Build the CSS

\`\`\`bash
cd crates/web
npm run build:css
\`\`\`

This produces `crates/web/assets/styles.built.css`, which is `include_str!`'d
into the binary at compile time. Re-run after editing `tailwind.config.js`,
adding new utility classes in component files, or upgrading DaisyUI.

For continuous build during dev:

\`\`\`bash
cd crates/web
npm run watch:css
\`\`\`

### Run the server

\`\`\`bash
# Start Restate (in another terminal).
docker compose up -d restate

# Run the web binary.
cargo run -p web
\`\`\`

The server binds `127.0.0.1:8787`. Hit `http://127.0.0.1:8787` to see the
home page. `/login` redirects to GitHub OAuth — set
`GHINVITE_GITHUB_CLIENT_ID` and `GHINVITE_GITHUB_CLIENT_SECRET` in your
environment to point at a real GitHub App, otherwise the redirect lands on
GitHub's "App not found" page.

### Run tests

\`\`\`bash
cargo test -p web
\`\`\`

## Architecture

`web::build_app(state, session_store) -> axum::Router` is the single entry
point. `state` carries `Arc<dyn Storage>`, `Arc<dyn HttpTransport>`,
`Arc<RestateClient>`, and `WebConfig`. `session_store` is any
`tower_sessions::SessionStore` impl — `MemoryStore` for tests, `SqliteStore`
for native dev, `D1Store` for production (Plan 7).

Every route handler that touches Restate calls
`state.restate.send::<Input>("Service", "key", "method", &input)`. The web
binary never writes domain state directly — it always goes through
Plan 3's handlers via `RestateClient`.

For OAuth, every route uses `crate::github_transport` to construct a
`UserApiClient` per-request with the session's access token.
\`\`\`

(Replace literal triple-backtick escapes with real fences when writing the file.)

- [ ] **Step 2: Commit**

```bash
git add crates/web/README.md
git commit -m "docs(web): local-dev guide"
```

---

### Task 25: Final clippy + fmt + plan-map README

**Files:** all of `crates/web/`, plus `docs/superpowers/plans/README.md`.

- [ ] **Step 1: Run rustfmt**

```bash
cargo fmt --all
```

- [ ] **Step 2: Run clippy with `-D warnings`**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Fix any genuine issues; for stylistic ones use focused `#[allow(clippy::xxx)]`. Common spots:
- Unused imports — drop or `#[allow(unused_imports)]` with comment.
- Dioxus's macro expansion sometimes triggers `redundant_clone` or `needless_borrow` — add `#[allow]` near the call site if so.

- [ ] **Step 3: Run the entire workspace test suite**

```bash
cargo test --workspace
```

Expected: every test passes (Plan 4 ships ~12 new tests, mostly route smokes + OAuth flow).

- [ ] **Step 4: Update the plan map README**

Edit `docs/superpowers/plans/README.md`. Change the row for Plan 4 from:

```
| 4 | **Web binary core** | _not yet written_ | Pending | ...
```

to:

```
| 4 | **Web binary core** | `2026-05-04-ghinvite-web-binary.md` | Implemented | ...
```

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "chore(web): apply rustfmt and clippy fixes; mark Plan 4 done"
```

---

## Plan self-review

**Spec coverage check** (each spec section → which task implements it):

- §3 (tech stack: axum, tower-sessions, Dioxus, Tailwind+DaisyUI) → Tasks 1, 14, 16
- §10.1 / §10.2 (OAuth flow) → Tasks 9, 10, 11
- §10.3 (auth middleware + admin recheck) → Task 12
- §10.4 (logout) → Task 9
- §11 (URL routing — every route declared) → Tasks 8, 9, 10, 19, 20, 21, 22
- §12 (three Dioxus layouts) → Tasks 17, 18, 19
- §16 (HTTP error mapping) → Task 3
- §18 security (Secure / HttpOnly / SameSite cookies, CSP, generic 404) → Tasks 7, 13, 3

**Spec sections NOT covered by this plan (intentionally — assigned to other plans):**
- §11 dashboard routes (`/accounts/...`) → Plan 5 (this plan ships 501 stubs).
- §11 recipient routes (`/i/...`) and `/webhooks/github` → Plan 6.
- §12 server functions for state-changing actions → Plan 5 (uses RestateClient from this plan).
- §15 audit emission → Plan 3 (already shipped); web binary doesn't audit.
- §13 webhook receiver — HMAC verification + envelope parsing + dispatch → Plan 6 (uses `github::hmac::verify_signature_256` from Plan 2).
- §17 e2e tests → Plan 8.

**Type consistency check:**
- `WebConfig`, `WebError`, `Result`, `AppState`, `RestateClient`, `Session`, `RequireSession`, `AdminCheck` are all defined in their respective tasks and used identically across handler tasks.
- `OAuthConfig` from Plan 2 (`github::oauth::OAuthConfig`) is consumed by `WebConfig`. The `redirect_uri` shape (`<base_url>/oauth/callback`) is consistent across `WebConfig::for_local_dev` and the actual `oauth_callback` route.
- `Storage::audit` is NOT called anywhere in this crate — Plan 3's handlers own audit emission. The web binary's only direct storage write is `upsert_user` on sign-in (Task 10).

**Placeholder check:** No `TBD`, `TODO`, `// implement later`. Steps reference each other by task number; every code block is complete-as-shown for the task scope. The Dioxus 0.7 SSR API (`render_to_string` vs. `dioxus_ssr::render`) and Restate ingress URL shape are flagged as iteration points where the SDK reality may force adjustments.

**Risk: Dioxus 0.7's SSR API and prop forms.** The plan's Dioxus code uses Dioxus 0.6/0.7 idioms (`#[component]`, `Props`, `rsx!`, `dioxus_ssr::render`). If 0.7 has shipped substantial API changes since the plan was written, the implementer may need to iterate. The pure-logic separation (route handlers don't depend on Dioxus shape) keeps the impact bounded to `crates/web/src/views/`.

**Risk: Tailwind v4 vs. DaisyUI v4 compatibility.** Tailwind v4 + DaisyUI v4 is a recent combination. If the build doesn't work cleanly, the implementer may fall back to Tailwind v3 + DaisyUI v4 (a known-working pair). Drop the `@plugin "daisyui";` import and use the JS plugin entry instead.

---

## Audit trail

This plan was written on 2026-05-04 against the just-implemented `crates/domain` + `crates/audit` + `crates/storage` (Plan 1), `crates/github` (Plan 2), and `crates/restate-svc` (Plan 3). User confirmed:

| # | Decision | Source |
|---|----------|--------|
| 1 | Library + native bin in Plan 4; Workers `#[event(fetch)]` deferred to Plan 7 | User pre-write decision |
| 2 | tower-sessions with SqliteStore for dev; D1 swap in Plan 7 | User pre-write decision |
| 3 | Roll our own RestateClient (no `restate-sdk` dep on web crate) | User pre-write decision |
| 4 | Tailwind v4 + DaisyUI v4 | User pre-write decision |
| 5 | Dioxus 0.7, SSR-only (no hydration in Plan 4) | User decision (overrode my 0.6 suggestion) |
| 6 | Server functions deferred to Plan 5 | User pre-write decision |
| 7 | npm-driven Tailwind build | User pre-write decision |
| 8 | 501 stubs for routes Plans 5–6 own; full skeleton of §11 | User pre-write decision |
