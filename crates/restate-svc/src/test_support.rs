//! Test-only fixture builders. Behind `#[cfg(test)]` (declared in `lib.rs`).
//!
//! `#[allow(dead_code)]` at the module level: each per-handler test in
//! Tasks 6–21 picks up a different subset of these helpers, so individual
//! fixtures can lag the call sites by a task or two. Silencing the lint
//! here keeps the in-flight build noise-free without requiring a per-item
//! annotation that would also have to be removed task by task.
#![allow(dead_code)]

use crate::state::AppState;
use chrono::{DateTime, Utc};
use github::jwt::AppJwtSigner;
use github::{HttpTransport, InstallationClient};
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
pub(crate) async fn fixture_state_with_transport(transport: Arc<dyn HttpTransport>) -> AppState {
    AppState::new(fixture_storage().await, fixture_github_client(transport))
}

/// Build a `DateTime<Utc>` from RFC-3339; same pattern used in storage tests.
pub(crate) fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}
