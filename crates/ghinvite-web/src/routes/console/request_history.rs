use super::*;
use crate::views::request_history::{
    HistoryRow, RepositoryDelivery, RequestDetailPage, RequestHistoryPage,
};
use axum::{extract::Path, http::StatusCode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ghinvite_core::{
    InvitationLinkId, RequestId, RequestState, storage::request_history::Boundary,
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
    if tokens.len() != 1 || tokens[0].1.len() > 512 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(tokens[0].1.as_bytes()).ok()?;
    let value: Cursor = serde_json::from_slice(&decoded).ok()?;
    (value.v == 1
        && value.link == link
        && (1..=9999).contains(&chrono::Datelike::year(&value.boundary.admitted_at)))
    .then_some(value.boundary)
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
    let Ok(id) = id.parse::<InvitationLinkId>() else {
        return console_not_found_response(&admin);
    };
    let link = match find_account_admin_invitation_link(
        state.storage.as_ref(),
        admin.account.account_id,
        id,
    )
    .await
    {
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
                let token = URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&Cursor {
                        v: 1,
                        link: id,
                        boundary,
                    })
                    .expect("cursor serializes"),
                );
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
    let Ok(id) = id.parse::<RequestId>() else {
        return console_not_found_response(&admin);
    };
    let record = match find_account_admin_request(
        state.storage.as_ref(),
        admin.account.account_id,
        id,
    )
    .await
    {
        Ok(record) => record,
        Err(crate::WebError::NotFound) => return console_not_found_response(&admin),
        Err(error) => {
            return error.into_response_with_recovery(
                format!("/console/accounts/{}/requests", admin.account.account_login),
                "Back to requests",
            );
        }
    };
    let request = record.request;
    let link = record.invitation_link;
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
    let delivery_unavailable = receipts.is_err() || invitations.is_err();
    let receipts = receipts.unwrap_or_default();
    let invitations = invitations.unwrap_or_default();
    let delivery: Vec<_> = link
        .repos
        .iter()
        .map(|repo| RepositoryDelivery {
            repo_id: repo.repo_id,
            presentation: crate::views::invitation::delivery_presentation(
                repo,
                &receipts,
                &[],
                &invitations,
                delivery_unavailable,
            ),
            receipts: receipts
                .iter()
                .filter(|r| r.command.repo_id == repo.repo_id)
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
