use crate::error::{Result, WebError};
use chrono::{DateTime, Utc};
use domain::{InvitationRequest, RequestId, ShareLink, ShareLinkId};
use storage::Storage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountAdminPendingRequestRow {
    pub(crate) request_id: RequestId,
    pub(crate) link_slug: String,
    pub(crate) link_id: Option<ShareLinkId>,
    pub(crate) requester_login: String,
    pub(crate) justification: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) permission: Option<domain::Permission>,
    pub(crate) repos: Vec<String>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) approval_required: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountAdminInvitationRequest {
    pub(crate) request: InvitationRequest,
    pub(crate) share_link: ShareLink,
}

pub(crate) async fn find_account_admin_share_link(
    storage: &dyn Storage,
    account_id: u64,
    link_id: ShareLinkId,
) -> Result<ShareLink> {
    let link = storage
        .get_share_link_by_id(link_id)
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
    let share_link =
        find_account_admin_share_link(storage, account_id, request.share_link_id).await?;

    Ok(AccountAdminInvitationRequest {
        request,
        share_link,
    })
}

pub(crate) async fn pending_request_queue(
    storage: &dyn Storage,
    account_id: u64,
) -> Result<Vec<AccountAdminPendingRequestRow>> {
    let pending = storage
        .list_pending_requests_for_account(account_id)
        .await?;
    let mut rows = Vec::with_capacity(pending.len());

    for request in pending {
        let share_link = storage.get_share_link_by_id(request.share_link_id).await?;
        let requester = storage.get_user(request.requester_id).await?;
        rows.push(pending_request_row(request, share_link, requester));
    }

    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(rows)
}

fn pending_request_row(
    request: InvitationRequest,
    share_link: Option<ShareLink>,
    requester: Option<domain::User>,
) -> AccountAdminPendingRequestRow {
    let (link_slug, link_id, permission, repos, expires_at, approval_required) = match share_link {
        Some(link) => {
            let repos = link
                .repos
                .into_iter()
                .map(|repo| repo.repo_full_name)
                .collect();
            (
                link.slug.as_str().to_string(),
                Some(link.id),
                Some(link.permission),
                repos,
                link.expires_at,
                Some(link.approval_required),
            )
        }
        None => (
            "(deleted link)".to_string(),
            None,
            None,
            Vec::new(),
            None,
            None,
        ),
    };
    let requester_login = requester
        .map(|user| user.login)
        .unwrap_or_else(|| format!("user-{}", request.requester_id));

    AccountAdminPendingRequestRow {
        request_id: request.id,
        link_slug,
        link_id,
        requester_login,
        justification: request.justification,
        created_at: request.created_at,
        permission,
        repos,
        expires_at,
        approval_required,
    }
}

#[cfg(test)]
mod tests {
    use audit::AuditEvent;
    use chrono::{DateTime, Utc};
    use domain::{
        Account, AccountType, GithubInvitation, GithubInvitationId, InvitationRequest, Permission,
        RequestId, RequestState, SelectedRepos, ShareLink, ShareLinkId, ShareLinkRepo, Slug, User,
    };
    use storage::{GithubInvitationUpdate, RequestDecision, Storage};

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
    ) -> ShareLink {
        ShareLink {
            id: ShareLinkId::new(),
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
        created_at: &str,
    ) -> InvitationRequest {
        InvitationRequest {
            id,
            share_link_id,
            requester_id,
            justification: Some("need repository access".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            created_at: dt(created_at),
        }
    }

    async fn storage_with_accounts() -> storage::SqlxStorage {
        let storage = storage::SqlxStorage::in_memory().await.unwrap();
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
        share_link: Option<ShareLink>,
        user: Option<User>,
        fail_share_link_lookup: bool,
        fail_user_lookup: bool,
    }

    #[async_trait::async_trait]
    impl Storage for FakeStorage {
        async fn insert_installation(&self, _account: &Account) -> storage::Result<()> {
            panic!("insert_installation is not used by Account Admin read tests")
        }

        async fn mark_installation_uninstalled(
            &self,
            _installation_id: u64,
            _when: DateTime<Utc>,
        ) -> storage::Result<()> {
            panic!("mark_installation_uninstalled is not used by Account Admin read tests")
        }

        async fn update_installation_repos(
            &self,
            _installation_id: u64,
            _selected: &SelectedRepos,
        ) -> storage::Result<()> {
            panic!("update_installation_repos is not used by Account Admin read tests")
        }

        async fn get_installation(
            &self,
            _installation_id: u64,
        ) -> storage::Result<Option<Account>> {
            panic!("get_installation is not used by Account Admin read tests")
        }

        async fn get_active_installation_by_account_id(
            &self,
            _account_id: u64,
        ) -> storage::Result<Option<Account>> {
            panic!("get_active_installation_by_account_id is not used by Account Admin read tests")
        }

        async fn get_active_installation_by_login(
            &self,
            _login: &str,
        ) -> storage::Result<Option<Account>> {
            panic!("get_active_installation_by_login is not used by Account Admin read tests")
        }

        async fn list_active_installations(&self) -> storage::Result<Vec<Account>> {
            panic!("list_active_installations is not used by Account Admin read tests")
        }

        async fn upsert_user(&self, _user: &User) -> storage::Result<()> {
            panic!("upsert_user is not used by Account Admin read tests")
        }

        async fn get_user(&self, _user_id: u64) -> storage::Result<Option<User>> {
            if self.fail_user_lookup {
                return Err(storage::Error::Database("user lookup failed".into()));
            }

            Ok(self.user.clone())
        }

        async fn insert_share_link(&self, _link: &ShareLink) -> storage::Result<()> {
            panic!("insert_share_link is not used by Account Admin read tests")
        }

        async fn mark_share_link_revoked(
            &self,
            _id: ShareLinkId,
            _by_user: u64,
            _when: DateTime<Utc>,
        ) -> storage::Result<()> {
            panic!("mark_share_link_revoked is not used by Account Admin read tests")
        }

        async fn get_share_link_by_id(
            &self,
            _id: ShareLinkId,
        ) -> storage::Result<Option<ShareLink>> {
            if self.fail_share_link_lookup {
                return Err(storage::Error::Database("share link lookup failed".into()));
            }

            Ok(self.share_link.clone())
        }

        async fn get_share_link_by_slug(&self, _slug: &str) -> storage::Result<Option<ShareLink>> {
            panic!("get_share_link_by_slug is not used by Account Admin read tests")
        }

        async fn list_share_links_for_account(
            &self,
            _account_id: u64,
        ) -> storage::Result<Vec<ShareLink>> {
            panic!("list_share_links_for_account is not used by Account Admin read tests")
        }

        async fn insert_invitation_request_and_increment_uses(
            &self,
            _request: &InvitationRequest,
        ) -> storage::Result<()> {
            panic!(
                "insert_invitation_request_and_increment_uses is not used by Account Admin read tests"
            )
        }

        async fn record_request_decision(
            &self,
            _decision: &RequestDecision,
        ) -> storage::Result<()> {
            panic!("record_request_decision is not used by Account Admin read tests")
        }

        async fn get_invitation_request(
            &self,
            _id: RequestId,
        ) -> storage::Result<Option<InvitationRequest>> {
            Ok(self.request.clone())
        }

        async fn list_pending_requests_for_account(
            &self,
            _account_id: u64,
        ) -> storage::Result<Vec<InvitationRequest>> {
            Ok(self.pending_requests.clone())
        }

        async fn list_requests_for_link(
            &self,
            _link_id: ShareLinkId,
        ) -> storage::Result<Vec<InvitationRequest>> {
            panic!("list_requests_for_link is not used by Account Admin read tests")
        }

        async fn insert_github_invitation(
            &self,
            _invitation: &GithubInvitation,
        ) -> storage::Result<()> {
            panic!("insert_github_invitation is not used by Account Admin read tests")
        }

        async fn update_github_invitation(
            &self,
            _update: &GithubInvitationUpdate,
        ) -> storage::Result<()> {
            panic!("update_github_invitation is not used by Account Admin read tests")
        }

        async fn get_github_invitation(
            &self,
            _id: GithubInvitationId,
        ) -> storage::Result<Option<GithubInvitation>> {
            panic!("get_github_invitation is not used by Account Admin read tests")
        }

        async fn get_github_invitation_by_github_id(
            &self,
            _github_id: u64,
        ) -> storage::Result<Option<GithubInvitation>> {
            panic!("get_github_invitation_by_github_id is not used by Account Admin read tests")
        }

        async fn list_pending_github_invitations_for_installation(
            &self,
            _installation_id: u64,
        ) -> storage::Result<Vec<GithubInvitation>> {
            panic!(
                "list_pending_github_invitations_for_installation is not used by Account Admin read tests"
            )
        }

        async fn audit(&self, _event: &AuditEvent) -> storage::Result<()> {
            panic!("audit is not used by Account Admin read tests")
        }
    }

    #[tokio::test]
    async fn account_admin_share_link_lookup_returns_owning_accounts_link() {
        let storage = storage_with_accounts().await;
        let link = sample_link(9001, 1, 701, "QueueSlug0000001");
        storage.insert_share_link(&link).await.unwrap();

        let found = super::find_account_admin_share_link(&storage, 9001, link.id)
            .await
            .unwrap();

        assert_eq!(found.id, link.id);
        assert_eq!(found.account_id, 9001);
        assert_eq!(found.slug.as_str(), "QueueSlug0000001");
    }

    #[tokio::test]
    async fn account_admin_share_link_lookup_hides_wrong_account_link() {
        let storage = storage_with_accounts().await;
        let link = sample_link(9002, 2, 701, "QueueSlug0000002");
        storage.insert_share_link(&link).await.unwrap();

        let err = super::find_account_admin_share_link(&storage, 9001, link.id)
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
        storage.insert_share_link(&link).await.unwrap();
        let request_id = RequestId::new();
        let request = sample_request(request_id, link.id, 802, "2026-05-04T12:30:00Z");
        storage
            .insert_invitation_request_and_increment_uses(&request)
            .await
            .unwrap();

        let found = super::find_account_admin_request(&storage, 9001, request_id)
            .await
            .unwrap();

        assert_eq!(found.request.id, request_id);
        assert_eq!(found.request.share_link_id, link.id);
        assert_eq!(found.share_link.id, link.id);
        assert_eq!(found.share_link.account_id, 9001);
    }

    #[tokio::test]
    async fn account_admin_request_lookup_hides_wrong_account_request() {
        let storage = storage_with_accounts().await;
        storage
            .upsert_user(&sample_user(802, "requester"))
            .await
            .unwrap();
        let link = sample_link(9002, 2, 701, "QueueSlug0000004");
        storage.insert_share_link(&link).await.unwrap();
        let request_id = RequestId::new();
        let request = sample_request(request_id, link.id, 802, "2026-05-04T12:30:00Z");
        storage
            .insert_invitation_request_and_increment_uses(&request)
            .await
            .unwrap();

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
    async fn account_admin_pending_request_queue_returns_hydrated_rows_oldest_first() {
        let storage = storage_with_accounts().await;
        storage
            .upsert_user(&sample_user(802, "alice"))
            .await
            .unwrap();
        storage.upsert_user(&sample_user(803, "bob")).await.unwrap();
        let link_a = sample_link(9001, 1, 701, "QueueSlug0000005");
        let link_b = sample_link(9001, 1, 701, "QueueSlug0000006");
        storage.insert_share_link(&link_a).await.unwrap();
        storage.insert_share_link(&link_b).await.unwrap();
        let newer = sample_request(RequestId::new(), link_a.id, 802, "2026-05-04T12:40:00Z");
        let older = sample_request(RequestId::new(), link_b.id, 803, "2026-05-04T12:30:00Z");
        storage
            .insert_invitation_request_and_increment_uses(&newer)
            .await
            .unwrap();
        storage
            .insert_invitation_request_and_increment_uses(&older)
            .await
            .unwrap();

        let rows = super::pending_request_queue(&storage, 9001).await.unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].request_id, older.id);
        assert_eq!(rows[0].link_slug, "QueueSlug0000006");
        assert_eq!(rows[0].link_id, Some(link_b.id));
        assert_eq!(rows[0].requester_login, "bob");
        assert_eq!(
            rows[0].justification.as_deref(),
            Some("need repository access")
        );
        assert_eq!(rows[0].created_at, dt("2026-05-04T12:30:00Z"));
        assert_eq!(rows[0].permission, Some(Permission::Pull));
        assert_eq!(rows[0].repos, vec!["acme/api".to_string()]);
        assert_eq!(rows[0].expires_at, None);
        assert_eq!(rows[0].approval_required, Some(true));
        assert_eq!(rows[1].request_id, newer.id);
        assert_eq!(rows[1].requester_login, "alice");
        assert_eq!(rows[1].created_at, dt("2026-05-04T12:40:00Z"));
    }

    #[tokio::test]
    async fn account_admin_pending_request_queue_uses_missing_related_record_fallbacks() {
        let missing_link_id = ShareLinkId::new();
        let request = sample_request(
            RequestId::new(),
            missing_link_id,
            999_999,
            "2026-05-04T12:30:00Z",
        );
        let storage = FakeStorage {
            pending_requests: vec![request.clone()],
            request: None,
            share_link: None,
            user: None,
            fail_share_link_lookup: false,
            fail_user_lookup: false,
        };

        let rows = super::pending_request_queue(&storage, 9001).await.unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, request.id);
        assert_eq!(rows[0].link_slug, "(deleted link)");
        assert_eq!(rows[0].link_id, None);
        assert_eq!(rows[0].requester_login, "user-999999");
        assert_eq!(rows[0].permission, None);
        assert!(rows[0].repos.is_empty());
        assert_eq!(rows[0].expires_at, None);
        assert_eq!(rows[0].approval_required, None);
    }

    #[tokio::test]
    async fn account_admin_pending_request_queue_propagates_related_record_storage_errors() {
        let request = sample_request(
            RequestId::new(),
            ShareLinkId::new(),
            802,
            "2026-05-04T12:30:00Z",
        );
        let storage = FakeStorage {
            pending_requests: vec![request],
            request: None,
            share_link: None,
            user: None,
            fail_share_link_lookup: true,
            fail_user_lookup: false,
        };

        let err = super::pending_request_queue(&storage, 9001)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            crate::WebError::Storage(storage::Error::Database(message))
                if message == "share link lookup failed"
        ));
    }

    #[tokio::test]
    async fn account_admin_request_lookup_hides_missing_related_share_link() {
        let request = sample_request(
            RequestId::new(),
            ShareLinkId::new(),
            802,
            "2026-05-04T12:30:00Z",
        );
        let storage = FakeStorage {
            pending_requests: Vec::new(),
            request: Some(request.clone()),
            share_link: None,
            user: None,
            fail_share_link_lookup: false,
            fail_user_lookup: false,
        };

        let err = super::find_account_admin_request(&storage, 9001, request.id)
            .await
            .unwrap_err();

        assert!(matches!(err, crate::WebError::NotFound));
    }
}
