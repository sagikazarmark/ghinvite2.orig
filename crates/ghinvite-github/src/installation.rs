//! App-installation client: holds the App-JWT signer + a token cache, and
//! exposes the v1 installation-side endpoints.

use crate::error::Result;
use crate::jwt::AppJwtSigner;
use crate::payloads::{
    GhCollaboratorInvite, GhInstallationRepos, GhInstallationToken, GhInvitationListItem, GhRepo,
};
use crate::token_cache::TokenCache;
use crate::transport::{HttpTransport, Method, Request};
use crate::util::path_segment;
use chrono::Utc;
use std::sync::Arc;

const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Backstop on the pending-invitation walk. At 100 per page this is far past
/// any real repository; exceeding it is a paging fault, not a complete list.
const MAX_INVITATION_PAGES: usize = 1000;

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
    /// App-authenticated identity/status observation, independent of cached tokens.
    pub async fn get_installation(
        &self,
        installation_id: u64,
    ) -> Result<crate::payloads::GhAppInstallation> {
        let jwt = self.signer.sign(Utc::now())?;
        let request = Request::new(
            Method::Get,
            format!("{}/app/installations/{installation_id}", self.base_url),
        )
        .header("accept", "application/vnd.github+json")
        .header("authorization", format!("Bearer {jwt}"))
        .header("user-agent", "ghinvite")
        .header("x-github-api-version", "2022-11-28");
        self.transport.send(request).await?.ensure_success()?.json()
    }

    /// Complete numeric repository scope. A partial or changing pagination result
    /// is unknown, never an authoritative removal of the missing repositories.
    pub async fn all_installation_repo_ids(&self, installation_id: u64) -> Result<Vec<u64>> {
        let mut ids = std::collections::BTreeSet::new();
        let mut expected = None;
        for page in 1..=1000 {
            let path = if page == 1 {
                "/installation/repositories?per_page=100".into()
            } else {
                format!("/installation/repositories?per_page=100&page={page}")
            };
            let request = self
                .auth_request(installation_id, Method::Get, &path)
                .await?;
            let result: GhInstallationRepos = self
                .transport
                .send(request)
                .await?
                .ensure_success()?
                .json()?;
            if expected.is_some_and(|count| count != result.total_count) {
                break;
            }
            expected = Some(result.total_count);
            let empty = result.repositories.is_empty();
            for repo in result.repositories {
                if repo.id == 0 || !ids.insert(repo.id) {
                    return Err(crate::Error::InvalidInput(
                        "inconsistent repository refresh".into(),
                    ));
                }
            }
            if ids.len() as u64 == result.total_count {
                return Ok(ids.into_iter().collect());
            }
            if empty || ids.len() as u64 > result.total_count {
                break;
            }
        }
        Err(crate::Error::InvalidInput(
            "incomplete repository refresh".into(),
        ))
    }

    /// Read collaborator permission and the identity returned with it. Unlike
    /// a login-only 204 membership probe this provides numeric identity evidence.
    pub async fn collaborator_permission(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
        login: &str,
    ) -> Result<(u64, String)> {
        #[derive(serde::Deserialize)]
        struct Permission {
            user: crate::payloads::GhUser,
            permission: String,
            #[serde(default)]
            role_name: Option<String>,
        }
        let path = format!(
            "/repos/{}/{}/collaborators/{}/permission",
            path_segment(owner),
            path_segment(repo),
            path_segment(login)
        );
        let req = self
            .auth_request(installation_id, Method::Get, &path)
            .await?;
        let result: Permission = self.transport.send(req).await?.ensure_success()?.json()?;
        Ok((
            result.user.id,
            result.role_name.unwrap_or(result.permission),
        ))
    }
    /// Resolve immutable user identity to addressing data and revalidate that
    /// address. These are read-only calls; a mismatch must never authorize PUT.
    pub async fn verified_user(
        &self,
        installation_id: u64,
        user_id: u64,
    ) -> Result<crate::payloads::GhUser> {
        let req = self
            .auth_request(installation_id, Method::Get, &format!("/user/{user_id}"))
            .await?;
        let user: crate::payloads::GhUser =
            self.transport.send(req).await?.ensure_success()?.json()?;
        let req = self
            .auth_request(
                installation_id,
                Method::Get,
                &format!("/users/{}", path_segment(&user.login)),
            )
            .await?;
        let addressed: crate::payloads::GhUser =
            self.transport.send(req).await?.ensure_success()?.json()?;
        if user.id != user_id || addressed.id != user_id || user.login.is_empty() {
            return Err(crate::Error::InvalidInput(
                "requester identity mismatch".into(),
            ));
        }
        Ok(user)
    }

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
    #[tracing::instrument(skip(self), fields(installation_id))]
    pub async fn mint_installation_token(
        &self,
        installation_id: u64,
    ) -> Result<GhInstallationToken> {
        tracing::info!("minting installation token");
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
        match self.transport.send(req).await?.ensure_success() {
            Ok(resp) => resp.json(),
            Err(err) => {
                tracing::warn!(?err, "installation token mint failed");
                Err(err)
            }
        }
    }

    /// Returns a non-expired installation token, refreshing through GitHub if
    /// the cached one is gone or within 60s of expiry.
    #[tracing::instrument(skip(self), fields(installation_id))]
    pub async fn installation_token(&self, installation_id: u64) -> Result<String> {
        if let Some(t) = self.cache.get_fresh(installation_id, Utc::now()) {
            tracing::trace!("installation token cache hit");
            return Ok(t);
        }
        tracing::debug!("installation token cache miss; refreshing from GitHub");
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
    #[tracing::instrument(skip(self), fields(installation_id, owner = tracing::field::Empty, repo = tracing::field::Empty))]
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
        match self.transport.send(req).await?.ensure_success() {
            Ok(resp) => resp.json(),
            Err(err) => {
                tracing::warn!(status = ?err.status(), "github request failed");
                Err(err)
            }
        }
    }

    /// `GET /repos/{owner}/{repo}` — small surface for display + access
    /// confirmation.
    ///
    /// **Errors:** `Error::Status { status: 404, .. }` if the App lost access
    /// to the repo (caller should treat that as `selected_repos` drift).
    #[tracing::instrument(skip(self), fields(installation_id, owner, repo))]
    pub async fn get_repo(&self, installation_id: u64, owner: &str, repo: &str) -> Result<GhRepo> {
        let path = format!("/repos/{}/{}", path_segment(owner), path_segment(repo));
        let req = self
            .auth_request(installation_id, Method::Get, &path)
            .await?;
        match self.transport.send(req).await?.ensure_success() {
            Ok(resp) => resp.json(),
            Err(err) => {
                tracing::warn!(status = ?err.status(), "github request failed");
                Err(err)
            }
        }
    }

    /// `PUT /repos/{owner}/{repo}/collaborators/{username}`. Returns:
    /// - `Ok(Some(invitation_id))` on 201 — recipient now has a pending invitation.
    /// - `Ok(None)` on 204 — recipient was already a collaborator (no invitation
    ///   created). Caller should treat as immediate-accept.
    /// - `Err(Error::Status)` on any other status.
    ///
    /// **422 sub-codes:** GitHub returns 422 with body `{"message":"Validation Failed",
    /// "errors":[{"code":"...","field":"..."}]}` for permission validation,
    /// already-declined invitations, etc. Callers wanting to distinguish these
    /// must currently parse `Error::Status::body` themselves; v1 just surfaces
    /// the raw body. See spec §16 for the agreed error-handling discipline.
    #[tracing::instrument(skip(self, username), fields(installation_id, owner, repo))]
    pub async fn add_collaborator(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
        username: &str,
        permission: ghinvite_core::Permission,
    ) -> Result<Option<u64>> {
        let path = format!(
            "/repos/{}/{}/collaborators/{}",
            path_segment(owner),
            path_segment(repo),
            path_segment(username)
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
            _ => {
                let err = resp.status_error();
                tracing::warn!(status = ?err.status(), "github request failed");
                Err(err)
            }
        }
    }

    /// `DELETE /repos/{owner}/{repo}/invitations/{invitation_id}` — used to
    /// cancel a pending invitation.
    ///
    /// **Errors:** `Error::Status { status: 404 }` if the invitation no longer
    /// exists (already accepted/declined/cancelled).
    #[tracing::instrument(skip(self), fields(installation_id, owner, repo))]
    pub async fn delete_invitation(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
        invitation_id: u64,
    ) -> Result<()> {
        let path = format!(
            "/repos/{}/{}/invitations/{}",
            path_segment(owner),
            path_segment(repo),
            invitation_id
        );
        let req = self
            .auth_request(installation_id, Method::Delete, &path)
            .await?;
        match self.transport.send(req).await?.ensure_success() {
            Ok(_) => Ok(()),
            Err(err) => {
                tracing::warn!(status = ?err.status(), "github request failed");
                Err(err)
            }
        }
    }

    /// `GET /repos/{owner}/{repo}/invitations` — every pending invitation on a
    /// repo. Used by the reconciler to check that our `github_invitations` rows
    /// in `sent` state still exist upstream, and by delivery recovery to look
    /// for a retained create.
    ///
    /// Follows GitHub's `Link: rel="next"` pages to the end. Callers read
    /// absence from this list as evidence that an invitation is gone, so a
    /// partial walk is an error, never a shorter list: a failed, malformed, or
    /// unfollowable later page leaves the observation unknown.
    #[tracing::instrument(skip(self), fields(installation_id, owner, repo))]
    pub async fn list_invitations(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GhInvitationListItem>> {
        let mut path = format!(
            "/repos/{}/{}/invitations?per_page=100",
            path_segment(owner),
            path_segment(repo)
        );
        let mut listed = Vec::new();
        // Every page already walked. A cycle of any length — not just a link
        // back to the page in hand — is a paging fault, and catching it here
        // spends one request on it instead of the whole page budget.
        let mut walked = std::collections::HashSet::new();
        for _ in 0..MAX_INVITATION_PAGES {
            if !walked.insert(path.clone()) {
                return Err(crate::Error::InvalidInput(format!(
                    "pagination returns to {path}"
                )));
            }
            let req = self
                .auth_request(installation_id, Method::Get, &path)
                .await?;
            let resp = match self.transport.send(req).await?.ensure_success() {
                Ok(resp) => resp,
                Err(err) => {
                    tracing::warn!(status = ?err.status(), "github request failed");
                    return Err(err);
                }
            };
            let next = crate::pagination::next_page_path(&resp, &self.base_url)?;
            listed.extend(resp.json::<Vec<GhInvitationListItem>>()?);
            match next {
                Some(next) => path = next,
                None => return Ok(listed),
            }
        }
        Err(crate::Error::InvalidInput(
            "incomplete pending invitation listing".into(),
        ))
    }

    /// `GET /repos/{owner}/{repo}/collaborators/{username}` — confirm membership.
    /// GitHub returns 204 if the user *is* a collaborator and 404 otherwise.
    /// Used to confirm an invitation acceptance when the webhook is missed.
    #[tracing::instrument(skip(self, username), fields(installation_id, owner, repo))]
    pub async fn is_collaborator(
        &self,
        installation_id: u64,
        owner: &str,
        repo: &str,
        username: &str,
    ) -> Result<bool> {
        let path = format!(
            "/repos/{}/{}/collaborators/{}",
            path_segment(owner),
            path_segment(repo),
            path_segment(username)
        );
        let req = self
            .auth_request(installation_id, Method::Get, &path)
            .await?;
        let resp = self.transport.send(req).await?;
        match resp.status {
            204 => Ok(true),
            404 => Ok(false),
            _ => {
                let err = resp.status_error();
                tracing::warn!(status = ?err.status(), "github request failed");
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod token_mint_tests {
    use super::*;
    use crate::mocks::{Expectation, MockTransport};
    use crate::transport::Response;
    use std::collections::BTreeMap;

    /// Use the same static test key as `crates/ghinvite-github/src/jwt.rs` to keep
    /// signing fast (~milliseconds, not seconds).
    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    fn signer() -> AppJwtSigner {
        AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap()
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
        AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap()
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
    use ghinvite_core::Permission;
    use std::collections::BTreeMap;

    /// Same static test key, same fast signer.
    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    fn signer() -> AppJwtSigner {
        AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap()
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
    async fn add_collaborator_separates_a_throttled_403_from_a_denied_one() {
        for (headers, message, expected_limit) in [
            (
                vec![("retry-after", "42")],
                "You have exceeded a secondary rate limit",
                Some(crate::RateLimit {
                    scope: crate::RateLimitScope::Secondary,
                    retry_after: Some(std::time::Duration::from_secs(42)),
                }),
            ),
            (vec![], "Resource not accessible by integration", None),
        ] {
            let mock = MockTransport::scripted(vec![
                token_mint_expectation(),
                Expectation {
                    method: Method::Put,
                    url: "https://api.github.test/repos/acme/api/collaborators/octocat".into(),
                    required_headers: BTreeMap::new(),
                    expected_body: None,
                    response: Response {
                        status: 403,
                        headers: headers
                            .into_iter()
                            .map(|(k, v)| (k.to_owned(), v.to_owned()))
                            .collect(),
                        body: serde_json::json!({ "message": message }).to_string().into(),
                    },
                },
            ]);
            let client = InstallationClient::new(Arc::new(mock.clone()), signer())
                .with_base("https://api.github.test");
            let err = client
                .add_collaborator(9, "acme", "api", "octocat", Permission::Push)
                .await
                .unwrap_err();
            // Same status either way; the classification is what separates them.
            assert_eq!(err.status(), Some(403));
            assert_eq!(
                err.rate_limit(),
                expected_limit,
                "unexpected classification of {message:?}: {err:?}"
            );
        }
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
        AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap()
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

    /// One pending-invitation page. `next` becomes a GitHub-style `Link`
    /// header so the client has to follow it to see the rest.
    fn invitation_page(url: &str, body: serde_json::Value, next: Option<&str>) -> Expectation {
        let mut expectation = Expectation::ok_json(Method::Get, url, body);
        if let Some(next) = next {
            expectation.response.headers.insert(
                "link".into(),
                format!("<{next}>; rel=\"next\", <{next}>; rel=\"last\""),
            );
        }
        expectation
    }

    fn invitation_json(id: u64) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "invitee": {"id": 42, "login": "octocat"},
            "permissions": "write",
            "created_at": "2026-05-04T12:00:00Z"
        })
    }

    #[tokio::test]
    async fn list_invitations_follows_every_page() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_json(1)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                serde_json::json!([invitation_json(2)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=3"),
            ),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=3",
                serde_json::json!([invitation_json(3)]),
                None,
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let listed = client.list_invitations(9, "acme", "api").await.unwrap();
        assert_eq!(
            listed.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn list_invitations_follows_repository_id_next_links() {
        // GitHub answers with `/repositories/{id}/...` next links on this endpoint.
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_json(1)]),
                Some("https://api.github.test/repositories/77/invitations?per_page=100&page=2"),
            ),
            invitation_page(
                "https://api.github.test/repositories/77/invitations?per_page=100&page=2",
                serde_json::json!([invitation_json(2)]),
                None,
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let listed = client.list_invitations(9, "acme", "api").await.unwrap();
        assert_eq!(listed.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2]);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn list_invitations_later_page_failure_is_not_a_short_list() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_json(1)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                500,
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client.list_invitations(9, "acme", "api").await.unwrap_err();
        assert_eq!(err.status(), Some(500));
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn list_invitations_malformed_later_page_is_not_a_short_list() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_json(1)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                serde_json::json!({"message": "not a list"}),
                None,
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client.list_invitations(9, "acme", "api").await.unwrap_err();
        assert!(matches!(err, crate::Error::Decode(_)), "got {err:?}");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn list_invitations_refuses_a_next_link_off_the_api_host() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_json(1)]),
                Some("https://evil.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client.list_invitations(9, "acme", "api").await.unwrap_err();
        assert!(matches!(err, crate::Error::InvalidInput(_)), "got {err:?}");
        // The off-host page was never requested.
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn list_invitations_rejects_a_next_link_that_does_not_advance() {
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_json(1)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100"),
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client.list_invitations(9, "acme", "api").await.unwrap_err();
        assert!(matches!(err, crate::Error::InvalidInput(_)), "got {err:?}");
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn list_invitations_rejects_a_pagination_cycle_on_the_first_repeat() {
        // Page two links back to page one. The script holds exactly the two
        // requests the walk is allowed to spend before it gives up — anything
        // more exhausts it and panics, so the page budget cannot absorb a cycle.
        let mock = MockTransport::scripted(vec![
            token_mint_expectation(),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([invitation_json(1)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                serde_json::json!([invitation_json(2)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100"),
            ),
        ]);
        let client = InstallationClient::new(Arc::new(mock.clone()), signer())
            .with_base("https://api.github.test");
        let err = client.list_invitations(9, "acme", "api").await.unwrap_err();
        assert!(matches!(err, crate::Error::InvalidInput(_)), "got {err:?}");
        mock.assert_exhausted();
    }
}
