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

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use domain::{
        Account, AccountType, Permission, SelectedRepos, ShareLink, ShareLinkId, ShareLinkRepo,
        Slug, User,
    };
    use storage::Storage;

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
        storage.upsert_user(&sample_user(701, "creator")).await.unwrap();
        storage
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
}
