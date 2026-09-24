use super::*;
use axum::{extract::Path, http::StatusCode};
use ghinvite_core::storage::{RecordStorage, request_history::Boundary};
use ghinvite_core::{InvitationLink, InvitationLinkId, InvitationRequest, RequestId, RequestState};
use ghinvite_ui::request_history::{
    HistoryRow, RepositoryDelivery, RequestDetailPage, RequestHistoryPage,
};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: u8,
    link: InvitationLinkId,
    boundary: Boundary,
}

fn cursor(uri: &Uri, link: InvitationLinkId) -> Option<Boundary> {
    let tokens: Vec<_> = url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .filter(|(key, _)| key == "before")
        .collect();
    let [(_, token)] = tokens.as_slice() else {
        return None;
    };
    let value = super::cursor::decode(token, |c: &Cursor| c.boundary.admitted_at)?;
    (value.v == 1 && value.link == link).then_some(value.boundary)
}

/// The account's own invitation link. Another account's link is concealed as missing.
async fn owned_link(
    storage: &dyn RecordStorage,
    account_id: u64,
    id: InvitationLinkId,
) -> crate::Result<InvitationLink> {
    match storage.get_invitation_link_by_id(id).await? {
        Some(link) if link.account_id == account_id => Ok(link),
        _ => Err(crate::WebError::NotFound),
    }
}

/// The account's own invitation request with its invitation link. A request
/// whose link is missing or belongs to another account is concealed as missing.
async fn owned_request(
    storage: &dyn RecordStorage,
    account_id: u64,
    id: RequestId,
) -> crate::Result<(InvitationRequest, InvitationLink)> {
    let request = storage
        .get_invitation_request(id)
        .await?
        .ok_or(crate::WebError::NotFound)?;
    let link = owned_link(storage, account_id, request.invitation_link_id).await?;
    Ok((request, link))
}

async fn user_label(state: &AppState, id: u64) -> String {
    let user = state.storage.get_user(id).await.ok().flatten();
    profile_label(id, user.as_ref().map(|user| user.login.as_str()))
}

fn profile_label(id: u64, login: Option<&str>) -> String {
    match login {
        Some(login) => format!("@{login} · GitHub user ID {id}"),
        None => format!("GitHub user ID {id} · Profile unavailable"),
    }
}

pub(super) async fn history(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, id)): Path<(String, String)>,
    uri: Uri,
) -> axum::response::Response {
    let id: InvitationLinkId = match path_id(&admin, &id) {
        Ok(id) => id,
        Err(not_found) => return not_found,
    };
    let link = match owned_link(state.storage.as_ref(), admin.account.account_id, id).await {
        Ok(link) => link,
        Err(crate::WebError::NotFound) => return console_not_found_response(&admin),
        Err(error) => {
            return error.into_response_with_recovery(
                format!("/console/accounts/{}/links", admin.account.account_login),
                "Back to invitation links",
            );
        }
    };
    let before = cursor(&uri, id);
    let mut older_href = None;
    let (status, rows) = match state
        .storage
        .request_history(admin.account.account_id, id, before)
        .await
    {
        Ok(page) => {
            if let Some(boundary) = page.older {
                let token = super::cursor::encode(&Cursor {
                    v: 1,
                    link: id,
                    boundary,
                });
                older_href = Some(format!(
                    "/console/accounts/{}/links/{id}/requests?before={token}",
                    admin.account.account_login
                ));
            }
            let mut rows = Vec::with_capacity(page.requests.len());
            for request in page.requests {
                let requester = profile_label(
                    request.requester_id,
                    page.requester_logins
                        .get(&request.requester_id)
                        .map(String::as_str),
                );
                rows.push(HistoryRow { request, requester });
            }
            (StatusCode::OK, Some(rows))
        }
        Err(_) => {
            tracing::warn!("request history read unavailable");
            (StatusCode::SERVICE_UNAVAILABLE, None)
        }
    };
    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            RequestHistoryPage {
                signed_in_login: Some(admin.session.login.clone()), account_login: admin.account.account_login.clone(),
                link_id: id.to_string(), description: link.description.clone(), rows: rows.clone(),
                older_href: older_href.clone(), in_range: before.is_some(),
            }
        }
    });
    (status, Html(html)).into_response()
}

pub(super) async fn detail(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, id)): Path<(String, String)>,
) -> axum::response::Response {
    let id: RequestId = match path_id(&admin, &id) {
        Ok(id) => id,
        Err(not_found) => return not_found,
    };
    let (request, link) =
        match owned_request(state.storage.as_ref(), admin.account.account_id, id).await {
            Ok(record) => record,
            Err(crate::WebError::NotFound) => return console_not_found_response(&admin),
            Err(error) => {
                return error.into_response_with_recovery(
                    format!("/console/accounts/{}/requests", admin.account.account_login),
                    "Back to requests",
                );
            }
        };
    let requester = user_label(&state, request.requester_id).await;
    let decision_actor = match request.decided_by {
        Some(id) => user_label(&state, id).await,
        None if request.state == RequestState::Approved && !link.approval_required => {
            "Auto-approved by invitation-link policy".into()
        }
        None if request.state == RequestState::Expired => "System (decision deadline)".into(),
        None => "Unavailable".into(),
    };
    let receipts = state.storage.list_delivery_for_request(id).await;
    let invitations = state.storage.list_github_invitations_for_request(id).await;
    let mut delivery_unavailable = receipts.is_err() || invitations.is_err();
    let mut receipts = receipts.unwrap_or_default();
    let mut invitations = invitations.unwrap_or_default();
    for repo in &link.repos {
        match state
            .link_authority
            .delivery_snapshot(id, repo.repo_id)
            .await
        {
            Ok(Some(snapshot)) => {
                receipts.retain(|r| r.create.command.repo_id != repo.repo_id);
                invitations.retain(|r| r.repo_id != repo.repo_id);
                invitations.push(snapshot.invitation());
                receipts.push(snapshot);
            }
            Ok(None) => (),
            Err(_) => delivery_unavailable = true,
        }
    }
    let delivery: Vec<_> = link
        .repos
        .iter()
        .map(|repo| RepositoryDelivery {
            repo_id: repo.repo_id,
            presentation: ghinvite_ui::invitation::delivery_presentation(
                repo,
                &receipts,
                &[],
                &invitations,
                delivery_unavailable,
            ),
            receipts: receipts
                .iter()
                .filter(|r| r.create.command.repo_id == repo.repo_id)
                .cloned()
                .collect(),
            invitations: invitations
                .iter()
                .filter(|r| r.repo_id == repo.repo_id)
                .cloned()
                .collect(),
        })
        .collect();
    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            RequestDetailPage {
                signed_in_login: Some(admin.session.login.clone()), account_login: admin.account.account_login.clone(),
                request: request.clone(), link: link.clone(), requester: requester.clone(), decision_actor: decision_actor.clone(),
                delivery: delivery.clone(), delivery_unavailable, now: Utc::now(),
            }
        }
    });
    Html(html).into_response()
}

#[cfg(test)]
mod tests {
    use super::{owned_link, owned_request};
    use chrono::{DateTime, Utc};
    use ghinvite_core::storage::projection::fixture::Seed;
    use ghinvite_core::storage::{InstallationStorage, RecordStorage, Result};
    use ghinvite_core::{
        Account, AccountType, GithubInvitation, GithubInvitationId, InvitationLink,
        InvitationLinkId, InvitationLinkRepo, InvitationRequest, Permission, RequestId,
        RequestState, SelectedRepos, User,
    };

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn account(installation_id: u64, account_id: u64, login: &str) -> Account {
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

    fn user(user_id: u64, login: &str) -> User {
        User {
            user_id,
            login: login.into(),
            avatar_url: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    fn link(account_id: u64, installation_id: u64) -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
            installation_id,
            account_id,
            created_by: 701,
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

    fn request(invitation_link_id: InvitationLinkId) -> InvitationRequest {
        InvitationRequest {
            id: RequestId::new(),
            invitation_link_id,
            requester_id: 802,
            justification: Some("need repository access".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            decision_deadline: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        }
    }

    async fn storage() -> ghinvite_storage_sqlx::SqlxStorage {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        for (installation, account_id, login) in [(1, 9001, "acme"), (2, 9002, "other")] {
            storage
                .insert_installation(&account(installation, account_id, login))
                .await
                .unwrap();
        }
        for (id, login) in [(701, "creator"), (802, "requester")] {
            storage.upsert_user(&user(id, login)).await.unwrap();
        }
        storage
    }

    #[tokio::test]
    async fn owned_link_returns_the_accounts_link_and_conceals_others() {
        let storage = storage().await;
        let own = link(9001, 1);
        let foreign = link(9002, 2);
        storage.seed_link(&own).await.unwrap();
        storage.seed_link(&foreign).await.unwrap();

        assert_eq!(owned_link(&storage, 9001, own.id).await.unwrap(), own);
        assert!(matches!(
            owned_link(&storage, 9001, foreign.id).await,
            Err(crate::WebError::NotFound)
        ));
    }

    #[tokio::test]
    async fn owned_request_returns_request_and_link_and_conceals_others() {
        let storage = storage().await;
        let own = link(9001, 1);
        let foreign = link(9002, 2);
        storage.seed_link(&own).await.unwrap();
        storage.seed_link(&foreign).await.unwrap();
        let own_request = request(own.id);
        let foreign_request = request(foreign.id);
        storage.seed_request(&own_request).await.unwrap();
        storage.seed_request(&foreign_request).await.unwrap();

        let (request, link) = owned_request(&storage, 9001, own_request.id).await.unwrap();
        assert_eq!((request.id, link.id), (own_request.id, own.id));
        for id in [foreign_request.id, RequestId::new()] {
            assert!(matches!(
                owned_request(&storage, 9001, id).await,
                Err(crate::WebError::NotFound)
            ));
        }
    }

    /// Projections admit no request without its link, so a fake presents one.
    struct OrphanedRequest(InvitationRequest);

    #[async_trait::async_trait]
    impl RecordStorage for OrphanedRequest {
        async fn get_invitation_request(&self, _: RequestId) -> Result<Option<InvitationRequest>> {
            Ok(Some(self.0.clone()))
        }
        async fn get_invitation_link_by_id(
            &self,
            _: InvitationLinkId,
        ) -> Result<Option<InvitationLink>> {
            Ok(None)
        }
        async fn get_installation(&self, _: u64) -> Result<Option<Account>> {
            unreachable!()
        }
        async fn get_active_installation_by_account_id(&self, _: u64) -> Result<Option<Account>> {
            unreachable!()
        }
        async fn upsert_user(&self, _: &User) -> Result<()> {
            unreachable!()
        }
        async fn get_user(&self, _: u64) -> Result<Option<User>> {
            unreachable!()
        }
        async fn get_github_invitation(
            &self,
            _: GithubInvitationId,
        ) -> Result<Option<GithubInvitation>> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn owned_request_conceals_a_request_whose_link_is_missing() {
        let orphan = request(InvitationLinkId::new());
        let storage = OrphanedRequest(orphan.clone());
        assert!(matches!(
            owned_request(&storage, 9001, orphan.id).await,
            Err(crate::WebError::NotFound)
        ));
    }
}
