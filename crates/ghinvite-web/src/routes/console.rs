//! `/console/accounts/{login}/...` routes. Plan 5.

use crate::account_admin_reads::{
    find_account_admin_invitation_link, find_account_admin_request, pending_request_queue,
};
use crate::commands::{
    CreateInvitationLink, DecideInvitationRequest, RevokeInvitationLink,
    UpdateInvitationLinkMetadata,
};
use crate::forms::create_link::{self as create_link_form, CreateLinkSubmission};
use crate::middleware::auth::RequireConsoleAdminOf;
use crate::middleware::csrf::{CsrfForm, EmptyForm};
use crate::session;
use crate::state::AppState;
use crate::views::link_edit::{self, LinkEditValues};
use crate::views::link_form::RepositoryChoice;
use crate::views::render::render_with_csrf as render;
use axum::Router;
use axum::extract::State;
use axum::http::{Method, Uri};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;

mod audit;

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
            get(links_list).post(create_link),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}",
            get(link_detail),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/edit",
            get(edit_link_form).post(save_link_details),
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
        .route(
            "/console/accounts/{login}/audit",
            get(audit::page).layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("private, no-store"),
            )),
        )
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
    let mut session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    if !session.is_authenticated() {
        return login_redirect("/console").into_response();
    }

    let accounts = match load_console_accounts(&state, &mut session).await {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(error = ?error, "failed to load console accounts");
            let signed_in_login = Some(session.login.clone());
            let html = render(session.csrf_token.clone(), move || {
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
    let html = render(session.csrf_token.clone(), move || {
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
    let user_api = ghinvite_github::oauth::UserApiClient::new(
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

        let is_admin = crate::middleware::auth::check_admin(state, session, &account).await?;

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
    let session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
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
    let html = render(admin.session.csrf_token.clone(), move || {
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

    let html = render(admin.session.csrf_token.clone(), move || {
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

async fn links_list(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let value = |key: &str| params.get(key).map(String::as_str).unwrap_or_default();
    let query = crate::views::link_list::LinkListQuery::new(
        value("filter"),
        value("sort"),
        value("direction"),
        value("page"),
    );
    let (status, all_links) = match state
        .storage
        .list_invitation_links_for_account(admin.account.account_id)
        .await
    {
        Ok(links) => (axum::http::StatusCode::OK, Some(links)),
        Err(error) => {
            tracing::warn!(error = ?error, "failed to load invitation links");
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, None)
        }
    };
    let now = Utc::now();
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            crate::views::link_list::LinkListPage {
                signed_in_login: Some(admin.session.login.clone()),
                flash: flash.clone(),
                account_login: admin.account.account_login.clone(),
                query: query.clone(),
                all_links: all_links.clone(),
                now,
            }
        }
    });
    (status, Html(html))
}

async fn new_link_form(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
) -> impl IntoResponse {
    let repos = load_installation_repos_for_form(&state, &admin).await;
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let form = crate::views::links::LinkFormValues::default();

    let now = Utc::now();
    if state.admission.is_some() {
        return creation_form_response(
            &admin,
            flash,
            repos,
            form,
            now,
            Some(ghinvite_core::InvitationLinkId::new()),
        );
    }
    link_form_response(&admin, flash, repos, form, now)
}

/// Render the new invitation link page, both fresh and re-rendered with
/// validation errors and preserved values after a failed POST.
///
/// `now` is the instant the island validates against in the browser (it is
/// serialised into the page's props blob): on a failed POST it is the same
/// instant the server just validated with, so both sides agree.
fn link_form_response(
    admin: &RequireConsoleAdminOf,
    flash: Option<session::Flash>,
    repos: Vec<RepositoryChoice>,
    form: crate::views::links::LinkFormValues,
    now: chrono::DateTime<Utc>,
) -> axum::response::Response {
    creation_form_response(admin, flash, repos, form, now, None)
}

fn creation_form_response(
    admin: &RequireConsoleAdminOf,
    flash: Option<session::Flash>,
    repos: Vec<RepositoryChoice>,
    form: crate::views::links::LinkFormValues,
    now: chrono::DateTime<Utc>,
    link_id: Option<ghinvite_core::InvitationLinkId>,
) -> axum::response::Response {
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let action = link_id.map(|id| {
        format!(
            "/console/accounts/{account_login}/links?link_id={id}&anchor={}",
            now.timestamp()
        )
    });

    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            crate::views::links::LinkCreateFormPage {
                action: action.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                repos: repos.clone(),
                form: form.clone(),
                now,
            }
        }
    });
    Html(html).into_response()
}

/// The installation's available repositories as the form offers them. This is
/// the one place the GitHub payload becomes the view's [`RepositoryChoice`];
/// validation and rendering downstream both work on that shape.
async fn load_installation_repos_for_form(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
) -> Vec<RepositoryChoice> {
    let user_api = ghinvite_github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        admin.session.access_token.clone(),
    );
    match user_api
        .list_user_installation_repos(admin.account.installation_id)
        .await
    {
        Ok(r) => r
            .repositories
            .into_iter()
            .map(|repo| RepositoryChoice {
                id: repo.id,
                full_name: repo.full_name,
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = ?e, "failed to list installation repos; rendering form with empty list");
            vec![]
        }
    }
}

async fn create_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Query(identity): axum::extract::Query<CreationIdentity>,
    CsrfForm(form): CsrfForm<CreateLinkSubmission>,
) -> impl IntoResponse {
    let (now, link_id) = if state.admission.is_some() {
        let (Some(id), Some(anchor)) = (identity.link_id, identity.anchor) else {
            return crate::WebError::BadRequest(
                "Missing creation identity. Open a new creation form.".into(),
            )
            .into_response();
        };
        let Some(now) = chrono::DateTime::from_timestamp(anchor, 0) else {
            return crate::WebError::BadRequest("Invalid creation time.".into()).into_response();
        };
        (now, Some(id))
    } else {
        (Utc::now(), None)
    };

    // Loaded once, before validating: the validators need the available
    // repositories to resolve the repository scope, and the error path needs
    // the same list to re-render the form. Validation itself is synchronous
    // (the dioform `FormCore` holds `Rc` and must not cross an `.await`).
    let repos = load_installation_repos_for_form(&state, &admin).await;
    let validated = match create_link_form::validate(&form, &repos, now) {
        Ok(validated) => validated,
        Err(errors) => {
            return creation_form_response(
                &admin,
                None,
                repos,
                form.into_view_values(*errors),
                now,
                link_id,
            );
        }
    };

    if let Some(admission) = &state.admission {
        let id = link_id.unwrap();
        let command = ghinvite_core::storage::projection::CreateLink {
            version: 1,
            link_id: id,
            admin: admin_assertion(&admin),
            account_id: admin.account.account_id,
            installation_id: admin.account.installation_id,
            description: validated.description,
            internal_note: validated.internal_note,
            expires_at: validated.expires_at,
            max_uses: validated.max_uses,
            permission: validated.permission,
            approval_required: validated.approval_required,
            repos: validated.repos,
        };
        return match admission.create(command).await {
            Ok(_) => axum::response::Redirect::to(&format!(
                "/console/accounts/{}/links/{id}",
                admin.account.account_login
            ))
            .into_response(),
            Err(crate::WebError::Restate(_)) => {
                let mut errors = crate::views::links::LinkFormErrors::default();
                errors.summary.push("Creation outcome unknown. Retry these same values or check the link detail URL before starting another link.".into());
                (
                    axum::http::StatusCode::BAD_GATEWAY,
                    creation_form_response(
                        &admin,
                        None,
                        repos,
                        form.into_view_values(errors),
                        now,
                        Some(id),
                    ),
                )
                    .into_response()
            }
            Err(error) => super::invitation_v1::safe_error(error),
        };
    }

    let output = match state
        .commands
        .create_invitation_link(CreateInvitationLink {
            installation_id: admin.account.installation_id,
            account_id: admin.account.account_id,
            created_by: admin.session.user_id,
            created_at: now,
            expires_at: validated.expires_at,
            max_uses: validated.max_uses,
            permission: validated.permission,
            approval_required: validated.approval_required,
            description: validated.description,
            internal_note: validated.internal_note,
            repos: validated.repos,
        })
        .await
    {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("create invitation link command failed");
            let mut errors = crate::views::links::LinkFormErrors::default();
            errors
                .summary
                .push("Failed to create invitation link. Please try again.".into());
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                link_form_response(&admin, None, repos, form.into_view_values(errors), now),
            )
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

#[derive(serde::Deserialize)]
struct CreationIdentity {
    link_id: Option<ghinvite_core::InvitationLinkId>,
    anchor: Option<i64>,
}

async fn edit_link_form(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, id)): axum::extract::Path<(String, String)>,
) -> axum::response::Response {
    let Ok(id) = id.parse() else {
        return console_not_found_response(&admin);
    };
    let link = match authoritative_or_projected_link(&state, &admin, id).await {
        Ok(link) => link,
        Err(crate::error::WebError::NotFound) => return console_not_found_response(&admin),
        Err(error) => return error.into_response(),
    };
    edit_link_response(
        &admin,
        id,
        LinkEditValues {
            description: link.description,
            internal_note: link.internal_note.unwrap_or_default(),
        },
        None,
        None,
    )
}

async fn save_link_details(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, id)): axum::extract::Path<(String, String)>,
    CsrfForm(values): CsrfForm<LinkEditValues>,
) -> axum::response::Response {
    let Ok(id) = id.parse() else {
        return console_not_found_response(&admin);
    };
    if let Err(error) = authoritative_or_projected_link(&state, &admin, id).await {
        return match error {
            crate::error::WebError::NotFound => console_not_found_response(&admin),
            _ => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                edit_link_response(
                    &admin,
                    id,
                    values,
                    None,
                    Some("Failed to load invitation link details. Please try again.".into()),
                ),
            )
                .into_response(),
        };
    }
    let (description, internal_note) = match link_edit::validate(&values) {
        Ok(metadata) => metadata,
        Err(error) => return edit_link_response(&admin, id, values, Some(error), None),
    };
    if let Some(admission) = &state.admission {
        return match admission.update_metadata(ghinvite_core::admission::UpdateMetadata {
            link_id: id, admin: admin_assertion(&admin), description, internal_note,
        }).await {
            Ok(_) => axum::response::Redirect::to(&format!("/console/accounts/{}/links/{id}", admin.account.account_login)).into_response(),
            Err(crate::WebError::Restate(_)) => (axum::http::StatusCode::BAD_GATEWAY,
                edit_link_response(&admin, id, values, None, Some("Save outcome unknown. Check the link details before retrying these values.".into()))).into_response(),
            Err(error) => super::invitation_v1::safe_error(error),
        };
    }
    if state
        .commands
        .update_invitation_link_metadata(UpdateInvitationLinkMetadata {
            account_id: admin.account.account_id,
            link_id: id,
            by_user: admin.session.user_id,
            description,
            internal_note,
        })
        .await
        .is_err()
    {
        // Ingress errors may include command payloads; do not log private metadata.
        tracing::warn!("update invitation link metadata command failed");
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            edit_link_response(
                &admin,
                id,
                values,
                None,
                Some("Failed to save invitation link details. Please try again.".into()),
            ),
        )
            .into_response();
    }
    let _ = session::set_flash(
        &admin.tower,
        session::Flash {
            level: session::FlashLevel::Success,
            message: "Invitation link details updated.".into(),
        },
    )
    .await;
    axum::response::Redirect::to(&format!(
        "/console/accounts/{}/links/{id}",
        admin.account.account_login
    ))
    .into_response()
}

fn edit_link_response(
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
    values: LinkEditValues,
    description_error: Option<String>,
    form_error: Option<String>,
) -> axum::response::Response {
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    Html(render(admin.session.csrf_token.clone(), move || {
        rsx! {
            link_edit::LinkEditPage {
                signed_in_login: signed_in_login.clone(),
                account_login: account_login.clone(),
                link_id,
                values: values.clone(),
                description_error: description_error.clone(),
                form_error: form_error.clone(),
            }
        }
    }))
    .into_response()
}

async fn link_detail(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let link_id = match ghinvite_core::InvitationLinkId::from_str(&link_id_str) {
        Ok(id) => id,
        Err(_) => return console_not_found_response(&admin),
    };
    let link = match authoritative_or_projected_link(&state, &admin, link_id).await {
        Ok(link) => link,
        Err(crate::error::WebError::NotFound) => return console_not_found_response(&admin),
        Err(e) => return e.into_response(),
    };

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let now = Utc::now();
    let invitation_url = format!("{}/i/{}", state.config.base_url, link.slug.as_str());

    let html = render(admin.session.csrf_token.clone(), move || {
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
    _form: CsrfForm<EmptyForm>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let link_id = match ghinvite_core::InvitationLinkId::from_str(&link_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };
    if let Some(admission) = &state.admission {
        return match admission
            .revoke(ghinvite_core::admission::AdminLinkCommand {
                link_id,
                admin: admin_assertion(&admin),
            })
            .await
        {
            Ok(_) => axum::response::Redirect::to(&format!(
                "/console/accounts/{}/links/{link_id}",
                admin.account.account_login
            ))
            .into_response(),
            Err(crate::WebError::Restate(_)) => (
                axum::http::StatusCode::BAD_GATEWAY,
                "Revocation outcome unknown. Check the link details or retry revocation.",
            )
                .into_response(),
            Err(error) => super::invitation_v1::safe_error(error),
        };
    }
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
                decision_deadline: row.decision_deadline,
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

    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            crate::views::requests::RequestsQueuePage {
                now: Utc::now(),
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
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, request_id_str)): axum::extract::Path<(String, String)>,
    CsrfForm(form): CsrfForm<LifecycleForm>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let request_id = match ghinvite_core::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    if let Some(lifecycle) = &state.request_lifecycle {
        return authoritative_decision(
            lifecycle.as_ref(),
            &admin,
            request_id,
            form,
            ghinvite_core::request_lifecycle::DecisionAction::Approve,
        )
        .await;
    }

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
    CsrfForm(form): CsrfForm<LifecycleForm>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let request_id = match ghinvite_core::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    if let Some(lifecycle) = &state.request_lifecycle {
        return authoritative_decision(
            lifecycle.as_ref(),
            &admin,
            request_id,
            form,
            ghinvite_core::request_lifecycle::DecisionAction::Decline { reason: None },
        )
        .await;
    }

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

#[derive(serde::Deserialize)]
struct LifecycleForm {
    link_id: Option<ghinvite_core::InvitationLinkId>,
    operation_id: Option<ghinvite_core::request_lifecycle::LifecycleOperationId>,
}

async fn authoritative_decision(
    lifecycle: &dyn crate::lifecycle::RequestLifecycle,
    admin: &RequireConsoleAdminOf,
    request_id: ghinvite_core::RequestId,
    form: LifecycleForm,
    action: ghinvite_core::request_lifecycle::DecisionAction,
) -> axum::response::Response {
    use ghinvite_core::request_lifecycle::{DecideRequest, DecisionOutcome};
    let (Some(link_id), Some(operation_id)) = (form.link_id, form.operation_id) else {
        return crate::WebError::BadRequest(
            "Missing lifecycle command identity. Reload the queue.".into(),
        )
        .into_response();
    };
    let command = DecideRequest {
        version: 1,
        link_id,
        request_id,
        operation_id,
        admin: ghinvite_core::storage::projection::AccountAdmin {
            account_id: admin.account.account_id,
            user_id: admin.session.user_id,
        },
        action,
    };
    match lifecycle.decide(command).await {
        Ok(receipt) if receipt.outcome == DecisionOutcome::Incompatible => (
            axum::http::StatusCode::CONFLICT,
            format!(
                "Request is {}. The requested decision was not applied.",
                receipt.request.state
            ),
        )
            .into_response(),
        Ok(receipt) => {
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Success,
                    message: format!(
                        "Request {}{}.{}",
                        if receipt.outcome == DecisionOutcome::AlreadyCompleted {
                            "already "
                        } else {
                            ""
                        },
                        receipt.request.state,
                        if receipt.request.state == ghinvite_core::RequestState::Approved {
                            " Unavailable repositories may block delivery. The original scope and decision deadline stay unchanged."
                        } else { "" }
                    ),
                },
            )
            .await;
            axum::response::Redirect::to(&format!(
                "/console/accounts/{}/requests",
                admin.account.account_login
            ))
            .into_response()
        }
        Err(
            error @ (crate::WebError::BadRequest(_)
            | crate::WebError::NotFound
            | crate::WebError::Conflict),
        ) => error.into_response(),
        Err(_) => (
            axum::http::StatusCode::BAD_GATEWAY,
            "Decision outcome could not be confirmed. Retry the same submitted form.",
        )
            .into_response(),
    }
}

fn admin_assertion(
    admin: &RequireConsoleAdminOf,
) -> ghinvite_core::storage::projection::AccountAdmin {
    ghinvite_core::storage::projection::AccountAdmin {
        account_id: admin.account.account_id,
        user_id: admin.session.user_id,
    }
}

async fn authoritative_or_projected_link(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
) -> crate::Result<ghinvite_core::InvitationLink> {
    match &state.admission {
        Some(admission) => admission
            .link_status(ghinvite_core::admission::AdminLinkCommand {
                link_id,
                admin: admin_assertion(admin),
            })
            .await
            .map(|s| s.as_link()),
        None => {
            find_account_admin_invitation_link(
                state.storage.as_ref(),
                admin.account.account_id,
                link_id,
            )
            .await
        }
    }
}

async fn settings_page(admin: RequireConsoleAdminOf) -> impl IntoResponse {
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account = admin.account.clone();

    let html = render(admin.session.csrf_token.clone(), move || {
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
