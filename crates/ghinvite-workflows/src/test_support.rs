//! Test-only fixture builders. Behind `#[cfg(test)]` (declared in `lib.rs`).
//!
//! `#[allow(dead_code)]`: this is a shared fixture kit, and not every helper
//! has a caller at all times.
#![allow(dead_code)]

use crate::state::AppState;
use chrono::{DateTime, Utc};
use ghinvite_github::jwt::AppJwtSigner;
use ghinvite_github::{HttpTransport, InstallationClient};
use std::sync::Arc;

/// Static test key shared with the github crate's test modules. Same path:
/// `crates/ghinvite-github/src/jwt_test_key.pem` — referenced via Cargo's path
/// resolution from this crate.
const TEST_KEY_PEM: &str = include_str!("../../ghinvite-github/src/jwt_test_key.pem");

/// Build an in-memory `SqlxStorage`.
pub(crate) async fn fixture_storage() -> Arc<dyn ghinvite_core::storage::Storage> {
    Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    )
}

/// Build an `InstallationClient` with the supplied transport and an in-test
/// App JWT signer. Pointed at `https://api.github.test` so test request URLs
/// are short and the production github.com host is never hit.
pub(crate) fn fixture_github_client(transport: Arc<dyn HttpTransport>) -> Arc<InstallationClient> {
    let signer = AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap();
    Arc::new(InstallationClient::new(transport, signer).with_base("https://api.github.test"))
}

/// Convenience: an `AppState` with in-memory storage and a `MockTransport`-
/// less InstallationClient. Tests that need a scripted transport call
/// `fixture_state_with_transport` instead.
pub(crate) async fn fixture_state() -> AppState {
    use ghinvite_github::mocks::MockTransport;
    let transport = Arc::new(MockTransport::scripted(vec![]));
    AppState::new(fixture_storage().await, fixture_github_client(transport))
}

/// Convenience: an `AppState` plus concrete storage for tests that need
/// debug-only audit reads.
pub(crate) async fn fixture_state_with_storage()
-> (AppState, Arc<ghinvite_storage_sqlx::SqlxStorage>) {
    use ghinvite_github::mocks::MockTransport;

    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let storage_for_state: Arc<dyn ghinvite_core::storage::Storage> = storage.clone();
    let transport = Arc::new(MockTransport::scripted(vec![]));
    let state = AppState::new(storage_for_state, fixture_github_client(transport));

    (state, storage)
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

/// A single mocked installation-token mint, consumed first by any test whose
/// scripted transport makes an installation-authenticated call.
pub(crate) fn token_mint(installation_id: u64) -> ghinvite_github::mocks::Expectation {
    use ghinvite_github::mocks::Expectation;
    use ghinvite_github::transport::{Method, Response};
    Expectation {
        method: Method::Post,
        url: format!("https://api.github.test/app/installations/{installation_id}/access_tokens"),
        required_headers: std::collections::BTreeMap::new(),
        expected_body: None,
        response: Response {
            status: 201,
            headers: std::collections::BTreeMap::new(),
            body: br#"{"token":"ghs_xxx","expires_at":"2099-01-01T00:00:00Z"}"#.to_vec(),
        },
    }
}

/// The `{"message": ...}` response GitHub returns for a refusal, carrying
/// whatever evidence `headers` gives it. Every throttling test varies the
/// evidence and nothing else, so the shape lives here rather than in each one.
pub(crate) fn refusal(
    status: u16,
    headers: &[(&str, &str)],
    message: &str,
) -> ghinvite_github::transport::Response {
    ghinvite_github::transport::Response {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: serde_json::json!({ "message": message }).to_string().into(),
    }
}

/// Identities of the rows [`seed_pending_invitation`] inserted, for tests that
/// address the request or the link as well as the invitation.
pub(crate) struct SeededInvitation {
    pub invitation_id: ghinvite_core::GithubInvitationId,
    pub link_id: ghinvite_core::InvitationLinkId,
    pub request_id: ghinvite_core::RequestId,
}

/// Seed: installation 9 / account 100, two users, one invitation link with one
/// repo (repo ID 10), one approved invitation request, and one
/// `github_invitation` row in `Sent` state with upstream ID 9988.
pub(crate) async fn seed_pending_invitation(
    storage: &ghinvite_storage_sqlx::SqlxStorage,
    repo_full_name: &str,
) -> SeededInvitation {
    use ghinvite_core::storage::{Storage, projection::ProjectionStorage};
    use ghinvite_core::{
        AccountType, GithubInvitationId, InvitationLink, InvitationLinkId, InvitationLinkRepo,
        InvitationState, Permission, RequestId, RequestState, SelectedRepos, Slug,
    };
    use rand::SeedableRng;

    storage
        .insert_installation(&ghinvite_core::Account {
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
    for (user_id, login) in [(7, "creator"), (8, "alice")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.into(),
                avatar_url: None,
                last_seen_at: dt("2026-05-04T12:00:00Z"),
            })
            .await
            .unwrap();
    }
    let link = InvitationLink {
        id: InvitationLinkId::new(),
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
        description: "AI coding workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: repo_full_name.into(),
        }],
    };
    let request_id = RequestId::new();
    let request = ghinvite_core::InvitationRequest {
        id: request_id,
        invitation_link_id: link.id,
        requester_id: 8,
        justification: None,
        state: RequestState::Approved,
        decided_by: Some(7),
        decided_at: Some(dt("2026-05-04T13:00:00Z")),
        decline_reason: None,
        decision_deadline: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    };
    storage
        .apply_transition(&ghinvite_core::storage::projection::fixture::envelope(
            &link,
            &[request],
            1,
        ))
        .await
        .unwrap();
    let invitation_id = GithubInvitationId::new();
    storage
        .insert_github_invitation(&ghinvite_core::GithubInvitation {
            id: invitation_id,
            invitation_request_id: request_id,
            repo_id: 10,
            github_invitation_id: Some(9988),
            state: InvitationState::Sent,
            error_message: None,
            created_at: dt("2026-05-04T13:00:00Z"),
            updated_at: dt("2026-05-04T13:00:00Z"),
        })
        .await
        .unwrap();
    SeededInvitation {
        invitation_id,
        link_id: link.id,
        request_id,
    }
}

/// One pending-invitation page from the GitHub list endpoint. `next` becomes a
/// `Link: rel="next"` header, so a client only sees the rest by following it.
pub(crate) fn invitation_page(
    url: &str,
    body: serde_json::Value,
    next: Option<&str>,
) -> ghinvite_github::mocks::Expectation {
    use ghinvite_github::mocks::Expectation;
    use ghinvite_github::transport::Method;
    let mut expectation = Expectation::ok_json(Method::Get, url, body);
    if let Some(next) = next {
        expectation
            .response
            .headers
            .insert("link".into(), format!("<{next}>; rel=\"next\""));
    }
    expectation
}

/// A pending-invitation list entry for GitHub invitation ID `id`, addressed to
/// the requester seeded by [`seed_pending_invitation`].
pub(crate) fn invitation_item(id: u64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "invitee": {"id": 8, "login": "alice"},
        "permissions": "write",
        "created_at": "2026-05-04T13:00:00Z"
    })
}
