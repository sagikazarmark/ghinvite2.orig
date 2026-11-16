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
        Some(_) => Err(crate::WebError::Forbidden),
        None => Err(crate::WebError::NotFound),
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
    let link = owned_link(
        storage,
        account_id,
        request
            .invitation_link_id
            .ok_or(crate::WebError::NotFound)?,
    )
    .await?;
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
        Ok(link) => Ok((link, true)),
        Err(crate::WebError::NotFound) => authoritative_link(&state, &admin, id)
            .await
            .map(|link| (link, false))
            .map_err(crate::WebError::from),
        Err(error) => Err(error),
    };
    let (link, projected) = match link {
        Ok(link) => link,
        Err(crate::WebError::NotFound | crate::WebError::Forbidden) => {
            return console_not_found_response(&admin);
        }
        Err(error) => {
            return error.into_response_with_recovery(
                format!("/console/accounts/{}/links", admin.account.account_login),
                "Back to invitation links",
            );
        }
    };
    if let Err(error) = authorize_targets(&admin, &link.targets, false).await {
        return error.into_response();
    }
    let before = cursor(&uri, id);
    let mut older_href = None;
    let history = if projected {
        state
            .storage
            .request_history(admin.account.account_id, id, before)
            .await
            .ok()
    } else {
        None
    };
    let (status, rows) = match history {
        Some(page) => {
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
                let avatar_href = page
                    .requester_avatars
                    .get(&request.requester_id)
                    .filter(|url| !url.is_empty())
                    .map(|_| {
                        format!(
                            "/console/accounts/{}/requests/{}/avatar",
                            admin.account.account_login, request.id
                        )
                    });
                let github_created_at = page
                    .requester_created_at
                    .get(&request.requester_id)
                    .copied();
                rows.push(HistoryRow {
                    request,
                    requester,
                    avatar_href,
                    github_created_at,
                });
            }
            (StatusCode::OK, Some(rows))
        }
        None => {
            tracing::warn!("request history read unavailable");
            (StatusCode::SERVICE_UNAVAILABLE, None)
        }
    };
    let html = render(admin.render_context(), move || {
        rsx! {
            RequestHistoryPage {
                now: Utc::now(),
                signed_in_login: Some(admin.session.login.clone()), account_login: admin.account.account_login.clone(),
                link_id: id.to_string(), description: link.description.clone(), rows: rows.clone(),
                older_href: older_href.clone(), in_range: before.is_some(),
            }
        }
    });
    (status, Html(html)).into_response()
}

#[derive(Default, serde::Deserialize)]
pub(super) struct DetailQuery {
    preview: Option<u8>,
}

pub(super) async fn detail(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, id)): Path<(String, String)>,
    axum::extract::Query(query): axum::extract::Query<DetailQuery>,
) -> axum::response::Response {
    let id: RequestId = match path_id(&admin, &id) {
        Ok(id) => id,
        Err(not_found) => return not_found,
    };
    let record = {
        let retained = async {
            let request = match state
                .request_authority
                .admin_status(ghinvite_core::request_lifecycle::AdminRequestStatus {
                    request_id: id,
                    admin: admin_assertion(&admin),
                })
                .await
            {
                Ok(snapshot) => snapshot.as_request(),
                Err(error) => return Err(crate::WebError::from(error)),
            };
            let link = authoritative_link(
                &state,
                &admin,
                request
                    .invitation_link_id
                    .ok_or(crate::WebError::NotFound)?,
            )
            .await
            .map_err(crate::WebError::from)?;
            Ok((request, link))
        };
        match owned_request(state.storage.as_ref(), admin.account.account_id, id).await {
            Ok(record) => Ok(record),
            Err(crate::WebError::NotFound) => retained.await,
            Err(error) => Err(error),
        }
    };
    let (request, link) = match record {
        Ok(record) => record,
        Err(crate::WebError::NotFound | crate::WebError::Forbidden) => {
            return console_not_found_response(&admin);
        }
        Err(error) => {
            return error.into_response_with_recovery(
                format!("/console/accounts/{}/requests", admin.account.account_login),
                "Back to requests",
            );
        }
    };
    if let Err(error) = authorize_targets(&admin, &link.targets, false).await {
        return error.into_response();
    }
    let profile = state
        .storage
        .get_user(request.requester_id)
        .await
        .ok()
        .flatten()
        .filter(|user| user.user_id == request.requester_id);
    let access_preview = if query.preview == Some(1) {
        let login = profile
            .as_ref()
            .map(|user| user.login.clone())
            .unwrap_or_default();
        Some(
            admin
                .authority
                .request_preview(&link.targets, request.requester_id, &login)
                .await,
        )
    } else {
        None
    };
    let requester = profile_label(
        request.requester_id,
        profile.as_ref().map(|user| user.login.as_str()),
    );
    let avatar_href = profile
        .as_ref()
        .and_then(|user| user.avatar_url.as_ref())
        .filter(|url| !url.is_empty())
        .map(|_| {
            format!(
                "/console/accounts/{}/requests/{}/avatar",
                admin.account.account_login, request.id
            )
        });
    let github_created_at = profile.as_ref().and_then(|user| user.github_created_at);
    let decision_actor = match request.decided_by {
        Some(id) => user_label(&state, id).await,
        None if matches!(request.state, RequestState::Approved | RequestState::Lapsed)
            && !link.approval_required =>
        {
            "Auto-approved by invitation-link policy".into()
        }
        None if request.state == RequestState::Expired => "System (decision deadline)".into(),
        None => "Unavailable".into(),
    };
    let (organization_delivery, organization_delivery_history) =
        if matches!(request.state, RequestState::Approved | RequestState::Lapsed) {
            crate::organization_delivery::load_console(
                &state,
                &link.targets,
                link.id,
                id,
                request.requester_id,
            )
            .await
        } else {
            (None, vec![])
        };
    let progress = state
        .request_authority
        .delivery_progress(ghinvite_core::request_lifecycle::RequestStatus {
            link_id: link.id,
            request_id: id,
            requester_id: request.requester_id,
        })
        .await
        .ok();
    let receipts = state.storage.list_delivery_for_request(id).await;
    let mut delivery_unavailable = receipts.is_err() || progress.is_none();
    let mut receipts = receipts.unwrap_or_default();
    receipts.retain(|s| {
        s.create.command.request_id == id
            && s.create.command.link_id == Some(link.id)
            && s.create.command.account_id == link.account_id
            && s.create.command.requester_id == request.requester_id
            && link.targets.repos.iter().any(|target| {
                target.repo_id == s.create.command.repo_id
                    && s.create.command.permission == target.grant.into()
            })
    });
    for repo in &link.targets.repos {
        let Some(generation) = progress
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|p| p.repo_id == repo.repo_id)
            .map(|p| p.delivery_generation)
            .filter(|g| *g > 0)
        else {
            delivery_unavailable = true;
            continue;
        };
        match state
            .delivery_authority
            .snapshot(id, repo.repo_id, generation)
            .await
        {
            Ok(Some(snapshot))
                if snapshot.create.command.request_id == id
                    && snapshot.create.command.link_id == Some(link.id)
                    && snapshot.create.command.account_id == link.account_id
                    && snapshot.create.command.requester_id == request.requester_id
                    && snapshot.create.command.repo_id == repo.repo_id
                    && snapshot.create.command.permission == repo.grant.into()
                    && snapshot.create.command.delivery_generation == generation =>
            {
                receipts.retain(|r| {
                    r.create.command.repo_id != repo.repo_id
                        || r.create.command.delivery_generation != generation
                });
                receipts.push(snapshot);
            }
            Ok(None | Some(_)) | Err(_) => delivery_unavailable = true,
        }
    }
    let delivery: Vec<_> = link
        .targets
        .repos
        .iter()
        .map(|repo| RepositoryDelivery {
            repo_id: repo.repo_id,
            presentation: ghinvite_ui::invitation::delivery_presentation(
                &ghinvite_core::InvitationLinkRepo {
                    repo_id: repo.repo_id,
                    repo_full_name: repo.repo_full_name.clone(),
                },
                &receipts
                    .iter()
                    .filter(|r| {
                        progress.as_deref().unwrap_or_default().iter().any(|p| {
                            p.repo_id == repo.repo_id
                                && p.delivery_generation == r.create.command.delivery_generation
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
                progress.as_deref().unwrap_or_default(),
                delivery_unavailable,
            ),
            receipts: receipts
                .iter()
                .filter(|r| r.create.command.repo_id == repo.repo_id)
                .cloned()
                .collect(),
        })
        .collect();
    let html = render(admin.render_context(), move || {
        rsx! {
            RequestDetailPage {
                access_preview: access_preview.as_ref().map(|p| p.targets.clone()),
                preview_observed_at: access_preview.as_ref().and_then(|p| p.observed_at),
                reissue_enabled: state.config.reissue_enabled,
                organization_reissue_enabled: state.config.organization_delivery_enabled,
                signed_in_login: Some(admin.session.login.clone()), account_login: admin.account.account_login.clone(),
                request: request.clone(), link: link.clone(), requester: requester.clone(), avatar_href: avatar_href.clone(), github_created_at, decision_actor: decision_actor.clone(),
                delivery: delivery.clone(), delivery_unavailable, organization_delivery: organization_delivery.clone(), organization_delivery_history: organization_delivery_history.clone(), now: Utc::now(),
            }
        }
    });
    Html(html).into_response()
}

/// Images use the same complete request visibility check as the details page.
/// Numeric IDs come from retained/projected request identity, never a URL input.
pub(super) async fn avatar(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, raw)): Path<(String, String)>,
) -> axum::response::Response {
    let result = async {
        let id: RequestId = raw.parse().ok()?;
        let (request, link) = owned_request(state.storage.as_ref(), admin.account.account_id, id)
            .await
            .ok()?;
        authorize_targets(&admin, &link.targets, false).await.ok()?;
        let user = state
            .storage
            .get_user(request.requester_id)
            .await
            .ok()
            .flatten()?;
        if user.user_id != request.requester_id
            || user.avatar_url.as_ref().is_none_or(|url| url.is_empty())
        {
            return None;
        }
        ghinvite_github::avatar::get_at(
            state.github_transport.as_ref(),
            state.config.github.avatar(),
            ghinvite_github::ids::UserId::new(request.requester_id),
        )
        .await
        .ok()
    }
    .await;
    let mut response = match result {
        Some(image) => (
            [(axum::http::header::CONTENT_TYPE, image.content_type)],
            image.bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    };
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
    ] {
        response.headers_mut().insert(
            axum::http::header::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::{owned_link, owned_request};
    use chrono::{DateTime, Utc};
    use ghinvite_core::storage::projection::fixture::Seed;
    use ghinvite_core::storage::{AppInstallationStorage, RecordStorage, Result};
    use ghinvite_core::{
        Account, AccountType, InvitationLink, InvitationLinkId, InvitationRequest, RequestId,
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
            github_created_at: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    fn link(account_id: u64, installation_id: u64) -> InvitationLink {
        InvitationLink {
            batch_source: None,
            access_lifetime_seconds: None,
            email_domains: vec![],
            id: InvitationLinkId::new(),
            organization_installation_id: None,
            installation_id,
            account_id,
            created_by: 701,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            invitation_lifetime_seconds: 604800,
            abuse_threshold_per_hour: None,
            budget_share_pct: 50,
            uses_count: 0,
            approval_required: true,
            description: "AI coding workshop".into(),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            pause: None,
            targets: ghinvite_core::targets::Targets {
                repos: vec![ghinvite_core::targets::RepoTarget {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                    grant: ghinvite_core::targets::RepoRole::Read,
                }],
                ..Default::default()
            },
            secret: ghinvite_core::LinkSecretState {
                secret_generation: 1,
                sha256: [42; 32].into(),
            },
        }
    }

    fn request(invitation_link_id: InvitationLinkId) -> InvitationRequest {
        InvitationRequest {
            batch: None,
            domain_evidence: None,
            id: RequestId::new(),
            invitation_link_id: Some(invitation_link_id),
            requester_id: 802,
            justification: Some("need repository access".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            cancellation_reason: None,
            decline_reason: None,
            requester_message: None,
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
                .put_app_installation(&ghinvite_core::account::Installation::repo(
                    (account(installation, account_id, login)).clone(),
                ))
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
            Err(crate::WebError::Forbidden)
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
                Err(crate::WebError::NotFound | crate::WebError::Forbidden)
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
        async fn upsert_user(&self, _: &User) -> Result<()> {
            unreachable!()
        }
        async fn get_user(&self, _: u64) -> Result<Option<User>> {
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
