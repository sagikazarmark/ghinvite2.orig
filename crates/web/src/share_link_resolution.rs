use crate::error::WebError;
use chrono::{DateTime, Utc};
use domain::{RequestId, RequestState, ShareLink, Slug};
use storage::Storage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingRequestPolicy {
    Ignore,
    RedirectForRecipient { recipient_id: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PublicShareLinkResolution {
    Available { slug: Slug, link: ShareLink },
    PendingRequest { slug: Slug, request_id: RequestId },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolutionError {
    #[error("invalid slug")]
    InvalidSlug,
    #[error("unknown slug")]
    UnknownSlug,
    #[error("stored slug mismatch")]
    SlugMismatch,
    #[error("inactive link")]
    Inactive,
    #[error("storage error: {0}")]
    Storage(#[from] storage::Error),
}

impl ResolutionError {
    pub(crate) fn into_public_error(self) -> WebError {
        WebError::NotFound
    }
}

pub(crate) async fn resolve_public_share_link(
    storage: &dyn Storage,
    raw_slug: &str,
    now: DateTime<Utc>,
    pending_policy: PendingRequestPolicy,
) -> Result<PublicShareLinkResolution, ResolutionError> {
    let slug = Slug::from_string(raw_slug.to_string()).map_err(|_| ResolutionError::InvalidSlug)?;
    let link = storage
        .get_share_link_by_slug(slug.as_str())
        .await
        .map_err(ResolutionError::Storage)?
        .ok_or(ResolutionError::UnknownSlug)?;

    if !link.slug.ct_eq(&slug) {
        return Err(ResolutionError::SlugMismatch);
    }
    if !link.is_active(now) {
        return Err(ResolutionError::Inactive);
    }

    if let PendingRequestPolicy::RedirectForRecipient { recipient_id } = pending_policy {
        let requests = storage
            .list_requests_for_link(link.id)
            .await
            .map_err(ResolutionError::Storage)?;
        if let Some(request) = requests.into_iter().find(|request| {
            request.requester_id == recipient_id && request.state == RequestState::Pending
        }) {
            return Ok(PublicShareLinkResolution::PendingRequest {
                slug,
                request_id: request.id,
            });
        }
    }

    Ok(PublicShareLinkResolution::Available { slug, link })
}

#[cfg(test)]
mod tests {
    use super::{PendingRequestPolicy, ResolutionError, resolve_public_share_link};
    use audit::AuditEvent;
    use chrono::{DateTime, Utc};
    use domain::{
        Account, GithubInvitation, GithubInvitationId, InvitationRequest, Permission, RequestId,
        RequestState, ShareLink, ShareLinkId, ShareLinkRepo, Slug, User,
    };
    use storage::{GithubInvitationUpdate, RequestDecision, Storage};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[derive(Default)]
    struct FakeStorage {
        link: Option<ShareLink>,
        requests: Vec<InvitationRequest>,
        fail_link_lookup: bool,
        fail_requests_lookup: bool,
        slug_lookup_count: std::sync::Mutex<usize>,
    }

    fn sample_link(slug: &str) -> ShareLink {
        ShareLink {
            id: ShareLinkId::new(),
            slug: Slug::from_string(slug.to_string()).unwrap(),
            installation_id: 1,
            account_id: 9001,
            created_by: 701,
            created_at: dt("2026-05-20T10:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Pull,
            approval_required: true,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![ShareLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        }
    }

    fn sample_request(
        id: RequestId,
        share_link_id: ShareLinkId,
        requester_id: u64,
        state: RequestState,
    ) -> InvitationRequest {
        InvitationRequest {
            id,
            share_link_id,
            requester_id,
            justification: None,
            state,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            created_at: dt("2026-05-20T11:00:00Z"),
        }
    }

    #[async_trait::async_trait]
    impl Storage for FakeStorage {
        async fn insert_installation(&self, _account: &Account) -> storage::Result<()> {
            unimplemented!()
        }

        async fn mark_installation_uninstalled(
            &self,
            _installation_id: u64,
            _when: DateTime<Utc>,
        ) -> storage::Result<()> {
            unimplemented!()
        }

        async fn update_installation_repos(
            &self,
            _installation_id: u64,
            _selected: &domain::SelectedRepos,
        ) -> storage::Result<()> {
            unimplemented!()
        }

        async fn get_installation(
            &self,
            _installation_id: u64,
        ) -> storage::Result<Option<Account>> {
            unimplemented!()
        }

        async fn get_active_installation_by_account_id(
            &self,
            _account_id: u64,
        ) -> storage::Result<Option<Account>> {
            unimplemented!()
        }

        async fn get_active_installation_by_login(
            &self,
            _login: &str,
        ) -> storage::Result<Option<Account>> {
            unimplemented!()
        }

        async fn list_active_installations(&self) -> storage::Result<Vec<Account>> {
            unimplemented!()
        }

        async fn upsert_user(&self, _user: &User) -> storage::Result<()> {
            unimplemented!()
        }

        async fn get_user(&self, _user_id: u64) -> storage::Result<Option<User>> {
            unimplemented!()
        }

        async fn insert_share_link(&self, _link: &ShareLink) -> storage::Result<()> {
            unimplemented!()
        }

        async fn mark_share_link_revoked(
            &self,
            _id: ShareLinkId,
            _by_user: u64,
            _when: DateTime<Utc>,
        ) -> storage::Result<()> {
            unimplemented!()
        }

        async fn get_share_link_by_id(
            &self,
            _id: ShareLinkId,
        ) -> storage::Result<Option<ShareLink>> {
            unimplemented!()
        }

        async fn get_share_link_by_slug(&self, _slug: &str) -> storage::Result<Option<ShareLink>> {
            *self.slug_lookup_count.lock().unwrap() += 1;
            if self.fail_link_lookup {
                return Err(storage::Error::Database("share link lookup failed".into()));
            }
            Ok(self.link.clone())
        }

        async fn list_share_links_for_account(
            &self,
            _account_id: u64,
        ) -> storage::Result<Vec<ShareLink>> {
            unimplemented!()
        }

        async fn insert_invitation_request_and_increment_uses(
            &self,
            _request: &InvitationRequest,
        ) -> storage::Result<()> {
            unimplemented!()
        }

        async fn record_request_decision(
            &self,
            _decision: &RequestDecision,
        ) -> storage::Result<()> {
            unimplemented!()
        }

        async fn get_invitation_request(
            &self,
            _id: RequestId,
        ) -> storage::Result<Option<InvitationRequest>> {
            unimplemented!()
        }

        async fn list_pending_requests_for_account(
            &self,
            _account_id: u64,
        ) -> storage::Result<Vec<InvitationRequest>> {
            unimplemented!()
        }

        async fn list_requests_for_link(
            &self,
            _link_id: ShareLinkId,
        ) -> storage::Result<Vec<InvitationRequest>> {
            if self.fail_requests_lookup {
                return Err(storage::Error::Database("request lookup failed".into()));
            }
            Ok(self.requests.clone())
        }

        async fn insert_github_invitation(
            &self,
            _invitation: &GithubInvitation,
        ) -> storage::Result<()> {
            unimplemented!()
        }

        async fn update_github_invitation(
            &self,
            _update: &GithubInvitationUpdate,
        ) -> storage::Result<()> {
            unimplemented!()
        }

        async fn get_github_invitation(
            &self,
            _id: GithubInvitationId,
        ) -> storage::Result<Option<GithubInvitation>> {
            unimplemented!()
        }

        async fn get_github_invitation_by_github_id(
            &self,
            _github_id: u64,
        ) -> storage::Result<Option<GithubInvitation>> {
            unimplemented!()
        }

        async fn list_pending_github_invitations_for_installation(
            &self,
            _installation_id: u64,
        ) -> storage::Result<Vec<GithubInvitation>> {
            unimplemented!()
        }

        async fn audit(&self, _event: &AuditEvent) -> storage::Result<()> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn malformed_slug_returns_invalid_slug_without_storage_lookup() {
        let storage = FakeStorage::default();

        let err = resolve_public_share_link(
            &storage,
            "not-a-valid-slug",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::Ignore,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ResolutionError::InvalidSlug));
        assert_eq!(*storage.slug_lookup_count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn unknown_slug_returns_unknown_slug_after_storage_lookup() {
        let storage = FakeStorage::default();

        let err = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::Ignore,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ResolutionError::UnknownSlug));
        assert_eq!(*storage.slug_lookup_count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn stored_slug_mismatch_returns_slug_mismatch() {
        let storage = FakeStorage {
            link: Some(sample_link("ZZZZZZZZZZZZZZZZ")),
            ..FakeStorage::default()
        };

        let err = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::Ignore,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ResolutionError::SlugMismatch));
    }

    #[tokio::test]
    async fn revoked_expired_and_exhausted_links_return_inactive() {
        let now = dt("2026-05-20T12:00:00Z");
        let mut revoked = sample_link("abcdEFGH01234567");
        revoked.revoked_at = Some(dt("2026-05-20T11:00:00Z"));
        revoked.revoked_by = Some(701);

        let mut expired = sample_link("abcdEFGH01234567");
        expired.expires_at = Some(dt("2026-05-20T12:00:00Z"));

        let mut exhausted = sample_link("abcdEFGH01234567");
        exhausted.max_uses = Some(1);
        exhausted.uses_count = 1;

        for link in [revoked, expired, exhausted] {
            let storage = FakeStorage {
                link: Some(link),
                ..FakeStorage::default()
            };

            let err = resolve_public_share_link(
                &storage,
                "abcdEFGH01234567",
                now,
                PendingRequestPolicy::Ignore,
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ResolutionError::Inactive));
        }
    }

    #[tokio::test]
    async fn same_recipient_pending_request_returns_pending_request_outcome() {
        let link = sample_link("abcdEFGH01234567");
        let request_id = RequestId::new();
        let storage = FakeStorage {
            requests: vec![sample_request(
                request_id,
                link.id,
                802,
                RequestState::Pending,
            )],
            link: Some(link),
            ..FakeStorage::default()
        };

        let resolved = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::RedirectForRecipient { recipient_id: 802 },
        )
        .await
        .unwrap();

        assert!(matches!(
            resolved,
            super::PublicShareLinkResolution::PendingRequest { request_id: id, .. } if id == request_id
        ));
    }

    #[tokio::test]
    async fn active_link_returns_available_with_requested_slug() {
        let link = sample_link("abcdEFGH01234567");
        let link_id = link.id;
        let storage = FakeStorage {
            link: Some(link),
            ..FakeStorage::default()
        };

        let resolved = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::Ignore,
        )
        .await
        .unwrap();

        assert!(matches!(
            resolved,
            super::PublicShareLinkResolution::Available { slug, link }
                if slug.as_str() == "abcdEFGH01234567" && link.id == link_id
        ));
    }

    #[tokio::test]
    async fn other_recipient_pending_request_does_not_redirect() {
        let link = sample_link("abcdEFGH01234567");
        let storage = FakeStorage {
            requests: vec![sample_request(
                RequestId::new(),
                link.id,
                999,
                RequestState::Pending,
            )],
            link: Some(link),
            ..FakeStorage::default()
        };

        let resolved = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::RedirectForRecipient { recipient_id: 802 },
        )
        .await
        .unwrap();

        assert!(matches!(
            resolved,
            super::PublicShareLinkResolution::Available { .. }
        ));
    }

    #[tokio::test]
    async fn same_recipient_non_pending_request_does_not_redirect() {
        let link = sample_link("abcdEFGH01234567");
        let storage = FakeStorage {
            requests: vec![sample_request(
                RequestId::new(),
                link.id,
                802,
                RequestState::Declined,
            )],
            link: Some(link),
            ..FakeStorage::default()
        };

        let resolved = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::RedirectForRecipient { recipient_id: 802 },
        )
        .await
        .unwrap();

        assert!(matches!(
            resolved,
            super::PublicShareLinkResolution::Available { .. }
        ));
    }

    #[tokio::test]
    async fn share_link_lookup_storage_error_returns_storage_error() {
        let storage = FakeStorage {
            fail_link_lookup: true,
            ..FakeStorage::default()
        };

        let err = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::Ignore,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ResolutionError::Storage(_)));
    }

    #[tokio::test]
    async fn pending_lookup_storage_error_returns_storage_error() {
        let storage = FakeStorage {
            fail_requests_lookup: true,
            link: Some(sample_link("abcdEFGH01234567")),
            ..FakeStorage::default()
        };

        let err = resolve_public_share_link(
            &storage,
            "abcdEFGH01234567",
            dt("2026-05-20T12:00:00Z"),
            PendingRequestPolicy::RedirectForRecipient { recipient_id: 802 },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ResolutionError::Storage(_)));
    }
}
