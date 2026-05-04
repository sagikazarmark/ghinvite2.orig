//! `/accounts/{login}/...` routes. Plan 5.

use crate::middleware::auth::RequireAdminOf;
use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{login}", get(overview))
        .route("/accounts/{login}/links/new", get(new_link_form))
        .route("/accounts/{login}/links", axum::routing::post(create_link))
        .route("/accounts/{login}/links/{link_id}", get(link_detail))
        .route(
            "/accounts/{login}/links/{link_id}/revoke",
            axum::routing::post(revoke_link),
        )
        .route("/accounts/{login}/requests", get(requests_queue))
        .route(
            "/accounts/{login}/requests/{request_id}/approve",
            axum::routing::post(approve_request),
        )
        .route(
            "/accounts/{login}/requests/{request_id}/decline",
            axum::routing::post(decline_request),
        )
        .route("/accounts/{login}/audit", get(stub))
        .route("/accounts/{login}/settings", get(stub))
}

async fn overview(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
) -> impl IntoResponse {
    let now = Utc::now();
    let pending = state
        .storage
        .list_pending_requests_for_account(admin.account.account_id)
        .await
        .map(|v| v.len() as u64)
        .unwrap_or(0);
    let all_links = state
        .storage
        .list_share_links_for_account(admin.account.account_id)
        .await
        .unwrap_or_default();
    let active_links = all_links.iter().filter(|l| l.is_active(now)).count() as u64;
    let recent_links = {
        let mut v = all_links.clone();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v.truncate(5);
        v
    };

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let account_type = admin.account.account_type.to_string();

    let html = render(move || {
        rsx! {
            crate::views::dashboard::OverviewPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                account_type: account_type.clone(),
                pending_requests: pending,
                active_links,
                recent_links: recent_links.clone(),
                now,
            }
        }
    });
    Html(html).into_response()
}

async fn new_link_form(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
) -> impl IntoResponse {
    let user_api = github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        admin.session.access_token.clone(),
    );
    let repos = match user_api
        .list_user_installation_repos(admin.account.installation_id)
        .await
    {
        Ok(r) => r.repositories,
        Err(e) => {
            tracing::warn!(error = ?e, "failed to list installation repos; rendering form with empty list");
            vec![]
        }
    };

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let form = crate::views::links::LinkFormValues::default();

    let html = render(move || {
        rsx! {
            crate::views::links::LinkCreateFormPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                repos: repos.clone(),
                form: form.clone(),
            }
        }
    });
    Html(html).into_response()
}

#[derive(Debug, Deserialize)]
struct CreateLinkForm {
    permission: String,
    #[serde(default)]
    approval_required: Option<String>,
    max_uses: Option<String>,
    expires_in_days: Option<String>,
    internal_note: Option<String>,
    #[serde(default)]
    repo_ids: Vec<u64>,
}

async fn create_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
    serde_qs::axum::QsForm(form): serde_qs::axum::QsForm<CreateLinkForm>,
) -> impl IntoResponse {
    use chrono::{Duration, Utc};
    use std::str::FromStr;

    let now = Utc::now();
    let permission = match domain::Permission::from_str(&form.permission) {
        Ok(p) => p,
        Err(_) => {
            return crate::error::WebError::BadRequest(format!(
                "invalid permission: {}",
                form.permission
            ))
            .into_response();
        }
    };
    let approval_required = form.approval_required.is_some();
    let max_uses: Option<u32> = form
        .max_uses
        .as_deref()
        .and_then(|s: &str| s.trim().parse::<u32>().ok());
    let expires_at = form
        .expires_in_days
        .as_deref()
        .and_then(|s: &str| s.trim().parse::<i64>().ok())
        .map(|d| now + Duration::days(d));
    let internal_note = form
        .internal_note
        .as_deref()
        .map(|s: &str| s.trim().to_string())
        .filter(|s: &String| !s.is_empty());

    let user_api = github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        admin.session.access_token.clone(),
    );
    let installation_repos = user_api
        .list_user_installation_repos(admin.account.installation_id)
        .await
        .map(|r| r.repositories)
        .unwrap_or_default();
    let repos: Vec<domain::ShareLinkRepo> = installation_repos
        .into_iter()
        .filter(|r| form.repo_ids.contains(&r.id))
        .map(|r| domain::ShareLinkRepo {
            repo_id: r.id,
            repo_full_name: r.full_name,
        })
        .collect();

    if repos.is_empty() {
        return crate::error::WebError::BadRequest("select at least one repository".into())
            .into_response();
    }

    let input = serde_json::json!({
        "installation_id": admin.account.installation_id,
        "account_id": admin.account.account_id,
        "created_by": admin.session.user_id,
        "created_at": now,
        "expires_at": expires_at,
        "max_uses": max_uses,
        "permission": permission,
        "approval_required": approval_required,
        "internal_note": internal_note,
        "repos": repos,
    });
    let output: serde_json::Value = match state
        .restate
        .call(
            "ShareLink",
            &admin.account.account_id.to_string(),
            "create",
            &input,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, "ShareLink::create failed");
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Error,
                    message: format!("Failed to create link: {e}"),
                },
            )
            .await;
            return axum::response::Redirect::to(&format!(
                "/accounts/{}/links/new",
                admin.account.account_login
            ))
            .into_response();
        }
    };
    let link_id = output
        .get("link_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let _ = session::set_flash(
        &admin.tower,
        session::Flash {
            level: session::FlashLevel::Success,
            message: "Link created.".into(),
        },
    )
    .await;
    axum::response::Redirect::to(&format!(
        "/accounts/{}/links/{}",
        admin.account.account_login, link_id
    ))
    .into_response()
}

async fn link_detail(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
    axum::extract::Path((_login, link_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let link_id = match domain::ShareLinkId::from_str(&link_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };
    let link = match state.storage.get_share_link_by_id(link_id).await {
        Ok(Some(l)) if l.account_id == admin.account.account_id => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let now = Utc::now();
    let share_url = format!("{}/i/{}", state.config.base_url, link.slug.as_str());

    let html = render(move || {
        rsx! {
            crate::views::links::LinkDetailPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                link: link.clone(),
                now,
                share_url: share_url.clone(),
            }
        }
    });
    Html(html).into_response()
}

async fn revoke_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
    axum::extract::Path((_login, link_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let link_id = match domain::ShareLinkId::from_str(&link_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };
    match state.storage.get_share_link_by_id(link_id).await {
        Ok(Some(l)) if l.account_id == admin.account.account_id => {}
        _ => return crate::error::WebError::NotFound.into_response(),
    };

    let input = serde_json::json!({
        "link_id": link_id,
        "by_user": admin.session.user_id,
        "when": Utc::now(),
    });
    if let Err(e) = state
        .restate
        .send(
            "ShareLink",
            &admin.account.account_id.to_string(),
            "revoke",
            &input,
        )
        .await
    {
        tracing::warn!(error = ?e, "ShareLink::revoke failed");
        let _ = session::set_flash(
            &admin.tower,
            session::Flash {
                level: session::FlashLevel::Error,
                message: format!("Failed to revoke link: {e}"),
            },
        )
        .await;
        return axum::response::Redirect::to(&format!(
            "/accounts/{}/links/{}",
            admin.account.account_login, link_id_str
        ))
        .into_response();
    }

    let _ = session::set_flash(
        &admin.tower,
        session::Flash {
            level: session::FlashLevel::Success,
            message: "Link revoked.".into(),
        },
    )
    .await;
    axum::response::Redirect::to(&format!("/accounts/{}", admin.account.account_login))
        .into_response()
}

async fn requests_queue(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
) -> impl IntoResponse {
    let pending = state
        .storage
        .list_pending_requests_for_account(admin.account.account_id)
        .await
        .unwrap_or_default();

    let mut rows: Vec<crate::views::requests::PendingRequestRow> = Vec::new();
    for req in pending {
        let link = state.storage.get_share_link_by_id(req.share_link_id).await.ok().flatten();
        let user = state.storage.get_user(req.requester_id).await.ok().flatten();
        let (link_slug, link_id) = match link {
            Some(l) => (l.slug.as_str().to_string(), l.id.to_string()),
            None => ("(deleted link)".to_string(), String::new()),
        };
        let requester_login = user
            .map(|u| u.login)
            .unwrap_or_else(|| format!("user-{}", req.requester_id));
        rows.push(crate::views::requests::PendingRequestRow {
            request_id: req.id.to_string(),
            link_slug,
            link_id,
            requester_login,
            justification: req.justification,
            created_at: req.created_at,
        });
    }
    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();

    let html = render(move || {
        rsx! {
            crate::views::requests::RequestsQueuePage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                rows: rows.clone(),
            }
        }
    });
    Html(html).into_response()
}

async fn approve_request(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
    axum::extract::Path((_login, request_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let request_id = match domain::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    // Verify the request belongs to this account via storage lookup.
    let req = match state.storage.get_invitation_request(request_id).await {
        Ok(Some(r)) => r,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    // Cross-check: the request's link must belong to this account.
    let link = match state.storage.get_share_link_by_id(req.share_link_id).await {
        Ok(Some(l)) if l.account_id == admin.account.account_id => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    let _ = link; // ownership verified

    let input = serde_json::json!({
        "Approve": {
            "decided_by": admin.session.user_id,
            "decided_at": Utc::now(),
        }
    });
    match state
        .restate
        .call::<serde_json::Value, serde_json::Value>(
            "InvitationRequest",
            &request_id_str,
            "decide",
            &input,
        )
        .await
    {
        Ok(_) => {
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Success,
                    message: "Request approved.".into(),
                },
            )
            .await;
        }
        Err(e) => {
            tracing::warn!(error = ?e, "InvitationRequest::decide(approve) failed");
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Error,
                    message: format!("Failed to approve request: {e}"),
                },
            )
            .await;
        }
    }
    axum::response::Redirect::to(&format!("/accounts/{}/requests", admin.account.account_login))
        .into_response()
}

async fn decline_request(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
    axum::extract::Path((_login, request_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let request_id = match domain::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    let req = match state.storage.get_invitation_request(request_id).await {
        Ok(Some(r)) => r,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    let link = match state.storage.get_share_link_by_id(req.share_link_id).await {
        Ok(Some(l)) if l.account_id == admin.account.account_id => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    let _ = link;

    let input = serde_json::json!({
        "Decline": {
            "decided_by": admin.session.user_id,
            "decided_at": Utc::now(),
            "reason": null,
        }
    });
    match state
        .restate
        .call::<serde_json::Value, serde_json::Value>(
            "InvitationRequest",
            &request_id_str,
            "decide",
            &input,
        )
        .await
    {
        Ok(_) => {
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Success,
                    message: "Request declined.".into(),
                },
            )
            .await;
        }
        Err(e) => {
            tracing::warn!(error = ?e, "InvitationRequest::decide(decline) failed");
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Error,
                    message: format!("Failed to decline request: {e}"),
                },
            )
            .await;
        }
    }
    axum::response::Redirect::to(&format!("/accounts/{}/requests", admin.account.account_login))
        .into_response()
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 5 will fill in the rest of the dashboard routes.",
    )
}
