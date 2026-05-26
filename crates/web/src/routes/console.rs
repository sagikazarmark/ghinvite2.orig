//! `/console/accounts/{login}/...` routes. Plan 5.

use crate::account_admin_reads::{
    find_account_admin_invitation_link, find_account_admin_request, pending_request_queue,
};
use crate::commands::{CreateInvitationLink, DecideInvitationRequest, RevokeInvitationLink};
use crate::middleware::auth::RequireConsoleAdminOf;
use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::extract::State;
use axum::http::{Method, Uri};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/console", get(console_index))
        .route("/console/accounts/{login}", get(overview))
        .route(
            "/console/accounts/{login}/",
            get(not_found).fallback(plain_not_found),
        )
        .route("/console/accounts/{login}/links/new", get(new_link_form))
        .route(
            "/console/accounts/{login}/links",
            axum::routing::post(create_link),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}",
            get(link_detail),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/revoke",
            axum::routing::post(revoke_link),
        )
        .route("/console/accounts/{login}/requests", get(requests_queue))
        .route(
            "/console/accounts/{login}/requests/{request_id}/approve",
            axum::routing::post(approve_request),
        )
        .route(
            "/console/accounts/{login}/requests/{request_id}/decline",
            axum::routing::post(decline_request),
        )
        .route("/console/accounts/{login}/settings", get(settings_page))
        .route("/console/accounts/{login}/audit", get(audit_page))
        .route(
            "/console/accounts/{login}/{*rest}",
            get(not_found).fallback(plain_not_found),
        )
        .route("/console/{*rest}", get(console_unknown))
}

async fn console_index(
    State(state): State<AppState>,
    tower: tower_sessions::Session,
) -> impl IntoResponse {
    let mut session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return login_redirect("/console").into_response();
    }

    let accounts = match load_console_accounts(&state, &mut session).await {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(error = ?error, "failed to load console accounts");
            let signed_in_login = Some(session.login.clone());
            let html = render(move || {
                rsx! {
                    crate::views::console::ConsoleIndexPage {
                        signed_in_login: signed_in_login.clone(),
                        state: crate::views::console::ConsoleIndexState::LoadError,
                    }
                }
            });
            return Html(html).into_response();
        }
    };

    let _ = session::save(&tower, &session).await;

    if accounts.len() == 1 {
        return axum::response::Redirect::to(&format!("/console/accounts/{}", accounts[0].login))
            .into_response();
    }

    let state_view = if accounts.is_empty() {
        crate::views::console::ConsoleIndexState::Empty
    } else {
        crate::views::console::ConsoleIndexState::AccountPicker { accounts }
    };
    let signed_in_login = Some(session.login.clone());
    let html = render(move || {
        rsx! {
            crate::views::console::ConsoleIndexPage {
                signed_in_login: signed_in_login.clone(),
                state: state_view.clone(),
            }
        }
    });
    Html(html).into_response()
}

async fn load_console_accounts(
    state: &AppState,
    session: &mut session::Session,
) -> Result<Vec<crate::views::console::ConsoleAccountChoice>, crate::error::WebError> {
    let user_api = github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        session.access_token.clone(),
    );
    let visible = user_api.list_user_installations().await?;
    let mut accounts = Vec::new();

    for installation in visible.installations {
        let Some(account) = state
            .storage
            .get_active_installation_by_account_id(installation.account.id)
            .await?
        else {
            continue;
        };

        let is_admin = if account.account_type == domain::AccountType::User {
            session.login == account.account_login
        } else {
            crate::middleware::auth::check_admin(state, session, &account.account_login).await?
        };

        if is_admin {
            accounts.push(crate::views::console::ConsoleAccountChoice {
                login: account.account_login,
                account_type: crate::views::components::account_type_label(account.account_type)
                    .to_string(),
            });
        }
    }

    accounts.sort_by(|a, b| a.login.cmp(&b.login));
    Ok(accounts)
}

fn login_redirect(return_to: &str) -> axum::response::Redirect {
    let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    axum::response::Redirect::to(&format!("/login?return_to={encoded}"))
}

async fn console_unknown(uri: Uri, tower: tower_sessions::Session) -> axum::response::Response {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        let return_to = uri
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("/console");
        return login_redirect(return_to).into_response();
    }

    crate::routes::not_found::public(Method::GET, uri, tower).await
}

fn console_not_found_response(admin: &RequireConsoleAdminOf) -> axum::response::Response {
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let html = render(move || {
        rsx! {
            crate::views::not_found::ConsoleNotFoundPage {
                signed_in_login: signed_in_login.clone(),
                account_login: account_login.clone(),
            }
        }
    });
    (axum::http::StatusCode::NOT_FOUND, Html(html)).into_response()
}

async fn overview(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
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
        .list_invitation_links_for_account(admin.account.account_id)
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
    let account_type = admin.account.account_type;

    let html = render(move || {
        rsx! {
            crate::views::console::OverviewPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                account_type,
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
    admin: RequireConsoleAdminOf,
) -> impl IntoResponse {
    let repos = load_installation_repos_for_form(&state, &admin).await;

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
    description: Option<String>,
    permission: String,
    #[serde(default)]
    approval_required: Option<String>,
    max_uses: Option<String>,
    expires_in_days: Option<String>,
    internal_note: Option<String>,
    #[serde(default)]
    repo_ids: Vec<u64>,
}

const DESCRIPTION_MAX_CHARS: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
struct DescriptionValidationError {
    message: String,
}

fn validate_description(raw: Option<&str>) -> Result<String, DescriptionValidationError> {
    let raw_description = raw.unwrap_or("");
    let description = raw_description.trim();
    if description.is_empty() {
        return Err(DescriptionValidationError {
            message: "Description is required. Use short, single-line admin-only context for this invitation link.".into(),
        });
    }
    if raw_description.contains('\n') || raw_description.contains('\r') {
        return Err(DescriptionValidationError {
            message: "Description must be a single line.".into(),
        });
    }
    if description.chars().count() > DESCRIPTION_MAX_CHARS {
        return Err(DescriptionValidationError {
            message: "Description must be 120 characters or fewer.".into(),
        });
    }
    Ok(description.to_string())
}

impl CreateLinkForm {
    fn into_view_values(self) -> crate::views::links::LinkFormValues {
        crate::views::links::LinkFormValues {
            description: self.description.unwrap_or_default(),
            permission: self.permission,
            approval_required: self.approval_required.is_some(),
            max_uses: self.max_uses.unwrap_or_default(),
            expires_in_days: self.expires_in_days.unwrap_or_default(),
            internal_note: self.internal_note.unwrap_or_default(),
            selected_repo_ids: self.repo_ids,
            errors: crate::views::links::LinkFormErrors::default(),
        }
    }
}

async fn load_installation_repos_for_form(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
) -> Vec<github::payloads::GhRepo> {
    let user_api = github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        admin.session.access_token.clone(),
    );
    match user_api
        .list_user_installation_repos(admin.account.installation_id)
        .await
    {
        Ok(r) => r.repositories,
        Err(e) => {
            tracing::warn!(error = ?e, "failed to list installation repos; rendering form with empty list");
            vec![]
        }
    }
}

async fn create_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
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
    let description = match validate_description(form.description.as_deref()) {
        Ok(description) => description,
        Err(error) => {
            let repos = load_installation_repos_for_form(&state, &admin).await;
            let mut form_values = form.into_view_values();
            form_values.errors.summary =
                vec!["Fix the highlighted fields before creating this invitation link.".into()];
            form_values.errors.description = Some(error.message);

            let signed_in_login = Some(admin.session.login.clone());
            let account_login = admin.account.account_login.clone();
            let html = render(move || {
                rsx! {
                    crate::views::links::LinkCreateFormPage {
                        signed_in_login: signed_in_login.clone(),
                        flash: None,
                        account_login: account_login.clone(),
                        repos: repos.clone(),
                        form: form_values.clone(),
                    }
                }
            });
            return Html(html).into_response();
        }
    };
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

    let installation_repos = load_installation_repos_for_form(&state, &admin).await;
    let repos: Vec<domain::InvitationLinkRepo> = installation_repos
        .into_iter()
        .filter(|r| form.repo_ids.contains(&r.id))
        .map(|r| domain::InvitationLinkRepo {
            repo_id: r.id,
            repo_full_name: r.full_name,
        })
        .collect();

    if repos.is_empty() {
        return crate::error::WebError::BadRequest("select at least one repository".into())
            .into_response();
    }

    let output = match state
        .commands
        .create_invitation_link(CreateInvitationLink {
            installation_id: admin.account.installation_id,
            account_id: admin.account.account_id,
            created_by: admin.session.user_id,
            created_at: now,
            expires_at,
            max_uses,
            permission,
            approval_required,
            description,
            internal_note,
            repos,
        })
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, "create invitation link command failed");
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Error,
                    message: "Failed to create invitation link. Please try again.".into(),
                },
            )
            .await;
            return axum::response::Redirect::to(&format!(
                "/console/accounts/{}/links/new",
                admin.account.account_login
            ))
            .into_response();
        }
    };
    let link_id = output.link_id.to_string();

    let _ = session::set_flash(
        &admin.tower,
        session::Flash {
            level: session::FlashLevel::Success,
            message: "Invitation link created.".into(),
        },
    )
    .await;
    axum::response::Redirect::to(&format!(
        "/console/accounts/{}/links/{}",
        admin.account.account_login, link_id
    ))
    .into_response()
}

async fn link_detail(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let link_id = match domain::InvitationLinkId::from_str(&link_id_str) {
        Ok(id) => id,
        Err(_) => return console_not_found_response(&admin),
    };
    let link = match find_account_admin_invitation_link(
        state.storage.as_ref(),
        admin.account.account_id,
        link_id,
    )
    .await
    {
        Ok(link) => link,
        Err(crate::error::WebError::NotFound) => return console_not_found_response(&admin),
        Err(e) => return e.into_response(),
    };

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let now = Utc::now();
    let invitation_url = format!("{}/i/{}", state.config.base_url, link.slug.as_str());

    let html = render(move || {
        rsx! {
            crate::views::links::LinkDetailPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                link: link.clone(),
                now,
                invitation_url: invitation_url.clone(),
            }
        }
    });
    Html(html).into_response()
}

async fn revoke_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let link_id = match domain::InvitationLinkId::from_str(&link_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };
    if let Err(e) = find_account_admin_invitation_link(
        state.storage.as_ref(),
        admin.account.account_id,
        link_id,
    )
    .await
    {
        return e.into_response();
    }

    if let Err(e) = state
        .commands
        .revoke_invitation_link(RevokeInvitationLink {
            account_id: admin.account.account_id,
            link_id,
            by_user: admin.session.user_id,
            when: Utc::now(),
        })
        .await
    {
        tracing::warn!(error = ?e, "revoke invitation link command failed");
        let _ = session::set_flash(
            &admin.tower,
            session::Flash {
                level: session::FlashLevel::Error,
                message: "Could not stop this invitation link. Please try again.".into(),
            },
        )
        .await;
        return axum::response::Redirect::to(&format!(
            "/console/accounts/{}/links/{}",
            admin.account.account_login, link_id_str
        ))
        .into_response();
    }

    let _ = session::set_flash(
        &admin.tower,
        session::Flash {
            level: session::FlashLevel::Success,
            message: "Invitation link stopped accepting new invitation requests.".into(),
        },
    )
    .await;
    axum::response::Redirect::to(&format!(
        "/console/accounts/{}",
        admin.account.account_login
    ))
    .into_response()
}

async fn requests_queue(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
) -> impl IntoResponse {
    let rows = match pending_request_queue(state.storage.as_ref(), admin.account.account_id).await {
        Ok(rows) => rows
            .into_iter()
            .map(|row| crate::views::requests::PendingRequestRow {
                request_id: row.request_id.to_string(),
                link_slug: row.link_slug,
                link_id: row.link_id.map(|id| id.to_string()).unwrap_or_default(),
                requester_login: row.requester_login,
                justification: row.justification,
                created_at: row.created_at,
                permission: row.permission.map(|permission| permission.to_string()),
                repos: row.repos,
                expires_at: row.expires_at,
                approval_required: row.approval_required,
            })
            .collect::<Vec<_>>(),
        Err(e) => return e.into_response(),
    };

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

async fn audit_page(admin: RequireConsoleAdminOf) -> impl IntoResponse {
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();

    let html = render(move || {
        rsx! {
            crate::views::audit::AuditLogPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
            }
        }
    });
    Html(html).into_response()
}

async fn approve_request(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, request_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let request_id = match domain::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    let _account_request = match find_account_admin_request(
        state.storage.as_ref(),
        admin.account.account_id,
        request_id,
    )
    .await
    {
        Ok(account_request) => account_request,
        Err(e) => return e.into_response(),
    };

    match state
        .commands
        .decide_invitation_request(DecideInvitationRequest::approve(
            request_id,
            admin.session.user_id,
            Utc::now(),
        ))
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
            tracing::warn!(error = ?e, "approve invitation request command failed");
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Error,
                    message: "Failed to approve request. Please try again.".into(),
                },
            )
            .await;
        }
    }
    axum::response::Redirect::to(&format!(
        "/console/accounts/{}/requests",
        admin.account.account_login
    ))
    .into_response()
}

async fn decline_request(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, request_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let request_id = match domain::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    let _account_request = match find_account_admin_request(
        state.storage.as_ref(),
        admin.account.account_id,
        request_id,
    )
    .await
    {
        Ok(account_request) => account_request,
        Err(e) => return e.into_response(),
    };

    // v1: no reason field in the form; v1.1 will add a textarea.
    match state
        .commands
        .decide_invitation_request(DecideInvitationRequest::decline(
            request_id,
            admin.session.user_id,
            Utc::now(),
            None,
        ))
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
            tracing::warn!(error = ?e, "decline invitation request command failed");
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Error,
                    message: "Failed to decline request. Please try again.".into(),
                },
            )
            .await;
        }
    }
    axum::response::Redirect::to(&format!(
        "/console/accounts/{}/requests",
        admin.account.account_login
    ))
    .into_response()
}

async fn settings_page(admin: RequireConsoleAdminOf) -> impl IntoResponse {
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account = admin.account.clone();

    let html = render(move || {
        rsx! {
            crate::views::settings::SettingsPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account: account.clone(),
            }
        }
    });
    Html(html).into_response()
}

async fn not_found(admin: RequireConsoleAdminOf) -> impl IntoResponse {
    console_not_found_response(&admin)
}

async fn plain_not_found() -> impl IntoResponse {
    crate::error::WebError::NotFound
}

#[cfg(test)]
mod tests {
    use super::validate_description;

    #[test]
    fn description_validation_trims_valid_input() {
        assert_eq!(
            validate_description(Some("  AI coding workshop  ")).unwrap(),
            "AI coding workshop"
        );
    }

    #[test]
    fn description_validation_requires_value() {
        let err = validate_description(None).unwrap_err();
        assert_eq!(
            err.message,
            "Description is required. Use short, single-line admin-only context for this invitation link."
        );

        let err = validate_description(Some("   ")).unwrap_err();
        assert_eq!(
            err.message,
            "Description is required. Use short, single-line admin-only context for this invitation link."
        );
    }

    #[test]
    fn description_validation_rejects_multiline_text() {
        let err = validate_description(Some("AI\nworkshop")).unwrap_err();
        assert_eq!(err.message, "Description must be a single line.");

        let err = validate_description(Some("AI workshop\n")).unwrap_err();
        assert_eq!(err.message, "Description must be a single line.");

        let err = validate_description(Some("\rAI workshop")).unwrap_err();
        assert_eq!(err.message, "Description must be a single line.");
    }

    #[test]
    fn description_validation_rejects_over_120_characters() {
        let too_long = "x".repeat(121);
        let err = validate_description(Some(&too_long)).unwrap_err();
        assert_eq!(err.message, "Description must be 120 characters or fewer.");
    }
}
