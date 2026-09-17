use ghinvite_core::storage::Storage;
use ghinvite_core::{InvitationLink, InvitationRequest, RequestState, Slug};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublicInvitationLinkContext {
    pub(crate) slug: Slug,
    pub(crate) link: InvitationLink,
    pub(crate) requests: Vec<InvitationRequest>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RequesterRequestSelection {
    pub(crate) current_status: Option<RequestState>,
    pub(crate) retry_notice: Option<RequestState>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolutionError {
    #[error("invalid slug")]
    InvalidSlug,
    #[error("unknown slug")]
    UnknownSlug,
    #[error("stored slug mismatch")]
    SlugMismatch,
    #[error("storage error: {0}")]
    Storage(#[from] ghinvite_core::storage::Error),
}

pub(crate) async fn resolve_public_invitation_link_context(
    storage: &dyn Storage,
    raw_slug: &str,
) -> Result<PublicInvitationLinkContext, ResolutionError> {
    let slug = Slug::from_string(raw_slug.to_string()).map_err(|_| ResolutionError::InvalidSlug)?;
    let link = storage
        .get_invitation_link_by_slug(slug.as_str())
        .await
        .map_err(ResolutionError::Storage)?
        .ok_or(ResolutionError::UnknownSlug)?;

    if !link.slug.ct_eq(&slug) {
        return Err(ResolutionError::SlugMismatch);
    }

    let requests = storage
        .list_requests_for_link(link.id)
        .await
        .map_err(ResolutionError::Storage)?;

    Ok(PublicInvitationLinkContext {
        slug,
        link,
        requests,
    })
}

pub(crate) fn select_requester_request_state(
    requests: &[InvitationRequest],
    requester_id: u64,
) -> RequesterRequestSelection {
    let mut approved = None;
    let mut retry_notice = None;

    for request in requests
        .iter()
        .filter(|request| request.requester_id == requester_id)
    {
        match request.state {
            RequestState::Pending => {
                return RequesterRequestSelection {
                    current_status: Some(RequestState::Pending),
                    retry_notice: None,
                };
            }
            RequestState::Approved => approved = Some(RequestState::Approved),
            RequestState::Declined | RequestState::Expired | RequestState::Cancelled => {
                if retry_notice.is_none() {
                    retry_notice = Some(request.state);
                }
            }
        }
    }

    if approved.is_some() {
        RequesterRequestSelection {
            current_status: approved,
            retry_notice: None,
        }
    } else {
        RequesterRequestSelection {
            current_status: None,
            retry_notice,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ResolutionError, resolve_public_invitation_link_context, select_requester_request_state,
    };
    use chrono::{DateTime, Utc};
    use ghinvite_core::audit::AuditEvent;
    use ghinvite_core::storage::{GithubInvitationUpdate, RequestDecision, Storage};
    use ghinvite_core::{
        Account, GithubInvitation, GithubInvitationId, InvitationLink, InvitationLinkId,
        InvitationLinkRepo, InvitationRequest, Permission, RequestId, RequestState, Slug, User,
    };

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[derive(Default)]
    struct FakeStorage {
        link: Option<InvitationLink>,
        requests: Vec<InvitationRequest>,
        fail_link_lookup: bool,
        fail_requests_lookup: bool,
        slug_lookup_count: std::sync::Mutex<usize>,
    }

    fn sample_link(slug: &str) -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
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
            description: "AI coding workshop".into(),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        }
    }

    fn sample_request(
        id: RequestId,
        invitation_link_id: InvitationLinkId,
        requester_id: u64,
        state: RequestState,
    ) -> InvitationRequest {
        InvitationRequest {
            id,
            invitation_link_id,
            requester_id,
            justification: None,
            state,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            decision_deadline: None,
            created_at: dt("2026-05-20T11:00:00Z"),
        }
    }

    #[async_trait::async_trait]
    impl Storage for FakeStorage {
        async fn insert_installation(
            &self,
            _account: &Account,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn mark_installation_uninstalled(
            &self,
            _installation_id: u64,
            _when: DateTime<Utc>,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn update_installation_repos(
            &self,
            _installation_id: u64,
            _selected: &ghinvite_core::SelectedRepos,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn get_installation(
            &self,
            _installation_id: u64,
        ) -> ghinvite_core::storage::Result<Option<Account>> {
            unimplemented!()
        }

        async fn get_active_installation_by_account_id(
            &self,
            _account_id: u64,
        ) -> ghinvite_core::storage::Result<Option<Account>> {
            unimplemented!()
        }

        async fn get_active_installation_by_login(
            &self,
            _login: &str,
        ) -> ghinvite_core::storage::Result<Option<Account>> {
            unimplemented!()
        }

        async fn list_active_installations(&self) -> ghinvite_core::storage::Result<Vec<Account>> {
            unimplemented!()
        }

        async fn upsert_user(&self, _user: &User) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn get_user(&self, _user_id: u64) -> ghinvite_core::storage::Result<Option<User>> {
            unimplemented!()
        }

        async fn insert_invitation_link(
            &self,
            _link: &InvitationLink,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn update_invitation_link_metadata(
            &self,
            _account_id: u64,
            _id: InvitationLinkId,
            _description: &str,
            _internal_note: Option<&str>,
        ) -> ghinvite_core::storage::Result<()> {
            panic!("unexpected update_invitation_link_metadata")
        }

        async fn mark_invitation_link_revoked(
            &self,
            _id: InvitationLinkId,
            _by_user: u64,
            _when: DateTime<Utc>,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn get_invitation_link_by_id(
            &self,
            _id: InvitationLinkId,
        ) -> ghinvite_core::storage::Result<Option<InvitationLink>> {
            unimplemented!()
        }

        async fn get_invitation_link_by_slug(
            &self,
            _slug: &str,
        ) -> ghinvite_core::storage::Result<Option<InvitationLink>> {
            *self.slug_lookup_count.lock().unwrap() += 1;
            if self.fail_link_lookup {
                return Err(ghinvite_core::storage::Error::Database(
                    "invitation link lookup failed".into(),
                ));
            }
            Ok(self.link.clone())
        }

        async fn list_invitation_links_for_account(
            &self,
            _account_id: u64,
        ) -> ghinvite_core::storage::Result<Vec<InvitationLink>> {
            unimplemented!()
        }

        async fn insert_invitation_request_and_increment_uses(
            &self,
            _request: &InvitationRequest,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn record_request_decision(
            &self,
            _decision: &RequestDecision,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn get_invitation_request(
            &self,
            _id: RequestId,
        ) -> ghinvite_core::storage::Result<Option<InvitationRequest>> {
            unimplemented!()
        }

        async fn list_pending_requests_for_account(
            &self,
            _account_id: u64,
        ) -> ghinvite_core::storage::Result<Vec<InvitationRequest>> {
            unimplemented!()
        }

        async fn list_requests_for_link(
            &self,
            _link_id: InvitationLinkId,
        ) -> ghinvite_core::storage::Result<Vec<InvitationRequest>> {
            if self.fail_requests_lookup {
                return Err(ghinvite_core::storage::Error::Database(
                    "request lookup failed".into(),
                ));
            }
            Ok(self.requests.clone())
        }

        async fn insert_github_invitation(
            &self,
            _invitation: &GithubInvitation,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn update_github_invitation(
            &self,
            _update: &GithubInvitationUpdate,
        ) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }

        async fn get_github_invitation(
            &self,
            _id: GithubInvitationId,
        ) -> ghinvite_core::storage::Result<Option<GithubInvitation>> {
            unimplemented!()
        }

        async fn get_github_invitation_by_github_id(
            &self,
            _github_id: u64,
        ) -> ghinvite_core::storage::Result<Option<GithubInvitation>> {
            unimplemented!()
        }

        async fn list_pending_github_invitations_for_installation(
            &self,
            _installation_id: u64,
        ) -> ghinvite_core::storage::Result<Vec<GithubInvitation>> {
            unimplemented!()
        }

        async fn list_audit_events(
            &self,
            _: u64,
            _: Option<ghinvite_core::audit::EventType>,
            _: ghinvite_core::storage::AuditPosition,
        ) -> ghinvite_core::storage::Result<ghinvite_core::storage::AuditPage> {
            unimplemented!()
        }

        async fn invitation_link_belongs_to_account(
            &self,
            _: u64,
            _: InvitationLinkId,
        ) -> ghinvite_core::storage::Result<bool> {
            unimplemented!()
        }

        async fn audit(&self, _event: &AuditEvent) -> ghinvite_core::storage::Result<()> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn malformed_slug_returns_invalid_slug_without_storage_lookup() {
        let storage = FakeStorage::default();

        let err = resolve_public_invitation_link_context(&storage, "not-a-valid-slug")
            .await
            .unwrap_err();

        assert!(matches!(err, ResolutionError::InvalidSlug));
        assert_eq!(*storage.slug_lookup_count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn unknown_slug_returns_unknown_slug_after_storage_lookup() {
        let storage = FakeStorage::default();

        let err = resolve_public_invitation_link_context(&storage, "abcdEFGH01234567")
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

        let err = resolve_public_invitation_link_context(&storage, "abcdEFGH01234567")
            .await
            .unwrap_err();

        assert!(matches!(err, ResolutionError::SlugMismatch));
    }

    #[tokio::test]
    async fn inactive_link_still_resolves_with_requests() {
        let mut link = sample_link("abcdEFGH01234567");
        link.revoked_at = Some(dt("2026-05-20T11:00:00Z"));
        link.revoked_by = Some(701);
        let request = sample_request(RequestId::new(), link.id, 802, RequestState::Pending);
        let storage = FakeStorage {
            requests: vec![request.clone()],
            link: Some(link),
            ..FakeStorage::default()
        };

        let context = resolve_public_invitation_link_context(&storage, "abcdEFGH01234567")
            .await
            .unwrap();

        assert_eq!(context.slug.as_str(), "abcdEFGH01234567");
        assert_eq!(context.requests, vec![request]);
        assert!(!context.link.is_active(dt("2026-05-20T12:00:00Z")));
    }

    #[test]
    fn requester_selection_prefers_pending_over_other_states() {
        let link = sample_link("abcdEFGH01234567");
        let requests = vec![
            sample_request(RequestId::new(), link.id, 802, RequestState::Declined),
            sample_request(RequestId::new(), link.id, 802, RequestState::Approved),
            sample_request(RequestId::new(), link.id, 802, RequestState::Pending),
            sample_request(RequestId::new(), link.id, 999, RequestState::Pending),
        ];

        let selection = select_requester_request_state(&requests, 802);

        assert_eq!(selection.current_status, Some(RequestState::Pending));
        assert_eq!(selection.retry_notice, None);
    }

    #[test]
    fn requester_selection_prefers_approved_before_retryable_states() {
        let link = sample_link("abcdEFGH01234567");
        let requests = vec![
            sample_request(RequestId::new(), link.id, 802, RequestState::Expired),
            sample_request(RequestId::new(), link.id, 802, RequestState::Approved),
        ];

        let selection = select_requester_request_state(&requests, 802);

        assert_eq!(selection.current_status, Some(RequestState::Approved));
        assert_eq!(selection.retry_notice, None);
    }

    #[test]
    fn requester_selection_keeps_newest_retryable_state() {
        let link = sample_link("abcdEFGH01234567");
        let requests = vec![
            sample_request(RequestId::new(), link.id, 802, RequestState::Cancelled),
            sample_request(RequestId::new(), link.id, 802, RequestState::Declined),
            sample_request(RequestId::new(), link.id, 999, RequestState::Approved),
        ];

        let selection = select_requester_request_state(&requests, 802);

        assert_eq!(selection.current_status, None);
        assert_eq!(selection.retry_notice, Some(RequestState::Cancelled));
    }
}
