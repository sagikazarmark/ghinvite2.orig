use crate::error::{Result, WebError};
use ghinvite_core::storage::Storage;
use ghinvite_core::{InvitationLink, InvitationLinkId, InvitationRequest, RequestId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountAdminInvitationRequest {
    pub(crate) request: InvitationRequest,
    pub(crate) invitation_link: InvitationLink,
}

pub(crate) async fn find_account_admin_invitation_link(
    storage: &dyn Storage,
    account_id: u64,
    link_id: InvitationLinkId,
) -> Result<InvitationLink> {
    let link = storage
        .get_invitation_link_by_id(link_id)
        .await?
        .ok_or(WebError::NotFound)?;

    if link.account_id != account_id {
        return Err(WebError::NotFound);
    }

    Ok(link)
}

pub(crate) async fn find_account_admin_request(
    storage: &dyn Storage,
    account_id: u64,
    request_id: RequestId,
) -> Result<AccountAdminInvitationRequest> {
    let request = storage
        .get_invitation_request(request_id)
        .await?
        .ok_or(WebError::NotFound)?;
    let invitation_link =
        find_account_admin_invitation_link(storage, account_id, request.invitation_link_id).await?;

    Ok(AccountAdminInvitationRequest {
        request,
        invitation_link,
    })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use ghinvite_core::audit::AuditEvent;
    use ghinvite_core::storage::Storage;
    use ghinvite_core::storage::projection::fixture::Seed;
    use ghinvite_core::{
        Account, AccountType, GithubInvitation, GithubInvitationId, InvitationLink,
        InvitationLinkId, InvitationLinkRepo, InvitationRequest, Permission, RequestId,
        RequestState, SelectedRepos, Slug, User,
    };

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_account(installation_id: u64, account_id: u64, login: &str) -> Account {
        Account {
            installation_id,
            account_id,
            account_login: login.into(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        }
    }

    fn sample_user(user_id: u64, login: &str) -> User {
        User {
            user_id,
            login: login.into(),
            avatar_url: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    fn sample_link(
        account_id: u64,
        installation_id: u64,
        created_by: u64,
        slug: &str,
    ) -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::from_string(slug.to_string()).unwrap(),
            installation_id,
            account_id,
            created_by,
            created_at: dt("2026-05-04T12:00:00Z"),
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
        created_at: &str,
    ) -> InvitationRequest {
        InvitationRequest {
            id,
            invitation_link_id,
            requester_id,
            justification: Some("need repository access".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            decision_deadline: None,
            created_at: dt(created_at),
        }
    }

    async fn storage_with_accounts() -> ghinvite_storage_sqlx::SqlxStorage {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        storage
            .insert_installation(&sample_account(1, 9001, "acme"))
            .await
            .unwrap();
        storage
            .insert_installation(&sample_account(2, 9002, "other"))
            .await
            .unwrap();
        storage
            .upsert_user(&sample_user(701, "creator"))
            .await
            .unwrap();
        storage
    }

    #[derive(Default)]
    struct FakeStorage {
        pending_requests: Vec<InvitationRequest>,
        request: Option<InvitationRequest>,
        invitation_link: Option<InvitationLink>,
        user: Option<User>,
        fail_invitation_link_lookup: bool,
        fail_user_lookup: bool,
    }

    #[async_trait::async_trait]
    impl Storage for FakeStorage {
        async fn insert_installation(
            &self,
            _account: &Account,
        ) -> ghinvite_core::storage::Result<()> {
            panic!("insert_installation is not used by Account Admin read tests")
        }

        async fn mark_installation_uninstalled(
            &self,
            _installation_id: u64,
            _when: DateTime<Utc>,
        ) -> ghinvite_core::storage::Result<()> {
            panic!("mark_installation_uninstalled is not used by Account Admin read tests")
        }

        async fn update_installation_repos(
            &self,
            _installation_id: u64,
            _selected: &SelectedRepos,
        ) -> ghinvite_core::storage::Result<()> {
            panic!("update_installation_repos is not used by Account Admin read tests")
        }

        async fn get_installation(
            &self,
            _installation_id: u64,
        ) -> ghinvite_core::storage::Result<Option<Account>> {
            panic!("get_installation is not used by Account Admin read tests")
        }

        async fn get_active_installation_by_account_id(
            &self,
            _account_id: u64,
        ) -> ghinvite_core::storage::Result<Option<Account>> {
            panic!("get_active_installation_by_account_id is not used by Account Admin read tests")
        }

        async fn get_active_installation_by_login(
            &self,
            _login: &str,
        ) -> ghinvite_core::storage::Result<Option<Account>> {
            panic!("get_active_installation_by_login is not used by Account Admin read tests")
        }

        async fn list_active_installations(&self) -> ghinvite_core::storage::Result<Vec<Account>> {
            panic!("list_active_installations is not used by Account Admin read tests")
        }

        async fn upsert_user(&self, _user: &User) -> ghinvite_core::storage::Result<()> {
            panic!("upsert_user is not used by Account Admin read tests")
        }

        async fn get_user(&self, _user_id: u64) -> ghinvite_core::storage::Result<Option<User>> {
            if self.fail_user_lookup {
                return Err(ghinvite_core::storage::Error::Database(
                    "user lookup failed".into(),
                ));
            }

            Ok(self.user.clone())
        }

        async fn get_invitation_link_by_id(
            &self,
            _id: InvitationLinkId,
        ) -> ghinvite_core::storage::Result<Option<InvitationLink>> {
            if self.fail_invitation_link_lookup {
                return Err(ghinvite_core::storage::Error::Database(
                    "invitation link lookup failed".into(),
                ));
            }

            Ok(self.invitation_link.clone())
        }

        async fn list_invitation_links_for_account(
            &self,
            _account_id: u64,
        ) -> ghinvite_core::storage::Result<Vec<InvitationLink>> {
            panic!("list_invitation_links_for_account is not used by Account Admin read tests")
        }

        async fn get_invitation_request(
            &self,
            _id: RequestId,
        ) -> ghinvite_core::storage::Result<Option<InvitationRequest>> {
            Ok(self.request.clone())
        }

        async fn list_pending_requests_for_account(
            &self,
            _account_id: u64,
        ) -> ghinvite_core::storage::Result<Vec<InvitationRequest>> {
            Ok(self.pending_requests.clone())
        }

        async fn insert_github_invitation(
            &self,
            _invitation: &GithubInvitation,
        ) -> ghinvite_core::storage::Result<()> {
            panic!("insert_github_invitation is not used by Account Admin read tests")
        }

        async fn get_github_invitation(
            &self,
            _id: GithubInvitationId,
        ) -> ghinvite_core::storage::Result<Option<GithubInvitation>> {
            panic!("get_github_invitation is not used by Account Admin read tests")
        }

        async fn get_github_invitation_by_github_id(
            &self,
            _github_id: u64,
        ) -> ghinvite_core::storage::Result<Option<GithubInvitation>> {
            panic!("get_github_invitation_by_github_id is not used by Account Admin read tests")
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
            panic!("audit is not used by Account Admin read tests")
        }
    }

    #[tokio::test]
    async fn account_admin_invitation_link_lookup_returns_owning_accounts_link() {
        let storage = storage_with_accounts().await;
        let link = sample_link(9001, 1, 701, "QueueSlug0000001");
        storage.seed_link(&link).await.unwrap();

        let found = super::find_account_admin_invitation_link(&storage, 9001, link.id)
            .await
            .unwrap();

        assert_eq!(found.id, link.id);
        assert_eq!(found.account_id, 9001);
        assert_eq!(found.slug.as_str(), "QueueSlug0000001");
    }

    #[tokio::test]
    async fn account_admin_invitation_link_lookup_hides_wrong_account_link() {
        let storage = storage_with_accounts().await;
        let link = sample_link(9002, 2, 701, "QueueSlug0000002");
        storage.seed_link(&link).await.unwrap();

        let err = super::find_account_admin_invitation_link(&storage, 9001, link.id)
            .await
            .unwrap_err();

        assert!(matches!(err, crate::WebError::NotFound));
    }

    #[tokio::test]
    async fn account_admin_request_lookup_returns_request_and_link_for_owner() {
        let storage = storage_with_accounts().await;
        storage
            .upsert_user(&sample_user(802, "requester"))
            .await
            .unwrap();
        let link = sample_link(9001, 1, 701, "QueueSlug0000003");
        storage.seed_link(&link).await.unwrap();
        let request_id = RequestId::new();
        let request = sample_request(request_id, link.id, 802, "2026-05-04T12:30:00Z");
        storage.seed_request(&request).await.unwrap();

        let found = super::find_account_admin_request(&storage, 9001, request_id)
            .await
            .unwrap();

        assert_eq!(found.request.id, request_id);
        assert_eq!(found.request.invitation_link_id, link.id);
        assert_eq!(found.invitation_link.id, link.id);
        assert_eq!(found.invitation_link.account_id, 9001);
    }

    #[tokio::test]
    async fn account_admin_request_lookup_hides_wrong_account_request() {
        let storage = storage_with_accounts().await;
        storage
            .upsert_user(&sample_user(802, "requester"))
            .await
            .unwrap();
        let link = sample_link(9002, 2, 701, "QueueSlug0000004");
        storage.seed_link(&link).await.unwrap();
        let request_id = RequestId::new();
        let request = sample_request(request_id, link.id, 802, "2026-05-04T12:30:00Z");
        storage.seed_request(&request).await.unwrap();

        let err = super::find_account_admin_request(&storage, 9001, request_id)
            .await
            .unwrap_err();

        assert!(matches!(err, crate::WebError::NotFound));
    }

    #[tokio::test]
    async fn account_admin_request_lookup_hides_missing_request() {
        let storage = storage_with_accounts().await;

        let err = super::find_account_admin_request(&storage, 9001, RequestId::new())
            .await
            .unwrap_err();

        assert!(matches!(err, crate::WebError::NotFound));
    }

    #[tokio::test]
    async fn account_admin_request_lookup_hides_missing_related_invitation_link() {
        let request = sample_request(
            RequestId::new(),
            InvitationLinkId::new(),
            802,
            "2026-05-04T12:30:00Z",
        );
        let storage = FakeStorage {
            pending_requests: Vec::new(),
            request: Some(request.clone()),
            invitation_link: None,
            user: None,
            fail_invitation_link_lookup: false,
            fail_user_lookup: false,
        };

        let err = super::find_account_admin_request(&storage, 9001, request.id)
            .await
            .unwrap_err();

        assert!(matches!(err, crate::WebError::NotFound));
    }
}
