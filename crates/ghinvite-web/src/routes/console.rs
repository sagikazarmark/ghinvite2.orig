//! `/console/accounts/{login}/...` routes. Plan 5.

use crate::account_admin_reads::{find_account_admin_invitation_link, find_account_admin_request};
use crate::forms::create_link::{self as create_link_form, CreateLinkSubmission};
use crate::middleware::auth::RequireConsoleAdminOf;
use crate::middleware::csrf::{CsrfForm, EmptyForm};
use crate::session;
use crate::state::AppState;
use crate::views::link_edit::{self, LinkEditValues};
use crate::views::link_form::{RepositoryChoice, missing_repository_notice};
use crate::views::render::render_with_csrf as render;
use axum::Router;
use axum::extract::State;
use axum::http::{Method, Uri};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;

mod attempts;
mod audit;
mod request_history;

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
            "/console/accounts/{login}/links/{link_id}/requests",
            get(request_history::history).layer(
                tower_http::set_header::SetResponseHeaderLayer::overriding(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("private, no-store"),
                ),
            ),
        )
        .route(
            "/console/accounts/{login}/requests/{request_id}",
            get(request_history::detail).layer(
                tower_http::set_header::SetResponseHeaderLayer::overriding(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("private, no-store"),
                ),
            ),
        )
        .route("/console/accounts/{login}/attempts", get(attempts::index))
        .route(
            "/console/accounts/{login}/attempts/{attempt_id}",
            get(attempts::page).post(attempts::retry),
        )
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
            tracing::warn!(
                kind = error.kind(),
                upstream_status = ?error.upstream_status(),
                "failed to load console accounts"
            );
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
        .map_err(|_| tracing::warn!("overview pending requests read failed"))
        .ok();
    let all_links = state
        .storage
        .list_invitation_links_for_account(admin.account.account_id)
        .await
        .map_err(|_| tracing::warn!("overview invitation links read failed"))
        .ok();
    let status = if pending.is_none() || all_links.is_none() {
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    } else {
        axum::http::StatusCode::OK
    };
    let active_links = all_links
        .as_ref()
        .map(|links| links.iter().filter(|l| l.is_active(now)).count() as u64);
    let recent_links = {
        let mut v = all_links.unwrap_or_default();
        v.sort_by_key(|l| std::cmp::Reverse(l.created_at));
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
    (status, Html(html)).into_response()
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

    creation_form_response(
        &admin,
        flash,
        repos,
        form,
        Utc::now(),
        ghinvite_core::InvitationLinkId::new(),
    )
}

/// Render the new invitation link page, both fresh and re-rendered with
/// validation errors and preserved values after a failed POST.
///
/// `now` is the instant the island validates against in the browser (it is
/// serialised into the page's props blob): on a failed POST it is the same
/// instant the server just validated with, so both sides agree. `link_id`
/// and `now` form the creation identity a retried submission replays.
fn creation_form_response(
    admin: &RequireConsoleAdminOf,
    flash: Option<session::Flash>,
    repos: Result<Vec<RepositoryChoice>, RepositoryLoadError>,
    mut form: crate::views::links::LinkFormValues,
    now: chrono::DateTime<Utc>,
    link_id: ghinvite_core::InvitationLinkId,
) -> axum::response::Response {
    if let Ok(available) = &repos
        && let Some(message) = missing_repository_notice(&form.selected_repo_ids, available)
    {
        if !form.errors.summary.contains(&message) {
            form.errors.summary.push(message);
        }
        // The warning makes the reduction explicit. The next submit confirms
        // the displayed scope; island props must match those native controls.
        form.selected_repo_ids
            .retain(|id| available.iter().any(|repo| repo.id == *id));
    }
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let (status, repos, repository_error) = match repos {
        Ok(repos) => (axum::http::StatusCode::OK, repos, None),
        Err(error) => (error.status, vec![], Some(error.message.to_string())),
    };
    let action = Some(format!(
        "/console/accounts/{account_login}/links?link_id={link_id}&anchor={}",
        now.timestamp()
    ));

    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            crate::views::links::LinkCreateFormPage {
                action: action.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                repos: repos.clone(),
                repository_error: repository_error.clone(),
                form: form.clone(),
                now,
            }
        }
    });
    (status, Html(html)).into_response()
}

struct RepositoryLoadError {
    status: axum::http::StatusCode,
    message: &'static str,
}

/// The installation's available repositories as the form offers them. This is
/// the one place the GitHub payload becomes the view's [`RepositoryChoice`];
/// validation and rendering downstream both work on that shape.
async fn load_installation_repos_for_form(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
) -> Result<Vec<RepositoryChoice>, RepositoryLoadError> {
    let user_api = ghinvite_github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        admin.session.access_token.clone(),
    );
    match user_api
        .list_user_installation_repos(admin.account.installation_id)
        .await
    {
        Ok(r) => Ok(r
            .repositories
            .into_iter()
            .map(|repo| RepositoryChoice {
                id: repo.id,
                full_name: repo.full_name,
            })
            .collect()),
        Err(e) => {
            use axum::http::StatusCode;
            // Never log transport URLs, credentials, or upstream response bodies.
            tracing::warn!(
                upstream_status = e.status(),
                "installation repository read failed"
            );
            let (status, message) = match e {
                ghinvite_github::Error::Status { status: 504, .. } => (
                    StatusCode::GATEWAY_TIMEOUT,
                    "GitHub did not respond in time. Try again in a moment.",
                ),
                // Every throttled response arrives here, a 429 and a
                // rate-limited 403 alike, so the refusal arm below no longer
                // has to hedge between denial and a limit.
                ghinvite_github::Error::RateLimited { .. } => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "GitHub is limiting requests. Wait a moment, then try again.",
                ),
                ghinvite_github::Error::Status { status: 401, .. } => (
                    StatusCode::BAD_GATEWAY,
                    "GitHub authorization needs attention. Sign out and sign in again, then retry.",
                ),
                ghinvite_github::Error::Status { status: 403, .. } => (
                    StatusCode::BAD_GATEWAY,
                    "GitHub denied repository access. Review GitHub App settings, then retry.",
                ),
                _ => (
                    StatusCode::BAD_GATEWAY,
                    "Repositories could not be loaded. Try again in a moment.",
                ),
            };
            Err(RepositoryLoadError { status, message })
        }
    }
}

async fn create_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: Result<RequireConsoleAdminOf, axum::response::Response>,
    tower: tower_sessions::Session,
    uri: Uri,
    axum::extract::Query(identity): axum::extract::Query<CreationIdentity>,
    form: Result<CsrfForm<CreateLinkSubmission>, axum::response::Response>,
) -> impl IntoResponse {
    let admin = match admin {
        Ok(admin) => admin,
        Err(response)
            if matches!(
                response.status(),
                axum::http::StatusCode::BAD_GATEWAY
                    | axum::http::StatusCode::SERVICE_UNAVAILABLE
                    | axum::http::StatusCode::GATEWAY_TIMEOUT
            ) =>
        {
            let Ok(CsrfForm(form)) = form else {
                return response;
            };
            let session = match session::load(&tower).await {
                Ok(session) => session,
                Err(_) => return response,
            };
            let action = uri.to_string();
            let values = form.into_view_values(Default::default());
            let html = render(session.csrf_token, move || {
                rsx! {
                    crate::views::links::AccessVerificationRetryPage {
                        signed_in_login: Some(session.login.clone()),
                        action: action.clone(),
                        values: values.clone(),
                    }
                }
            });
            return (response.status(), Html(html)).into_response();
        }
        Err(response) => return response,
    };
    let form = match form {
        Ok(CsrfForm(form)) => form,
        Err(response) => return response,
    };
    let (Some(link_id), Some(anchor)) = (identity.link_id, identity.anchor) else {
        return crate::WebError::BadRequest(
            "Missing creation identity. Open a new creation form.".into(),
        )
        .into_response();
    };
    let Some(now) = chrono::DateTime::from_timestamp(anchor, 0) else {
        return crate::WebError::BadRequest("Invalid creation time.".into()).into_response();
    };

    // Recover before present repository eligibility; expiry and repository names
    // are canonical business input, not values to recompute on retry.
    match attempts::load(&state, &admin, &format!("create-{link_id}")).await {
        Ok(Some(attempts::Command::Create(original))) => {
            let repos = original
                .repos
                .iter()
                .map(|r| RepositoryChoice {
                    id: r.repo_id,
                    full_name: r.repo_full_name.clone(),
                })
                .collect::<Vec<_>>();
            let matches = create_link_form::validate(&form, &repos, now).is_ok_and(|v| {
                v.description == original.description
                    && v.internal_note == original.internal_note
                    && v.expires_at == original.expires_at
                    && v.max_uses == original.max_uses
                    && v.permission == original.permission
                    && v.approval_required == original.approval_required
                    && v.repos == original.repos
            });
            let command = attempts::Command::Create(original);
            if !matches {
                return attempts::failed(&admin, &command, crate::WebError::Conflict);
            }
            return attempts::execute(&state, &admin, command).await;
        }
        Ok(_) => {}
        Err(e) => return e.into_response(),
    }

    // Loaded once, before validating: the validators need the available
    // repositories to resolve the repository scope, and the error path needs
    // the same list to re-render the form. Validation itself is synchronous
    // (the dioform `FormCore` holds `Rc` and must not cross an `.await`).
    let repos = load_installation_repos_for_form(&state, &admin).await;
    let repos = match repos {
        Ok(repos) => repos,
        Err(error) => {
            return creation_form_response(
                &admin,
                None,
                Err(error),
                form.into_view_values(Default::default()),
                now,
                link_id,
            );
        }
    };
    if form.reload_repos {
        return creation_form_response(
            &admin,
            None,
            Ok(repos),
            form.into_view_values(Default::default()),
            now,
            link_id,
        );
    }
    let validated = match create_link_form::validate(&form, &repos, now) {
        Ok(validated) => validated,
        Err(errors) => {
            return creation_form_response(
                &admin,
                None,
                Ok(repos),
                form.into_view_values(*errors),
                now,
                link_id,
            );
        }
    };

    let mut command = ghinvite_core::storage::projection::CreateLink {
        version: 1,
        link_id,
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
    command.repos.sort_by_key(|repo| repo.repo_id);
    attempts::execute(&state, &admin, attempts::Command::Create(command)).await
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
    let link = match authoritative_link(&state, &admin, id).await {
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
    if let Err(error) = authoritative_link(&state, &admin, id).await {
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
    match state
        .admission
        .update_metadata(ghinvite_core::admission::UpdateMetadata {
            link_id: id,
            admin: admin_assertion(&admin),
            description,
            internal_note,
        })
        .await
    {
        Ok(_) => axum::response::Redirect::to(&format!(
            "/console/accounts/{}/links/{id}",
            admin.account.account_login
        ))
        .into_response(),
        Err(crate::WebError::Restate(failure)) => {
            tracing::warn!(
                ingress_failure = %failure,
                upstream_status = ?failure.upstream_status(),
                link_id = %id,
                "invitation link metadata save outcome unknown"
            );
            (axum::http::StatusCode::BAD_GATEWAY,
            edit_link_response(&admin, id, values, None, Some("Save outcome unknown. Check the link details before retrying these values.".into()))).into_response()
        }
        Err(error) => error.into_response_with_recovery(
            format!(
                "/console/accounts/{}/links/{id}",
                admin.account.account_login
            ),
            "Back to link details",
        ),
    }
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
    let link = match authoritative_link(&state, &admin, link_id).await {
        Ok(link) => link,
        Err(e) => {
            match attempts::load(&state, &admin, &format!("create-{link_id}")).await {
                Ok(Some(command)) => return attempts::unknown(&admin, &command),
                Err(error) => return error.into_response(),
                Ok(None) => {}
            }
            return match e {
                crate::WebError::NotFound => console_not_found_response(&admin),
                error => error.into_response_with_recovery(
                    format!("/console/accounts/{}/links", admin.account.account_login),
                    "Back to invitation links",
                ),
            };
        }
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
    attempts::execute(
        &state,
        &admin,
        attempts::Command::Revoke(ghinvite_core::admission::AdminLinkCommand {
            link_id,
            admin: admin_assertion(&admin),
        }),
    )
    .await
}

async fn requests_queue(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Query(query): axum::extract::Query<QueueQuery>,
) -> impl IntoResponse {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let after = match query.after.as_deref().map(|token| {
        if token.len() > 512 {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
        let boundary: ghinvite_core::storage::pending_queue::PendingBoundary =
            serde_json::from_slice(&bytes).ok()?;
        (1..=9999)
            .contains(&chrono::Datelike::year(&boundary.created_at))
            .then_some(boundary)
    }) {
        Some(Some(boundary)) => Some(boundary),
        None => None,
        Some(None) => {
            return crate::error::WebError::BadRequest(
                "Invalid queue cursor. Return to the oldest requests.".into(),
            )
            .into_response();
        }
    };
    let page = match state
        .storage
        .pending_request_page(admin.account.account_id, after)
        .await
    {
        Ok(page) => page,
        Err(e) => return crate::error::WebError::from(e).into_response(),
    };
    let next_href = page.next.map(|boundary| {
        format!(
            "/console/accounts/{}/requests?after={}",
            admin.account.account_login,
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&boundary).expect("queue cursor serializes"))
        )
    });
    let rows = page
        .rows
        .into_iter()
        .map(|row| crate::views::requests::PendingRequestRow {
            request_id: row.request_id.to_string(),
            link_slug: row.link_slug,
            link_description: row.link_description,
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
        .collect::<Vec<_>>();

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
                next_href: next_href.clone(),
                is_continuation: after.is_some(),
            }
        }
    });
    Html(html).into_response()
}

#[derive(serde::Deserialize)]
struct QueueQuery {
    after: Option<String>,
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

    authoritative_decision(
        &state,
        &admin,
        request_id,
        form,
        ghinvite_core::request_lifecycle::DecisionAction::Approve,
    )
    .await
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

    authoritative_decision(
        &state,
        &admin,
        request_id,
        form,
        ghinvite_core::request_lifecycle::DecisionAction::Decline { reason: None },
    )
    .await
}

#[derive(serde::Deserialize)]
struct LifecycleForm {
    link_id: Option<ghinvite_core::InvitationLinkId>,
    operation_id: Option<ghinvite_core::request_lifecycle::LifecycleOperationId>,
}

async fn authoritative_decision(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    request_id: ghinvite_core::RequestId,
    form: LifecycleForm,
    action: ghinvite_core::request_lifecycle::DecisionAction,
) -> axum::response::Response {
    use ghinvite_core::request_lifecycle::DecideRequest;
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
    attempts::execute(state, admin, attempts::Command::Decision(command)).await
}

fn admin_assertion(
    admin: &RequireConsoleAdminOf,
) -> ghinvite_core::storage::projection::AccountAdmin {
    ghinvite_core::storage::projection::AccountAdmin {
        account_id: admin.account.account_id,
        user_id: admin.session.user_id,
    }
}

async fn authoritative_link(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
) -> crate::Result<ghinvite_core::InvitationLink> {
    state
        .admission
        .link_status(ghinvite_core::admission::AdminLinkCommand {
            link_id,
            admin: admin_assertion(admin),
        })
        .await
        .map(|s| s.as_link())
}

async fn settings_page(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
) -> impl IntoResponse {
    let error = if admin.account.uninstalled_at.is_none() {
        load_installation_repos_for_form(&state, &admin).await.err()
    } else {
        None
    };
    let status = error
        .as_ref()
        .map_or(axum::http::StatusCode::OK, |error| error.status);
    let availability_error = error.map(|error| error.message.to_string());
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account = admin.account.clone();

    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            crate::views::settings::SettingsPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account: account.clone(),
                availability_error: availability_error.clone(),
            }
        }
    });
    (status, Html(html)).into_response()
}

async fn not_found(admin: RequireConsoleAdminOf) -> impl IntoResponse {
    console_not_found_response(&admin)
}

async fn plain_not_found() -> impl IntoResponse {
    crate::error::WebError::NotFound
}
