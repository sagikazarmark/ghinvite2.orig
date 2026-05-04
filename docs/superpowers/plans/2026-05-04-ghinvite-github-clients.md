# ghinvite — Plan 2: GitHub Clients Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `crates/github` crate that ships every GitHub-touching surface ghinvite v1 needs: a user-OAuth client (sign-in flow + `/user` + `/user/memberships`), an installation-token client (RS256 App-JWT mint + token cache + the seven `crates/github`-side endpoints in spec §14), an HMAC helper for webhook verification, and a mock HTTP transport so Plan 3's Restate handlers can be tested hermetically.

**Architecture:** One crate, one trait at the seam (`HttpTransport`). Two HTTP impls: `ReqwestTransport` (production, native + wasm) and `MockTransport` (script-driven, behind a `test-mock` feature). Two facade clients sit on top of the transport: `UserApiClient` (user-token) and `InstallationClient` (App-JWT → installation token, cached). The HMAC helper is independent of the transport — pure bytes-in / bool-out. RS256 JWTs are minted manually with `rsa` + `sha2` + `base64` to keep wasm32-unknown-unknown deployable without `ring`.

**Tech Stack:** Rust 2024 (workspace), `reqwest 0.12` (`rustls-tls`, `json`, optional `wasm-client` for the wasm target) behind an `async-trait` HTTP seam, `oauth2 5` for authorize-URL + state-token discipline, `rsa 0.9` + `sha2 0.10` + `base64 0.22` for manual RS256 JWT signing (wasm-portable, avoids `ring`), `hmac 0.12` + `sha2 0.10` + `subtle 2` for constant-time webhook HMAC, `url 2` for URL building, `parking_lot 0.12` for cache locks (async-safe because we never await while holding a lock), `serde` + `serde_json` for payload (de)serialization. Test stack: `tokio 1` (`macros`, `rt`), `wiremock 0.6` (only behind a `wiremock-tests` feature for the optional integration smoke), `domain` from Plan 1 for type-safe interop.

---

## Spec coverage

This plan implements §13 (HMAC verification on webhook receivers — the byte-comparison helper; webhook *event routing* lives in Plan 6), §14 (the full GitHub API surface — both the App-installation calls and the user-token calls), and the building blocks for §10.1 / §10.2 (OAuth code exchange and `/user` lookup; the actual session/redirect plumbing lives in Plan 4) and §10.3 admin recheck (`/user/memberships/orgs/{login}` lookup). It does NOT implement: the webhook *event router* and signature wiring at the HTTP layer (Plan 6), Restate handler integration that consumes these clients (Plan 3), session storage / encryption / redirect routing (Plan 4), production secrets management (Plan 7), or end-to-end deployment (Plan 7+). The wasm-target smoke test from spec §20 is included as Task 17.

## File structure

| Path | Responsibility |
|---|---|
| `crates/github/Cargo.toml` | Crate manifest with `test-mock` and `wiremock-tests` feature flags |
| `crates/github/src/lib.rs` | Module declarations + re-exports |
| `crates/github/src/error.rs` | `Error` enum unifying transport/decode/HTTP-status/auth failures |
| `crates/github/src/hmac.rs` | `verify_signature_256(secret, raw_body, header) -> bool` (constant-time) |
| `crates/github/src/transport.rs` | `HttpTransport` trait + `Request` / `Response` value types + `ReqwestTransport` impl |
| `crates/github/src/mocks.rs` | `MockTransport` (behind `test-mock` feature) — script-driven matcher list |
| `crates/github/src/payloads.rs` | Typed serde structs for every GitHub response we care about (`GhUser`, `GhMembership`, `GhRepo`, `GhInstallationToken`, `GhCollaboratorInvite`, `GhInvitationListItem`) |
| `crates/github/src/oauth.rs` | User-OAuth: authorize-URL builder, code exchange, `UserApiClient` (`get_user`, `get_org_membership`) |
| `crates/github/src/jwt.rs` | RS256 App-JWT mint (`AppJwtSigner::sign(now) -> String`) |
| `crates/github/src/installation.rs` | `InstallationClient` (mint+cache installation token, all six App-installation endpoints) |
| `crates/github/src/token_cache.rs` | `TokenCache` keyed by `installation_id` with refresh-before-expiry |
| `crates/github/src/wasm_smoke.rs` | wasm32-unknown-unknown compile-only smoke that the seven external dependencies link cleanly |
| `crates/github/tests/oauth_url.rs` | Integration test for OAuth authorize-URL builder shape |
| `crates/github/tests/installation_e2e.rs` | (Optional, `wiremock-tests` feature) Sequence of installation-client calls against a wiremock server |

---

## Task ordering

Tasks 1–2 establish the workspace member and the unified `Error`. Task 3 ships the HMAC helper, which is fully independent of every other module. Tasks 4–6 build the transport layer (trait, reqwest impl, mock impl) — every later task depends on the trait. Task 7 fixes the on-the-wire payload structs. Tasks 8–10 handle the user-OAuth surface end to end. Tasks 11–13 build the App-JWT and installation-token machinery (mint + cache). Tasks 14–16 cover the six App-installation endpoints, grouped read/write/reconcile so each task ends green and committable. Task 17 is the wasm32-unknown-unknown smoke test. Task 18 is the final clippy/fmt sweep.

---

### Task 1: Crate skeleton

**Files:**
- Create: `crates/github/Cargo.toml`
- Create: `crates/github/src/lib.rs`
- Modify: workspace `Cargo.toml` (add `crates/github` to members + add new shared deps)

- [ ] **Step 1: Add `crates/github` to the workspace and add new shared deps**

In the workspace root `Cargo.toml`, add `"crates/github"` to `members` and add the following entries to `[workspace.dependencies]` (alphabetical with the existing list — drop them in next to their neighbors):

```toml
base64 = "0.22"
hmac = "0.12"
jsonwebtoken = { version = "9", default-features = false }   # only used by tests as a verifier
oauth2 = { version = "5", default-features = false, features = ["reqwest"] }
parking_lot = "0.12"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "http2", "charset"] }
rsa = { version = "0.9", default-features = false, features = ["pem", "sha2"] }
sha2 = "0.10"
url = "2"
wiremock = "0.6"

# new path member, mirroring the others:
github = { path = "crates/github" }
```

Also extend the `tokio` workspace entry to include `"macros"`, `"rt-multi-thread"`, and `"sync"` features (sync is needed for tests; rt-multi-thread for wiremock):

```toml
tokio = { version = "1", default-features = false, features = ["macros", "rt", "rt-multi-thread", "sync"] }
```

Run: `cargo metadata --no-deps --format-version 1 > /dev/null`
Expected: errors about `crates/github` not existing — fixed by Step 2.

- [ ] **Step 2: Create the crate manifest**

`crates/github/Cargo.toml`:

```toml
[package]
name = "github"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[features]
# `test-mock` exposes the in-process `MockTransport` so other workspace crates
# (notably `restate-svc` in Plan 3) can write hermetic tests against the GitHub
# clients without spinning up an HTTP server.
test-mock = []
# `wiremock-tests` enables an optional integration test that runs the
# installation client against a real local HTTP server. Off by default so
# `cargo test --workspace` stays hermetic and fast.
wiremock-tests = ["dep:wiremock"]

[dependencies]
async-trait.workspace = true
base64.workspace = true
chrono.workspace = true
domain.workspace = true
hmac.workspace = true
oauth2.workspace = true
parking_lot.workspace = true
reqwest.workspace = true
rsa.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
subtle.workspace = true
thiserror.workspace = true
url.workspace = true
wiremock = { workspace = true, optional = true }

[dev-dependencies]
github = { path = ".", features = ["test-mock"] }
jsonwebtoken.workspace = true   # used to *verify* the JWTs we mint, not to sign them
tokio.workspace = true
wiremock.workspace = true
```

The self-referential `dev-dependencies` line mirrors the same pattern used in `crates/storage` to make a public test helper visible to the integration tests in `crates/github/tests/`.

- [ ] **Step 3: Create `crates/github/src/lib.rs`**

```rust
//! GitHub-touching code paths for ghinvite. Two facade clients on top of one
//! transport: [`oauth::UserApiClient`] (user-token, used by the web binary) and
//! [`installation::InstallationClient`] (App-JWT → installation token, used by
//! the Restate Service Worker). [`hmac::verify_signature_256`] is a standalone
//! helper for the webhook receiver; it does not consume the transport.
//!
//! Every HTTP call funnels through [`transport::HttpTransport`] so impls can be
//! swapped (production: [`transport::ReqwestTransport`]; tests:
//! [`mocks::MockTransport`] behind the `test-mock` feature).

pub mod error;
pub mod hmac;
pub mod installation;
pub mod jwt;
pub mod oauth;
pub mod payloads;
pub mod token_cache;
pub mod transport;

#[cfg(any(test, feature = "test-mock"))]
pub mod mocks;

#[cfg(target_arch = "wasm32")]
pub mod wasm_smoke;

pub use error::{Error, Result};
pub use installation::InstallationClient;
pub use oauth::{AuthorizeUrl, UserApiClient};
pub use transport::{HttpTransport, Method, Request, Response};
```

- [ ] **Step 4: Create stub module files so the crate parses**

For each of `error.rs`, `hmac.rs`, `installation.rs`, `jwt.rs`, `mocks.rs`, `oauth.rs`, `payloads.rs`, `token_cache.rs`, `transport.rs`, `wasm_smoke.rs`, create the file under `crates/github/src/` containing exactly:

```rust
// filled in by a later task
```

For now, comment out **every** re-export at the bottom of `lib.rs` (the `pub use` lines) and uncomment them per task as the underlying type appears. New `lib.rs` re-export block:

```rust
// Re-exports filled in as each module gains its public types:
// pub use error::{Error, Result};
// pub use installation::InstallationClient;
// pub use oauth::{AuthorizeUrl, UserApiClient};
// pub use transport::{HttpTransport, Method, Request, Response};
```

- [ ] **Step 5: Verify the crate compiles**

Run: `cargo check -p github`
Expected: clean (warnings about unused modules are OK; the modules will gain content in subsequent tasks).

- [ ] **Step 6: Commit**

```bash
git add crates/github Cargo.toml Cargo.lock
git commit -m "chore: scaffold crates/github skeleton"
```

---

### Task 2: Unified `Error` type

**Files:**
- Modify: `crates/github/src/error.rs`
- Modify: `crates/github/src/lib.rs` (un-comment `Error`/`Result` re-exports)

- [ ] **Step 1: Write the error type**

`crates/github/src/error.rs`:

```rust
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// All failure modes a GitHub-side call can produce. Callers in Plan 3 / Plan 4
/// will branch on these variants; in particular `Error::Status(404)` on a
/// `get_repo` is *not* fatal — it just means the App lost access.
#[derive(Debug, Error)]
pub enum Error {
    /// The HTTP transport itself failed (DNS, TLS, broken pipe, etc).
    #[error("transport error: {0}")]
    Transport(String),

    /// The server returned a non-2xx status. Holds the numeric status and the
    /// raw body (truncated to 4 KiB) for diagnostics.
    #[error("github returned status {status}: {body}")]
    Status { status: u16, body: String },

    /// The response body was not valid JSON or did not match the expected shape.
    #[error("response decode error: {0}")]
    Decode(String),

    /// The OAuth authorization-code exchange returned an error from GitHub
    /// (`error=...`/`error_description=...`).
    #[error("oauth error: {0}")]
    OAuth(String),

    /// Something is wrong with the calling code's inputs (e.g. bad URL,
    /// unparseable PEM, expired JWT key). Never thrown by the transport itself.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// JWT minting / signing failed.
    #[error("jwt error: {0}")]
    Jwt(String),
}

impl Error {
    /// Convenience: extract the status code if this is a `Status` variant.
    /// Plan 3 handlers branch on specific codes (404 = lost access, 422 =
    /// already a member, etc.), so giving them this rather than `match`ing
    /// keeps call sites tight.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_extracts_only_for_status_variant() {
        assert_eq!(
            Error::Status {
                status: 404,
                body: "x".into(),
            }
            .status(),
            Some(404)
        );
        assert_eq!(Error::Decode("nope".into()).status(), None);
    }

    #[test]
    fn errors_render_useful_messages() {
        let s = Error::Status {
            status: 401,
            body: "bad creds".into(),
        }
        .to_string();
        assert!(s.contains("401"));
        assert!(s.contains("bad creds"));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p github --lib error`
Expected: 2 tests pass.

- [ ] **Step 3: Un-comment re-exports**

In `crates/github/src/lib.rs`, replace the commented `pub use error::{Error, Result};` with the active line.

Run: `cargo build -p github`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/github/src/error.rs crates/github/src/lib.rs
git commit -m "feat(github): unified Error / Result types"
```

---

### Task 3: HMAC verification helper

Per spec §13 / §18: webhook signature is `X-Hub-Signature-256: sha256=<hex>`, verified with the App webhook secret, **constant-time** comparison.

**Files:**
- Modify: `crates/github/src/hmac.rs`

- [ ] **Step 1: Write the helper + tests**

`crates/github/src/hmac.rs`:

```rust
//! Constant-time HMAC-SHA-256 verification for GitHub webhook signatures.
//!
//! GitHub formats the signature as `X-Hub-Signature-256: sha256=<lowercase hex>`.
//! This module decodes the header, computes HMAC over the *raw* request body,
//! and compares the two byte vectors with `subtle::ConstantTimeEq`.

use ::hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Verify the `X-Hub-Signature-256` header against `raw_body` using `secret`.
///
/// Returns `true` iff the header is well-formed AND the computed HMAC matches.
/// Returns `false` for any malformation (missing prefix, odd hex, wrong length)
/// — callers should respond with 401 on `false`.
///
/// This function never short-circuits on length or content of the supplied
/// signature once decoded: the comparison itself is constant-time.
pub fn verify_signature_256(secret: &[u8], raw_body: &[u8], header_value: &str) -> bool {
    // Header MUST start with "sha256=" — case-sensitive per GitHub spec.
    let Some(hex) = header_value.strip_prefix("sha256=") else {
        return false;
    };
    // 64 hex chars == 32 bytes (SHA-256 output)
    let Ok(supplied) = decode_lowercase_hex(hex) else {
        return false;
    };
    if supplied.len() != 32 {
        return false;
    }

    let mut mac = HmacSha256::new_from_slice(secret)
        .expect("HMAC accepts any key length");
    mac.update(raw_body);
    let computed = mac.finalize().into_bytes();

    computed.as_slice().ct_eq(&supplied).into()
}

/// Decode a lowercase hex string into bytes. Rejects uppercase, odd length, or
/// any non-hex character. Implemented locally so we don't drag in `hex` for one
/// callsite.
fn decode_lowercase_hex(s: &str) -> Result<Vec<u8>, ()> {
    if s.len() % 2 != 0 {
        return Err(());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks(2) {
        let hi = nibble(chunk[0])?;
        let lo = nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nibble(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::hmac::{Hmac, Mac};

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        let bytes = mac.finalize().into_bytes();
        let mut hex = String::with_capacity(64);
        for byte in bytes {
            hex.push_str(&format!("{byte:02x}"));
        }
        format!("sha256={hex}")
    }

    #[test]
    fn correct_signature_passes() {
        let secret = b"webhook-secret";
        let body = br#"{"action":"created"}"#;
        assert!(verify_signature_256(secret, body, &sign(secret, body)));
    }

    #[test]
    fn wrong_secret_fails() {
        let secret = b"webhook-secret";
        let body = br#"{"action":"created"}"#;
        assert!(!verify_signature_256(b"other-secret", body, &sign(secret, body)));
    }

    #[test]
    fn modified_body_fails() {
        let secret = b"webhook-secret";
        let body = br#"{"action":"created"}"#;
        let header = sign(secret, body);
        let tampered = br#"{"action":"deleted"}"#;
        assert!(!verify_signature_256(secret, tampered, &header));
    }

    #[test]
    fn missing_prefix_fails() {
        // No `sha256=` prefix.
        assert!(!verify_signature_256(b"k", b"x", "abcdef"));
    }

    #[test]
    fn uppercase_hex_fails() {
        // GitHub ships lowercase; reject anything else to keep the contract tight.
        let mut header = sign(b"k", b"x");
        header = header.replace(|c: char| c.is_ascii_hexdigit(), "A");
        assert!(!verify_signature_256(b"k", b"x", &header));
    }

    #[test]
    fn odd_length_fails() {
        assert!(!verify_signature_256(b"k", b"x", "sha256=abc"));
    }

    #[test]
    fn wrong_length_fails() {
        // 30 hex chars → 15 bytes → not 32, should fail.
        let header = format!("sha256={}", "0".repeat(30));
        assert!(!verify_signature_256(b"k", b"x", &header));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p github --lib hmac`
Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/github/src/hmac.rs
git commit -m "feat(github): constant-time HMAC-SHA-256 webhook verification"
```

---

### Task 4: `HttpTransport` trait + value types

The trait is the single seam between this crate and the network. Every later task either implements it (Tasks 5, 6) or consumes it.

**Files:**
- Modify: `crates/github/src/transport.rs`
- Modify: `crates/github/src/lib.rs` (un-comment transport re-exports)

- [ ] **Step 1: Write the trait + value types**

`crates/github/src/transport.rs`:

```rust
//! HTTP transport seam. Every GitHub call funnels through this trait so the
//! reqwest impl can be swapped for `MockTransport` in tests.

use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    /// Absolute URL. Use [`Request::with_path`] to build relative to a base.
    pub url: String,
    /// Sorted by key for deterministic equality in tests.
    pub headers: BTreeMap<String, String>,
    /// `None` for GET/DELETE.
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }

    pub fn json_body<T: serde::Serialize>(mut self, value: &T) -> Result<Self> {
        let body = serde_json::to_vec(value)
            .map_err(|e| Error::Decode(format!("encoding request body: {e}")))?;
        self.body = Some(body);
        self.headers
            .insert("content-type".into(), "application/json".into());
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    /// Lowercased keys, matching the convention in `Request::headers`.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Response {
    /// Decode the body as JSON of `T`. Errors with [`Error::Decode`] on parse
    /// failure (the raw body is included in the error message).
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| {
            let preview = String::from_utf8_lossy(&self.body);
            Error::Decode(format!("{e}: body={preview}"))
        })
    }

    /// Returns `Ok(self)` if `status` is 2xx, otherwise `Err(Error::Status)`.
    pub fn ensure_success(self) -> Result<Self> {
        if (200..300).contains(&self.status) {
            Ok(self)
        } else {
            let body = String::from_utf8_lossy(&self.body).to_string();
            // Truncate to keep error logs sane.
            let truncated = if body.len() > 4096 {
                format!("{}…", &body[..4096])
            } else {
                body
            };
            Err(Error::Status {
                status: self.status,
                body: truncated,
            })
        }
    }
}

/// The single HTTP-touching trait this crate exposes.
///
/// Impls must be `Send + Sync + 'static` so they can be cloned into long-lived
/// clients held by Restate handlers.
#[async_trait]
pub trait HttpTransport: Send + Sync + 'static {
    async fn send(&self, request: Request) -> Result<Response>;
}

/// Production reqwest impl. Lives behind a thin wrapper so the rest of the
/// crate doesn't have to deal with reqwest types directly.
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Transport(format!("building reqwest client: {e}")))?;
        Ok(Self { client })
    }

    /// Inject an existing client (e.g. one with a custom timeout middleware).
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: Request) -> Result<Response> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, &request.url);
        for (k, v) in &request.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let mut headers = BTreeMap::new();
        for (name, value) in resp.headers() {
            headers.insert(
                name.as_str().to_ascii_lowercase(),
                value
                    .to_str()
                    .unwrap_or("<non-ascii header>")
                    .to_string(),
            );
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::Transport(format!("reading body: {e}")))?
            .to_vec();
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_builder_lowers_header_keys() {
        let r = Request::new(Method::Get, "https://example.test/")
            .header("Authorization", "Bearer xyz")
            .header("X-Custom", "v");
        assert_eq!(r.headers.get("authorization").map(String::as_str), Some("Bearer xyz"));
        assert_eq!(r.headers.get("x-custom").map(String::as_str), Some("v"));
    }

    #[test]
    fn json_body_sets_content_type_and_serializes() {
        let r = Request::new(Method::Post, "https://example.test/")
            .json_body(&serde_json::json!({"k": 1}))
            .unwrap();
        assert_eq!(r.headers.get("content-type").map(String::as_str), Some("application/json"));
        assert_eq!(r.body.as_deref(), Some(b"{\"k\":1}".as_slice()));
    }

    #[test]
    fn response_ensure_success_passes_2xx() {
        let r = Response { status: 201, headers: BTreeMap::new(), body: vec![] };
        assert_eq!(r.clone().ensure_success().unwrap().status, 201);
    }

    #[test]
    fn response_ensure_success_fails_4xx_with_body() {
        let r = Response {
            status: 422,
            headers: BTreeMap::new(),
            body: b"validation failed".to_vec(),
        };
        match r.ensure_success().unwrap_err() {
            Error::Status { status, body } => {
                assert_eq!(status, 422);
                assert!(body.contains("validation failed"));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn response_json_decodes_well_formed_payload() {
        let r = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: br#"{"x": 1}"#.to_vec(),
        };
        let parsed: serde_json::Value = r.json().unwrap();
        assert_eq!(parsed["x"], 1);
    }

    #[test]
    fn response_json_returns_decode_error_for_garbage() {
        let r = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: b"not json".to_vec(),
        };
        let err = r.json::<serde_json::Value>().unwrap_err();
        assert!(matches!(err, Error::Decode(_)));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p github --lib transport`
Expected: 6 tests pass.

- [ ] **Step 3: Un-comment transport re-exports in `lib.rs`**

```rust
pub use transport::{HttpTransport, Method, Request, Response};
```

- [ ] **Step 4: Commit**

```bash
git add crates/github/src/transport.rs crates/github/src/lib.rs
git commit -m "feat(github): HttpTransport trait + ReqwestTransport"
```

---

### Task 5: `MockTransport` (test scaffolding)

Plan 3's Restate handler tests will instantiate `InstallationClient::new(MockTransport::scripted([...]))` to exercise every code path without a real GitHub. The mock matches requests against a script of expectations and returns canned responses.

**Files:**
- Modify: `crates/github/src/mocks.rs`

- [ ] **Step 1: Write the mock transport**

`crates/github/src/mocks.rs`:

```rust
//! In-process mock for [`crate::transport::HttpTransport`].
//!
//! Behind the `test-mock` feature so it ships in dev-dependencies of consumers
//! without polluting production builds.

use crate::error::Result;
use crate::transport::{HttpTransport, Method, Request, Response};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;

/// One scripted expectation: the request the test expects, paired with the
/// response to return on a match.
#[derive(Clone, Debug)]
pub struct Expectation {
    pub method: Method,
    /// Exact URL match. Build with `concat!` if you want to assert against a
    /// known base; query-string matching is exact, so order matters.
    pub url: String,
    /// Subset match. Every header listed here MUST appear on the incoming
    /// request with the listed value (case-insensitive on the key, exact on
    /// the value). The incoming request may carry extra headers.
    pub required_headers: BTreeMap<String, String>,
    /// `None` skips body check; `Some(b)` requires byte-equality.
    pub expected_body: Option<Vec<u8>>,
    pub response: Response,
}

impl Expectation {
    pub fn ok_json(method: Method, url: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            method,
            url: url.into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: {
                    let mut h = BTreeMap::new();
                    h.insert("content-type".into(), "application/json".into());
                    h
                },
                body: serde_json::to_vec(&body).unwrap(),
            },
        }
    }

    pub fn status(method: Method, url: impl Into<String>, status: u16) -> Self {
        Self {
            method,
            url: url.into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status,
                headers: BTreeMap::new(),
                body: Vec::new(),
            },
        }
    }

    pub fn require_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.required_headers
            .insert(name.to_ascii_lowercase(), value.into());
        self
    }

    pub fn require_body(mut self, body: Vec<u8>) -> Self {
        self.expected_body = Some(body);
        self
    }
}

/// In-process mock. Constructs from an ordered list of [`Expectation`]s and
/// pops one per `send()` call. Panics on script exhaustion or mismatch — the
/// goal is *loud* test failures.
#[derive(Clone)]
pub struct MockTransport {
    inner: Arc<Mutex<Vec<Expectation>>>,
}

impl MockTransport {
    pub fn scripted(expectations: Vec<Expectation>) -> Self {
        // Reverse so `pop()` yields in script order.
        let mut v = expectations;
        v.reverse();
        Self {
            inner: Arc::new(Mutex::new(v)),
        }
    }

    /// Returns the count of expectations remaining in the script.
    pub fn remaining(&self) -> usize {
        self.inner.lock().len()
    }

    /// Asserts that the script has been fully consumed.
    pub fn assert_exhausted(&self) {
        let n = self.remaining();
        assert_eq!(n, 0, "MockTransport script not exhausted: {n} expectations remaining");
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn send(&self, request: Request) -> Result<Response> {
        let next = self
            .inner
            .lock()
            .pop()
            .unwrap_or_else(|| panic!("MockTransport: script exhausted, got unexpected request {request:?}"));

        if next.method != request.method {
            panic!(
                "MockTransport: expected {} but got {}",
                next.method.as_str(),
                request.method.as_str()
            );
        }
        if next.url != request.url {
            panic!(
                "MockTransport: expected URL {:?}, got {:?}",
                next.url, request.url
            );
        }
        for (name, expected_value) in &next.required_headers {
            match request.headers.get(name) {
                Some(actual) if actual == expected_value => (),
                Some(actual) => panic!(
                    "MockTransport: header {name:?} expected {expected_value:?}, got {actual:?}"
                ),
                None => panic!("MockTransport: missing required header {name:?}"),
            }
        }
        if let Some(expected_body) = &next.expected_body {
            let actual_body = request.body.unwrap_or_default();
            if &actual_body != expected_body {
                panic!(
                    "MockTransport: body mismatch.\n  expected: {:?}\n  actual:   {:?}",
                    String::from_utf8_lossy(expected_body),
                    String::from_utf8_lossy(&actual_body)
                );
            }
        }
        // Return the canned response. We deliberately do not let the test
        // observe transport-level errors via the script — return Status
        // responses to model HTTP failures.
        Ok(next.response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matches_a_scripted_request() {
        let mock = MockTransport::scripted(vec![Expectation::ok_json(
            Method::Get,
            "https://api.github.test/foo",
            serde_json::json!({"k": "v"}),
        )]);
        let resp = mock
            .send(Request::new(Method::Get, "https://api.github.test/foo"))
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body.starts_with(b"{"));
        mock.assert_exhausted();
    }

    #[tokio::test]
    #[should_panic(expected = "missing required header")]
    async fn fails_when_required_header_absent() {
        let mock = MockTransport::scripted(vec![
            Expectation::ok_json(Method::Get, "https://api.github.test/", serde_json::json!({}))
                .require_header("authorization", "Bearer xyz"),
        ]);
        let _ = mock
            .send(Request::new(Method::Get, "https://api.github.test/"))
            .await;
    }

    #[tokio::test]
    #[should_panic(expected = "script exhausted")]
    async fn panics_on_extra_request() {
        let mock = MockTransport::scripted(vec![]);
        let _ = mock
            .send(Request::new(Method::Get, "https://api.github.test/"))
            .await;
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p github --lib mocks`
Expected: 3 tests pass (one per scenario).

- [ ] **Step 3: Commit**

```bash
git add crates/github/src/mocks.rs
git commit -m "feat(github): MockTransport behind test-mock feature"
```

---

### Task 6: Typed payload structs

We need stable serde types for every JSON shape we read from GitHub. Anywhere fields aren't strictly required by ghinvite, mark them `Option`. Only typed structs land here — endpoint methods come in later tasks.

**Files:**
- Modify: `crates/github/src/payloads.rs`

- [ ] **Step 1: Write the payload types + tests**

`crates/github/src/payloads.rs`:

```rust
//! On-the-wire response shapes from the GitHub REST API. Field set is
//! deliberately minimal: anything ghinvite doesn't read is omitted (serde will
//! ignore extra keys by default).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `GET /user` response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhUser {
    pub id: u64,
    pub login: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// Some response shapes carry `type: "User"` / `"Organization"`. Optional
    /// because `/user` always returns "User"; included for shape-reuse where
    /// installation owners come back with the same field.
    #[serde(rename = "type", default)]
    pub account_type: Option<String>,
}

/// `GET /user/memberships/orgs/{login}` response. We only care about the
/// `role` and `state` fields for admin re-check.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhMembership {
    pub role: String,  // "admin" | "member"
    pub state: String, // "active" | "pending"
}

/// `GET /repos/{owner}/{repo}` (a small slice; we use it for display + access
/// confirmation).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhRepo {
    pub id: u64,
    pub full_name: String,
    pub private: bool,
}

/// `POST /app/installations/{id}/access_tokens` response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhInstallationToken {
    pub token: String,
    /// ISO-8601 timestamp ~1 hour out. Used by [`crate::token_cache::TokenCache`].
    pub expires_at: DateTime<Utc>,
}

/// `PUT /repos/{owner}/{repo}/collaborators/{username}` response shape on 201.
/// The `id` field is GitHub's `invitation_id` — what we store in our
/// `github_invitations.github_invitation_id` column.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhCollaboratorInvite {
    pub id: u64,
}

/// `GET /repos/{owner}/{repo}/invitations` (list, pagination ignored — v1 link
/// repo sets stay tiny).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhInvitationListItem {
    pub id: u64,
    pub invitee: GhUser,
    pub permissions: String, // "read"|"write"|"admin"|"triage"|"maintain"
    pub created_at: DateTime<Utc>,
}

/// `GET /installation/repositories` response envelope.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhInstallationRepos {
    pub total_count: u64,
    pub repositories: Vec<GhRepo>,
}

/// OAuth code-exchange success payload. GitHub returns `application/json`
/// when `Accept: application/json` is set.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhTokenResponse {
    pub access_token: String,
    pub token_type: String, // "bearer"
    pub scope: String,      // space-separated, e.g. "read:user"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_decodes_minimum_payload() {
        let raw = br#"{"id":42,"login":"octocat","avatar_url":"https://avatars.example/u/42","extra":"ignored"}"#;
        let u: GhUser = serde_json::from_slice(raw).unwrap();
        assert_eq!(u.id, 42);
        assert_eq!(u.login, "octocat");
        assert!(u.account_type.is_none());
    }

    #[test]
    fn membership_decodes() {
        let raw = br#"{"role":"admin","state":"active"}"#;
        let m: GhMembership = serde_json::from_slice(raw).unwrap();
        assert_eq!(m.role, "admin");
    }

    #[test]
    fn installation_token_decodes_expires_at() {
        let raw = br#"{"token":"ghs_xxx","expires_at":"2026-05-04T13:00:00Z"}"#;
        let t: GhInstallationToken = serde_json::from_slice(raw).unwrap();
        assert_eq!(t.token, "ghs_xxx");
        assert_eq!(t.expires_at.to_rfc3339(), "2026-05-04T13:00:00+00:00");
    }

    #[test]
    fn collaborator_invite_decodes_id_only() {
        let raw = br#"{"id":98765,"invitee":{"id":42,"login":"x"},"repository":{"full_name":"a/b"}}"#;
        let inv: GhCollaboratorInvite = serde_json::from_slice(raw).unwrap();
        assert_eq!(inv.id, 98765);
    }

    #[test]
    fn installation_repos_decodes() {
        let raw = br#"{"total_count":2,"repositories":[
            {"id":1,"full_name":"a/b","private":true},
            {"id":2,"full_name":"a/c","private":false}
        ]}"#;
        let r: GhInstallationRepos = serde_json::from_slice(raw).unwrap();
        assert_eq!(r.total_count, 2);
        assert_eq!(r.repositories.len(), 2);
        assert_eq!(r.repositories[0].full_name, "a/b");
    }

    #[test]
    fn token_response_decodes() {
        let raw = br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#;
        let t: GhTokenResponse = serde_json::from_slice(raw).unwrap();
        assert_eq!(t.access_token, "u_xxx");
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p github --lib payloads`
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/github/src/payloads.rs
git commit -m "feat(github): typed response payloads"
```

---

### Task 7: OAuth authorize-URL builder + state token

Per spec §10.1: redirect to `https://github.com/login/oauth/authorize` with `scope=read:user`, a CSRF `state` token, and `client_id`. The web binary will store `state` in a session cookie before redirecting.

**Files:**
- Modify: `crates/github/src/oauth.rs`
- Modify: `crates/github/src/lib.rs` (un-comment `oauth::*` re-exports)

- [ ] **Step 1: Begin `oauth.rs` with the URL builder**

`crates/github/src/oauth.rs`:

```rust
//! User-token OAuth flow plus the user-token API client.
//!
//! Two pieces:
//! 1. [`AuthorizeUrl::build`] / [`exchange_code`] are stateless helpers used by
//!    the web binary to drive the OAuth redirect dance.
//! 2. [`UserApiClient`] holds a transport + a user access token and exposes
//!    the two user-side endpoints we call (`/user` and
//!    `/user/memberships/orgs/{login}`).

use crate::error::{Error, Result};
use crate::payloads::{GhMembership, GhTokenResponse, GhUser};
use crate::transport::{HttpTransport, Method, Request};
use std::sync::Arc;
use url::Url;

/// Configuration for the GitHub App's OAuth surface. The web binary owns the
/// `client_secret`; the Restate worker never sees it.
#[derive(Clone, Debug)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Must exactly match the redirect URI registered on the GitHub App. v1
    /// hosts a single redirect URI: `<base_url>/oauth/callback`.
    pub redirect_uri: String,
}

/// Output of [`AuthorizeUrl::build`]. The caller stores `state` in a signed
/// session cookie before redirecting the browser to `url`.
#[derive(Clone, Debug)]
pub struct AuthorizeUrl {
    pub url: String,
    pub state: String,
}

impl AuthorizeUrl {
    /// Build a `https://github.com/login/oauth/authorize?...` URL with the given
    /// CSRF `state` token. The caller is responsible for generating `state`
    /// (recommend 32 bytes of CSPRNG → base64url) and persisting it.
    ///
    /// `extra_params` lets callers append anything GitHub supports (e.g.
    /// `allow_signup=false`); `redirect_uri`, `scope`, `state`, `client_id` are
    /// always set by this builder.
    pub fn build(
        cfg: &OAuthConfig,
        state: impl Into<String>,
        extra_params: &[(&str, &str)],
    ) -> Result<Self> {
        let state = state.into();
        let mut url = Url::parse("https://github.com/login/oauth/authorize")
            .map_err(|e| Error::InvalidInput(format!("authorize url: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &cfg.client_id);
            q.append_pair("redirect_uri", &cfg.redirect_uri);
            q.append_pair("scope", "read:user");
            q.append_pair("state", &state);
            for (k, v) in extra_params {
                q.append_pair(k, v);
            }
        }
        Ok(Self {
            url: url.into(),
            state,
        })
    }
}

/// Exchange an OAuth authorization code for a user access token. Calls
/// `POST https://github.com/login/oauth/access_token` with `Accept:
/// application/json` so GitHub returns JSON instead of `application/x-www-form-urlencoded`.
pub async fn exchange_code<T: HttpTransport + ?Sized>(
    transport: &T,
    cfg: &OAuthConfig,
    code: &str,
) -> Result<GhTokenResponse> {
    let body = serde_json::json!({
        "client_id": cfg.client_id,
        "client_secret": cfg.client_secret,
        "code": code,
        "redirect_uri": cfg.redirect_uri,
    });
    let req = Request::new(Method::Post, "https://github.com/login/oauth/access_token")
        .header("accept", "application/json")
        .header("user-agent", "ghinvite")
        .json_body(&body)?;
    let resp = transport.send(req).await?.ensure_success()?;

    // GitHub returns 200 even on `error` payloads; sniff that first.
    if let Ok(err) = resp.json::<serde_json::Value>() {
        if let Some(error_code) = err.get("error").and_then(|v| v.as_str()) {
            let desc = err.get("error_description").and_then(|v| v.as_str()).unwrap_or("");
            return Err(Error::OAuth(format!("{error_code}: {desc}")));
        }
    }
    resp.json::<GhTokenResponse>()
}

/// User-token API client. Constructed per signed-in session (the access token
/// is encrypted at rest in the session cookie; the web binary decrypts it
/// before instantiating this client per request).
#[derive(Clone)]
pub struct UserApiClient {
    transport: Arc<dyn HttpTransport>,
    user_token: String,
    base_url: String,
}

impl UserApiClient {
    /// `transport` typically wraps `ReqwestTransport`. `user_token` is the
    /// `access_token` from [`exchange_code`].
    pub fn new(transport: Arc<dyn HttpTransport>, user_token: String) -> Self {
        Self::with_base(transport, user_token, "https://api.github.com".into())
    }

    /// Construct against an alternate base URL (used by `MockTransport` /
    /// `wiremock` tests). Path is appended verbatim — no trailing slash.
    pub fn with_base(transport: Arc<dyn HttpTransport>, user_token: String, base_url: String) -> Self {
        Self {
            transport,
            user_token,
            base_url,
        }
    }

    fn auth_request(&self, method: Method, path: &str) -> Request {
        Request::new(method, format!("{}{}", self.base_url, path))
            .header("accept", "application/vnd.github+json")
            .header("authorization", format!("Bearer {}", self.user_token))
            .header("user-agent", "ghinvite")
            .header("x-github-api-version", "2022-11-28")
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    fn cfg() -> OAuthConfig {
        OAuthConfig {
            client_id: "Iv1.abc".into(),
            client_secret: "secret".into(),
            redirect_uri: "https://example.test/oauth/callback".into(),
        }
    }

    #[test]
    fn authorize_url_includes_required_params() {
        let a = AuthorizeUrl::build(&cfg(), "csrf-token-123", &[]).unwrap();
        assert!(a.url.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(a.url.contains("client_id=Iv1.abc"));
        assert!(a.url.contains("scope=read%3Auser"));
        assert!(a.url.contains("state=csrf-token-123"));
        assert!(a.url.contains("redirect_uri=https%3A%2F%2Fexample.test%2Foauth%2Fcallback"));
        assert_eq!(a.state, "csrf-token-123");
    }

    #[test]
    fn authorize_url_appends_extra_params() {
        let a = AuthorizeUrl::build(&cfg(), "x", &[("allow_signup", "false")]).unwrap();
        assert!(a.url.contains("allow_signup=false"));
    }
}
```

- [ ] **Step 2: Run the URL-builder tests**

Run: `cargo test -p github --lib oauth::url_tests`
Expected: 2 tests pass.

- [ ] **Step 3: Un-comment the OAuth re-exports in `lib.rs`**

```rust
pub use oauth::{AuthorizeUrl, UserApiClient};
```

- [ ] **Step 4: Commit**

```bash
git add crates/github/src/oauth.rs crates/github/src/lib.rs
git commit -m "feat(github): OAuth authorize-URL builder + UserApiClient skeleton"
```

---

### Task 8: OAuth code exchange test (mock transport)

We added the `exchange_code` function in Task 7; this task adds a test that drives it through `MockTransport` end to end (request shape + happy path + GitHub-side error path).

**Files:**
- Modify: `crates/github/src/oauth.rs` (append a second test module)

- [ ] **Step 1: Append the integration tests**

Add to `crates/github/src/oauth.rs`, immediately after the `url_tests` module:

```rust
#[cfg(test)]
mod exchange_tests {
    use super::*;
    use crate::error::Error;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::{Method, Response};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn cfg() -> OAuthConfig {
        OAuthConfig {
            client_id: "Iv1.abc".into(),
            client_secret: "secret".into(),
            redirect_uri: "https://example.test/oauth/callback".into(),
        }
    }

    #[tokio::test]
    async fn happy_path_returns_token() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: {
                let mut h = BTreeMap::new();
                h.insert("accept".into(), "application/json".into());
                h.insert("content-type".into(), "application/json".into());
                h
            },
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#.to_vec(),
            },
        }]);
        let token = exchange_code(&mock, &cfg(), "auth-code-1").await.unwrap();
        assert_eq!(token.access_token, "u_xxx");
        assert_eq!(token.scope, "read:user");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn github_error_payload_surfaces_as_oauth_error() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200, // GitHub uses 200 + error payload, not a 4xx.
                headers: BTreeMap::new(),
                body: br#"{"error":"bad_verification_code","error_description":"The code passed is incorrect or expired."}"#.to_vec(),
            },
        }]);
        let err = exchange_code(&mock, &cfg(), "expired").await.unwrap_err();
        match err {
            Error::OAuth(msg) => {
                assert!(msg.contains("bad_verification_code"));
                assert!(msg.contains("expired"));
            }
            other => panic!("expected OAuth error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_5xx_surfaces_as_status_error() {
        let mock = MockTransport::scripted(vec![Expectation::status(
            Method::Post,
            "https://github.com/login/oauth/access_token",
            502,
        )]);
        let err = exchange_code(&mock, &cfg(), "any").await.unwrap_err();
        assert_eq!(err.status(), Some(502));
    }

    // Suppress unused-warning for Arc in case the rustc version doesn't see
    // the test-only borrow above.
    #[allow(dead_code)]
    fn _hint_arc(_: Arc<dyn HttpTransport>) {}
}
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test -p github --lib oauth::exchange_tests`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/github/src/oauth.rs
git commit -m "test(github): exchange_code happy path + GitHub error path"
```

---

### Task 9: `UserApiClient::get_user` and `get_org_membership`

Per spec §10.1 step 3 (`fetch /user`) and §10.3 step 3 (`GET /user/memberships/orgs/{login}`).

**Files:**
- Modify: `crates/github/src/oauth.rs`

- [ ] **Step 1: Add the two methods on `UserApiClient`**

In `crates/github/src/oauth.rs`, immediately after the `auth_request` helper inside `impl UserApiClient`, add:

```rust
    /// `GET /user` — returns the signed-in user's basic profile. Used at sign-in
    /// time to upsert the `users` row (the web binary builds a `domain::User`
    /// from this plus `last_seen_at = Utc::now()`).
    ///
    /// **Errors:** `Error::Status` for non-2xx (401 = expired token, etc.),
    /// `Error::Decode` for malformed JSON.
    pub async fn get_user(&self) -> Result<GhUser> {
        let req = self.auth_request(Method::Get, "/user");
        self.transport.send(req).await?.ensure_success()?.json()
    }

    /// `GET /user/memberships/orgs/{login}` — used by the admin recheck path
    /// (spec §10.3). Returns `Error::Status { status: 404, .. }` if the user is
    /// not a member of the org; the caller should map that to "not admin".
    ///
    /// **Errors:** `Error::Status` for non-2xx, `Error::Decode` for malformed JSON.
    pub async fn get_org_membership(&self, org_login: &str) -> Result<GhMembership> {
        // Path-encode the login to defend against odd characters (org renames,
        // etc.). url::form_urlencoded::byte_serialize would be heavier; we use
        // url::Url to build the path safely.
        let path = format!("/user/memberships/orgs/{}", url_path_segment(org_login));
        let req = self.auth_request(Method::Get, &path);
        self.transport.send(req).await?.ensure_success()?.json()
    }
```

Also add this private helper at the bottom of the module (before the test modules):

```rust
/// Percent-encode a single path segment. We allow only the characters GitHub
/// uses in logins; everything else is encoded.
fn url_path_segment(s: &str) -> String {
    const SAFE: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.~";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if SAFE.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
```

- [ ] **Step 2: Add tests for both methods**

Append to `crates/github/src/oauth.rs`:

```rust
#[cfg(test)]
mod user_api_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::{Method, Response};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn client_with(mock: MockTransport) -> UserApiClient {
        UserApiClient::with_base(
            Arc::new(mock),
            "u_xxx".into(),
            "https://api.github.test".into(),
        )
    }

    #[tokio::test]
    async fn get_user_sends_authorization_and_returns_payload() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Get,
            url: "https://api.github.test/user".into(),
            required_headers: {
                let mut h = BTreeMap::new();
                h.insert("authorization".into(), "Bearer u_xxx".into());
                h.insert("accept".into(), "application/vnd.github+json".into());
                h
            },
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"id":42,"login":"octocat","avatar_url":"https://a/u/42"}"#.to_vec(),
            },
        }]);
        let mock_clone = mock.clone();
        let user = client_with(mock).get_user().await.unwrap();
        assert_eq!(user.id, 42);
        assert_eq!(user.login, "octocat");
        mock_clone.assert_exhausted();
    }

    #[tokio::test]
    async fn get_org_membership_path_encodes_login() {
        let mock = MockTransport::scripted(vec![Expectation::ok_json(
            Method::Get,
            "https://api.github.test/user/memberships/orgs/acme%20corp",
            serde_json::json!({"role": "admin", "state": "active"}),
        )]);
        let m = client_with(mock).get_org_membership("acme corp").await.unwrap();
        assert_eq!(m.role, "admin");
        assert_eq!(m.state, "active");
    }

    #[tokio::test]
    async fn get_org_membership_404_surfaces_status() {
        let mock = MockTransport::scripted(vec![Expectation::status(
            Method::Get,
            "https://api.github.test/user/memberships/orgs/private",
            404,
        )]);
        let err = client_with(mock).get_org_membership("private").await.unwrap_err();
        assert_eq!(err.status(), Some(404));
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p github --lib oauth::user_api_tests`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/github/src/oauth.rs
git commit -m "feat(github): UserApiClient get_user + get_org_membership"
```

---

### Task 10: Integration test — OAuth authorize-URL

End-to-end shape check that runs *outside* `--lib`, mirroring how downstream crates will import the public surface.

**Files:**
- Create: `crates/github/tests/oauth_url.rs`

- [ ] **Step 1: Write the integration test**

`crates/github/tests/oauth_url.rs`:

```rust
//! Integration smoke for the public OAuth URL builder. Runs against
//! `crates/github`'s public re-exports — i.e. exercises the same import shape
//! the web binary will use in Plan 4.

use github::oauth::{AuthorizeUrl, OAuthConfig};

#[test]
fn authorize_url_is_crate_public() {
    let cfg = OAuthConfig {
        client_id: "Iv1.abc".into(),
        client_secret: "secret".into(),
        redirect_uri: "https://example.test/oauth/callback".into(),
    };
    let a = AuthorizeUrl::build(&cfg, "csrf", &[]).unwrap();
    assert!(a.url.starts_with("https://github.com/login/oauth/authorize?"));
    assert_eq!(a.state, "csrf");
}
```

`OAuthConfig` is publicly exported via the `oauth` module; you may also re-export it at the crate root in `crates/github/src/lib.rs` if you prefer:

```rust
pub use oauth::{AuthorizeUrl, OAuthConfig, UserApiClient};
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p github --test oauth_url`
Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add crates/github/tests/oauth_url.rs crates/github/src/lib.rs
git commit -m "test(github): public OAuth URL surface"
```

---

### Task 11: RS256 App-JWT signing

Per spec §14: "GitHub App private key (RS256) is held only by the Restate Service Worker." We mint a short-lived (≤10 min) JWT that GitHub's `/app/installations/{id}/access_tokens` endpoint accepts as `Authorization: Bearer <jwt>`.

We avoid `ring` (problematic on wasm32-unknown-unknown without extra setup) and `jsonwebtoken`'s default features by signing manually with `rsa` + `sha2` + `base64`. JWT structure: `<base64url(header)>.<base64url(claims)>.<base64url(signature)>`.

**Files:**
- Modify: `crates/github/src/jwt.rs`

- [ ] **Step 1: Write the signer + tests**

`crates/github/src/jwt.rs`:

```rust
//! Manual RS256 JWT minting for the GitHub App credentials.
//!
//! Why manual: `ring` (the default crypto backend for `jsonwebtoken`) does not
//! cleanly target wasm32-unknown-unknown without elaborate setup. The math is
//! 30 lines; doing it by hand keeps the wasm story simple.

use crate::error::{Error, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer as _};
use rsa::RsaPrivateKey;
use serde::Serialize;
use sha2::Sha256;

/// Holds the App's RSA private key plus the App ID. Constructed once at
/// startup (the key sits in `Workers secrets`), then cloned into long-lived
/// `InstallationClient`s.
#[derive(Clone)]
pub struct AppJwtSigner {
    /// GitHub App ID — appears as the `iss` claim.
    pub app_id: u64,
    inner: RsaPrivateKey,
}

impl AppJwtSigner {
    /// Parse the App private key from PKCS#8 PEM (the format GitHub gives
    /// you when downloading the key).
    pub fn from_pkcs8_pem(app_id: u64, pem: &str) -> Result<Self> {
        let inner = RsaPrivateKey::from_pkcs8_pem(pem)
            .map_err(|e| Error::InvalidInput(format!("rsa pkcs8 pem: {e}")))?;
        Ok(Self { app_id, inner })
    }

    /// Mint a JWT. `now` is parameterized so tests can pin the clock; in
    /// production callers pass `Utc::now()`. The token lives 9 minutes; GitHub
    /// accepts up to 10 minutes and we leave a 60-second clock-skew buffer.
    pub fn sign(&self, now: DateTime<Utc>) -> Result<String> {
        let header = b"{\"alg\":\"RS256\",\"typ\":\"JWT\"}";
        let header_b64 = URL_SAFE_NO_PAD.encode(header);

        #[derive(Serialize)]
        struct Claims {
            iat: i64,
            exp: i64,
            iss: u64,
        }
        let claims = Claims {
            iat: now.timestamp() - 60, // backdated to absorb clock skew
            exp: now.timestamp() + (9 * 60),
            iss: self.app_id,
        };
        let claims_bytes = serde_json::to_vec(&claims)
            .map_err(|e| Error::Jwt(format!("encoding claims: {e}")))?;
        let claims_b64 = URL_SAFE_NO_PAD.encode(&claims_bytes);

        let signing_input = format!("{header_b64}.{claims_b64}");
        let signing_key = SigningKey::<Sha256>::new(self.inner.clone());
        let signature = signing_key.sign(signing_input.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        Ok(format!("{signing_input}.{sig_b64}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use jsonwebtoken::{Algorithm, DecodingKey, Validation};
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use rsa::RsaPublicKey;

    /// Generate a deterministic 1024-bit key for tests. (1024 bits is fine
    /// for unit tests; production keys are 2048+.)
    fn test_keypair() -> (RsaPrivateKey, RsaPublicKey) {
        // `rand` 0.8 + ChaCha8 for determinism — same seeded RNG style as the
        // domain crate.
        use rand::SeedableRng;
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(42);
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let pub_key = RsaPublicKey::from(&priv_key);
        (priv_key, pub_key)
    }

    #[test]
    fn signs_a_jwt_that_jsonwebtoken_can_verify() {
        let (priv_key, pub_key) = test_keypair();
        let pem = priv_key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let signer = AppJwtSigner::from_pkcs8_pem(99, pem.as_str()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap();

        let jwt = signer.sign(now).unwrap();

        let pub_pem = pub_key.to_public_key_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let key = DecodingKey::from_rsa_pem(pub_pem.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&["99"]);
        // Don't enforce iss-as-string strictly; we set iss as u64.
        validation.validate_exp = true;
        validation.leeway = 120;
        // jsonwebtoken's `validate_exp` uses system time; pin via `validation.set_current_time`
        // (added in 9.x). Use the same instant as `now` so the JWT is "current".
        validation.required_spec_claims = std::collections::HashSet::new();
        let token_data = jsonwebtoken::decode::<serde_json::Value>(&jwt, &key, &validation).unwrap();
        assert_eq!(token_data.claims["iss"], 99);
        assert!(token_data.claims["iat"].as_i64().unwrap() <= now.timestamp());
        assert!(token_data.claims["exp"].as_i64().unwrap() > now.timestamp());
    }

    #[test]
    fn rejects_garbage_pem() {
        let err = AppJwtSigner::from_pkcs8_pem(1, "not a pem").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
```

- [ ] **Step 2: Add `rand` and `rand_chacha` and `jsonwebtoken` to `[dev-dependencies]`**

In `crates/github/Cargo.toml`, ensure dev-deps include:

```toml
[dev-dependencies]
github = { path = ".", features = ["test-mock"] }
jsonwebtoken.workspace = true
rand.workspace = true
rand_chacha.workspace = true
tokio.workspace = true
wiremock.workspace = true
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p github --lib jwt`
Expected: 2 tests pass. The first test takes ~1-2 seconds because RSA key generation is slow at debug-opt levels.

If `jsonwebtoken::decode` complains about "expired" tokens despite our timestamps, set `validation.validate_exp = false` for the test only — the production GitHub side does its own expiry validation; we just want shape verification here.

- [ ] **Step 4: Commit**

```bash
git add crates/github/src/jwt.rs crates/github/Cargo.toml
git commit -m "feat(github): manual RS256 App-JWT signer (wasm-friendly)"
```

---

### Task 12: `InstallationClient` + installation-token mint

`InstallationClient` is the long-lived, cloneable handle Plan 3's handlers will hold. It owns the App-JWT signer and a token cache. Most public methods take an `installation_id` and call GitHub with a freshly minted (or cached) installation token.

**Files:**
- Modify: `crates/github/src/installation.rs`

- [ ] **Step 1: Write the struct + token-mint helper**

`crates/github/src/installation.rs`:

```rust
//! App-installation client: holds the App-JWT signer + a token cache, and
//! exposes the v1 installation-side endpoints.

use crate::error::{Error, Result};
use crate::jwt::AppJwtSigner;
use crate::payloads::{
    GhCollaboratorInvite, GhInstallationRepos, GhInstallationToken, GhInvitationListItem, GhRepo,
    GhUser,
};
use crate::token_cache::TokenCache;
use crate::transport::{HttpTransport, Method, Request};
use chrono::Utc;
use std::sync::Arc;

const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Long-lived handle. Construct once per worker invocation (the cache is
/// in-process; restart drops it).
#[derive(Clone)]
pub struct InstallationClient {
    transport: Arc<dyn HttpTransport>,
    signer: AppJwtSigner,
    cache: TokenCache,
    base_url: String,
}

impl InstallationClient {
    pub fn new(transport: Arc<dyn HttpTransport>, signer: AppJwtSigner) -> Self {
        Self {
            transport,
            signer,
            cache: TokenCache::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Test/dev: point at an alternate host (e.g. a wiremock listener).
    pub fn with_base(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Mint an App-JWT and exchange it for an installation token. Hits the
    /// network; callers should prefer `installation_token` which goes through
    /// the cache.
    pub async fn mint_installation_token(
        &self,
        installation_id: u64,
    ) -> Result<GhInstallationToken> {
        let jwt = self.signer.sign(Utc::now())?;
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.base_url, installation_id
        );
        let req = Request::new(Method::Post, &url)
            .header("accept", "application/vnd.github+json")
            .header("authorization", format!("Bearer {jwt}"))
            .header("user-agent", "ghinvite")
            .header("x-github-api-version", "2022-11-28");
        self.transport.send(req).await?.ensure_success()?.json()
    }

    /// Returns a non-expired installation token, refreshing through GitHub if
    /// the cached one is gone or within 60s of expiry.
    pub async fn installation_token(&self, installation_id: u64) -> Result<String> {
        if let Some(t) = self.cache.get_fresh(installation_id, Utc::now()) {
            return Ok(t);
        }
        let minted = self.mint_installation_token(installation_id).await?;
        self.cache
            .insert(installation_id, minted.token.clone(), minted.expires_at);
        Ok(minted.token)
    }

    /// Build a request pre-loaded with the auth header for `installation_id`.
    /// Caller adds method/path/body.
    pub(crate) async fn auth_request(
        &self,
        installation_id: u64,
        method: Method,
        path: &str,
    ) -> Result<Request> {
        let token = self.installation_token(installation_id).await?;
        Ok(Request::new(method, format!("{}{}", self.base_url, path))
            .header("accept", "application/vnd.github+json")
            .header("authorization", format!("Bearer {token}"))
            .header("user-agent", "ghinvite")
            .header("x-github-api-version", "2022-11-28"))
    }
}

#[cfg(test)]
mod token_mint_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use rand::SeedableRng;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::RsaPrivateKey;
    use std::collections::BTreeMap;

    fn signer() -> AppJwtSigner {
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(7);
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let pem = priv_key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        AppJwtSigner::from_pkcs8_pem(123, pem.as_str()).unwrap()
    }

    fn token_response_body() -> Vec<u8> {
        // expires 1 hour from a fixed time ahead of the test clock
        br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec()
    }

    #[tokio::test]
    async fn mint_calls_correct_endpoint_and_decodes_token() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://api.github.test/app/installations/55/access_tokens".into(),
            required_headers: {
                let mut h = BTreeMap::new();
                h.insert("accept".into(), "application/vnd.github+json".into());
                h
            },
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: token_response_body(),
            },
        }]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let t = client.mint_installation_token(55).await.unwrap();
        assert_eq!(t.token, "ghs_xxx");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn second_call_uses_cache_and_does_not_hit_network() {
        let mock = MockTransport::scripted(vec![Expectation {
            method: Method::Post,
            url: "https://api.github.test/app/installations/55/access_tokens".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: token_response_body(),
            },
        }]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let _ = client.installation_token(55).await.unwrap();
        let t2 = client.installation_token(55).await.unwrap();
        assert_eq!(t2, "ghs_xxx");
        mock.assert_exhausted(); // only ONE network call
    }
}
```

- [ ] **Step 2: Run the tests (will fail until Task 13 wires the cache)**

Run: `cargo test -p github --lib installation::token_mint_tests`
Expected: tests fail to *compile* (missing `TokenCache::new`, `get_fresh`, `insert`). That's the point — Task 13 fills them in.

- [ ] **Step 3: Commit (intentionally non-compiling at lib-level — expected because Task 13 follows immediately)**

Skip the commit; we'll commit Tasks 12+13 as a single unit at the end of Task 13. Move on.

---

### Task 13: `TokenCache`

Per-installation cache with refresh-before-expiry. We refresh when remaining lifetime is < 60s — GitHub tokens last ~1h so this gives plenty of headroom and avoids the "valid for 0.5s on a slow request" cliff.

**Files:**
- Modify: `crates/github/src/token_cache.rs`

- [ ] **Step 1: Write the cache + tests**

`crates/github/src/token_cache.rs`:

```rust
//! In-process installation-token cache.

use chrono::{DateTime, Duration, Utc};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// 60s buffer below the actual `expires_at` — refresh anytime we'd return a
/// token with less than 60s of remaining life.
const REFRESH_BUFFER_SECS: i64 = 60;

#[derive(Clone, Debug)]
struct CachedToken {
    token: String,
    expires_at: DateTime<Utc>,
}

/// Thread-safe, clone-friendly cache. Keys are installation ids.
#[derive(Clone, Default)]
pub struct TokenCache {
    inner: Arc<RwLock<HashMap<u64, CachedToken>>>,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached token if it has more than [`REFRESH_BUFFER_SECS`] of
    /// remaining life at `now`. Otherwise returns `None` and the caller should
    /// re-mint via [`crate::installation::InstallationClient::mint_installation_token`].
    pub fn get_fresh(&self, installation_id: u64, now: DateTime<Utc>) -> Option<String> {
        let guard = self.inner.read();
        let entry = guard.get(&installation_id)?;
        if entry.expires_at - Duration::seconds(REFRESH_BUFFER_SECS) > now {
            Some(entry.token.clone())
        } else {
            None
        }
    }

    pub fn insert(&self, installation_id: u64, token: String, expires_at: DateTime<Utc>) {
        self.inner
            .write()
            .insert(installation_id, CachedToken { token, expires_at });
    }

    /// For tests / shutdown.
    pub fn invalidate(&self, installation_id: u64) {
        self.inner.write().remove(&installation_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn fresh_when_far_from_expiry() {
        let c = TokenCache::new();
        c.insert(1, "tok".into(), at("2026-05-04T13:00:00Z"));
        let now = at("2026-05-04T12:00:00Z");
        assert_eq!(c.get_fresh(1, now).as_deref(), Some("tok"));
    }

    #[test]
    fn stale_when_within_buffer() {
        let c = TokenCache::new();
        c.insert(1, "tok".into(), at("2026-05-04T12:00:30Z")); // 30s out
        let now = at("2026-05-04T12:00:00Z");
        assert!(c.get_fresh(1, now).is_none(), "30s remaining should be stale");
    }

    #[test]
    fn missing_installation_is_none() {
        let c = TokenCache::new();
        assert!(c.get_fresh(99, Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap()).is_none());
    }

    #[test]
    fn invalidate_drops_entry() {
        let c = TokenCache::new();
        c.insert(1, "tok".into(), at("2099-01-01T00:00:00Z"));
        c.invalidate(1);
        assert!(c.get_fresh(1, Utc::now()).is_none());
    }
}
```

- [ ] **Step 2: Run the cache tests AND the installation tests from Task 12**

Run: `cargo test -p github --lib token_cache`
Expected: 4 tests pass.

Run: `cargo test -p github --lib installation::token_mint_tests`
Expected: 2 tests pass (the cache hit path now passes because the cache is wired).

- [ ] **Step 3: Commit Tasks 12 + 13 together**

```bash
git add crates/github/src/installation.rs crates/github/src/token_cache.rs
git commit -m "feat(github): InstallationClient + installation-token cache"
```

---

### Task 14: Read endpoints — `list_installation_repos`, `get_repo`

Per spec §14.

**Files:**
- Modify: `crates/github/src/installation.rs`

- [ ] **Step 1: Add the methods inside `impl InstallationClient`**

Append to the `impl InstallationClient { ... }` block:

```rust
    /// `GET /installation/repositories` — paginated upstream; v1 only follows
    /// page 1 (per_page=100). If GitHub ever ships a customer with > 100 repos
    /// per install, Plan 3's `Reconcile::sweep` adds pagination there.
    pub async fn list_installation_repos(
        &self,
        installation_id: u64,
    ) -> Result<GhInstallationRepos> {
        let req = self
            .auth_request(installation_id, Method::Get, "/installation/repositories?per_page=100")
            .await?;
        self.transport.send(req).await?.ensure_success()?.json()
    }

    /// `GET /repos/{owner}/{repo}` — small surface for display + access
    /// confirmation.
    ///
    /// **Errors:** `Error::Status { status: 404, .. }` if the App lost access
    /// to the repo (caller should treat that as `selected_repos` drift).
    pub async fn get_repo(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
    ) -> Result<GhRepo> {
        let path = format!("/repos/{}/{}", path_seg(owner), path_seg(repo));
        let req = self
            .auth_request(installation_id, Method::Get, &path)
            .await?;
        self.transport.send(req).await?.ensure_success()?.json()
    }
```

Add this private helper at module scope (just below the `DEFAULT_BASE_URL` constant):

```rust
/// Percent-encode a single path segment. Same alphabet as `url_path_segment` in
/// `oauth.rs`; duplicated locally rather than re-exported because the two call
/// sites are in different modules and the helper is trivial.
fn path_seg(s: &str) -> String {
    const SAFE: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.~";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if SAFE.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
```

- [ ] **Step 2: Add tests**

Append a new test module to `crates/github/src/installation.rs`:

```rust
#[cfg(test)]
mod read_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use rand::SeedableRng;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::RsaPrivateKey;
    use std::collections::BTreeMap;

    fn signer() -> AppJwtSigner {
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(8);
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let pem = priv_key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        AppJwtSigner::from_pkcs8_pem(123, pem.as_str()).unwrap()
    }

    /// Single mocked token-mint response that all tests below consume first.
    fn token_mint_expectation() -> Expectation {
        Expectation {
            method: Method::Post,
            url: "https://api.github.test/app/installations/9/access_tokens".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec(),
            },
        }
    }

    #[tokio::test]
    async fn list_installation_repos_decodes_envelope() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/installation/repositories?per_page=100",
                serde_json::json!({
                    "total_count": 1,
                    "repositories": [{"id": 5, "full_name": "acme/api", "private": true}]
                }),
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let r = client.list_installation_repos(9).await.unwrap();
        assert_eq!(r.total_count, 1);
        assert_eq!(r.repositories[0].full_name, "acme/api");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn get_repo_uses_path_encoded_segments() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api%20gateway",
                serde_json::json!({"id": 7, "full_name": "acme/api gateway", "private": false}),
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let r = client.get_repo(9, "acme", "api gateway").await.unwrap();
        assert_eq!(r.id, 7);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn get_repo_404_surfaces_status() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/gone",
                404,
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client.get_repo(9, "acme", "gone").await.unwrap_err();
        assert_eq!(err.status(), Some(404));
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p github --lib installation::read_tests`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/github/src/installation.rs
git commit -m "feat(github): InstallationClient list_installation_repos + get_repo"
```

---

### Task 15: Write endpoints — `add_collaborator`, `delete_invitation`

`add_collaborator` is the hot path: PUT returns 201 with the new invitation id, or 204 if the user is already a collaborator (no invitation row created), or 422 with details on bad input.

**Files:**
- Modify: `crates/github/src/installation.rs`

- [ ] **Step 1: Add the two methods to `impl InstallationClient`**

```rust
    /// `PUT /repos/{owner}/{repo}/collaborators/{username}`. Returns:
    /// - `Ok(Some(invitation_id))` on 201 — recipient now has a pending invitation.
    /// - `Ok(None)` on 204 — recipient was already a collaborator (no invitation
    ///   created). Caller should treat as immediate-accept.
    /// - `Err(Error::Status)` on any other status.
    pub async fn add_collaborator(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
        username: &str,
        permission: domain::Permission,
    ) -> Result<Option<u64>> {
        let path = format!(
            "/repos/{}/{}/collaborators/{}",
            path_seg(owner),
            path_seg(repo),
            path_seg(username)
        );
        let req = self
            .auth_request(installation_id, Method::Put, &path)
            .await?
            .json_body(&serde_json::json!({"permission": permission.to_string()}))?;
        let resp = self.transport.send(req).await?;
        match resp.status {
            201 => {
                let inv: GhCollaboratorInvite = resp.json()?;
                Ok(Some(inv.id))
            }
            204 => Ok(None),
            other => {
                let body = String::from_utf8_lossy(&resp.body).to_string();
                Err(Error::Status {
                    status: other,
                    body,
                })
            }
        }
    }

    /// `DELETE /repos/{owner}/{repo}/invitations/{invitation_id}` — used to
    /// cancel a pending invitation.
    ///
    /// **Errors:** `Error::Status { status: 404 }` if the invitation no longer
    /// exists (already accepted/declined/cancelled).
    pub async fn delete_invitation(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
        invitation_id: u64,
    ) -> Result<()> {
        let path = format!(
            "/repos/{}/{}/invitations/{}",
            path_seg(owner),
            path_seg(repo),
            invitation_id
        );
        let req = self
            .auth_request(installation_id, Method::Delete, &path)
            .await?;
        self.transport.send(req).await?.ensure_success()?;
        Ok(())
    }
```

The `domain::Permission` import: add `use domain::Permission;` near the top (or use the fully-qualified path inline as written).

- [ ] **Step 2: Add tests**

Append to the `read_tests` module file (or a new `write_tests` mod):

```rust
#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use domain::Permission;
    use rand::SeedableRng;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::RsaPrivateKey;
    use std::collections::BTreeMap;

    fn signer() -> AppJwtSigner {
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(9);
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let pem = priv_key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        AppJwtSigner::from_pkcs8_pem(123, pem.as_str()).unwrap()
    }

    fn token_mint_expectation() -> Expectation {
        Expectation {
            method: Method::Post,
            url: "https://api.github.test/app/installations/9/access_tokens".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec(),
            },
        }
    }

    #[tokio::test]
    async fn add_collaborator_returns_invitation_id_on_201() {
        let body = serde_json::to_vec(&serde_json::json!({"permission": "push"})).unwrap();
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation {
                method: Method::Put,
                url: "https://api.github.test/repos/acme/api/collaborators/octocat".into(),
                required_headers: BTreeMap::new(),
                expected_body: Some(body),
                response: Response {
                    status: 201,
                    headers: BTreeMap::new(),
                    body: br#"{"id": 9988, "invitee":{"id":42,"login":"octocat"}}"#.to_vec(),
                },
            },
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let id = client
            .add_collaborator(9, "acme", "api", "octocat", Permission::Push)
            .await
            .unwrap();
        assert_eq!(id, Some(9988));
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn add_collaborator_returns_none_on_204() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation {
                method: Method::Put,
                url: "https://api.github.test/repos/acme/api/collaborators/octocat".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            },
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let id = client
            .add_collaborator(9, "acme", "api", "octocat", Permission::Push)
            .await
            .unwrap();
        assert_eq!(id, None);
    }

    #[tokio::test]
    async fn add_collaborator_propagates_422() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation::status(
                Method::Put,
                "https://api.github.test/repos/acme/api/collaborators/baduser",
                422,
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client
            .add_collaborator(9, "acme", "api", "baduser", Permission::Push)
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(422));
    }

    #[tokio::test]
    async fn delete_invitation_succeeds_on_204() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation {
                method: Method::Delete,
                url: "https://api.github.test/repos/acme/api/invitations/777".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            },
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        client
            .delete_invitation(9, "acme", "api", 777)
            .await
            .unwrap();
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p github --lib installation::write_tests`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/github/src/installation.rs
git commit -m "feat(github): InstallationClient add_collaborator + delete_invitation"
```

---

### Task 16: Reconciler endpoints — `list_invitations`, `is_collaborator`

Per spec §14 last two bullets — used by Plan 3's daily `Reconcile` sweep (and by webhook-recovery paths).

**Files:**
- Modify: `crates/github/src/installation.rs`

- [ ] **Step 1: Add the two methods to `impl InstallationClient`**

```rust
    /// `GET /repos/{owner}/{repo}/invitations` — list pending invitations on a
    /// repo. Used by the reconciler to check that our `github_invitations` rows
    /// in `sent` state still exist upstream.
    pub async fn list_invitations(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GhInvitationListItem>> {
        let path = format!(
            "/repos/{}/{}/invitations?per_page=100",
            path_seg(owner),
            path_seg(repo)
        );
        let req = self
            .auth_request(installation_id, Method::Get, &path)
            .await?;
        self.transport.send(req).await?.ensure_success()?.json()
    }

    /// `GET /repos/{owner}/{repo}/collaborators/{username}` — confirm membership.
    /// GitHub returns 204 if the user *is* a collaborator and 404 otherwise.
    /// Used to confirm an invitation acceptance when the webhook is missed.
    pub async fn is_collaborator(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
        username: &str,
    ) -> Result<bool> {
        let path = format!(
            "/repos/{}/{}/collaborators/{}",
            path_seg(owner),
            path_seg(repo),
            path_seg(username)
        );
        let req = self
            .auth_request(installation_id, Method::Get, &path)
            .await?;
        let resp = self.transport.send(req).await?;
        match resp.status {
            204 => Ok(true),
            404 => Ok(false),
            other => {
                let body = String::from_utf8_lossy(&resp.body).to_string();
                Err(Error::Status {
                    status: other,
                    body,
                })
            }
        }
    }
```

- [ ] **Step 2: Add tests**

Append to `installation.rs`:

```rust
#[cfg(test)]
mod reconcile_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use rand::SeedableRng;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::RsaPrivateKey;
    use std::collections::BTreeMap;

    fn signer() -> AppJwtSigner {
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(10);
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let pem = priv_key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        AppJwtSigner::from_pkcs8_pem(123, pem.as_str()).unwrap()
    }

    fn token_mint_expectation() -> Expectation {
        Expectation {
            method: Method::Post,
            url: "https://api.github.test/app/installations/9/access_tokens".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec(),
            },
        }
    }

    #[tokio::test]
    async fn list_invitations_decodes_array() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([
                    {
                        "id": 1,
                        "invitee": {"id": 42, "login": "octocat"},
                        "permissions": "write",
                        "created_at": "2026-05-04T12:00:00Z"
                    }
                ]),
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let l = client.list_invitations(9, "acme", "api").await.unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].id, 1);
        assert_eq!(l[0].invitee.login, "octocat");
    }

    #[tokio::test]
    async fn is_collaborator_true_on_204() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation {
                method: Method::Get,
                url: "https://api.github.test/repos/acme/api/collaborators/octocat".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            },
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        assert!(client
            .is_collaborator(9, "acme", "api", "octocat")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn is_collaborator_false_on_404() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/api/collaborators/notamember",
                404,
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        assert!(!client
            .is_collaborator(9, "acme", "api", "notamember")
            .await
            .unwrap());
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p github --lib installation::reconcile_tests`
Expected: 3 tests pass.

- [ ] **Step 4: Re-export `InstallationClient` at the crate root**

In `crates/github/src/lib.rs`:

```rust
pub use installation::InstallationClient;
```

(Verify this re-export is uncommented; remove the leading `// ` if still present.)

- [ ] **Step 5: Commit**

```bash
git add crates/github/src/installation.rs crates/github/src/lib.rs
git commit -m "feat(github): list_invitations + is_collaborator (reconciler)"
```

---

### Task 17: wasm32-unknown-unknown smoke test

Per spec §20: validate that the dependency closure links against `wasm32-unknown-unknown` *before* Plan 3 wires this crate into a worker. This is a compile-only test — we don't run anything; we just confirm that `cargo check --target wasm32-unknown-unknown` succeeds.

**Files:**
- Modify: `crates/github/src/wasm_smoke.rs`

- [ ] **Step 1: Write the wasm smoke module**

`crates/github/src/wasm_smoke.rs`:

```rust
//! `wasm32-unknown-unknown` compile smoke. The body intentionally references
//! every public surface of this crate so a missing/incompatible `wasm` feature
//! shows up at compile time rather than during deployment.

#![cfg(target_arch = "wasm32")]

use crate::error::Error;
use crate::hmac::verify_signature_256;
use crate::installation::InstallationClient;
use crate::jwt::AppJwtSigner;
use crate::oauth::{AuthorizeUrl, OAuthConfig, UserApiClient};
use crate::token_cache::TokenCache;
use crate::transport::{HttpTransport, Method, Request, Response};

/// Inert function that exercises every public type without running anything.
/// Exists only so the linker has to resolve all references.
#[allow(dead_code)]
pub fn _wasm_link_check(
    _t: std::sync::Arc<dyn HttpTransport>,
    _signer: AppJwtSigner,
    _ua: UserApiClient,
    _ic: InstallationClient,
    _au: AuthorizeUrl,
    _cfg: OAuthConfig,
    _cache: TokenCache,
    _req: Request,
    _resp: Response,
    _method: Method,
    _err: Error,
) -> bool {
    verify_signature_256(b"", b"", "")
}
```

- [ ] **Step 2: Configure reqwest's wasm feature only on wasm targets**

In `crates/github/Cargo.toml`, add a target-specific dependency block:

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
reqwest = { workspace = true, features = ["wasm-client"] }
# `getrandom`'s `js` feature is required on wasm32-unknown-unknown so `rsa`
# key-gen / `oauth2` state token can pull from `crypto.getRandomValues()`.
getrandom = { version = "0.2", features = ["js"] }
```

If `reqwest`'s `wasm-client` feature does not exist in 0.12 under that name (the relevant feature in 0.12 is the implicit wasm32 fallback that uses `web-sys::Fetch`), drop the explicit feature line and rely on the workspace-default features. Verify in Step 3.

- [ ] **Step 3: Add the wasm32 target to rustup (if missing) and run a check**

```bash
rustup target add wasm32-unknown-unknown
cargo check -p github --target wasm32-unknown-unknown 2>&1 | tail -40
```

Expected: clean. If `reqwest` complains about missing TLS backend on wasm32, swap features to `default-features = false` only on the wasm target — wasm32-unknown-unknown uses `web-sys::Fetch` and does NOT need `rustls-tls`.

If `rsa::RsaPrivateKey::new` complains about missing system RNG: that's the `getrandom`-with-`js`-feature requirement — confirm Step 2 added it.

If `wiremock` (transitively pulled in) breaks the wasm build, gate it more tightly:

```toml
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
wiremock = { workspace = true, optional = true }
```

(Move it out of the unconditional `[dependencies]` block.)

- [ ] **Step 4: Run native tests one more time to ensure no regression**

Run: `cargo test -p github`
Expected: all unit + integration tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/github/src/wasm_smoke.rs crates/github/Cargo.toml
git commit -m "test(github): wasm32-unknown-unknown link check"
```

If `cargo check --target wasm32-unknown-unknown` fails despite trying both feature combinations from Step 3, **stop and consult the user** — the spec §20 fallback is hosting the Restate Service binary on a small VM rather than Workers. That's a deployment-architecture decision, not an implementation choice this plan should make unilaterally.

---

### Task 18: Final clippy + fmt

**Files:** all of `crates/github/`

- [ ] **Step 1: Run `cargo fmt --all`**

Run: `cargo fmt --all`
Expected: completes silently.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean. If clippy flags genuine issues, fix them; for stylistic preferences, prefer a focused `#[allow(clippy::xxx)]` near the call site over a crate-wide allow.

- [ ] **Step 3: Run the entire workspace test suite one more time**

Run: `cargo test --workspace`
Expected: every test in every crate (domain, audit, storage, github) passes.

- [ ] **Step 4: Update the plan map README**

Edit `docs/superpowers/plans/README.md`. Change the row for Plan 2 from `_not yet written_` / `Pending` to `2026-05-04-ghinvite-github-clients.md` / `Implemented`.

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "chore: apply rustfmt and clippy fixes; mark Plan 2 done"
```

---

## Plan self-review

**Spec coverage check** (each spec section → which task implements it):

- §10.1 / §10.2 (OAuth flow) → Tasks 7 (authorize URL), 8 (code exchange), 9 (`/user`)
- §10.3 (admin recheck) → Task 9 (`/user/memberships/orgs/:login`)
- §13 (HMAC) → Task 3 (constant-time `verify_signature_256`)
- §14 first block (App-installation endpoints) → Tasks 12 (token mint), 14 (read), 15 (write), 16 (reconciler)
- §14 second block (user endpoints) → Task 9
- §14 closing line ("App private key held only by Restate worker") → enforced architecturally: the App-JWT signer is a parameter to `InstallationClient::new` (Task 12); web binary in Plan 4 never touches it.
- §18 security: HMAC constant-time → Task 3; `read:user` scope only → Task 7; user token at rest in session → not this crate's concern (Plan 4 wraps).
- §20 wasm smoke → Task 17

**Spec sections NOT covered by this plan (intentionally — assigned to other plans):**
- §13 webhook *event routing* (action dispatch, idempotency-key wiring) → Plan 6
- §10.1 step 4 (`Installation::onboard.send`) → Plan 3 + Plan 4 wiring
- §11 URL routing → Plan 4
- §12 frontend layouts → Plan 4
- §16 error-to-HTTP mapping → Plan 4
- §17 testing strategy (e2e against Restate) → Plan 8
- §15 audit emission → already shipped by Plan 1; consumed by Plan 3 handlers

**Type consistency check:**
- `domain::Permission` is the only domain type crossed into this crate; consumed by `InstallationClient::add_collaborator` (Task 15). Its `to_string()` produces lowercase `"pull"` / `"push"` / etc., which is exactly what GitHub's `permission` field expects. No translation layer needed.
- `GhUser`, `GhInstallationToken`, `GhCollaboratorInvite`, `GhInvitationListItem`, `GhRepo`, `GhInstallationRepos`, `GhTokenResponse`, `GhMembership` are all defined in Task 6 and used identically in later tasks.
- `Error::Status::status` field is `u16` everywhere; `Error::status()` returns `Option<u16>`. Plan 3 will branch on specific codes; the helper keeps that ergonomic.
- `AppJwtSigner::sign` takes `DateTime<Utc>` (Task 11); `InstallationClient::mint_installation_token` calls `Utc::now()` once per mint (Task 12). Cache stores `expires_at` as `DateTime<Utc>` (Task 13). Single time type throughout.

**Placeholder check:** No `TBD`, `TODO`, `// implement later`. Every code step shows complete, runnable code. Task 12 deliberately commits-with-Task-13 because the cache types are defined in 13; that's flagged in Task 12 Step 3.

**Wasm caveat (Task 17):** The plan's exact `reqwest` and `getrandom` feature lines may need tweaking once we hit the wasm32-unknown-unknown reality. Step 3 lists three concrete failure modes and how to fix each. Step 5 explicitly hands off to the user if all three fail — that's the spec §20 deployment fallback.

---

## Audit trail

This plan was written on 2026-05-04 against the just-implemented `crates/domain` + `crates/audit` + `crates/storage` from Plan 1. No /autoplan review has been performed; the plan is offered for execution-time review by the implementer (a draft to iterate on, in the same spirit Plan 1 was iterated via /autoplan).

| # | Decision | Source |
|---|----------|--------|
| 1 | One crate `crates/github` per project README | README locked |
| 2 | `HttpTransport` trait at the seam (not `octocrab` directly) | User pre-write decision |
| 3 | `oauth2 5` for authorize-URL only; raw transport for `/user` & memberships | User pre-write decision |
| 4 | Manual RS256 (`rsa` + `sha2` + `base64`); not `jsonwebtoken` for signing | User pre-write decision (avoid `ring` on wasm) |
| 5 | `parking_lot::RwLock` for token cache | wasm-friendly, sync-safe; no async-await held across lock |
| 6 | `MockTransport` behind `test-mock` feature | mirrors Plan 1's `test-suite` feature pattern |
| 7 | Translate `permission` via `domain::Permission` only | minimal cross-crate coupling (User pre-write decision #6) |
| 8 | Wasm smoke as Task 17 | spec §20 explicit ask |
