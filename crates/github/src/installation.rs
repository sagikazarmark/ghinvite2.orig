//! App-installation client: holds the App-JWT signer + a token cache, and
//! exposes the v1 installation-side endpoints.

use crate::error::Result;
use crate::jwt::AppJwtSigner;
use crate::payloads::{
    GhCollaboratorInvite, GhInstallationRepos, GhInstallationToken, GhInvitationListItem, GhRepo,
};
use crate::token_cache::TokenCache;
use crate::transport::{HttpTransport, Method, Request};
use chrono::Utc;
use std::sync::Arc;

const DEFAULT_BASE_URL: &str = "https://api.github.com";

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

    /// `GET /installation/repositories` — paginated upstream; v1 only follows
    /// page 1 (per_page=100). If GitHub ever ships a customer with > 100 repos
    /// per install, Plan 3's `Reconcile::sweep` adds pagination there.
    pub async fn list_installation_repos(
        &self,
        installation_id: u64,
    ) -> Result<GhInstallationRepos> {
        let req = self
            .auth_request(
                installation_id,
                Method::Get,
                "/installation/repositories?per_page=100",
            )
            .await?;
        self.transport.send(req).await?.ensure_success()?.json()
    }

    /// `GET /repos/{owner}/{repo}` — small surface for display + access
    /// confirmation.
    ///
    /// **Errors:** `Error::Status { status: 404, .. }` if the App lost access
    /// to the repo (caller should treat that as `selected_repos` drift).
    pub async fn get_repo(&self, installation_id: u64, owner: &str, repo: &str) -> Result<GhRepo> {
        let path = format!("/repos/{}/{}", path_seg(owner), path_seg(repo));
        let req = self
            .auth_request(installation_id, Method::Get, &path)
            .await?;
        self.transport.send(req).await?.ensure_success()?.json()
    }

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
            _ => Err(resp.status_error()),
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
            _ => Err(resp.status_error()),
        }
    }
}

#[cfg(test)]
mod token_mint_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use std::collections::BTreeMap;

    /// Use the same static test key as `crates/github/src/jwt.rs` to keep
    /// signing fast (~milliseconds, not seconds).
    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    fn signer() -> AppJwtSigner {
        AppJwtSigner::from_pkcs8_pem(123, TEST_KEY_PEM).unwrap()
    }

    fn token_response_body() -> Vec<u8> {
        // expires far in the future — these tests don't exercise expiry logic.
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

#[cfg(test)]
mod read_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use std::collections::BTreeMap;

    /// Use the same static test key as the other test modules to keep
    /// signing fast.
    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    fn signer() -> AppJwtSigner {
        AppJwtSigner::from_pkcs8_pem(123, TEST_KEY_PEM).unwrap()
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
            Expectation::status(Method::Get, "https://api.github.test/repos/acme/gone", 404),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client.get_repo(9, "acme", "gone").await.unwrap_err();
        assert_eq!(err.status(), Some(404));
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use domain::Permission;
    use std::collections::BTreeMap;

    /// Same static test key, same fast signer.
    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    fn signer() -> AppJwtSigner {
        AppJwtSigner::from_pkcs8_pem(123, TEST_KEY_PEM).unwrap()
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

#[cfg(test)]
mod reconcile_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use std::collections::BTreeMap;

    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    fn signer() -> AppJwtSigner {
        AppJwtSigner::from_pkcs8_pem(123, TEST_KEY_PEM).unwrap()
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
        assert!(
            client
                .is_collaborator(9, "acme", "api", "octocat")
                .await
                .unwrap()
        );
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
        assert!(
            !client
                .is_collaborator(9, "acme", "api", "notamember")
                .await
                .unwrap()
        );
    }
}
