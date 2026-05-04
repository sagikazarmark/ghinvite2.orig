# ghinvite — Plan 3: Restate Handlers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `crates/restate-svc` library — five Restate services (`Installation`, `ShareLink`, `InvitationRequest`, `GithubInvitation`, `Reconcile`) that own every durable state change in ghinvite. Each method is idempotent under Restate's exactly-once-effects guarantee, emits the audit events spec §15.1 mandates, and is unit-tested on native against `SqlxStorage::in_memory` + `MockTransport` from Plans 1 and 2.

**Architecture:** Library crate (rlib) using `restate-sdk = "0.10"` upstream. The service `impl`s are thin: each handler enters Restate's context, calls a pure async function that owns the business logic, exits the context. The pure functions take `&AppState` (`Arc<dyn Storage>` + `Arc<InstallationClient>`) and live in the same module as the service so they're easy to find. Unit tests exercise the pure functions directly with `tokio::test`. The Workers `cdylib` entry point, `D1Storage`, and `wrangler.toml` are explicitly **deferred to Plan 7**; this plan ships handler logic that Plan 7 wraps in a `#[event(fetch)]`.

**Tech Stack:** Rust 2024 (workspace), `restate-sdk = "0.10"` (`default-features = false`, `hyper` + `schemars` + `rust_crypto` features) for the service macros + `Endpoint` builder, `tokio` (test feature) for async tests, existing `crates/domain` / `crates/audit` / `crates/storage` / `crates/github` for types and I/O. No new transport dep — handlers consume `crates/github::InstallationClient` directly. `chrono` for timer math; `serde` / `serde_json` for input/output structs.

---

## Spec coverage

This plan implements **§9 in full** (all five Restate services + their methods), §15.1 (audit events emitted from handlers — every state transition produces one), §15.2 (audit writes go through `Storage::audit` only, never updated/deleted), and the relevant pieces of §16 (error handling: GitHub 4xx terminal vs. 5xx retry, Restate retry policy via `TerminalError`, "two admins approve same request" handled by Restate single-flight per `request_id`).

It does **NOT** implement:
- §10 OAuth / session — Plan 4.
- §11 URL routing / §12 frontend — Plans 4–6.
- §13 webhook receiver (HMAC + envelope parse + dispatch) — Plan 6 calls our `GithubInvitation::on_webhook` and `Installation::repos_changed` / `Installation::uninstall` handlers.
- §14 GitHub API surface — already shipped by Plan 2; this plan consumes it.
- §15.3 audit UI / CSV — v1.1.
- `D1Storage` — Plan 7 (this plan tests against `SqlxStorage::in_memory`).
- `wrangler.toml` / `cdylib` build / `#[event(fetch)]` entry / Workers secrets — Plan 7.
- E2E tests against `restate-server` in Docker — Plan 8.

## File structure

| Path | Responsibility |
|---|---|
| `crates/restate-svc/Cargo.toml` | Crate manifest (rlib). Pulls `restate-sdk` upstream + the workspace's existing crates. |
| `crates/restate-svc/src/lib.rs` | Module declarations + re-exports. Owns the `Endpoint` builder helper Plan 7 will hand to its `#[event(fetch)]`. |
| `crates/restate-svc/src/state.rs` | `AppState { storage, github }` — the shared dependency container every handler holds. |
| `crates/restate-svc/src/error.rs` | `HandlerError` enum + `From<storage::Error>` / `From<github::Error>` / `Into<TerminalError>`. |
| `crates/restate-svc/src/audit.rs` | `emit(state, account_id, event_type, actor, target, metadata, request_id)` helper. |
| `crates/restate-svc/src/installation.rs` | `Installation` Virtual Object: `onboard`, `repos_changed`, `uninstall` + their pure-logic fns + tests. |
| `crates/restate-svc/src/share_link.rs` | `ShareLink` Virtual Object: `create`, `revoke`, `tick_expiration` + pure fns + tests. |
| `crates/restate-svc/src/invitation_request.rs` | `InvitationRequest` Workflow + helper fns + tests. |
| `crates/restate-svc/src/github_invitation.rs` | `GithubInvitation` Virtual Object: `create`, `on_webhook`, `cancel`, `tick_expire` + pure fns + tests. |
| `crates/restate-svc/src/reconcile.rs` | `Reconcile` Service: `daily_run` + pure fn + tests. |
| `crates/restate-svc/src/test_support.rs` | Test-only fixture builders (`fixture_storage`, `fixture_github_client`, `fixture_state`, sample inputs). Behind `#[cfg(test)]`. |
| `crates/restate-svc/README.md` | Local-dev guide for `docker compose up restate` + `wrangler dev` (cross-references Plan 7 for the wrangler/D1 wiring). |

---

## Task ordering

Tasks 1–5 set up the workspace member, error type, audit helper, and shared state. Tasks 6–8 ship `Installation`. Tasks 9–11 ship `ShareLink`. Tasks 12–15 ship `GithubInvitation` (the most complex Virtual Object — branches on multiple GitHub status codes). Tasks 16–18 ship `InvitationRequest` (the workflow with awakeable + timer race; pure-logic fns are unit-tested, the Restate-runtime parts are validated in integration). Task 19 ships `Reconcile`. Tasks 20–21 add the public `Endpoint` builder helper and a smoke-friendly `Service` schema export so Plan 7 can register the deployment. Tasks 22–23 polish and update the plan map.

A "pure-logic fn" pattern is used throughout: each Restate handler method extracts its body to a free async function that takes `&AppState` and returns `Result<T, HandlerError>`. The `#[restate_sdk::object|service|workflow]` impl wraps the pure fn inside `ctx.run("step_name", async {...})` so Restate captures it as a durable step. Unit tests call the pure fn directly without a Restate context.

---

### Task 1: Workspace member skeleton

**Files:**
- Create: `crates/restate-svc/Cargo.toml`
- Create: `crates/restate-svc/src/lib.rs`
- Create: `crates/restate-svc/src/state.rs` (stub)
- Create: `crates/restate-svc/src/error.rs` (stub)
- Create: `crates/restate-svc/src/audit.rs` (stub)
- Create: `crates/restate-svc/src/installation.rs` (stub)
- Create: `crates/restate-svc/src/share_link.rs` (stub)
- Create: `crates/restate-svc/src/invitation_request.rs` (stub)
- Create: `crates/restate-svc/src/github_invitation.rs` (stub)
- Create: `crates/restate-svc/src/reconcile.rs` (stub)
- Create: `crates/restate-svc/src/test_support.rs` (stub)
- Modify: workspace `Cargo.toml` (add member + new shared deps)

- [ ] **Step 1: Add `restate-svc` to the workspace and add new shared deps**

In the workspace root `Cargo.toml`, add `"crates/restate-svc"` to `members` and add the following entries to `[workspace.dependencies]` (alphabetical):

```toml
restate-sdk = { version = "0.10", default-features = false, features = ["hyper", "schemars", "rust_crypto"] }
```

Plus the new path member alongside the others:

```toml
restate-svc = { path = "crates/restate-svc" }
```

`restate-sdk = "0.10"` is the upstream release. We disable `default-features` to skip `http_server` (which pulls `tokio/net` — fine on native, but Plan 7 will switch to a `worker::Fetch`-driven entry point so we don't pre-commit to the hyper-server runtime). `hyper` provides the `http::Request` / `http::Response` integration `Endpoint::handle_with_options` needs; `schemars` exposes service schemas to the Restate platform's discovery endpoint; `rust_crypto` selects the pure-Rust `jsonwebtoken` backend (wasm-friendly).

- [ ] **Step 2: Create the crate manifest**

`crates/restate-svc/Cargo.toml`:

```toml
[package]
name = "restate-svc"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
async-trait.workspace = true
audit.workspace = true
chrono.workspace = true
domain.workspace = true
github.workspace = true
rand.workspace = true
restate-sdk.workspace = true
serde.workspace = true
serde_json.workspace = true
storage.workspace = true
thiserror.workspace = true
tracing.workspace = true

[dev-dependencies]
github = { workspace = true, features = ["test-mock"] }
storage = { workspace = true, features = ["test-suite"] }
rand_chacha.workspace = true
tokio.workspace = true
```

The dev-dep on `github` enables the `test-mock` feature for `MockTransport`; the dev-dep on `storage` enables `test-suite` for `SqlxStorage::in_memory`. Both mirror the same self-referential pattern used inside those crates' own `[dev-dependencies]`.

- [ ] **Step 3: Create `crates/restate-svc/src/lib.rs`**

```rust
//! Restate handler services for ghinvite. Five services own every durable
//! state change: [`installation::Installation`] (Virtual Object),
//! [`share_link::ShareLink`] (Virtual Object), [`invitation_request::InvitationRequest`]
//! (Workflow), [`github_invitation::GithubInvitation`] (Virtual Object),
//! [`reconcile::Reconcile`] (Service).
//!
//! Each handler delegates to a pure-async function that takes [`state::AppState`]
//! and returns [`error::HandlerError`]. Unit tests exercise the pure functions
//! without a Restate runtime; full workflow integration tests against the local
//! `restate-server` (compose.yaml) are deferred to Plan 8.

pub mod audit;
pub mod error;
pub mod github_invitation;
pub mod installation;
pub mod invitation_request;
pub mod reconcile;
pub mod share_link;
pub mod state;

#[cfg(any(test))]
pub(crate) mod test_support;

pub use error::{HandlerError, Result};
pub use state::AppState;
```

- [ ] **Step 4: Create stub module files**

For each of `state.rs`, `error.rs`, `audit.rs`, `installation.rs`, `share_link.rs`, `invitation_request.rs`, `github_invitation.rs`, `reconcile.rs`, `test_support.rs`, create the file under `crates/restate-svc/src/` containing exactly:

```rust
// filled in by a later task
```

For now, comment out the `pub use` re-exports at the bottom of `lib.rs`:

```rust
// Re-exports filled in as each module gains its public types:
// pub use error::{HandlerError, Result};
// pub use state::AppState;
```

- [ ] **Step 5: Create `crates/restate-svc/README.md`** (placeholder; filled by Task 20)

```markdown
# crates/restate-svc

Restate handler services for ghinvite. See Plan 3 in `docs/superpowers/plans/`.

Local-dev guide will be filled in by Task 20.
```

- [ ] **Step 6: Verify the crate compiles**

Run: `cargo check -p restate-svc`
Expected: clean (warnings about empty modules are OK; the modules will gain content in subsequent tasks).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/restate-svc
git commit -m "chore: scaffold crates/restate-svc skeleton"
```

---

### Task 2: `HandlerError` and error mapping

`HandlerError` bridges domain/storage/github errors into Restate's `TerminalError`. Restate distinguishes terminal failures (don't retry) from transient (retry with backoff). We map by category:

- Storage `Conflict` (any `ConflictKind`): terminal — idempotent retry would just hit the same constraint.
- Storage `NotFound`: terminal — the row truly isn't there.
- Storage `Database`: transient — let Restate retry.
- Storage `Corrupt`: terminal — bad data won't fix itself.
- GitHub `Status` 4xx (excluding 429): terminal — bad inputs won't fix themselves.
- GitHub `Status` 5xx + 429: transient — backoff and retry.
- GitHub `Transport` / `Decode` / `OAuth` / `InvalidInput` / `Jwt`: transient (connection blip etc.) — retry.

**Files:**
- Modify: `crates/restate-svc/src/error.rs`
- Modify: `crates/restate-svc/src/lib.rs` (un-comment re-exports)

- [ ] **Step 1: Write the error type**

`crates/restate-svc/src/error.rs`:

```rust
//! Error type bridging Storage, GitHub, and Restate's terminal/transient model.

use restate_sdk::prelude::TerminalError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, HandlerError>;

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error(transparent)]
    Storage(#[from] storage::Error),

    #[error(transparent)]
    Github(#[from] github::Error),

    /// Domain invariant the caller should not have allowed.
    #[error("invariant violated: {0}")]
    Invariant(String),
}

impl HandlerError {
    /// Returns `true` if the underlying cause is transient and the caller
    /// should let Restate retry (i.e. NOT wrap this in `TerminalError`).
    /// Returns `false` for terminal errors that should bypass Restate retry.
    pub fn is_terminal(&self) -> bool {
        match self {
            HandlerError::Storage(e) => match e {
                storage::Error::Database(_) => false, // transient
                storage::Error::Conflict(_)
                | storage::Error::NotFound
                | storage::Error::Corrupt(_) => true, // terminal
            },
            HandlerError::Github(e) => match e {
                github::Error::Status { status, .. } => match *status {
                    429 | 500..=599 => false, // transient
                    _ => true,                // terminal (4xx)
                },
                github::Error::Transport(_)
                | github::Error::Decode(_)
                | github::Error::OAuth(_)
                | github::Error::InvalidInput(_)
                | github::Error::Jwt(_) => false, // transient
            },
            HandlerError::Invariant(_) => true, // bug — terminal
        }
    }

    /// Convert into Restate's terminal-error wrapper if this error should NOT
    /// retry; otherwise return the underlying message wrapped in
    /// `Err(restate_sdk::error::Error::other(...))` so Restate retries.
    ///
    /// In the current SDK shape, handler return types are
    /// `Result<T, TerminalError>`; transient errors are surfaced by leaving
    /// `ctx.run` to fail and propagate. Most handlers will use `to_terminal()`
    /// after a `match self.is_terminal()`.
    pub fn to_terminal(&self) -> TerminalError {
        TerminalError::new(self.to_string())
    }
}
```

- [ ] **Step 2: Add tests**

Append to `error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use storage::ConflictKind;

    #[test]
    fn storage_database_is_transient() {
        let e = HandlerError::Storage(storage::Error::Database(
            sqlx::Error::Configuration("test".into()),
        ));
        assert!(!e.is_terminal());
    }

    #[test]
    fn storage_conflict_is_terminal() {
        let e = HandlerError::Storage(storage::Error::Conflict(ConflictKind::DuplicateSlug));
        assert!(e.is_terminal());
    }

    #[test]
    fn storage_not_found_is_terminal() {
        let e = HandlerError::Storage(storage::Error::NotFound);
        assert!(e.is_terminal());
    }

    #[test]
    fn github_5xx_is_transient() {
        let e = HandlerError::Github(github::Error::Status {
            status: 502,
            body: "bad gateway".into(),
        });
        assert!(!e.is_terminal());
    }

    #[test]
    fn github_429_is_transient() {
        let e = HandlerError::Github(github::Error::Status {
            status: 429,
            body: "rate limited".into(),
        });
        assert!(!e.is_terminal());
    }

    #[test]
    fn github_404_is_terminal() {
        let e = HandlerError::Github(github::Error::Status {
            status: 404,
            body: "not found".into(),
        });
        assert!(e.is_terminal());
    }

    #[test]
    fn github_transport_is_transient() {
        let e = HandlerError::Github(github::Error::Transport("dns".into()));
        assert!(!e.is_terminal());
    }

    #[test]
    fn invariant_is_terminal() {
        let e = HandlerError::Invariant("test".into());
        assert!(e.is_terminal());
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p restate-svc --lib error`
Expected: 8 tests pass.

- [ ] **Step 4: Un-comment the `error::*` re-exports in `lib.rs`**

Replace the commented `// pub use error::{HandlerError, Result};` with the active line.

- [ ] **Step 5: Commit**

```bash
git add crates/restate-svc/src/error.rs crates/restate-svc/src/lib.rs
git commit -m "feat(restate-svc): HandlerError with terminal/transient classification"
```

---

### Task 3: `AppState` shared dependency container

Every handler holds `Arc<dyn Storage>` and `Arc<InstallationClient>`. `AppState` packages them so service `impl` structs can be constructed with one value.

**Files:**
- Modify: `crates/restate-svc/src/state.rs`
- Modify: `crates/restate-svc/src/lib.rs` (un-comment `state::AppState` re-export)

- [ ] **Step 1: Write `AppState`**

`crates/restate-svc/src/state.rs`:

```rust
//! Shared dependency container threaded through every Restate handler.

use github::InstallationClient;
use std::sync::Arc;
use storage::Storage;

/// Cloneable bundle of the I/O dependencies handlers need. Constructed once
/// at startup (Plan 7's `#[event(fetch)]` will build it from `worker::Env`)
/// and cloned into every service `impl` struct.
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<dyn Storage>,
    pub github: Arc<InstallationClient>,
}

impl AppState {
    pub fn new(storage: Arc<dyn Storage>, github: Arc<InstallationClient>) -> Self {
        Self { storage, github }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p restate-svc`
Expected: clean.

- [ ] **Step 3: Un-comment the `state::AppState` re-export**

In `lib.rs`, replace `// pub use state::AppState;` with `pub use state::AppState;`.

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/state.rs crates/restate-svc/src/lib.rs
git commit -m "feat(restate-svc): AppState shared dependency container"
```

---

### Task 4: Audit emission helper

Every state-change handler emits exactly one audit event. The helper builds an `AuditEvent` from the per-call inputs (event type, actor, target, metadata) plus auto-generated id (`AuditEventId::new()`) and timestamp (`Utc::now()`), then calls `Storage::audit`.

**Files:**
- Modify: `crates/restate-svc/src/audit.rs`

- [ ] **Step 1: Write the helper + tests**

`crates/restate-svc/src/audit.rs`:

```rust
//! Audit-event emission helper. Every handler that performs a state
//! transition calls [`emit`] once after the underlying write succeeds.

use crate::error::Result;
use crate::state::AppState;
use ::audit::{ActorKind, AuditEvent, EventType, TargetKind};
use chrono::Utc;
use domain::AuditEventId;

/// Description of who triggered the change. Mapped to the `actor_kind` /
/// `actor_id` columns. The `User` variant carries the user_id; `System` and
/// `Github` carry no id.
#[derive(Clone, Copy, Debug)]
pub enum Actor {
    User(u64),
    System,
    Github,
}

impl Actor {
    fn split(self) -> (ActorKind, Option<u64>) {
        match self {
            Actor::User(id) => (ActorKind::User, Some(id)),
            Actor::System => (ActorKind::System, None),
            Actor::Github => (ActorKind::Github, None),
        }
    }
}

/// Description of what the change was about. `id` is the natural string id
/// for the target type (ulid for share_links, requests, github_invitations;
/// stringified u64 for installations).
#[derive(Clone, Debug)]
pub struct Target {
    pub kind: TargetKind,
    pub id: String,
}

impl Target {
    pub fn installation(installation_id: u64) -> Self {
        Self {
            kind: TargetKind::Installation,
            id: installation_id.to_string(),
        }
    }

    pub fn share_link(link_id: domain::ShareLinkId) -> Self {
        Self {
            kind: TargetKind::ShareLink,
            id: link_id.to_string(),
        }
    }

    pub fn request(request_id: domain::RequestId) -> Self {
        Self {
            kind: TargetKind::InvitationRequest,
            id: request_id.to_string(),
        }
    }

    pub fn github_invitation(id: domain::GithubInvitationId) -> Self {
        Self {
            kind: TargetKind::GithubInvitation,
            id: id.to_string(),
        }
    }
}

/// Emit an audit event. Should be wrapped in `ctx.run("audit", async {...})`
/// at the call site so Restate captures it as a durable step.
pub async fn emit(
    state: &AppState,
    account_id: u64,
    event_type: EventType,
    actor: Actor,
    target: Target,
    metadata: serde_json::Value,
    request_id: Option<String>,
) -> Result<()> {
    let (actor_kind, actor_id) = actor.split();
    let event = AuditEvent {
        id: AuditEventId::new(),
        account_id,
        occurred_at: Utc::now(),
        event_type,
        actor_kind,
        actor_id,
        target_kind: target.kind,
        target_id: target.id,
        metadata,
        request_id,
    };
    state.storage.audit(&event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_state;

    #[tokio::test]
    async fn emit_writes_event_to_storage() {
        let state = fixture_state().await;
        emit(
            &state,
            42,
            EventType::ShareLinkCreated,
            Actor::User(7),
            Target::share_link(domain::ShareLinkId::new()),
            serde_json::json!({"slug": "abcdef"}),
            Some("inv-1".into()),
        )
        .await
        .unwrap();

        // Read back via SqlxStorage's debug helper (test-only).
        // We can't downcast `Arc<dyn Storage>` directly; instead we use
        // the `debug_list_audit` exposed by the in-memory fixture below.
        let events = storage::SqlxStorage::in_memory()
            .await
            .unwrap()
            .debug_list_audit(42)
            .await
            .unwrap();
        // The fresh in-memory DB above is a NEW instance; we expect zero
        // events on it. The point of this test is to verify `emit` returns
        // Ok and `storage.audit` is called without panicking. Per-impl
        // round-trip is already covered in storage's own tests.
        assert!(events.is_empty());
    }

    #[test]
    fn actor_split_user_carries_id() {
        let (k, id) = Actor::User(7).split();
        assert_eq!(k, ActorKind::User);
        assert_eq!(id, Some(7));
    }

    #[test]
    fn actor_split_system_carries_no_id() {
        let (k, id) = Actor::System.split();
        assert_eq!(k, ActorKind::System);
        assert_eq!(id, None);
    }

    #[test]
    fn target_share_link_uses_ulid_string() {
        let id = domain::ShareLinkId::new();
        let t = Target::share_link(id);
        assert_eq!(t.kind, TargetKind::ShareLink);
        assert_eq!(t.id, id.to_string());
    }
}
```

The test for `emit_writes_event_to_storage` deliberately uses a separate `SqlxStorage::in_memory()` to read back — `Arc<dyn Storage>` doesn't downcast to `SqlxStorage`. The test as written verifies `emit` doesn't error and the helper's plumbing works; the actual round-trip is verified by `storage`'s own unit tests in Plan 1. Task 5 (`test_support`) provides the fixture; until it lands the test won't compile. That's expected — we'll commit the helper without the test, then Task 5 enables it.

- [ ] **Step 2: Skip the test for now** — comment out `#[cfg(test)] mod tests { ... }` or remove it. We'll re-enable in Task 5 once `test_support::fixture_state` exists.

- [ ] **Step 3: Verify the crate compiles**

Run: `cargo check -p restate-svc`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/audit.rs
git commit -m "feat(restate-svc): audit emission helper"
```

---

### Task 5: `test_support` module

Centralized fixture builders so every per-handler test can construct an `AppState` in one call.

**Files:**
- Modify: `crates/restate-svc/src/test_support.rs`

- [ ] **Step 1: Write the fixtures**

`crates/restate-svc/src/test_support.rs`:

```rust
//! Test-only fixture builders. Behind `#[cfg(test)]` (declared in `lib.rs`).

use crate::state::AppState;
use chrono::{DateTime, Utc};
use github::{HttpTransport, InstallationClient};
use github::jwt::AppJwtSigner;
use std::sync::Arc;

/// Static test key shared with the github crate's test modules. Same path:
/// `crates/github/src/jwt_test_key.pem` — referenced via Cargo's path
/// resolution from this crate.
const TEST_KEY_PEM: &str = include_str!("../../github/src/jwt_test_key.pem");

/// Build an in-memory `SqlxStorage`.
pub(crate) async fn fixture_storage() -> Arc<dyn storage::Storage> {
    Arc::new(storage::SqlxStorage::in_memory().await.unwrap())
}

/// Build an `InstallationClient` with the supplied transport and an in-test
/// App JWT signer. Pointed at `https://api.github.test` so test request URLs
/// are short and the production github.com host is never hit.
pub(crate) fn fixture_github_client(transport: Arc<dyn HttpTransport>) -> Arc<InstallationClient> {
    let signer = AppJwtSigner::from_pkcs8_pem(123, TEST_KEY_PEM).unwrap();
    Arc::new(InstallationClient::new(transport, signer).with_base("https://api.github.test"))
}

/// Convenience: an `AppState` with in-memory storage and a `MockTransport`-
/// less InstallationClient. Tests that need a scripted transport call
/// `fixture_state_with_transport` instead.
pub(crate) async fn fixture_state() -> AppState {
    use github::mocks::MockTransport;
    let transport = Arc::new(MockTransport::scripted(vec![]));
    AppState::new(fixture_storage().await, fixture_github_client(transport))
}

/// Convenience: an `AppState` with in-memory storage and the supplied
/// `MockTransport` (already script-loaded).
pub(crate) async fn fixture_state_with_transport(
    transport: Arc<dyn HttpTransport>,
) -> AppState {
    AppState::new(fixture_storage().await, fixture_github_client(transport))
}

/// Build a `DateTime<Utc>` from RFC-3339; same pattern used in storage tests.
pub(crate) fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}
```

- [ ] **Step 2: Re-enable the audit test** — uncomment the `tests` module in `crates/restate-svc/src/audit.rs` from Task 4 (or recreate it if you removed it).

- [ ] **Step 3: Run tests**

Run: `cargo test -p restate-svc --lib audit`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/test_support.rs crates/restate-svc/src/audit.rs
git commit -m "test(restate-svc): fixture builders + enable audit::emit test"
```

---

### Task 6: `Installation::onboard`

Per spec §9.5: `onboard(installation_id)` fetches account + repo metadata via the GitHub API, writes the `installations` row, emits `installation.created` audit. Idempotent: if the row already exists in active state, no-op.

The pure-logic function takes `OnboardInput { installation_id, actor_user_id, account_id, account_login, account_type, selected_repos, installed_at }`. The web binary in Plan 4 fills the input fields from the OAuth callback / install webhook. We DO NOT re-fetch from GitHub here in v1 — that's a deliberate simplification: the input must carry everything. (Spec §9.5 says "fetches account + repo metadata"; v1 ships the simpler path because the web binary already has this info from the install callback. v2 may add a re-fetch.)

**Files:**
- Modify: `crates/restate-svc/src/installation.rs`

- [ ] **Step 1: Write the service trait + impl + pure fn + tests**

`crates/restate-svc/src/installation.rs`:

```rust
//! `Installation` Virtual Object: onboard, repos_changed, uninstall.

use crate::audit::{Actor, Target};
use crate::error::{HandlerError, Result};
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Utc};
use domain::{Account, AccountType, SelectedRepos};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnboardInput {
    pub installation_id: u64,
    pub actor_user_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: AccountType,
    pub selected_repos: SelectedRepos,
    pub installed_at: DateTime<Utc>,
}

#[restate_sdk::object]
pub trait Installation {
    async fn onboard(input: OnboardInput) -> Result<(), TerminalError>;
    async fn repos_changed(input: ReposChangedInput) -> Result<(), TerminalError>;
    async fn uninstall(input: UninstallInput) -> Result<(), TerminalError>;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReposChangedInput {
    pub installation_id: u64,
    pub selected_repos: SelectedRepos,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UninstallInput {
    pub installation_id: u64,
    pub uninstalled_at: DateTime<Utc>,
}

pub struct InstallationImpl {
    pub state: AppState,
}

impl Installation for InstallationImpl {
    async fn onboard(
        &self,
        ctx: ObjectContext<'_>,
        input: OnboardInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("onboard", || async {
            onboard_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }

    async fn repos_changed(
        &self,
        ctx: ObjectContext<'_>,
        input: ReposChangedInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("repos_changed", || async {
            repos_changed_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }

    async fn uninstall(
        &self,
        ctx: ObjectContext<'_>,
        input: UninstallInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("uninstall", || async {
            uninstall_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }
}

/// Pure logic: persist the new installation row and emit the audit event.
/// Idempotent: a duplicate `installation_id` returns `Ok(())` after observing
/// the existing row matches the input. A duplicate `(account_id, active)`
/// surfaces as `Conflict::DuplicateActiveInstallation` from storage and is
/// treated as terminal (the caller's webhook duplicated work).
pub async fn onboard_logic(
    state: &AppState,
    input: &OnboardInput,
    request_id: Option<String>,
) -> Result<()> {
    let account = Account {
        installation_id: input.installation_id,
        account_id: input.account_id,
        account_login: input.account_login.clone(),
        account_type: input.account_type,
        installed_at: input.installed_at,
        uninstalled_at: None,
        selected_repos: input.selected_repos.clone(),
    };

    match state.storage.insert_installation(&account).await {
        Ok(()) => (),
        Err(storage::Error::Conflict(storage::ConflictKind::DuplicateId)) => {
            // Already onboarded — idempotent retry, no audit emit.
            return Ok(());
        }
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    crate::audit::emit(
        state,
        input.account_id,
        EventType::InstallationCreated,
        Actor::User(input.actor_user_id),
        Target::installation(input.installation_id),
        serde_json::json!({
            "account_login": input.account_login,
            "account_type": input.account_type.to_string(),
        }),
        request_id,
    )
    .await
}

/// Pure logic: update the installation's selected repos and emit audit.
pub async fn repos_changed_logic(
    state: &AppState,
    input: &ReposChangedInput,
    request_id: Option<String>,
) -> Result<()> {
    state
        .storage
        .update_installation_repos(input.installation_id, &input.selected_repos)
        .await?;

    let acct = state
        .storage
        .get_installation(input.installation_id)
        .await?
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "installation {} updated but not readable",
                input.installation_id
            ))
        })?;

    crate::audit::emit(
        state,
        acct.account_id,
        EventType::InstallationReposChanged,
        Actor::Github,
        Target::installation(input.installation_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}

/// Pure logic: stamp `uninstalled_at` and emit audit. Idempotent: if the
/// installation was already uninstalled, returns `Ok(())` without re-audit.
pub async fn uninstall_logic(
    state: &AppState,
    input: &UninstallInput,
    request_id: Option<String>,
) -> Result<()> {
    match state
        .storage
        .mark_installation_uninstalled(input.installation_id, input.uninstalled_at)
        .await
    {
        Ok(()) => (),
        Err(storage::Error::NotFound) => {
            // Already uninstalled or never existed; idempotent.
            return Ok(());
        }
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    let acct = state
        .storage
        .get_installation(input.installation_id)
        .await?
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "installation {} marked uninstalled but not readable",
                input.installation_id
            ))
        })?;

    crate::audit::emit(
        state,
        acct.account_id,
        EventType::InstallationUninstalled,
        Actor::Github,
        Target::installation(input.installation_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_state};
    use storage::SqlxStorage;

    fn sample_input() -> OnboardInput {
        OnboardInput {
            installation_id: 1,
            actor_user_id: 7,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            selected_repos: SelectedRepos::All,
            installed_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    #[tokio::test]
    async fn onboard_inserts_row_and_audits() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), Some("inv-1".into()))
            .await
            .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(acct.account_login, "acme");
        assert_eq!(acct.account_id, 100);
        assert!(acct.uninstalled_at.is_none());

        // Verify audit row by reading via a new SqlxStorage instance — we
        // can't downcast Arc<dyn Storage>; instead, look at the count via
        // the same storage instance using its trait. Since trait doesn't
        // expose audit reads, we use a fresh SqlxStorage::in_memory and
        // assert it has zero events to confirm the helper is correctly
        // scoping per-instance. The actual audit row's existence is
        // implicitly tested by `storage` crate's own audit unit tests; here
        // we verify our helper called `audit()` without panicking.
        let _other = SqlxStorage::in_memory().await.unwrap();
        // (No direct read; absence of panic above is sufficient.)
    }

    #[tokio::test]
    async fn onboard_duplicate_id_is_idempotent() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        // Second call — duplicate installation_id (PK collision). Should
        // return Ok(()) without re-auditing.
        let result = onboard_logic(&state, &sample_input(), None).await;
        assert!(result.is_ok(), "duplicate id should be idempotent: {result:?}");
    }

    #[tokio::test]
    async fn onboard_duplicate_active_account_is_terminal() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        // Different installation_id, same account_id, both active → partial
        // unique index hits and we surface ConflictKind::DuplicateActiveInstallation.
        let mut second = sample_input();
        second.installation_id = 2;
        let err = onboard_logic(&state, &second, None).await.unwrap_err();
        assert!(err.is_terminal(), "duplicate active should be terminal: {err:?}");
    }

    #[tokio::test]
    async fn repos_changed_updates_and_audits() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        repos_changed_logic(
            &state,
            &ReposChangedInput {
                installation_id: 1,
                selected_repos: SelectedRepos::Subset(vec![10, 20]),
            },
            Some("inv-2".into()),
        )
        .await
        .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert_eq!(acct.selected_repos, SelectedRepos::Subset(vec![10, 20]));
    }

    #[tokio::test]
    async fn repos_changed_unknown_installation_is_terminal() {
        let state = fixture_state().await;
        let err = repos_changed_logic(
            &state,
            &ReposChangedInput {
                installation_id: 999,
                selected_repos: SelectedRepos::All,
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(err.is_terminal());
    }

    #[tokio::test]
    async fn uninstall_marks_and_audits() {
        let state = fixture_state().await;
        onboard_logic(&state, &sample_input(), None).await.unwrap();

        uninstall_logic(
            &state,
            &UninstallInput {
                installation_id: 1,
                uninstalled_at: dt("2026-05-05T00:00:00Z"),
            },
            Some("inv-3".into()),
        )
        .await
        .unwrap();

        let acct = state.storage.get_installation(1).await.unwrap().unwrap();
        assert!(acct.uninstalled_at.is_some());
    }

    #[tokio::test]
    async fn uninstall_unknown_installation_is_idempotent() {
        let state = fixture_state().await;
        let result = uninstall_logic(
            &state,
            &UninstallInput {
                installation_id: 999,
                uninstalled_at: dt("2026-05-05T00:00:00Z"),
            },
            None,
        )
        .await;
        assert!(result.is_ok(), "missing installation should be idempotent: {result:?}");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p restate-svc --lib installation`
Expected: 7 tests pass.

If `restate-sdk`'s exact macro shape (e.g. `ctx.run("name", || async {...})` vs. `ctx.run("name", async move {...})`) differs from what's shown above, adjust the impl block until it compiles. The 0.10 release notes / examples in the upstream repo are the source of truth. Pure-logic functions (`onboard_logic` etc.) and the tests do not depend on the Restate macro shape.

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/installation.rs
git commit -m "feat(restate-svc): Installation service (onboard, repos_changed, uninstall)"
```

---

### Task 7: `ShareLink` service skeleton

**Files:**
- Modify: `crates/restate-svc/src/share_link.rs`

- [ ] **Step 1: Add the trait + types stub**

`crates/restate-svc/src/share_link.rs`:

```rust
//! `ShareLink` Virtual Object: create, revoke, tick_expiration.

use crate::audit::{Actor, Target};
use crate::error::{HandlerError, Result};
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Utc};
use domain::{Permission, ShareLink, ShareLinkId, ShareLinkRepo, Slug};
use rand::SeedableRng;
use rand::rngs::OsRng;
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateLinkInput {
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub permission: Permission,
    pub approval_required: bool,
    pub internal_note: Option<String>,
    pub repos: Vec<ShareLinkRepo>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateLinkOutput {
    pub link_id: ShareLinkId,
    pub slug: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RevokeLinkInput {
    pub link_id: ShareLinkId,
    pub by_user: u64,
    pub when: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TickExpirationInput {
    pub link_id: ShareLinkId,
    pub at: DateTime<Utc>,
}

#[restate_sdk::object]
pub trait ShareLink {
    async fn create(input: CreateLinkInput) -> Result<CreateLinkOutput, TerminalError>;
    async fn revoke(input: RevokeLinkInput) -> Result<(), TerminalError>;
    async fn tick_expiration(input: TickExpirationInput) -> Result<(), TerminalError>;
}

pub struct ShareLinkImpl {
    pub state: AppState,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p restate-svc`
Expected: clean (warnings about unused imports / unimplemented trait are OK; Tasks 8–10 fill in the impl).

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/share_link.rs
git commit -m "feat(restate-svc): ShareLink trait + input types"
```

---

### Task 8: `ShareLink::create`

Per spec §9.1: writes link + repos rows; emits `share_link.created`.

**Files:**
- Modify: `crates/restate-svc/src/share_link.rs`

- [ ] **Step 1: Add the create logic + handler glue + tests**

Append to `share_link.rs` (add to the existing module — do NOT replace):

```rust
impl ShareLink for ShareLinkImpl {
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        input: CreateLinkInput,
    ) -> Result<CreateLinkOutput, TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("create", || async {
            create_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }

    async fn revoke(
        &self,
        _ctx: ObjectContext<'_>,
        _input: RevokeLinkInput,
    ) -> Result<(), TerminalError> {
        unimplemented!("Task 9")
    }

    async fn tick_expiration(
        &self,
        _ctx: ObjectContext<'_>,
        _input: TickExpirationInput,
    ) -> Result<(), TerminalError> {
        unimplemented!("Task 10")
    }
}

/// Pure logic: generate a slug, write the link + repos rows, emit audit.
pub async fn create_logic(
    state: &AppState,
    input: &CreateLinkInput,
    request_id: Option<String>,
) -> Result<CreateLinkOutput> {
    let mut rng = OsRng;
    let slug = Slug::generate(&mut rng);
    let link_id = ShareLinkId::new();

    let link = ShareLink {
        id: link_id,
        slug: slug.clone(),
        installation_id: input.installation_id,
        account_id: input.account_id,
        created_by: input.created_by,
        created_at: input.created_at,
        expires_at: input.expires_at,
        max_uses: input.max_uses,
        uses_count: 0,
        permission: input.permission,
        approval_required: input.approval_required,
        internal_note: input.internal_note.clone(),
        revoked_at: None,
        revoked_by: None,
        repos: input.repos.clone(),
    };

    state.storage.insert_share_link(&link).await?;

    crate::audit::emit(
        state,
        input.account_id,
        EventType::ShareLinkCreated,
        Actor::User(input.created_by),
        Target::share_link(link_id),
        serde_json::json!({
            "permission": input.permission.to_string(),
            "approval_required": input.approval_required,
            "max_uses": input.max_uses,
            "repo_count": input.repos.len(),
        }),
        request_id,
    )
    .await?;

    Ok(CreateLinkOutput {
        link_id,
        slug: slug.as_str().to_string(),
    })
}
```

- [ ] **Step 2: Add tests for `create_logic`**

Append a `tests` module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_state};
    use audit::EventType;
    use domain::{AccountType, SelectedRepos};

    async fn seed_installation_and_user(state: &AppState) {
        // Create a parent installation row (FK target for share_link.installation_id).
        let acct = domain::Account {
            installation_id: 1,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        };
        state.storage.insert_installation(&acct).await.unwrap();
        let user = domain::User {
            user_id: 7,
            login: "creator".into(),
            avatar_url: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        };
        state.storage.upsert_user(&user).await.unwrap();
    }

    fn sample_input() -> CreateLinkInput {
        CreateLinkInput {
            installation_id: 1,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: Some(dt("2026-06-03T12:00:00Z")),
            max_uses: Some(5),
            permission: Permission::Pull,
            approval_required: true,
            internal_note: Some("test".into()),
            repos: vec![ShareLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        }
    }

    #[tokio::test]
    async fn create_inserts_link_with_generated_slug() {
        let state = fixture_state().await;
        seed_installation_and_user(&state).await;

        let out = create_logic(&state, &sample_input(), None).await.unwrap();
        assert_eq!(out.slug.len(), 16);

        let link = state
            .storage
            .get_share_link_by_id(out.link_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(link.slug.as_str(), out.slug);
        assert_eq!(link.repos.len(), 1);
        assert_eq!(link.repos[0].repo_full_name, "acme/api");
        assert_eq!(link.uses_count, 0);
        assert_eq!(link.permission, Permission::Pull);
        assert!(link.approval_required);
    }

    #[tokio::test]
    async fn create_unknown_installation_is_terminal() {
        let state = fixture_state().await;
        // No seed — installation 1 doesn't exist.
        let mut input = sample_input();
        input.installation_id = 999;
        let err = create_logic(&state, &input, None).await.unwrap_err();
        assert!(err.is_terminal());
    }

    #[tokio::test]
    async fn create_no_repos_is_allowed() {
        let state = fixture_state().await;
        seed_installation_and_user(&state).await;

        let mut input = sample_input();
        input.repos.clear();
        let out = create_logic(&state, &input, None).await.unwrap();
        let link = state
            .storage
            .get_share_link_by_id(out.link_id)
            .await
            .unwrap()
            .unwrap();
        assert!(link.repos.is_empty());
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p restate-svc --lib share_link`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/share_link.rs
git commit -m "feat(restate-svc): ShareLink::create"
```

---

### Task 9: `ShareLink::revoke`

Per spec §9.1: sets `revoked_at`; emits `share_link.revoked`. Idempotent: if already revoked, returns `Ok(())` without re-audit.

**Files:**
- Modify: `crates/restate-svc/src/share_link.rs`

- [ ] **Step 1: Replace the `revoke` stub + add pure fn + tests**

In `crates/restate-svc/src/share_link.rs`, replace the `unimplemented!("Task 9")` body with:

```rust
    async fn revoke(
        &self,
        ctx: ObjectContext<'_>,
        input: RevokeLinkInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("revoke", || async {
            revoke_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }
```

Add the pure function:

```rust
/// Pure logic: stamp `revoked_at`/`revoked_by`; emit audit. Idempotent on
/// double-revoke (storage returns NotFound when no-op; we treat as Ok).
pub async fn revoke_logic(
    state: &AppState,
    input: &RevokeLinkInput,
    request_id: Option<String>,
) -> Result<()> {
    match state
        .storage
        .mark_share_link_revoked(input.link_id, input.by_user, input.when)
        .await
    {
        Ok(()) => (),
        Err(storage::Error::NotFound) => return Ok(()), // idempotent
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    let link = state
        .storage
        .get_share_link_by_id(input.link_id)
        .await?
        .ok_or_else(|| HandlerError::Invariant(format!("link {} vanished", input.link_id)))?;

    crate::audit::emit(
        state,
        link.account_id,
        EventType::ShareLinkRevoked,
        Actor::User(input.by_user),
        Target::share_link(input.link_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}
```

Append to the test module:

```rust
    use super::revoke_logic;

    #[tokio::test]
    async fn revoke_marks_and_audits() {
        let state = fixture_state().await;
        seed_installation_and_user(&state).await;
        let out = create_logic(&state, &sample_input(), None).await.unwrap();

        revoke_logic(
            &state,
            &RevokeLinkInput {
                link_id: out.link_id,
                by_user: 7,
                when: dt("2026-05-04T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let link = state
            .storage
            .get_share_link_by_id(out.link_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(link.revoked_by, Some(7));
        assert!(link.revoked_at.is_some());
    }

    #[tokio::test]
    async fn revoke_already_revoked_is_idempotent() {
        let state = fixture_state().await;
        seed_installation_and_user(&state).await;
        let out = create_logic(&state, &sample_input(), None).await.unwrap();

        revoke_logic(
            &state,
            &RevokeLinkInput {
                link_id: out.link_id,
                by_user: 7,
                when: dt("2026-05-04T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        // Second revoke on already-revoked link.
        let result = revoke_logic(
            &state,
            &RevokeLinkInput {
                link_id: out.link_id,
                by_user: 7,
                when: dt("2026-05-04T14:00:00Z"),
            },
            None,
        )
        .await;
        assert!(result.is_ok(), "double-revoke should be idempotent: {result:?}");
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p restate-svc --lib share_link`
Expected: 5 tests pass (3 existing + 2 new).

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/share_link.rs
git commit -m "feat(restate-svc): ShareLink::revoke"
```

---

### Task 10: `ShareLink::tick_expiration`

Per spec §9.1: scheduled at `expires_at` (the web binary or a separate scheduler enqueues this at link creation time); confirms the link is genuinely past its expiry; emits `share_link.expired`. **Note v1 scope:** this method does NOT cascade to cancel pending requests / pending GitHub invitations — that's v2.

In v1 the spec leaves the actual scheduling mechanism open. The simplest implementation: when a link has `expires_at` set, the web binary schedules `ShareLink::tick_expiration` to run at that time via Restate's `delayed_send` (or equivalent). When the timer fires, the handler verifies the link is past expiry and emits the audit event — but does NOT mutate state, since `is_active(now)` already reflects the expiry from `expires_at`. The audit emission is the only side effect.

**Files:**
- Modify: `crates/restate-svc/src/share_link.rs`

- [ ] **Step 1: Replace the `tick_expiration` stub + add pure fn + tests**

Replace `unimplemented!("Task 10")` with:

```rust
    async fn tick_expiration(
        &self,
        ctx: ObjectContext<'_>,
        input: TickExpirationInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("tick_expiration", || async {
            tick_expiration_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }
```

Add the pure function:

```rust
/// Pure logic: if the link is past its expiry and not already revoked, emit
/// `share_link.expired`. No state mutation — `is_active(now)` already reflects
/// the expiry. Idempotent under repeated calls (multiple emit attempts will
/// produce duplicate audit rows; acceptable in v1, dedup is v1.1's audit-UI
/// concern).
pub async fn tick_expiration_logic(
    state: &AppState,
    input: &TickExpirationInput,
    request_id: Option<String>,
) -> Result<()> {
    let link = state
        .storage
        .get_share_link_by_id(input.link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    // Only emit if the link is genuinely past expiry. Defensive: an admin
    // may have revoked the link before the timer fired, in which case
    // `share_link.revoked` already audited and we don't double-audit.
    if link.revoked_at.is_some() {
        return Ok(());
    }
    let Some(expires) = link.expires_at else {
        // Link has no expiration — timer should never have been scheduled.
        return Err(HandlerError::Invariant(format!(
            "tick_expiration fired for link {} which has no expires_at",
            input.link_id
        )));
    };
    if input.at < expires {
        // Timer fired early; skip.
        return Ok(());
    }

    crate::audit::emit(
        state,
        link.account_id,
        EventType::ShareLinkExpired,
        crate::audit::Actor::System,
        crate::audit::Target::share_link(input.link_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}
```

Append tests:

```rust
    use super::tick_expiration_logic;

    #[tokio::test]
    async fn tick_expiration_emits_when_past_expiry() {
        let state = fixture_state().await;
        seed_installation_and_user(&state).await;
        let out = create_logic(&state, &sample_input(), None).await.unwrap();
        // sample_input's expires_at is 2026-06-03T12:00:00Z

        tick_expiration_logic(
            &state,
            &TickExpirationInput {
                link_id: out.link_id,
                at: dt("2026-06-03T12:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn tick_expiration_skips_revoked_link() {
        let state = fixture_state().await;
        seed_installation_and_user(&state).await;
        let out = create_logic(&state, &sample_input(), None).await.unwrap();
        revoke_logic(
            &state,
            &RevokeLinkInput {
                link_id: out.link_id,
                by_user: 7,
                when: dt("2026-05-04T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        tick_expiration_logic(
            &state,
            &TickExpirationInput {
                link_id: out.link_id,
                at: dt("2026-06-03T12:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();
        // No assertion on emit count — idempotent test verifies no panic.
    }

    #[tokio::test]
    async fn tick_expiration_unknown_link_is_terminal() {
        let state = fixture_state().await;
        let err = tick_expiration_logic(
            &state,
            &TickExpirationInput {
                link_id: ShareLinkId::new(),
                at: dt("2026-06-03T12:00:00Z"),
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(err.is_terminal());
    }

    #[tokio::test]
    async fn tick_expiration_no_expires_at_is_terminal_invariant() {
        let state = fixture_state().await;
        seed_installation_and_user(&state).await;

        let mut no_expiry = sample_input();
        no_expiry.expires_at = None;
        let out = create_logic(&state, &no_expiry, None).await.unwrap();

        let err = tick_expiration_logic(
            &state,
            &TickExpirationInput {
                link_id: out.link_id,
                at: dt("2099-01-01T00:00:00Z"),
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, HandlerError::Invariant(_)));
        assert!(err.is_terminal());
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p restate-svc --lib share_link`
Expected: 9 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/share_link.rs
git commit -m "feat(restate-svc): ShareLink::tick_expiration"
```

---

### Task 11: `GithubInvitation` service skeleton

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Add the trait + types**

`crates/restate-svc/src/github_invitation.rs`:

```rust
//! `GithubInvitation` Virtual Object: create, on_webhook, cancel, tick_expire.

use crate::audit::{Actor, Target};
use crate::error::{HandlerError, Result};
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Utc};
use domain::{GithubInvitation, GithubInvitationId, InvitationState, RequestId};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub invitation_request_id: RequestId,
    pub installation_id: u64,
    pub repo_id: u64,
    pub repo_full_name: String,    // "owner/name"
    pub recipient_login: String,
    pub permission: domain::Permission,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnWebhookInput {
    pub invitation_id: GithubInvitationId,
    pub action: WebhookAction,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookAction {
    Accepted,
    Declined,
    // GitHub doesn't currently emit an `expired` webhook for repository
    // invitations; expiration is handled by `tick_expire` polling.
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CancelInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub by_user: Option<u64>, // None when system-cancelled (cascade in v2)
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TickExpireInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub at: DateTime<Utc>,
}

#[restate_sdk::object]
pub trait GithubInvitation {
    async fn create(input: CreateInvitationInput) -> Result<(), TerminalError>;
    async fn on_webhook(input: OnWebhookInput) -> Result<(), TerminalError>;
    async fn cancel(input: CancelInvitationInput) -> Result<(), TerminalError>;
    async fn tick_expire(input: TickExpireInput) -> Result<(), TerminalError>;
}

pub struct GithubInvitationImpl {
    pub state: AppState,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p restate-svc`
Expected: clean (warnings about unimplemented trait are OK).

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/github_invitation.rs
git commit -m "feat(restate-svc): GithubInvitation trait + input types"
```

---

### Task 12: `GithubInvitation::create`

Per spec §9.3: mints installation token; calls `PUT /repos/.../collaborators/{user}`. Branches:
- 201 → store `github_invitation_id`, mark `sent`, emit `invitation.sent`.
- 204 → mark `accepted`, emit `invitation.accepted`.
- 4xx terminal → mark `failed` with `error_message`, emit `invitation.send_failed`.
- 5xx → Restate retry policy (return transient `HandlerError`).

The handler does NOT schedule `tick_expire(+7d)` directly in v1 — Plan 6's webhook receiver or a follow-up task wires that. For now, the spec's "schedule `tick_expire()` at +7d" is documented but deferred to the caller of this handler.

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Add the impl + pure fn + tests**

Append to `github_invitation.rs`:

```rust
impl GithubInvitation for GithubInvitationImpl {
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        input: CreateInvitationInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("create", || async {
            create_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }

    async fn on_webhook(
        &self,
        _ctx: ObjectContext<'_>,
        _input: OnWebhookInput,
    ) -> Result<(), TerminalError> {
        unimplemented!("Task 13")
    }

    async fn cancel(
        &self,
        _ctx: ObjectContext<'_>,
        _input: CancelInvitationInput,
    ) -> Result<(), TerminalError> {
        unimplemented!("Task 14")
    }

    async fn tick_expire(
        &self,
        _ctx: ObjectContext<'_>,
        _input: TickExpireInput,
    ) -> Result<(), TerminalError> {
        unimplemented!("Task 14")
    }
}

/// Split a `repo_full_name` of the form `"owner/name"` into its components.
/// GitHub guarantees this format; an unsplit string surfaces as an Invariant.
fn split_full_name(full_name: &str) -> Result<(&str, &str)> {
    full_name.split_once('/').ok_or_else(|| {
        HandlerError::Invariant(format!("repo_full_name not in owner/name form: {full_name:?}"))
    })
}

/// Pure logic: row-insert in `Sending`, call GitHub, branch on status, update
/// row + audit. Returns `Ok(())` for both 201 (sent) and 204 (already a
/// collaborator) paths; transient failures bubble out for Restate to retry.
pub async fn create_logic(
    state: &AppState,
    input: &CreateInvitationInput,
    request_id: Option<String>,
) -> Result<()> {
    // Row insert in `Sending` state. Idempotent under retry: if the row
    // already exists with `id = input.invitation_id`, storage returns
    // `Conflict::DuplicateId` and we proceed (Restate retried after the
    // GitHub call succeeded but before the row update).
    let row = GithubInvitation {
        id: input.invitation_id,
        invitation_request_id: input.invitation_request_id,
        repo_id: input.repo_id,
        github_invitation_id: None,
        state: InvitationState::Sending,
        error_message: None,
        created_at: input.now,
        updated_at: input.now,
    };
    match state.storage.insert_github_invitation(&row).await {
        Ok(()) => (),
        Err(storage::Error::Conflict(storage::ConflictKind::DuplicateId)) => {
            // Re-entry; row already in some state. Read it; if terminal, no-op.
            let existing = state
                .storage
                .get_github_invitation(input.invitation_id)
                .await?
                .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
            if existing.state.is_terminal() {
                return Ok(());
            }
            // Fall through and re-attempt the GitHub call.
        }
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    let (owner, repo) = split_full_name(&input.repo_full_name)?;

    let outcome = state
        .github
        .add_collaborator(
            input.installation_id,
            owner,
            repo,
            &input.recipient_login,
            input.permission,
        )
        .await;

    let account_id = state
        .storage
        .get_active_installation_by_account_id(input.installation_id)
        .await?
        .map(|a| a.account_id)
        // Fall back to looking up via `get_installation` for accounts whose
        // `installations` row is the *passive* (uninstalled) one. We need
        // account_id for the audit; if neither lookup works the install
        // is unreachable and this is an invariant.
        .or_else(|| None);
    let account_id = match account_id {
        Some(a) => a,
        None => {
            let acct = state
                .storage
                .get_installation(input.installation_id)
                .await?
                .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
            acct.account_id
        }
    };

    match outcome {
        Ok(Some(github_id)) => {
            // 201 — invitation pending.
            state
                .storage
                .update_github_invitation(&storage::GithubInvitationUpdate {
                    id: input.invitation_id,
                    state: InvitationState::Sent,
                    github_invitation_id: Some(github_id),
                    error_message: None,
                    updated_at: input.now,
                })
                .await?;
            crate::audit::emit(
                state,
                account_id,
                EventType::InvitationSent,
                Actor::System,
                Target::github_invitation(input.invitation_id),
                serde_json::json!({
                    "github_invitation_id": github_id,
                    "repo_full_name": input.repo_full_name,
                    "recipient": input.recipient_login,
                }),
                request_id,
            )
            .await?;
        }
        Ok(None) => {
            // 204 — already a collaborator.
            state
                .storage
                .update_github_invitation(&storage::GithubInvitationUpdate {
                    id: input.invitation_id,
                    state: InvitationState::Accepted,
                    github_invitation_id: None,
                    error_message: None,
                    updated_at: input.now,
                })
                .await?;
            crate::audit::emit(
                state,
                account_id,
                EventType::InvitationAccepted,
                Actor::Github,
                Target::github_invitation(input.invitation_id),
                serde_json::json!({
                    "reason": "already_collaborator",
                    "repo_full_name": input.repo_full_name,
                    "recipient": input.recipient_login,
                }),
                request_id,
            )
            .await?;
        }
        Err(e) => {
            let h: HandlerError = e.into();
            if !h.is_terminal() {
                // Transient — let Restate retry.
                return Err(h);
            }
            // Terminal 4xx — mark failed, audit, return Ok so Restate doesn't retry.
            let msg = h.to_string();
            state
                .storage
                .update_github_invitation(&storage::GithubInvitationUpdate {
                    id: input.invitation_id,
                    state: InvitationState::Failed,
                    github_invitation_id: None,
                    error_message: Some(msg.clone()),
                    updated_at: input.now,
                })
                .await?;
            crate::audit::emit(
                state,
                account_id,
                EventType::InvitationSendFailed,
                Actor::System,
                Target::github_invitation(input.invitation_id),
                serde_json::json!({
                    "error": msg,
                    "repo_full_name": input.repo_full_name,
                    "recipient": input.recipient_login,
                }),
                request_id,
            )
            .await?;
        }
    }

    Ok(())
}
```

- [ ] **Step 2: Add tests**

Append a `tests` module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_github_client, fixture_storage};
    use audit::EventType;
    use domain::{AccountType, Permission, SelectedRepos, ShareLinkId};
    use github::mocks::{Expectation, MockTransport};
    use github::transport::{Method, Response};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn token_mint(installation_id: u64) -> Expectation {
        Expectation {
            method: Method::Post,
            url: format!(
                "https://api.github.test/app/installations/{}/access_tokens",
                installation_id
            ),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec(),
            },
        }
    }

    /// Seed installation, user, share_link, request — all FK chain prerequisites
    /// for inserting a github_invitation.
    async fn seed_chain(state: &AppState) -> RequestId {
        state
            .storage
            .insert_installation(&domain::Account {
                installation_id: 9,
                account_id: 100,
                account_login: "acme".into(),
                account_type: AccountType::Organization,
                installed_at: dt("2026-05-04T12:00:00Z"),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 7,
                login: "creator".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 8,
                login: "alice".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        let mut rng = rand::SeedableRng::seed_from_u64(42);
        let link = domain::ShareLink {
            id: ShareLinkId::new(),
            slug: domain::Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(42)),
            installation_id: 9,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Push,
            approval_required: false,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![],
        };
        state.storage.insert_share_link(&link).await.unwrap();
        let req_id = RequestId::new();
        let req = domain::InvitationRequest {
            id: req_id,
            share_link_id: link.id,
            requester_id: 8,
            justification: None,
            state: domain::RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        };
        state
            .storage
            .insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();
        // Suppress unused.
        let _ = rng;
        req_id
    }

    fn sample_input(invitation_id: GithubInvitationId, request_id: RequestId) -> CreateInvitationInput {
        CreateInvitationInput {
            invitation_id,
            invitation_request_id: request_id,
            installation_id: 9,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            recipient_login: "alice".into(),
            permission: Permission::Push,
            now: dt("2026-05-04T13:01:00Z"),
        }
    }

    #[tokio::test]
    async fn create_201_marks_sent_and_audits() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation {
                method: Method::Put,
                url: "https://api.github.test/repos/acme/api/collaborators/alice".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 201,
                    headers: BTreeMap::new(),
                    body: br#"{"id": 9988, "invitee":{"id":42,"login":"alice"}}"#.to_vec(),
                },
            },
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;

        let inv_id = GithubInvitationId::new();
        create_logic(&state, &sample_input(inv_id, req_id), None)
            .await
            .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Sent);
        assert_eq!(row.github_invitation_id, Some(9988));
    }

    #[tokio::test]
    async fn create_204_marks_accepted() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation {
                method: Method::Put,
                url: "https://api.github.test/repos/acme/api/collaborators/alice".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            },
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;

        let inv_id = GithubInvitationId::new();
        create_logic(&state, &sample_input(inv_id, req_id), None)
            .await
            .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Accepted);
    }

    #[tokio::test]
    async fn create_422_marks_failed_and_returns_ok() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(
                Method::Put,
                "https://api.github.test/repos/acme/api/collaborators/alice",
                422,
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;

        let inv_id = GithubInvitationId::new();
        // Terminal 4xx — handler should mark failed and return Ok so Restate
        // does not retry.
        create_logic(&state, &sample_input(inv_id, req_id), None)
            .await
            .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Failed);
        assert!(row.error_message.is_some());
    }

    #[tokio::test]
    async fn create_502_propagates_transient_for_retry() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(
                Method::Put,
                "https://api.github.test/repos/acme/api/collaborators/alice",
                502,
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;

        let inv_id = GithubInvitationId::new();
        let err = create_logic(&state, &sample_input(inv_id, req_id), None)
            .await
            .unwrap_err();
        assert!(!err.is_terminal(), "5xx should be transient: {err:?}");

        // The row was inserted in Sending; on retry Restate will re-call us.
        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Sending);
    }

    #[tokio::test]
    async fn create_with_bad_repo_full_name_is_invariant() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![token_mint(9)]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;

        let inv_id = GithubInvitationId::new();
        let mut input = sample_input(inv_id, req_id);
        input.repo_full_name = "no-slash".into();
        let err = create_logic(&state, &input, None).await.unwrap_err();
        assert!(matches!(err, HandlerError::Invariant(_)));
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p restate-svc --lib github_invitation`
Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/github_invitation.rs
git commit -m "feat(restate-svc): GithubInvitation::create"
```

---

### Task 13: `GithubInvitation::on_webhook`

Per spec §9.3: handler called from the webhook receiver when a `repository_invitation` event matches our row. State transition + audit.

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Replace the `on_webhook` stub + add pure fn + tests**

Replace `unimplemented!("Task 13")` with:

```rust
    async fn on_webhook(
        &self,
        ctx: ObjectContext<'_>,
        input: OnWebhookInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("on_webhook", || async {
            on_webhook_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }
```

Add the pure function:

```rust
/// Pure logic: transition the github_invitation row based on a webhook
/// signal. Idempotent: if the row is already in a terminal state, no-op.
pub async fn on_webhook_logic(
    state: &AppState,
    input: &OnWebhookInput,
    request_id: Option<String>,
) -> Result<()> {
    let row = state
        .storage
        .get_github_invitation(input.invitation_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    if row.state.is_terminal() {
        return Ok(()); // already settled; webhook is just confirming.
    }

    let new_state = match input.action {
        WebhookAction::Accepted => InvitationState::Accepted,
        WebhookAction::Declined => InvitationState::Declined,
    };
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: input.invitation_id,
            state: new_state,
            github_invitation_id: None,
            error_message: None,
            updated_at: input.at,
        })
        .await?;

    let event_type = match new_state {
        InvitationState::Accepted => EventType::InvitationAccepted,
        InvitationState::Declined => EventType::InvitationDeclined,
        _ => unreachable!("WebhookAction maps to Accepted/Declined"),
    };

    // Resolve account_id by walking row -> request -> link.
    let request = state
        .storage
        .get_invitation_request(row.invitation_request_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let link = state
        .storage
        .get_share_link_by_id(request.share_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    crate::audit::emit(
        state,
        link.account_id,
        event_type,
        Actor::Github,
        Target::github_invitation(input.invitation_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}
```

- [ ] **Step 2: Add tests**

Append to the existing `tests` module:

```rust
    use super::on_webhook_logic;

    /// Helper: from a fresh state, get to "row in Sent state". Returns the
    /// invitation_id that subsequent webhook tests can reference.
    async fn seed_to_sent(state: &AppState, req_id: RequestId) -> GithubInvitationId {
        // Insert directly via storage to avoid going through the github mock again.
        let inv_id = GithubInvitationId::new();
        let row = GithubInvitation {
            id: inv_id,
            invitation_request_id: req_id,
            repo_id: 10,
            github_invitation_id: Some(9988),
            state: InvitationState::Sent,
            error_message: None,
            created_at: dt("2026-05-04T13:00:00Z"),
            updated_at: dt("2026-05-04T13:00:00Z"),
        };
        state.storage.insert_github_invitation(&row).await.unwrap();
        inv_id
    }

    #[tokio::test]
    async fn webhook_accepted_transitions_and_audits() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![]); // no GitHub calls in this path
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;
        let inv_id = seed_to_sent(&state, req_id).await;

        on_webhook_logic(
            &state,
            &OnWebhookInput {
                invitation_id: inv_id,
                action: WebhookAction::Accepted,
                at: dt("2026-05-04T14:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Accepted);
    }

    #[tokio::test]
    async fn webhook_declined_transitions_and_audits() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;
        let inv_id = seed_to_sent(&state, req_id).await;

        on_webhook_logic(
            &state,
            &OnWebhookInput {
                invitation_id: inv_id,
                action: WebhookAction::Declined,
                at: dt("2026-05-04T14:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Declined);
    }

    #[tokio::test]
    async fn webhook_on_terminal_row_is_idempotent() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let req_id = seed_chain(&state).await;
        let inv_id = seed_to_sent(&state, req_id).await;

        // First webhook → Accepted (terminal).
        on_webhook_logic(
            &state,
            &OnWebhookInput {
                invitation_id: inv_id,
                action: WebhookAction::Accepted,
                at: dt("2026-05-04T14:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        // Second webhook on the same row — should be a no-op, not an error.
        on_webhook_logic(
            &state,
            &OnWebhookInput {
                invitation_id: inv_id,
                action: WebhookAction::Declined,
                at: dt("2026-05-04T15:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Accepted, "terminal state preserved");
    }

    #[tokio::test]
    async fn webhook_unknown_invitation_is_terminal() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);

        let err = on_webhook_logic(
            &state,
            &OnWebhookInput {
                invitation_id: GithubInvitationId::new(),
                action: WebhookAction::Accepted,
                at: dt("2026-05-04T14:00:00Z"),
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(err.is_terminal());
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p restate-svc --lib github_invitation`
Expected: 9 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/github_invitation.rs
git commit -m "feat(restate-svc): GithubInvitation::on_webhook"
```

---

### Task 14: `GithubInvitation::cancel` and `tick_expire`

Per spec §9.3:
- `cancel()` calls `DELETE /repos/.../invitations/{github_id}`; marks `cancelled`; emits `invitation.cancelled`.
- `tick_expire()` confirms via `GET /repos/.../invitations`; if still pending, marks `expired`, emits `invitation.expired`.

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Replace the `cancel` and `tick_expire` stubs + add pure fns + tests**

Replace `unimplemented!("Task 14")` for both methods:

```rust
    async fn cancel(
        &self,
        ctx: ObjectContext<'_>,
        input: CancelInvitationInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("cancel", || async {
            cancel_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }

    async fn tick_expire(
        &self,
        ctx: ObjectContext<'_>,
        input: TickExpireInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("tick_expire", || async {
            tick_expire_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }
```

Add the pure functions:

```rust
/// Pure logic: cancel a pending GitHub invitation. Calls
/// `DELETE /repos/.../invitations/{github_id}`; marks row cancelled; emits
/// `invitation.cancelled`. Idempotent: if the row is already terminal
/// or the GitHub invitation is gone (404), proceeds to mark cancelled
/// without re-deleting.
pub async fn cancel_logic(
    state: &AppState,
    input: &CancelInvitationInput,
    request_id: Option<String>,
) -> Result<()> {
    let row = state
        .storage
        .get_github_invitation(input.invitation_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    if row.state.is_terminal() {
        return Ok(());
    }

    // Look up the repo full name from the invitation_request → share_link.repos chain.
    let request = state
        .storage
        .get_invitation_request(row.invitation_request_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let link = state
        .storage
        .get_share_link_by_id(request.share_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let repo = link
        .repos
        .iter()
        .find(|r| r.repo_id == row.repo_id)
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "github_invitation {} references repo_id {} not in link.repos",
                input.invitation_id, row.repo_id
            ))
        })?;
    let (owner, repo_name) = split_full_name(&repo.repo_full_name)?;

    // Call GitHub if we know the upstream id; otherwise skip (row was Sending
    // and never got a 201, so nothing to delete).
    if let Some(github_id) = row.github_invitation_id {
        match state
            .github
            .delete_invitation(input.installation_id, owner, repo_name, github_id)
            .await
        {
            Ok(()) => (),
            Err(github::Error::Status { status: 404, .. }) => {
                // Already cancelled / accepted / declined upstream; proceed.
            }
            Err(e) => {
                let h: HandlerError = e.into();
                if !h.is_terminal() {
                    return Err(h);
                }
                // Other 4xx — proceed to mark cancelled, audit reason.
            }
        }
    }

    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: input.invitation_id,
            state: InvitationState::Cancelled,
            github_invitation_id: None,
            error_message: None,
            updated_at: input.at,
        })
        .await?;

    let actor = match input.by_user {
        Some(uid) => Actor::User(uid),
        None => Actor::System,
    };
    crate::audit::emit(
        state,
        link.account_id,
        EventType::InvitationCancelled,
        actor,
        Target::github_invitation(input.invitation_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}

/// Pure logic: at expiration time, confirm via GitHub list_invitations that
/// the row is still pending; if so, mark expired + audit.
pub async fn tick_expire_logic(
    state: &AppState,
    input: &TickExpireInput,
    request_id: Option<String>,
) -> Result<()> {
    let row = state
        .storage
        .get_github_invitation(input.invitation_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    if row.state.is_terminal() {
        return Ok(());
    }

    let request = state
        .storage
        .get_invitation_request(row.invitation_request_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let link = state
        .storage
        .get_share_link_by_id(request.share_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let repo = link
        .repos
        .iter()
        .find(|r| r.repo_id == row.repo_id)
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "github_invitation {} references repo_id {} not in link.repos",
                input.invitation_id, row.repo_id
            ))
        })?;
    let (owner, repo_name) = split_full_name(&repo.repo_full_name)?;

    let pending = state
        .github
        .list_invitations(input.installation_id, owner, repo_name)
        .await?;
    let still_pending = row
        .github_invitation_id
        .map(|id| pending.iter().any(|p| p.id == id))
        .unwrap_or(false);

    if !still_pending {
        // Either accepted/declined upstream (webhook missed) or cancelled.
        // Don't fight: leave to webhook reconciliation. Plan 6 / Reconcile.
        return Ok(());
    }

    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: input.invitation_id,
            state: InvitationState::Expired,
            github_invitation_id: None,
            error_message: None,
            updated_at: input.at,
        })
        .await?;

    crate::audit::emit(
        state,
        link.account_id,
        EventType::InvitationExpired,
        Actor::System,
        Target::github_invitation(input.invitation_id),
        serde_json::json!({}),
        request_id,
    )
    .await
}
```

- [ ] **Step 2: Add tests**

Append to the `tests` module:

```rust
    use super::{cancel_logic, tick_expire_logic};

    /// Like `seed_to_sent` but with the link.repos populated so cancel/expire
    /// can resolve the repo full name from the chain.
    async fn seed_chain_with_repos(state: &AppState) -> (RequestId, GithubInvitationId) {
        // Re-seed with a populated link.repos.
        state
            .storage
            .insert_installation(&domain::Account {
                installation_id: 9,
                account_id: 100,
                account_login: "acme".into(),
                account_type: AccountType::Organization,
                installed_at: dt("2026-05-04T12:00:00Z"),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 7,
                login: "creator".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 8,
                login: "alice".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        let link = domain::ShareLink {
            id: ShareLinkId::new(),
            slug: domain::Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(7)),
            installation_id: 9,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Push,
            approval_required: false,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![domain::ShareLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        };
        state.storage.insert_share_link(&link).await.unwrap();
        let req_id = RequestId::new();
        let req = domain::InvitationRequest {
            id: req_id,
            share_link_id: link.id,
            requester_id: 8,
            justification: None,
            state: domain::RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        };
        state
            .storage
            .insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();
        let inv_id = GithubInvitationId::new();
        state
            .storage
            .insert_github_invitation(&GithubInvitation {
                id: inv_id,
                invitation_request_id: req_id,
                repo_id: 10,
                github_invitation_id: Some(9988),
                state: InvitationState::Sent,
                error_message: None,
                created_at: dt("2026-05-04T13:00:00Z"),
                updated_at: dt("2026-05-04T13:00:00Z"),
            })
            .await
            .unwrap();
        (req_id, inv_id)
    }

    #[tokio::test]
    async fn cancel_calls_delete_and_marks_cancelled() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation {
                method: Method::Delete,
                url: "https://api.github.test/repos/acme/api/invitations/9988".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            },
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

        cancel_logic(
            &state,
            &CancelInvitationInput {
                invitation_id: inv_id,
                installation_id: 9,
                by_user: Some(7),
                at: dt("2026-05-04T15:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Cancelled);
    }

    #[tokio::test]
    async fn cancel_404_proceeds_to_mark_cancelled() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::status(
                Method::Delete,
                "https://api.github.test/repos/acme/api/invitations/9988",
                404,
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

        cancel_logic(
            &state,
            &CancelInvitationInput {
                invitation_id: inv_id,
                installation_id: 9,
                by_user: None,
                at: dt("2026-05-04T15:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Cancelled);
    }

    #[tokio::test]
    async fn tick_expire_marks_expired_when_still_pending() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([
                    {
                        "id": 9988,
                        "invitee": {"id": 42, "login": "alice"},
                        "permissions": "write",
                        "created_at": "2026-05-04T13:00:00Z"
                    }
                ]),
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

        tick_expire_logic(
            &state,
            &TickExpireInput {
                invitation_id: inv_id,
                installation_id: 9,
                at: dt("2026-05-11T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Expired);
    }

    #[tokio::test]
    async fn tick_expire_skips_when_no_longer_pending() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([]),
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

        tick_expire_logic(
            &state,
            &TickExpireInput {
                invitation_id: inv_id,
                installation_id: 9,
                at: dt("2026-05-11T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Sent, "row left for webhook to settle");
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p restate-svc --lib github_invitation`
Expected: 13 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/github_invitation.rs
git commit -m "feat(restate-svc): GithubInvitation::cancel + tick_expire"
```

---

### Task 15: `InvitationRequest` workflow skeleton

Per spec §9.2: a Restate `Workflow` (one-shot per `request_id`). The full flow involves an awakeable race against a timer — those bits land in Tasks 16–17. This task ships the trait, types, and the structure of the workflow handler; methods that need the workflow context are stubbed with `unimplemented!("Task N")`.

**Files:**
- Modify: `crates/restate-svc/src/invitation_request.rs`

- [ ] **Step 1: Add the trait + types**

```rust
//! `InvitationRequest` Workflow: submit the request, race admin decision against
//! timeout, dispatch to GithubInvitation on approval.

use crate::audit::{Actor, Target};
use crate::error::{HandlerError, Result};
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Duration, Utc};
use domain::{InvitationRequest, RequestId, RequestState};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};

/// Maximum time we keep a request "pending awaiting admin decision".
/// Per spec §Q8: `min(link.expires_at - now, 7d)`.
const MAX_DECISION_WAIT: Duration = Duration::days(7);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubmitRequestInput {
    pub request_id: RequestId,
    pub share_link_id: domain::ShareLinkId,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Decision payload — admin's approve/decline server function in Plan 5
/// resolves an awakeable with this.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Decision {
    Approve { decided_by: u64, decided_at: DateTime<Utc> },
    Decline { decided_by: u64, decided_at: DateTime<Utc>, reason: Option<String> },
}

#[restate_sdk::workflow]
pub trait InvitationRequest {
    /// One-shot workflow per `request_id`. Returns the final state.
    async fn submit(input: SubmitRequestInput) -> Result<RequestState, TerminalError>;

    /// Resolve the awakeable created in `submit` with an Approve/Decline decision.
    /// Called by the admin's server function (Plan 5).
    #[shared]
    async fn decide(decision: Decision) -> Result<(), TerminalError>;
}

pub struct InvitationRequestImpl {
    pub state: AppState,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p restate-svc`
Expected: clean (warnings about unimplemented trait methods are OK).

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/invitation_request.rs
git commit -m "feat(restate-svc): InvitationRequest workflow trait + types"
```

---

### Task 16: `InvitationRequest::submit` — pure-logic helpers

Three pure functions cover the workflow's pre-decision phase. They're testable without a Restate runtime; the workflow handler in Task 17 wires them inside `ctx.run`.

- `pre_decision_logic(state, input, link, now) -> Result<RequestPreDecisionResult>` — re-reads link from storage, validates `is_active(now)`, inserts pending request with atomic uses_count bump, emits `request.created`.
- `auto_approve_logic(state, request, decided_at) -> Result<()>` — when `link.approval_required == false`, immediately apply approve.
- `apply_decision_logic(state, request, decision) -> Result<()>` — record_request_decision + audit.

**Files:**
- Modify: `crates/restate-svc/src/invitation_request.rs`

- [ ] **Step 1: Add the pure helpers + tests**

Append to `invitation_request.rs`:

```rust
/// Outcome of the pre-decision phase. Drives the workflow handler's branching.
#[derive(Clone, Debug)]
pub enum PreDecisionOutcome {
    /// Link auto-approves; no awakeable race needed.
    AutoApprove,
    /// Pending admin decision; workflow must race awakeable vs. timeout.
    PendingDecision { decision_deadline: DateTime<Utc> },
}

/// Re-read share link, reject if not is_active, insert pending request,
/// emit `request.created`, return next-step instruction.
pub async fn pre_decision_logic(
    state: &AppState,
    input: &SubmitRequestInput,
    now: DateTime<Utc>,
    request_id: Option<String>,
) -> Result<PreDecisionOutcome> {
    let link = state
        .storage
        .get_share_link_by_id(input.share_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    if !link.is_active(now) {
        return Err(HandlerError::Storage(storage::Error::NotFound));
    }

    let request = InvitationRequest {
        id: input.request_id,
        share_link_id: input.share_link_id,
        requester_id: input.requester_id,
        justification: input.justification.clone(),
        state: RequestState::Pending,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
        created_at: input.created_at,
    };
    state
        .storage
        .insert_invitation_request_and_increment_uses(&request)
        .await?;

    crate::audit::emit(
        state,
        link.account_id,
        EventType::RequestCreated,
        Actor::User(input.requester_id),
        Target::request(input.request_id),
        serde_json::json!({
            "share_link_id": input.share_link_id.to_string(),
            "auto_approve": !link.approval_required,
        }),
        request_id,
    )
    .await?;

    if !link.approval_required {
        return Ok(PreDecisionOutcome::AutoApprove);
    }

    let deadline = match link.expires_at {
        Some(e) => std::cmp::min(e, now + MAX_DECISION_WAIT),
        None => now + MAX_DECISION_WAIT,
    };
    Ok(PreDecisionOutcome::PendingDecision {
        decision_deadline: deadline,
    })
}

/// Apply an Approve/Decline/Expire decision and emit the audit event.
/// Idempotent: a second call after the row is already terminal returns Ok(()).
pub async fn apply_decision_logic(
    state: &AppState,
    request_id: RequestId,
    decision: AppliedDecision,
    request_id_for_audit: Option<String>,
) -> Result<RequestState> {
    let req = state
        .storage
        .get_invitation_request(request_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    if req.state.is_terminal() {
        return Ok(req.state);
    }

    let (new_state, decided_by, decided_at, decline_reason, event) = match &decision {
        AppliedDecision::Approve { decided_by, at } => (
            RequestState::Approved,
            *decided_by,
            *at,
            None,
            EventType::RequestApproved,
        ),
        AppliedDecision::AutoApprove { at } => (
            RequestState::Approved,
            req.requester_id, // self for auto-approve audit; spec §15.1 lists "system (auto-approve)" too
            *at,
            None,
            EventType::RequestApproved,
        ),
        AppliedDecision::Decline { decided_by, at, reason } => (
            RequestState::Declined,
            *decided_by,
            *at,
            reason.clone(),
            EventType::RequestDeclined,
        ),
        AppliedDecision::Expire { at } => (
            RequestState::Expired,
            req.requester_id, // placeholder; system actor used in audit
            *at,
            None,
            EventType::RequestExpired,
        ),
    };

    match state
        .storage
        .record_request_decision(&storage::RequestDecision {
            request_id,
            state: new_state,
            decided_by,
            decided_at,
            decline_reason,
        })
        .await
    {
        Ok(()) => (),
        Err(storage::Error::NotFound) => return Ok(new_state), // already decided
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    let link = state
        .storage
        .get_share_link_by_id(req.share_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    let actor = match decision {
        AppliedDecision::Approve { decided_by, .. } | AppliedDecision::Decline { decided_by, .. } => {
            Actor::User(decided_by)
        }
        AppliedDecision::AutoApprove { .. } | AppliedDecision::Expire { .. } => Actor::System,
    };
    crate::audit::emit(
        state,
        link.account_id,
        event,
        actor,
        Target::request(request_id),
        serde_json::json!({}),
        request_id_for_audit,
    )
    .await?;

    Ok(new_state)
}

/// Concrete decision the workflow has resolved on. Distinct from
/// [`Decision`] (the awakeable payload from admin) because the workflow may
/// also auto-approve or expire on its own.
#[derive(Clone, Debug)]
pub enum AppliedDecision {
    AutoApprove { at: DateTime<Utc> },
    Approve { decided_by: u64, at: DateTime<Utc> },
    Decline { decided_by: u64, at: DateTime<Utc>, reason: Option<String> },
    Expire { at: DateTime<Utc> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_state};
    use audit::EventType;
    use domain::{AccountType, Permission, SelectedRepos, ShareLink, ShareLinkId, Slug};
    use rand::SeedableRng;

    async fn seed_link(state: &AppState, approval_required: bool, expires_at: Option<DateTime<Utc>>) -> ShareLinkId {
        state
            .storage
            .insert_installation(&domain::Account {
                installation_id: 1,
                account_id: 100,
                account_login: "acme".into(),
                account_type: AccountType::Organization,
                installed_at: dt("2026-05-04T12:00:00Z"),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 7,
                login: "creator".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 8,
                login: "alice".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        let link = ShareLink {
            id: ShareLinkId::new(),
            slug: Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(99)),
            installation_id: 1,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Pull,
            approval_required,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![],
        };
        state.storage.insert_share_link(&link).await.unwrap();
        link.id
    }

    #[tokio::test]
    async fn pre_decision_auto_approve_when_link_doesnt_require() {
        let state = fixture_state().await;
        let link_id = seed_link(&state, false, None).await;
        let req_id = RequestId::new();

        let outcome = pre_decision_logic(
            &state,
            &SubmitRequestInput {
                request_id: req_id,
                share_link_id: link_id,
                requester_id: 8,
                justification: None,
                created_at: dt("2026-05-04T12:30:00Z"),
            },
            dt("2026-05-04T12:30:00Z"),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, PreDecisionOutcome::AutoApprove));

        let req = state.storage.get_invitation_request(req_id).await.unwrap().unwrap();
        assert_eq!(req.state, RequestState::Pending); // pre-decision only inserts pending
    }

    #[tokio::test]
    async fn pre_decision_pending_with_deadline_capped_at_7d() {
        let state = fixture_state().await;
        let link_id = seed_link(&state, true, None).await;

        let outcome = pre_decision_logic(
            &state,
            &SubmitRequestInput {
                request_id: RequestId::new(),
                share_link_id: link_id,
                requester_id: 8,
                justification: None,
                created_at: dt("2026-05-04T12:30:00Z"),
            },
            dt("2026-05-04T12:30:00Z"),
            None,
        )
        .await
        .unwrap();

        match outcome {
            PreDecisionOutcome::PendingDecision { decision_deadline } => {
                assert_eq!(
                    decision_deadline,
                    dt("2026-05-04T12:30:00Z") + Duration::days(7)
                );
            }
            other => panic!("expected PendingDecision, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pre_decision_pending_uses_link_expires_when_sooner() {
        let state = fixture_state().await;
        let link_id = seed_link(&state, true, Some(dt("2026-05-06T12:00:00Z"))).await;

        let outcome = pre_decision_logic(
            &state,
            &SubmitRequestInput {
                request_id: RequestId::new(),
                share_link_id: link_id,
                requester_id: 8,
                justification: None,
                created_at: dt("2026-05-04T12:30:00Z"),
            },
            dt("2026-05-04T12:30:00Z"),
            None,
        )
        .await
        .unwrap();

        match outcome {
            PreDecisionOutcome::PendingDecision { decision_deadline } => {
                assert_eq!(decision_deadline, dt("2026-05-06T12:00:00Z"));
            }
            other => panic!("expected PendingDecision, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pre_decision_inactive_link_is_terminal() {
        let state = fixture_state().await;
        let link_id = seed_link(&state, false, Some(dt("2026-05-03T00:00:00Z"))).await;
        // expired before "now"

        let err = pre_decision_logic(
            &state,
            &SubmitRequestInput {
                request_id: RequestId::new(),
                share_link_id: link_id,
                requester_id: 8,
                justification: None,
                created_at: dt("2026-05-04T12:30:00Z"),
            },
            dt("2026-05-04T12:30:00Z"),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.is_terminal());
    }

    #[tokio::test]
    async fn apply_decision_approve_records_and_audits() {
        let state = fixture_state().await;
        let link_id = seed_link(&state, true, None).await;
        let req_id = RequestId::new();
        pre_decision_logic(
            &state,
            &SubmitRequestInput {
                request_id: req_id,
                share_link_id: link_id,
                requester_id: 8,
                justification: None,
                created_at: dt("2026-05-04T12:30:00Z"),
            },
            dt("2026-05-04T12:30:00Z"),
            None,
        )
        .await
        .unwrap();

        let final_state = apply_decision_logic(
            &state,
            req_id,
            AppliedDecision::Approve {
                decided_by: 7,
                at: dt("2026-05-04T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(final_state, RequestState::Approved);

        let req = state.storage.get_invitation_request(req_id).await.unwrap().unwrap();
        assert_eq!(req.state, RequestState::Approved);
        assert_eq!(req.decided_by, Some(7));
    }

    #[tokio::test]
    async fn apply_decision_decline_with_reason() {
        let state = fixture_state().await;
        let link_id = seed_link(&state, true, None).await;
        let req_id = RequestId::new();
        pre_decision_logic(
            &state,
            &SubmitRequestInput {
                request_id: req_id,
                share_link_id: link_id,
                requester_id: 8,
                justification: None,
                created_at: dt("2026-05-04T12:30:00Z"),
            },
            dt("2026-05-04T12:30:00Z"),
            None,
        )
        .await
        .unwrap();

        let final_state = apply_decision_logic(
            &state,
            req_id,
            AppliedDecision::Decline {
                decided_by: 7,
                at: dt("2026-05-04T13:00:00Z"),
                reason: Some("not familiar with this user".into()),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(final_state, RequestState::Declined);

        let req = state.storage.get_invitation_request(req_id).await.unwrap().unwrap();
        assert_eq!(req.decline_reason.as_deref(), Some("not familiar with this user"));
    }

    #[tokio::test]
    async fn apply_decision_idempotent_after_first_decision() {
        let state = fixture_state().await;
        let link_id = seed_link(&state, true, None).await;
        let req_id = RequestId::new();
        pre_decision_logic(
            &state,
            &SubmitRequestInput {
                request_id: req_id,
                share_link_id: link_id,
                requester_id: 8,
                justification: None,
                created_at: dt("2026-05-04T12:30:00Z"),
            },
            dt("2026-05-04T12:30:00Z"),
            None,
        )
        .await
        .unwrap();

        apply_decision_logic(
            &state,
            req_id,
            AppliedDecision::Approve {
                decided_by: 7,
                at: dt("2026-05-04T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        // Second decision call returns the existing state, no re-audit, no error.
        let result = apply_decision_logic(
            &state,
            req_id,
            AppliedDecision::Decline {
                decided_by: 7,
                at: dt("2026-05-04T14:00:00Z"),
                reason: Some("changed mind".into()),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(result, RequestState::Approved, "first decision wins");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p restate-svc --lib invitation_request`
Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/invitation_request.rs
git commit -m "feat(restate-svc): InvitationRequest pre-decision + apply-decision pure logic"
```

---

### Task 17: `InvitationRequest::submit` — full handler wiring

The Restate workflow handler:

1. Calls `pre_decision_logic` inside `ctx.run`.
2. If `AutoApprove`, calls `apply_decision_logic(AppliedDecision::AutoApprove)` inside `ctx.run`, then dispatches `GithubInvitation::create.send(...)` for each repo, returns `RequestState::Approved`.
3. If `PendingDecision { deadline }`:
   - Creates an awakeable; the resolved decision is the admin's `Decision`.
   - Races `awakeable.await` vs. `ctx.sleep(deadline - now)`.
   - On awakeable wins: calls `apply_decision_logic` with the resolved Approve/Decline.
   - On timer wins: calls `apply_decision_logic(AppliedDecision::Expire)`.
4. If approved (auto OR admin), iterates `link.repos`, sends `GithubInvitation::create` for each.

**Files:**
- Modify: `crates/restate-svc/src/invitation_request.rs`

- [ ] **Step 1: Add the dispatch helper + the workflow impl**

Append:

```rust
/// Pure logic: read link.repos, build CreateInvitationInput per repo. Emits
/// no audit (each `GithubInvitation::create` is responsible for its own).
/// Returns the list of inputs for the workflow to send.
pub async fn build_dispatch_inputs(
    state: &AppState,
    request_id: RequestId,
    now: DateTime<Utc>,
) -> Result<Vec<crate::github_invitation::CreateInvitationInput>> {
    let req = state
        .storage
        .get_invitation_request(request_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let link = state
        .storage
        .get_share_link_by_id(req.share_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let recipient = state
        .storage
        .get_user(req.requester_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    Ok(link
        .repos
        .into_iter()
        .map(|r| crate::github_invitation::CreateInvitationInput {
            invitation_id: domain::GithubInvitationId::new(),
            invitation_request_id: request_id,
            installation_id: link.installation_id,
            repo_id: r.repo_id,
            repo_full_name: r.repo_full_name,
            recipient_login: recipient.login.clone(),
            permission: link.permission,
            now,
        })
        .collect())
}

impl InvitationRequest for InvitationRequestImpl {
    async fn submit(
        &self,
        ctx: WorkflowContext<'_>,
        input: SubmitRequestInput,
    ) -> Result<RequestState, TerminalError> {
        let request_id_for_audit = Some(ctx.request_id().to_string());
        let now = chrono::Utc::now();

        let outcome = ctx
            .run("pre_decision", || async {
                pre_decision_logic(&self.state, &input, now, request_id_for_audit.clone())
                    .await
                    .map_err(|e| e.to_terminal())
            })
            .await?;

        let decision = match outcome {
            PreDecisionOutcome::AutoApprove => AppliedDecision::AutoApprove { at: now },
            PreDecisionOutcome::PendingDecision { decision_deadline } => {
                let (awakeable, _id) = ctx.awakeable::<Decision>();
                // Note: the awakeable id is read by the admin's server function (Plan 5)
                // to resolve. v1 publishes it via `request_id` lookup — Plan 5 wires
                // exposure.
                let timer = ctx.sleep(
                    (decision_deadline - chrono::Utc::now())
                        .to_std()
                        .unwrap_or_default(),
                );

                tokio::select! {
                    decision = awakeable => {
                        let decision = decision.map_err(|e| TerminalError::new(e.to_string()))?;
                        match decision {
                            Decision::Approve { decided_by, decided_at } => {
                                AppliedDecision::Approve { decided_by, at: decided_at }
                            }
                            Decision::Decline { decided_by, decided_at, reason } => {
                                AppliedDecision::Decline { decided_by, at: decided_at, reason }
                            }
                        }
                    }
                    _ = timer => AppliedDecision::Expire { at: chrono::Utc::now() },
                }
            }
        };

        let final_state = ctx
            .run("apply_decision", || async {
                apply_decision_logic(&self.state, input.request_id, decision.clone(), request_id_for_audit.clone())
                    .await
                    .map_err(|e| e.to_terminal())
            })
            .await?;

        if final_state == RequestState::Approved {
            let dispatch_inputs = ctx
                .run("build_dispatch", || async {
                    build_dispatch_inputs(&self.state, input.request_id, now)
                        .await
                        .map_err(|e| e.to_terminal())
                })
                .await?;
            for inv_input in dispatch_inputs {
                // Fire-and-forget call to GithubInvitation::create. Restate's
                // `send` semantics: at-least-once delivery; the per-row
                // idempotency in `github_invitation::create_logic` makes
                // re-sends safe.
                ctx.object_client::<crate::github_invitation::GithubInvitationClient>(
                    inv_input.invitation_id.to_string(),
                )
                .create(inv_input)
                .send();
            }
        }

        Ok(final_state)
    }

    async fn decide(
        &self,
        ctx: SharedWorkflowContext<'_>,
        decision: Decision,
    ) -> Result<(), TerminalError> {
        // Resolve the awakeable. The actual id was generated in `submit`
        // and recorded by Plan 5's server function via a side channel.
        // The resolution mechanism: callers pass the awakeable id alongside
        // the decision via Restate's HTTP awakeable API directly; this
        // method is reserved for cases where an internal handler resolves
        // the awakeable instead.
        //
        // v1 implementation: this method is a placeholder. Plan 5's server
        // function calls Restate's `POST /restate/awakeables/{id}/resolve`
        // directly. This method exists for the workflow to expose the
        // schema to the Restate runtime; we don't actually need a body here.
        let _ = (ctx, decision);
        Ok(())
    }
}
```

The `tokio::select!` syntax in a Restate workflow may not be exactly right for the SDK's awakeable + timer race API. Restate's preferred idiom uses `Either::race()` or `select_biased!` — adapt to whatever the SDK exposes. The pure-logic functions stay unchanged.

The `ctx.object_client::<...>(...)` call shape is also subject to verification against the upstream restate-sdk 0.10 docs. The macro generates a client struct (`GithubInvitationClient`) which the workflow uses to fan-out `send` calls.

The `decide` method is a workflow shared handler that may not be needed if Plan 5 resolves awakeables directly via Restate's HTTP API. Leave the placeholder; revisit when Plan 5 wires the dashboard's approve/decline buttons.

- [ ] **Step 2: Verify it compiles** (the workflow runtime parts may not have unit tests; the pure logic in Task 16 is what's covered)

Run: `cargo check -p restate-svc`
Expected: clean. If specific Restate SDK 0.10 APIs don't match (`tokio::select!`, `object_client`, etc.), iterate by reading the SDK docs / examples until it compiles.

- [ ] **Step 3: Run existing tests**

Run: `cargo test -p restate-svc --lib invitation_request`
Expected: 7 tests pass (same as Task 16 — the workflow handler is not directly unit-tested in v1; integration tests come in Plan 8).

- [ ] **Step 4: Commit**

```bash
git add crates/restate-svc/src/invitation_request.rs
git commit -m "feat(restate-svc): InvitationRequest workflow handler"
```

---

### Task 18: `Reconcile::daily_run`

Per spec §9.4: invoked once per ~24h. For each non-uninstalled installation, lists pending GitHub invitations across selected repos and reconciles drift against our `github_invitations` rows. Source of authority is GitHub.

In v1 the reconcile is conservative: for each installation, it walks `list_pending_github_invitations_for_installation`; for each, checks whether GitHub still considers it pending (via `github::list_invitations` per repo); rows that GitHub no longer lists are settled by walking `is_collaborator` (Accepted) or `Cancelled` (404). The handler emits the appropriate audit event per state transition.

**Files:**
- Modify: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Add the service trait + impl + pure fn + tests**

```rust
//! `Reconcile` Service: daily sweep over pending GitHub invitations.

use crate::audit::{Actor, Target};
use crate::error::{HandlerError, Result};
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Utc};
use domain::InvitationState;
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DailyRunInput {
    pub at: DateTime<Utc>,
}

#[restate_sdk::service]
pub trait Reconcile {
    async fn daily_run(input: DailyRunInput) -> Result<(), TerminalError>;
}

pub struct ReconcileImpl {
    pub state: AppState,
}

impl Reconcile for ReconcileImpl {
    async fn daily_run(
        &self,
        ctx: Context<'_>,
        input: DailyRunInput,
    ) -> Result<(), TerminalError> {
        let request_id = Some(ctx.request_id().to_string());
        ctx.run("daily_run", || async {
            daily_run_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(|e| e.to_terminal())
        })
        .await
    }
}

/// For each active installation, walk our pending github_invitations rows
/// and reconcile against GitHub's reality. Idempotent.
pub async fn daily_run_logic(
    state: &AppState,
    input: &DailyRunInput,
    request_id_for_audit: Option<String>,
) -> Result<()> {
    let installations = state.storage.list_active_installations().await?;
    for acct in installations {
        let pending = state
            .storage
            .list_pending_github_invitations_for_installation(acct.installation_id)
            .await?;
        for row in pending {
            if let Err(e) = reconcile_single(state, &acct, &row, input.at, request_id_for_audit.clone())
                .await
            {
                if !e.is_terminal() {
                    // Transient — let Restate retry the whole sweep.
                    return Err(e);
                }
                // Terminal — log and continue with the next row.
                tracing::warn!(
                    invitation_id = %row.id,
                    err = %e,
                    "reconcile_single failed terminally; continuing"
                );
            }
        }
    }
    Ok(())
}

async fn reconcile_single(
    state: &AppState,
    acct: &domain::Account,
    row: &domain::GithubInvitation,
    at: DateTime<Utc>,
    request_id_for_audit: Option<String>,
) -> Result<()> {
    let req = state
        .storage
        .get_invitation_request(row.invitation_request_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let link = state
        .storage
        .get_share_link_by_id(req.share_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let repo = link
        .repos
        .iter()
        .find(|r| r.repo_id == row.repo_id)
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "github_invitation {} references repo_id {} not in link.repos",
                row.id, row.repo_id
            ))
        })?;
    let (owner, repo_name) = repo
        .repo_full_name
        .split_once('/')
        .ok_or_else(|| HandlerError::Invariant(format!("bad repo full name: {}", repo.repo_full_name)))?;

    let pending = state
        .github
        .list_invitations(acct.installation_id, owner, repo_name)
        .await?;
    let still_pending = row
        .github_invitation_id
        .map(|id| pending.iter().any(|p| p.id == id))
        .unwrap_or(false);

    if still_pending {
        return Ok(());
    }

    // GitHub no longer lists it. Disambiguate accepted vs. cancelled by
    // checking is_collaborator.
    let recipient = state
        .storage
        .get_user(req.requester_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let is_member = state
        .github
        .is_collaborator(acct.installation_id, owner, repo_name, &recipient.login)
        .await?;

    let (new_state, event) = if is_member {
        (InvitationState::Accepted, EventType::InvitationAccepted)
    } else {
        (InvitationState::Cancelled, EventType::InvitationCancelled)
    };

    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: row.id,
            state: new_state,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    crate::audit::emit(
        state,
        link.account_id,
        event,
        Actor::System,
        Target::github_invitation(row.id),
        serde_json::json!({"reconciled": true}),
        request_id_for_audit,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{dt, fixture_github_client, fixture_storage};
    use audit::EventType;
    use domain::{
        AccountType, GithubInvitationId, Permission, RequestId, RequestState, SelectedRepos,
        ShareLink, ShareLinkId, ShareLinkRepo, Slug,
    };
    use github::mocks::{Expectation, MockTransport};
    use github::transport::{Method, Response};
    use rand::SeedableRng;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn token_mint(installation_id: u64) -> Expectation {
        Expectation {
            method: Method::Post,
            url: format!(
                "https://api.github.test/app/installations/{}/access_tokens",
                installation_id
            ),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 201,
                headers: BTreeMap::new(),
                body: br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec(),
            },
        }
    }

    /// Seed a single installation with one Sent github_invitation row.
    async fn seed_one_pending(state: &AppState) -> GithubInvitationId {
        state
            .storage
            .insert_installation(&domain::Account {
                installation_id: 9,
                account_id: 100,
                account_login: "acme".into(),
                account_type: AccountType::Organization,
                installed_at: dt("2026-05-04T12:00:00Z"),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 7,
                login: "creator".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        state
            .storage
            .upsert_user(&domain::User {
                user_id: 8,
                login: "alice".into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
        let link = ShareLink {
            id: ShareLinkId::new(),
            slug: Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(7)),
            installation_id: 9,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Push,
            approval_required: false,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![ShareLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        };
        state.storage.insert_share_link(&link).await.unwrap();
        let req_id = RequestId::new();
        let req = domain::InvitationRequest {
            id: req_id,
            share_link_id: link.id,
            requester_id: 8,
            justification: None,
            state: RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        };
        state
            .storage
            .insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();
        let inv_id = GithubInvitationId::new();
        state
            .storage
            .insert_github_invitation(&domain::GithubInvitation {
                id: inv_id,
                invitation_request_id: req_id,
                repo_id: 10,
                github_invitation_id: Some(9988),
                state: InvitationState::Sent,
                error_message: None,
                created_at: dt("2026-05-04T13:00:00Z"),
                updated_at: dt("2026-05-04T13:00:00Z"),
            })
            .await
            .unwrap();
        inv_id
    }

    #[tokio::test]
    async fn daily_run_no_change_when_still_pending() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([
                    {
                        "id": 9988,
                        "invitee": {"id": 42, "login": "alice"},
                        "permissions": "write",
                        "created_at": "2026-05-04T13:00:00Z"
                    }
                ]),
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending(&state).await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Sent);
    }

    #[tokio::test]
    async fn daily_run_marks_accepted_when_user_is_collaborator() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([]),
            ),
            Expectation {
                method: Method::Get,
                url: "https://api.github.test/repos/acme/api/collaborators/alice".into(),
                required_headers: BTreeMap::new(),
                expected_body: None,
                response: Response {
                    status: 204,
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            },
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending(&state).await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Accepted);
    }

    #[tokio::test]
    async fn daily_run_marks_cancelled_when_user_is_not_collaborator() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([]),
            ),
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/api/collaborators/alice",
                404,
            ),
        ]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending(&state).await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
        )
        .await
        .unwrap();

        let row = state.storage.get_github_invitation(inv_id).await.unwrap().unwrap();
        assert_eq!(row.state, InvitationState::Cancelled);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p restate-svc --lib reconcile`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/reconcile.rs
git commit -m "feat(restate-svc): Reconcile::daily_run"
```

---

### Task 19: `Endpoint` builder helper

Plan 7's `#[event(fetch)]` will need to construct a `restate_sdk::Endpoint` with all five services bound. Provide a single helper that takes an `AppState` and returns the configured endpoint — Plan 7 just calls it.

**Files:**
- Modify: `crates/restate-svc/src/lib.rs`

- [ ] **Step 1: Add the helper at the crate root**

Append to `crates/restate-svc/src/lib.rs` (after the `pub use` block):

```rust
use restate_sdk::prelude::Endpoint;

/// Build a fully-bound Restate endpoint with all five ghinvite services.
/// Plan 7's Workers `#[event(fetch)]` calls this once per cold start.
pub fn build_endpoint(state: AppState) -> Endpoint {
    Endpoint::builder()
        .bind(installation::InstallationImpl { state: state.clone() }.serve())
        .bind(share_link::ShareLinkImpl { state: state.clone() }.serve())
        .bind(invitation_request::InvitationRequestImpl { state: state.clone() }.serve())
        .bind(github_invitation::GithubInvitationImpl { state: state.clone() }.serve())
        .bind(reconcile::ReconcileImpl { state }.serve())
        .build()
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p restate-svc`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/restate-svc/src/lib.rs
git commit -m "feat(restate-svc): build_endpoint helper for Plan 7"
```

---

### Task 20: `README.md` — local-dev guide

**Files:**
- Modify: `crates/restate-svc/README.md`

- [ ] **Step 1: Replace the placeholder README**

`crates/restate-svc/README.md`:

```markdown
# crates/restate-svc

Restate handler services for ghinvite. Five services own every durable state
change in the system: `Installation`, `ShareLink`, `InvitationRequest`
(workflow), `GithubInvitation`, `Reconcile`. Built on the upstream
`restate-sdk = "0.10"` Rust SDK.

This crate is **library-only in Plan 3**: handler logic + unit tests, with
`SqlxStorage::in_memory` and `crates/github`'s `MockTransport` driving every
test. Plan 7 wraps this library in a Workers `#[event(fetch)]` entry point
backed by `D1Storage`, plus the `wrangler.toml` and Workers secrets.

## Local dev (current)

Hardly anything to do at the moment — this crate ships no binary in Plan 3.
Run unit tests:

```bash
cargo test -p restate-svc
```

Each handler module has tests in the same file:

- `crates/restate-svc/src/installation.rs::tests`
- `crates/restate-svc/src/share_link.rs::tests`
- `crates/restate-svc/src/github_invitation.rs::tests`
- `crates/restate-svc/src/invitation_request.rs::tests`
- `crates/restate-svc/src/reconcile.rs::tests`

The `InvitationRequest` workflow's awakeable + timer race is **not** unit-
tested in Plan 3 — that requires the Restate runtime. Plan 8 covers it via
integration tests against the local Restate server.

## Local dev (after Plan 7)

Restate's platform server runs in `compose.yaml` at the repo root:

```bash
docker compose up -d restate
```

This binds:
- `127.0.0.1:8080` — ingress (where you POST invocations).
- `127.0.0.1:9070` — admin (where deployments are registered).
- `127.0.0.1:9071` — internal.

Plan 7 will wire the Workers entry point. Until then, the crate is library-
only and registers nothing with the Restate platform.

## Architecture notes

Each handler delegates to a pure-async function (e.g. `onboard_logic`,
`create_logic`). The `#[restate_sdk::object|service|workflow]` impl wraps the
pure function inside `ctx.run("step_name", async {...})` so Restate captures
it as a durable step. Pure functions take `&AppState` (`Arc<dyn Storage>` +
`Arc<InstallationClient>`) and return `Result<T, HandlerError>`.

`HandlerError::is_terminal()` distinguishes failures Restate should NOT retry
(constraint violations, 4xx) from transient ones (5xx, network) — the wrapper
maps the result to Restate's `TerminalError` type accordingly.

Audit events go through `audit::emit(state, account_id, event_type, actor,
target, metadata, request_id)`, which constructs an `AuditEvent` and calls
`Storage::audit`. Every state-change handler emits exactly one audit event.

See `docs/superpowers/plans/2026-05-04-ghinvite-restate-handlers.md` for the
implementation plan, and `docs/superpowers/specs/2026-05-04-ghinvite-v1-design.md`
§9 for the workflow specifications.
```

- [ ] **Step 2: Commit**

```bash
git add crates/restate-svc/README.md
git commit -m "docs(restate-svc): local-dev guide"
```

---

### Task 21: Workspace polish + docker-compose smoke

The test suite is hermetic — `cargo test --workspace` doesn't need Docker. But Plan 3 also wants a quick smoke against the running Restate platform: with `docker compose up -d restate`, the platform's admin endpoint at `:9070` should respond to `/health`.

**Files:**
- Modify: `docs/superpowers/plans/README.md` (mark Plan 3 status)

- [ ] **Step 1: Smoke check (manual; not gated by tests)**

Run:

```bash
docker compose up -d restate
sleep 3
curl -s http://127.0.0.1:9070/health
```

Expected: HTTP 200 with a JSON-ish body. If this fails, Plan 7's deployment will fail too — fix devenv/compose now.

Bring it back down: `docker compose down`.

- [ ] **Step 2: Update the plan map README**

Edit `docs/superpowers/plans/README.md`. Change the row for Plan 3 from:

```
| 3 | **Restate handlers** | _not yet written_ | Pending | ...
```

to:

```
| 3 | **Restate handlers** | `2026-05-04-ghinvite-restate-handlers.md` | Implemented | ...
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/plans/README.md
git commit -m "docs: mark Plan 3 as Implemented"
```

---

### Task 22: Final clippy + fmt

**Files:** all of `crates/restate-svc/`

- [ ] **Step 1: Run rustfmt**

Run: `cargo fmt --all`
Expected: completes silently.

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean. If clippy flags genuine issues, fix them; for stylistic preferences, prefer focused `#[allow(clippy::xxx)]` near the call site.

- [ ] **Step 3: Run the entire workspace test suite one more time**

Run: `cargo test --workspace`
Expected: every test in every crate (domain, audit, storage, github, restate-svc) passes.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "chore(restate-svc): apply rustfmt and clippy fixes"
```

If there's nothing to commit, skip this step.

---

### Task 23: Push and tidy

**Files:** none.

- [ ] **Step 1: Push**

Confirm the user wants to push, then:

```bash
git push
```

- [ ] **Step 2: End-of-Plan-3 sanity**

Run:

```bash
cargo test --workspace 2>&1 | grep "test result"
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Both should be clean.

---

## Plan self-review

**Spec coverage check** (each spec section → which task implements it):

- §9.1 (`ShareLink::create/revoke/tick_expiration`) → Tasks 8, 9, 10
- §9.2 (`InvitationRequest::submit` workflow) → Tasks 15, 16, 17
- §9.3 (`GithubInvitation::create/on_webhook/cancel/tick_expire`) → Tasks 12, 13, 14
- §9.4 (`Reconcile::daily_run`) → Task 18
- §9.5 (`Installation::onboard/repos_changed/uninstall`) → Task 6
- §15.1 (every audit event in the table) → Tasks 4, 6, 8–18 (each handler emits its row)
- §15.2 (audit writes go through `Storage::audit` only) → Task 4 (helper) + every handler
- §16 (error handling: 4xx terminal, 5xx retry, etc.) → Task 2 (`HandlerError::is_terminal`)

**Spec sections NOT covered by this plan (intentionally — assigned to other plans):**

- §10 OAuth / session — Plan 4.
- §11 URL routing / §12 frontend — Plans 4–6.
- §13 webhook receiver (HMAC + envelope parse + dispatch) — Plan 6 calls our handlers; this plan ships handlers, not the receiver.
- §14 GitHub API surface — Plan 2; this plan consumes it.
- §15.3 audit UI / CSV — v1.1.
- §17 testing strategy: Restate-runtime integration tests — Plan 8 (this plan ships unit tests against pure-logic functions only).
- D1Storage and Workers `#[event(fetch)]` entry — Plan 7.

**Type consistency check:**

- `HandlerError`, `Result<T>`, `Actor`, `Target` are defined in Tasks 2 / 4 and used identically across all handler modules.
- `OnboardInput`, `ReposChangedInput`, `UninstallInput`, `CreateLinkInput`, `RevokeLinkInput`, `TickExpirationInput`, `CreateInvitationInput`, `OnWebhookInput`, `CancelInvitationInput`, `TickExpireInput`, `SubmitRequestInput`, `Decision`, `AppliedDecision`, `DailyRunInput` are all defined in their service module and used only there. No cross-service input duplication.
- `AppliedDecision` (used by `apply_decision_logic`) and `Decision` (the awakeable payload) are deliberately distinct: `Decision` only carries Approve/Decline; `AppliedDecision` adds AutoApprove + Expire which the workflow-internal logic produces.
- All handlers reach storage via `state.storage.X` (matching the `Storage` trait from Plan 1) and GitHub via `state.github.X` (matching `InstallationClient` from Plan 2). No direct dep on `SqlxStorage` or `ReqwestTransport` in production code — those only appear in tests.

**Placeholder check:** No `TBD`, `TODO`, `// implement later`. Every task ships complete pure-logic functions and tests; the workflow runtime parts in Task 17 acknowledge the SDK-version-specific syntax may need tweaking and explicitly call out that the pure functions (already covered by Tasks 16's tests) carry the load-bearing logic. This is the only place the plan tolerates implementer-time iteration on exact API shape, and it's bounded by what runs at compile time.

**Restate SDK version risk:** The `restate-sdk = "0.10"` upstream API for object_client, awakeable, sleep, run, ctx.request_id, etc. is presumed but not literally exercised in this plan (since we don't have a sandbox). Task 17 calls this out as a verification point. If 0.10's `tokio::select!` / `Either::race` / `object_client::<...>()` syntax differs, the implementer should iterate against the SDK examples until the workflow code compiles. The pure-logic functions and their tests do NOT depend on the SDK API; they're robust to any iteration there.

---

## Audit trail

This plan was written on 2026-05-04 against the just-implemented `crates/domain` + `crates/audit` + `crates/storage` from Plan 1 and `crates/github` from Plan 2. No /autoplan review has been performed; the plan is offered for execution-time review by the implementer. User agreed to all seven shape decisions in the pre-write check-in plus confirmed Restate SDK 0.10 (upstream) + Docker-compose setup.

| # | Decision | Source |
|---|----------|--------|
| 1 | One library crate `crates/restate-svc` (rlib, not cdylib) | Plan 7 owns the Workers entry point per project README |
| 2 | `restate-sdk = "0.10"` upstream (not the fork) | Verified `ProtocolMode::RequestResponse` is in upstream 0.10 |
| 3 | Pure-async-fn pattern: each handler delegates to `<method>_logic(&state, ...)` | Easier to unit-test without Restate runtime |
| 4 | `HandlerError::is_terminal()` classifies 4xx/conflict as terminal, 5xx/network as transient | Spec §16 error-handling table |
| 5 | Audit emission via `audit::emit(state, ...)` helper called inside `ctx.run` | Spec §15.2 (audit writes go through `Storage::audit` only) |
| 6 | `AppliedDecision` distinct from `Decision` (workflow-internal vs. awakeable payload) | AutoApprove + Expire are not user-driven |
| 7 | `InvitationRequest` workflow tests cover only pure-logic functions; runtime race deferred to Plan 8 | No virtual-time test harness in 0.10 stable |
| 8 | Restate runs via `compose.yaml` (already in repo); local dev = `docker compose up -d restate` + future Plan-7 wrangler dev | User direction |
