//! `/console/accounts/{login}/...` routes.

use crate::forms::create_link::{self as create_link_form, CreateLinkSubmission, PickerRequest};
use crate::link_authority::AuthorityError;
use crate::middleware::auth::{ConsoleAdminRejection, RequireConsoleAdminOf};
use crate::middleware::csrf::{CsrfForm, EmptyForm};
use crate::render::render_with_csrf as render;
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;
use ghinvite_ui::link_edit::{self, LinkEditValues};
use ghinvite_ui::link_form::{RepositoryChoice, RepositoryPicker, missing_repository_notice};

mod attempts;
mod audit;
mod bulk;
mod cursor;
mod reissue;
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
        .route("/console/accounts/{login}/bulk", get(bulk::new))
        .route("/console/accounts/{login}/bulk/new", get(bulk::new))
        .route(
            "/console/accounts/{login}/bulk/{batch_id}",
            get(bulk::detail),
        )
        .route(
            "/console/accounts/{login}/bulk/{batch_id}/resolve",
            axum::routing::post(bulk::resolve),
        )
        .route(
            "/console/accounts/{login}/bulk/{batch_id}/retry",
            axum::routing::post(bulk::retry),
        )
        .route(
            "/console/accounts/{login}/bulk/{batch_id}/rows/remove",
            axum::routing::post(bulk::edit),
        )
        .route(
            "/console/accounts/{login}/bulk/{batch_id}/submit",
            axum::routing::post(bulk::submit),
        )
        .route(
            "/console/accounts/{login}/bulk/{batch_id}/cancel",
            axum::routing::post(bulk::cancel),
        )
        .route(
            "/console/accounts/{login}/links/new/selection",
            get(check_repository_selection),
        )
        .route(
            "/console/accounts/{login}/links",
            get(links_list).post(create_link),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}",
            get(link_detail),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/duplicate",
            get(duplicate_link),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/edit",
            get(edit_link_form).post(save_link_details),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/revoke",
            axum::routing::get(revoke_confirmation).post(revoke_link),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/rotate",
            axum::routing::post(rotate_link),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/pause",
            axum::routing::post(pause_link),
        )
        .route(
            "/console/accounts/{login}/links/{link_id}/resume",
            axum::routing::post(resume_link),
        )
        .route("/console/accounts/{login}/requests", get(requests_queue))
        .route(
            "/console/accounts/{login}/links/{link_id}/requests",
            get(request_history::history),
        )
        .route(
            "/console/accounts/{login}/requests/{request_id}",
            get(request_history::detail),
        )
        .route(
            "/console/accounts/{login}/requests/{request_id}/avatar",
            get(request_history::avatar),
        )
        .route(
            "/console/accounts/{login}/requests/{request_id}/reissue/{kind}/{target_id}",
            get(reissue::confirmation).post(reissue::submit),
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
            "/console/accounts/{login}/settings/invitation-cap",
            axum::routing::post(set_org_cap),
        )
        .route(
            "/console/accounts/{login}/settings/delegation",
            axum::routing::post(set_delegation),
        )
        .route("/console/accounts/{login}/audit", get(audit::page))
        .route(
            "/console/accounts/{login}/{*rest}",
            get(not_found).fallback(plain_not_found),
        )
        .route("/console/{*rest}", get(console_unknown))
        // Every Console answer belongs to one signed-in admin, including the
        // redirects and plain refusals the HTML-only layer in `build_app` skips.
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("private, no-store"),
        ))
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
                    ghinvite_ui::console::ConsoleIndexPage {
                        signed_in_login: signed_in_login.clone(),
                        state: ghinvite_ui::console::ConsoleIndexState::LoadError,
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
        ghinvite_ui::console::ConsoleIndexState::Empty
    } else {
        ghinvite_ui::console::ConsoleIndexState::AccountPicker { accounts }
    };
    let signed_in_login = Some(session.login.clone());
    let html = render(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::console::ConsoleIndexPage {
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
) -> Result<Vec<ghinvite_ui::console::ConsoleAccountChoice>, crate::error::WebError> {
    let mut accounts = Vec::new();
    let mut candidates = std::collections::BTreeMap::<u64, ghinvite_core::Account>::new();
    for app in [
        ghinvite_core::account::App::Repo,
        ghinvite_core::account::App::Org,
    ] {
        for installation in state.storage.list_active_app_installations(app).await? {
            let account = installation.account;
            if let Some(prior) = candidates.get(&account.account_id) {
                if prior.account_type != account.account_type
                    || !prior
                        .account_login
                        .eq_ignore_ascii_case(&account.account_login)
                {
                    return Err(crate::WebError::ConsoleAuthority);
                }
            } else {
                candidates.insert(account.account_id, account);
            }
        }
    }
    for account in candidates.into_values() {
        let Some(authority) = crate::middleware::auth::account_authority(state, session, &account)
            .await
            .map_err(|_| crate::WebError::ConsoleAuthority)?
        else {
            continue;
        };
        if authority.role() == crate::authority::Role::Delegated
            && authority
                .console_scope()
                .await
                .map_err(|_| crate::WebError::ConsoleAuthority)?
                .authorized_repositories
                == 0
        {
            continue;
        }
        accounts.push(ghinvite_ui::console::ConsoleAccountChoice {
            login: account.account_login,
            account_type: ghinvite_ui::components::account_type_label(account.account_type)
                .to_string(),
        });
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
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::not_found::ConsoleNotFoundPage {
                signed_in_login: signed_in_login.clone(),
                account_login: account_login.clone(),
            }
        }
    });
    (axum::http::StatusCode::NOT_FOUND, Html(html)).into_response()
}

/// The id a Console route names in its path. A malformed id names nothing,
/// so it answers exactly like a missing resource: the Console's own
/// not-found page, which only a verified admin may see.
// The rejection is the rendered response the handler returns as-is.
#[allow(clippy::result_large_err)]
fn path_id<T: std::str::FromStr>(
    admin: &RequireConsoleAdminOf,
    raw: &str,
) -> Result<T, axum::response::Response> {
    raw.parse().map_err(|_| console_not_found_response(admin))
}

async fn overview(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
) -> impl IntoResponse {
    let scope = match admin.authority.console_scope().await {
        Ok(scope) => scope,
        Err(error) => return crate::middleware::auth::authority_unavailable(error),
    };

    let now = Utc::now();
    let pending = state
        .storage
        .scoped_count_pending_requests_for_account(&scope.scope)
        .await
        .map_err(|_| tracing::warn!("overview pending requests read failed"))
        .ok();
    let all_links = state
        .storage
        .scoped_list_invitation_links_for_account(&scope.scope)
        .await
        .map_err(|_| tracing::warn!("overview invitation links read failed"))
        .ok();
    let digest = state
        .storage
        .scoped_blocked_digest(&scope.scope, now.date_naive())
        .await
        .map_err(|_| tracing::warn!("overview daily delivery summary read failed"));
    let digest_unavailable = digest.is_err();
    let daily_digest = digest.ok().flatten();
    let status = if pending.is_none() || all_links.is_none() || digest_unavailable {
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
    let bulk_enabled = state.config.bulk_enabled;

    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::console::OverviewPage {
                bulk_enabled,
                daily_digest: daily_digest.clone(),
                digest_unavailable,
                authority_pending: scope.pending,
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
    let scope = match admin.authority.console_scope().await {
        Ok(scope) => scope,
        Err(error) => return crate::middleware::auth::authority_unavailable(error),
    };

    let value = |key: &str| params.get(key).map(String::as_str).unwrap_or_default();
    let query = ghinvite_ui::link_list::LinkListQuery::new(
        value("filter"),
        value("sort"),
        value("direction"),
        value("page"),
    );
    let (status, all_links) = match state
        .storage
        .scoped_list_invitation_links_for_account(&scope.scope)
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
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::link_list::LinkListPage {
                authority_pending: scope.pending,
                signed_in_login: Some(admin.session.login.clone()),
                flash: flash.clone(),
                account_login: admin.account.account_login.clone(),
                query: query.clone(),
                all_links: all_links.clone(),
                now,
            }
        }
    });
    (status, Html(html)).into_response()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositorySelectionQuery {
    repo_id: u64,
}

/// Selection feedback grants no durable authority. Creation independently
/// checks the complete submitted selection through the same bounded service.
async fn check_repository_selection(
    admin: RequireConsoleAdminOf,
    axum::extract::Query(query): axum::extract::Query<RepositorySelectionQuery>,
) -> axum::response::Response {
    match admin.authority.repositories_by_ids(&[query.repo_id]).await {
        Ok(_) => axum::Json(serde_json::json!({"allowed": true})).into_response(),
        Err(
            crate::authority::AuthorityError::Denied
            | crate::authority::AuthorityError::InvalidSelection,
        ) => crate::WebError::NotFound.into_response(),
        Err(_) => crate::WebError::ConsoleAuthority.into_response(),
    }
}

async fn new_link_form(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let picker = PickerRequest::new(
        query.get("q").map(String::as_str).unwrap_or_default(),
        query.get("page").map(String::as_str),
    );
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let form = ghinvite_ui::links::LinkFormValues::default();

    creation_form_response(
        &state,
        &admin,
        flash,
        form,
        picker,
        Utc::now(),
        ghinvite_core::InvitationLinkId::new(),
        None,
    )
    .await
}

/// Render the new invitation link page, both fresh and re-rendered with
/// validation errors and preserved values after a failed POST.
///
/// `now` is the instant the island validates against in the browser (it is
/// serialised into the page's props blob): on a failed POST it is the same
/// instant the server just validated with, so both sides agree. `link_id`
/// and `now` form the creation identity a retried submission replays.
///
/// The repository scope lists the page of the installation's repositories
/// that `picker` asks for, plus every selected repository the installation
/// still exposes. `load_error` renders the scope as unavailable instead.
#[allow(clippy::too_many_arguments)]
async fn creation_form_response(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    flash: Option<session::Flash>,
    mut form: ghinvite_ui::links::LinkFormValues,
    picker: PickerRequest,
    now: chrono::DateTime<Utc>,
    link_id: ghinvite_core::InvitationLinkId,
    load_error: Option<RepositoryLoadError>,
) -> axum::response::Response {
    let window = match load_error {
        Some(error) => Err(error),
        None => repository_window(
            admin
                .authority
                .repository_picker(&picker.query, picker.page, &form.selected_repo_ids)
                .await,
            &picker.query,
        ),
    };
    if let Ok(window) = &window
        && let Some(message) = missing_repository_notice(&form.selected_repo_ids, &window.selected)
    {
        if !form.errors.summary.contains(&message) {
            form.errors.summary.push(message);
        }
        // The warning makes the reduction explicit. The next submit confirms
        // the displayed scope; island props must match those native controls.
        form.selected_repo_ids
            .retain(|id| window.selected.iter().any(|repo| repo.id == *id));
    }
    let organization = admin
        .authority
        .organization_offer()
        .await
        .unwrap_or(ghinvite_ui::link_form::OrganizationOffer::Unavailable);
    let eligibility = eligibility_panel(state, admin).await;
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let (status, repos, picker, repository_error) = match window {
        Ok(window) => (
            axum::http::StatusCode::OK,
            window.repos,
            window.picker,
            None,
        ),
        Err(error) => (
            error.status,
            vec![],
            RepositoryPicker::default(),
            Some(error.message.to_string()),
        ),
    };
    let action = format!(
        "/console/accounts/{account_login}/links?link_id={link_id}&anchor={}",
        now.timestamp()
    );

    let access_removal_enabled = state.config.access_removal_enabled;
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::links::LinkCreateFormPage {
                access_removal_enabled,
                action: action.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                repos: repos.clone(),
                picker: picker.clone(),
                eligibility: eligibility.clone(),
                organization: organization.clone(),
                repository_error: repository_error.clone(),
                form: form.clone(),
                now,
            }
        }
    });
    (status, Html(html)).into_response()
}

async fn eligibility_panel(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
) -> ghinvite_ui::organization_eligibility::Eligibility {
    use ghinvite_ui::organization_eligibility::Eligibility;
    if admin.account.account_type != ghinvite_core::AccountType::Organization {
        return Eligibility::NotApplicable;
    }
    let Some(installation_id) = admin.authority.current_org_installation_id() else {
        return Eligibility::Unknown;
    };
    match state
        .commands
        .organization_facts(admin.account.account_id)
        .await
    {
        Ok(Some(facts)) if facts.fresh(admin.account.account_id, installation_id, Utc::now()) => {
            Eligibility::Known(facts)
        }
        _ => Eligibility::Unknown,
    }
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
    let _ = state;
    repository_choices(admin.authority.search_repositories("").await)
}
fn repository_choices(
    result: Result<crate::authority::RepositoryChoices, crate::authority::AuthorityError>,
) -> Result<Vec<RepositoryChoice>, RepositoryLoadError> {
    result
        .and_then(|choices| {
            if choices.pending {
                Err(crate::authority::AuthorityError::Busy)
            } else {
                Ok(choices.repositories)
            }
        })
        .map(|repos| {
            repos
                .into_iter()
                .map(|repo| RepositoryChoice {
                    id: repo.id,
                    full_name: repo.full_name,
                })
                .collect()
        })
        .map_err(|_| RepositoryLoadError {
            status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            message: "Repository access could not be checked. Try again in a minute.",
        })
}
/// The repository scope the create form offers for one picker page.
struct PickerView {
    /// Pinned selections from outside the page, then the page.
    repos: Vec<RepositoryChoice>,
    /// Every selected repository the installation still exposes.
    selected: Vec<RepositoryChoice>,
    picker: RepositoryPicker,
}
fn repository_window(
    result: Result<crate::authority::RepositoryWindow, crate::authority::AuthorityError>,
    query: &str,
) -> Result<PickerView, RepositoryLoadError> {
    let choice = |repo: ghinvite_core::storage::console_scope::RepoTarget| RepositoryChoice {
        id: repo.id,
        full_name: repo.full_name,
    };
    match result {
        Ok(window) if !window.pending => {
            let pinned: Vec<_> = window
                .selected
                .iter()
                .filter(|repo| !window.listed.contains(repo))
                .cloned()
                .map(choice)
                .collect();
            let picker = RepositoryPicker {
                query: query.to_owned(),
                page: window.page,
                pages: window.pages,
                matches: window.matches,
                pinned: pinned.len(),
                search_required: window.search_required,
                truncated: window.truncated,
            };
            Ok(PickerView {
                repos: pinned
                    .into_iter()
                    .chain(window.listed.into_iter().map(choice))
                    .collect(),
                selected: window.selected.into_iter().map(choice).collect(),
                picker,
            })
        }
        _ => Err(RepositoryLoadError {
            status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            message: "Repository access could not be checked. Try again in a minute.",
        }),
    }
}
async fn selected_repositories(
    admin: &RequireConsoleAdminOf,
    form: &CreateLinkSubmission,
) -> Result<Vec<RepositoryChoice>, RepositoryLoadError> {
    if form.repo_ids.is_empty() {
        return repository_choices(admin.authority.repositories_for_review(&[]).await);
    }
    match admin.authority.repositories_by_ids(&form.repo_ids).await {
        Ok(repos) => Ok(repos
            .into_iter()
            .map(|repo| RepositoryChoice {
                id: repo.id,
                full_name: repo.full_name,
            })
            .collect()),
        Err(crate::authority::AuthorityError::InvalidSelection) => repository_choices(
            admin
                .authority
                .repositories_for_review(&form.repo_ids)
                .await,
        ),
        Err(error) => Err(RepositoryLoadError {
            status: if error == crate::authority::AuthorityError::Denied {
                axum::http::StatusCode::NOT_FOUND
            } else {
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            },
            message: "Repository access could not be checked. Try again in a minute.",
        }),
    }
}

async fn create_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: Result<RequireConsoleAdminOf, ConsoleAdminRejection>,
    tower: tower_sessions::Session,
    uri: Uri,
    axum::extract::Query(identity): axum::extract::Query<CreationIdentity>,
    form: Result<CsrfForm<CreateLinkSubmission>, axum::response::Response>,
) -> impl IntoResponse {
    let admin = match admin {
        Ok(admin) => admin,
        // GitHub could not say whether this user may create links here. Keep
        // the submission so verification can be retried without retyping it.
        Err(rejection @ ConsoleAdminRejection::AuthorityUnavailable(_)) => {
            let response = rejection.into_response();
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
                    ghinvite_ui::links::AccessVerificationRetryPage {
                        signed_in_login: Some(session.login.clone()),
                        action: action.clone(),
                        values: values.clone(),
                    }
                }
            });
            return (response.status(), Html(html)).into_response();
        }
        Err(rejection) => return rejection.into_response(),
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

    // Form helpers never dispatch or replay a business mutation. They only
    // re-render the form; its repository page is loaded like any other.
    let picker = form.picker_request();
    if form.apply_all || form.reload_repos || form.is_picker_helper() {
        match attempts::pending_create(&state, &admin, link_id).await {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(error) => return error.into_response(),
        }
        let apply_all = form.apply_all;
        let offered: std::collections::BTreeSet<u64> = form
            .repo_grants
            .iter()
            .filter_map(|value| value.split_once(':').and_then(|(id, _)| id.parse().ok()))
            .chain(form.repo_ids.iter().copied())
            .collect();
        let mut values = form.into_view_values(Default::default());
        if apply_all {
            if values
                .permission
                .parse::<ghinvite_core::Permission>()
                .is_err()
            {
                values.errors.permission =
                    Some(ghinvite_ui::link_form::PERMISSION_UNSUPPORTED.into());
                values
                    .errors
                    .summary
                    .push(ghinvite_ui::link_form::SUMMARY_MESSAGE.into());
            } else {
                // Every repository the form offered, as the island's set-all
                // does. A grant names a repository; it never selects or
                // authorizes one, and creation validates the selection anew.
                values.repo_grants = offered
                    .iter()
                    .map(|id| format!("{id}:{}", values.permission))
                    .collect();
            }
        }
        return creation_form_response(&state, &admin, None, values, picker, now, link_id, None)
            .await;
    }
    // Recover before present repository eligibility; expiry and repository names
    // are canonical business input, not values to recompute on retry.
    let recovery = attempts::recover_create(&state, &admin, link_id, &form, now).await;
    match recovery {
        Ok(attempts::CreateRecovery::Fresh) => {}
        Ok(attempts::CreateRecovery::Answered(response)) => return response,
        Ok(attempts::CreateRecovery::Rejected(rejected)) => {
            return rejected_creation_response(&state, &admin, form, now, link_id, rejected).await;
        }
        Err(e) => return e.into_response(),
    }

    // Loaded once, before validating: the validators need the available
    // repositories to resolve the repository scope, and the error path needs
    // the same list to re-render the form. Validation itself is synchronous
    // (the dioform `FormCore` holds `Rc` and must not cross an `.await`).
    let repos = selected_repositories(&admin, &form).await;
    let repos = match repos {
        Ok(repos) => repos,
        Err(error) => {
            if error.status == axum::http::StatusCode::NOT_FOUND {
                return crate::WebError::Forbidden.into_response();
            }
            return creation_form_response(
                &state,
                &admin,
                None,
                form.into_view_values(Default::default()),
                picker,
                now,
                link_id,
                Some(error),
            )
            .await;
        }
    };
    let organization = admin
        .authority
        .organization_offer()
        .await
        .unwrap_or(ghinvite_ui::link_form::OrganizationOffer::Unavailable);
    let validated = match create_link_form::validate_selection(&form, &repos, &organization, now) {
        Ok(validated) => validated,
        Err(errors) => {
            return creation_form_response(
                &state,
                &admin,
                None,
                form.into_view_values(*errors),
                picker,
                now,
                link_id,
                None,
            )
            .await;
        }
    };

    if validated.access_lifetime_seconds.is_some() && !state.config.access_removal_enabled {
        let errors = ghinvite_ui::link_form::LinkFormErrors {
            access_lifetime_hours: Some(
                "Access expiry is unavailable. Contact the operator before applying a lifetime."
                    .into(),
            ),
            ..Default::default()
        };
        let mut response = creation_form_response(
            &state,
            &admin,
            None,
            form.into_view_values(errors),
            picker,
            now,
            link_id,
            None,
        )
        .await;
        *response.status_mut() = axum::http::StatusCode::SERVICE_UNAVAILABLE;
        return response;
    }
    if !validated.email_domains.is_empty() && state.config.signin_apps.is_none() {
        let errors = ghinvite_ui::link_form::LinkFormErrors {
            email_domains: Some("Email-domain restrictions are unavailable. Remove the restriction or contact the operator.".into()),
            ..Default::default()
        };
        return creation_form_response(
            &state,
            &admin,
            None,
            form.into_view_values(errors),
            picker,
            now,
            link_id,
            None,
        )
        .await;
    }
    let secret = match crate::link_secret::LinkSecret::generate() {
        Ok(secret) => secret,
        Err(_) => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Invitation URL could not be generated. Try again.",
            )
                .into_response();
        }
    };
    let (installation_id, organization_installation_id) =
        match admin.authority.creation_installations(&validated.targets) {
            Ok(ids) => ids,
            Err(_) => return crate::WebError::Forbidden.into_response(),
        };
    let mut command = validated.into_command(
        link_id,
        admin_assertion(&admin),
        admin.account.account_id,
        installation_id,
        secret.hash(),
    );
    command.organization_installation_id = organization_installation_id;
    match admin.authority.can_create_targets(&command.targets).await {
        Ok(crate::authority::Access::Allowed) => {}
        Ok(crate::authority::Access::Denied) => return crate::WebError::Forbidden.into_response(),
        Ok(crate::authority::Access::Pending) | Err(_) => {
            return crate::WebError::ConsoleAuthority.into_response();
        }
    }
    match attempts::submit_with_secret(
        &state,
        &admin,
        attempts::Command::Create(Box::new(command)),
        Some(secret),
    )
    .await
    {
        Ok(response) => response,
        Err(rejected) => {
            rejected_creation_response(&state, &admin, form, now, link_id, rejected).await
        }
    }
}

/// The authority refused a validated creation (for example, an expiry it
/// considers already past). Preserve submitted values and show a bounded
/// typed reason; business refusals allocate a fresh creation identity.
async fn rejected_creation_response(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    form: CreateLinkSubmission,
    now: chrono::DateTime<Utc>,
    link_id: ghinvite_core::InvitationLinkId,
    rejected: attempts::CreateRejected,
) -> axum::response::Response {
    let picker = form.picker_request();
    let mut values = form.into_view_values(Default::default());
    use ghinvite_core::invitation_link::{CreateLinkError, GuardrailField};
    match rejected.refusal {
        Some(CreateLinkError::NoGuardrail) => values
            .errors
            .summary
            .push(ghinvite_ui::link_form::NO_GUARDRAIL.to_owned()),
        Some(CreateLinkError::InvalidTargets { .. }) => {
            values.errors.repo_scope = Some("Choose valid repositories from this account.".into())
        }
        Some(CreateLinkError::OrgAutoApproveNeedsMaxUses) => {
            values.errors.max_uses =
                Some("Automatically approved organization links need a maximum use limit.".into())
        }
        Some(CreateLinkError::InvalidGuardrail {
            field: GuardrailField::MaxUses,
        }) => values.errors.max_uses = Some(ghinvite_ui::link_form::MAX_USES_NOT_POSITIVE.into()),
        Some(CreateLinkError::InvalidGuardrail {
            field: GuardrailField::ExpiresAt,
        }) => values.errors.expires_in_days = Some("Choose an expiration in the future.".into()),
        Some(CreateLinkError::InvalidGuardrail {
            field: GuardrailField::InvitationLifetime,
        }) => {
            values.errors.invitation_lifetime_hours =
                Some(ghinvite_ui::link_form::INVITATION_LIFETIME_INVALID.into())
        }
        Some(CreateLinkError::InvalidGuardrail {
            field: GuardrailField::AccessLifetime,
        }) => {
            values.errors.access_lifetime_hours =
                Some(ghinvite_ui::link_form::ACCESS_LIFETIME_INVALID.into())
        }
        Some(CreateLinkError::InvalidGuardrail {
            field: GuardrailField::AbuseThreshold,
        }) => {
            values.errors.abuse_threshold_per_hour =
                Some(ghinvite_ui::link_form::ABUSE_THRESHOLD_INVALID.into())
        }
        Some(CreateLinkError::InvalidGuardrail {
            field: GuardrailField::BudgetShare,
        }) => {
            values.errors.budget_share_pct =
                Some(ghinvite_ui::link_form::BUDGET_SHARE_INVALID.into())
        }
        Some(CreateLinkError::InvalidGuardrail {
            field: GuardrailField::EmailDomains,
        }) => {
            values.errors.email_domains =
                Some("Choose at most 32 unique email-domain rules.".into());
        }
        Some(CreateLinkError::AlreadyExists) | None => values
            .errors
            .summary
            .push(attempts::CREATE_REJECTED.to_owned()),
    }
    // A retained business refusal owns the old identity. Preserve the values,
    // but allocate a new identity and anchor before accepting corrections.
    let (link_id, now) = if rejected.refusal.is_some() {
        (ghinvite_core::InvitationLinkId::new(), Utc::now())
    } else {
        (link_id, now)
    };
    let mut response =
        creation_form_response(state, admin, None, values, picker, now, link_id, None).await;
    // A repository load failure keeps its own status.
    if response.status() == axum::http::StatusCode::OK {
        *response.status_mut() = axum::http::StatusCode::BAD_REQUEST;
    }
    response
}

#[derive(serde::Deserialize)]
struct CreationIdentity {
    link_id: Option<ghinvite_core::InvitationLinkId>,
    anchor: Option<i64>,
}

async fn duplicate_link(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, id)): axum::extract::Path<(String, String)>,
) -> axum::response::Response {
    let id = match path_id(&admin, &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let link = match authoritative_link(&state, &admin, id).await {
        Ok(link) => link,
        Err(AuthorityError::Missing) => return console_not_found_response(&admin),
        Err(error) => return crate::WebError::from(error).into_response(),
    };
    if let Err(error) = authorize_targets(&admin, &link.targets, false).await {
        return error.into_response();
    }
    // The source's repositories are the selection, so the picker lists them
    // wherever they are in the installation.
    let values = ghinvite_ui::links::LinkFormValues {
        email_domains: link
            .email_domains
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        description: link.description,
        internal_note: link.internal_note.unwrap_or_default(),
        approval_required: link.approval_required,
        include_organization: link.targets.organization.is_some(),
        selected_team_ids: link.targets.teams.iter().map(|team| team.team_id).collect(),
        selected_repo_ids: link.targets.repos.iter().map(|repo| repo.repo_id).collect(),
        repo_grants: link
            .targets
            .repos
            .iter()
            .map(|repo| {
                format!(
                    "{}:{}",
                    repo.repo_id,
                    ghinvite_core::Permission::from(repo.grant)
                )
            })
            .collect(),
        max_uses: link
            .max_uses
            .map(|value| value.to_string())
            .unwrap_or_default(),
        never_expires: link.expires_at.is_none(),
        expires_in_days: "14".into(),
        expiration_timestamp: link
            .expires_at
            .map(|expiry| expiry.to_rfc3339())
            .unwrap_or_default(),
        access_lifetime_hours: link
            .access_lifetime_seconds
            .map(|seconds| format!("{seconds}s"))
            .unwrap_or_default(),
        invitation_lifetime_seconds: link.invitation_lifetime_seconds.to_string(),
        invitation_lifetime_hours: (link.invitation_lifetime_seconds / 3600).to_string(),
        abuse_threshold_per_hour: link
            .abuse_threshold_per_hour
            .map(|value| value.to_string())
            .unwrap_or_default(),
        budget_share_pct: link.budget_share_pct.to_string(),
        ..Default::default()
    };
    creation_form_response(
        &state,
        &admin,
        None,
        values,
        PickerRequest::default(),
        Utc::now(),
        ghinvite_core::InvitationLinkId::new(),
        None,
    )
    .await
}

async fn edit_link_form(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, id)): axum::extract::Path<(String, String)>,
) -> axum::response::Response {
    let id = match path_id(&admin, &id) {
        Ok(id) => id,
        Err(not_found) => return not_found,
    };
    let link = match authoritative_link(&state, &admin, id).await {
        Ok(link) => link,
        Err(AuthorityError::Missing) => return console_not_found_response(&admin),
        Err(error) => return crate::WebError::from(error).into_response(),
    };
    edit_link_response(
        &admin,
        id,
        LinkEditValues {
            description: link.description,
            internal_note: link.internal_note.unwrap_or_default(),
        },
        Default::default(),
        None,
    )
}

async fn save_link_details(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, id)): axum::extract::Path<(String, String)>,
    CsrfForm(values): CsrfForm<LinkEditValues>,
) -> axum::response::Response {
    let id = match path_id(&admin, &id) {
        Ok(id) => id,
        Err(not_found) => return not_found,
    };
    if let Err(error) = authorize_link_mutation(&state, &admin, id).await {
        return error.into_response();
    }
    let (description, internal_note) = match link_edit::validate(&values) {
        Ok(metadata) => metadata,
        Err(errors) => return edit_link_response(&admin, id, values, errors, None),
    };
    match state
        .link_authority
        .update_metadata(ghinvite_core::admission::UpdateMetadata {
            link_id: id,
            admin: admin_assertion(&admin),
            description,
            internal_note,
        })
        .await
    {
        Ok(_) => {
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
        Err(AuthorityError::Unknown(failure)) => {
            tracing::warn!(
                ingress_failure = %failure,
                upstream_status = ?failure.upstream_status(),
                link_id = %id,
                "invitation link metadata save outcome unknown"
            );
            (axum::http::StatusCode::BAD_GATEWAY,
            edit_link_response(&admin, id, values, Default::default(), Some("Save outcome unknown. Check the link details before retrying these values.".into()))).into_response()
        }
        // The authority answers an unknown link and another account's link
        // alike, so neither says which it was.
        Err(AuthorityError::Missing) => console_not_found_response(&admin),
        Err(error) => crate::WebError::from(error).into_response_with_recovery(
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
    errors: link_edit::LinkEditErrors,
    form_error: Option<String>,
) -> axum::response::Response {
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    Html(render(admin.render_context(), move || {
        rsx! {
            link_edit::LinkEditPage {
                signed_in_login: signed_in_login.clone(),
                account_login: account_login.clone(),
                link_id,
                values: values.clone(),
                description_error: errors.description.clone(),
                internal_note_error: errors.internal_note.clone(),
                form_error: form_error.clone(),
            }
        }
    }))
    .into_response()
}

async fn link_detail(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let link_id = match path_id(&admin, &link_id) {
        Ok(id) => id,
        Err(not_found) => return not_found,
    };
    let link = match authoritative_link(&state, &admin, link_id).await {
        Ok(link) => link,
        Err(e) => {
            match attempts::pending_create(&state, &admin, link_id).await {
                Ok(Some(response)) => return response,
                Err(error) => return error.into_response(),
                Ok(None) => {}
            }
            return match e {
                AuthorityError::Missing => console_not_found_response(&admin),
                error => crate::WebError::from(error).into_response_with_recovery(
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
    let progress = if link.revoked_at.is_some() {
        state
            .link_authority
            .cascade_progress(ghinvite_core::admission::AdminLinkCommand {
                link_id,
                admin: admin_assertion(&admin),
            })
            .await
            .map_err(|_| ())
    } else {
        Ok(None)
    };
    let progress_unavailable = progress.is_err();
    let progress = progress.ok().flatten();
    let mut share_limited = false;
    let mut budget_unavailable = false;
    for app in [
        ghinvite_core::account::App::Repo,
        ghinvite_core::account::App::Org,
    ] {
        match state
            .storage
            .share_limited_links(
                ghinvite_core::account::AccountInstallationRef {
                    app,
                    account_id: admin.account.account_id,
                },
                Some(link_id),
            )
            .await
        {
            Ok(rows) => share_limited |= !rows.is_empty(),
            Err(_) => budget_unavailable = true,
        }
    }

    let eligibility = eligibility_panel(&state, &admin).await;
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::links::LinkDetailPage {
                eligibility: eligibility.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                link: link.clone(),
                now,
                cascade_progress:progress.clone(),
                cascade_unavailable:progress_unavailable,
                share_limited,
                budget_unavailable,
            }
        }
    });
    Html(html).into_response()
}

#[derive(Clone, serde::Deserialize)]
struct RevokeForm {
    operation_id: ghinvite_core::link_lifecycle::LifecycleOperationId,
    #[serde(default)]
    decline_pending_requests: bool,
    #[serde(default)]
    cancel_pending_invitations: bool,
    #[serde(default)]
    remove_granted_access: bool,
    #[serde(default)]
    confirmation: String,
}
async fn revoke_confirmation(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id)): axum::extract::Path<(String, String)>,
) -> axum::response::Response {
    let link_id = match path_id(&admin, &link_id) {
        Ok(id) => id,
        Err(e) => return e,
    };
    render_revoke(
        &state,
        &admin,
        link_id,
        RevokeForm {
            operation_id: ghinvite_core::RequestId::new()
                .to_string()
                .try_into()
                .unwrap(),
            decline_pending_requests: false,
            cancel_pending_invitations: false,
            remove_granted_access: false,
            confirmation: String::new(),
        },
        None,
    )
    .await
}
async fn render_revoke(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
    form: RevokeForm,
    error: Option<String>,
) -> axum::response::Response {
    let link = match authoritative_link(state, admin, link_id).await {
        Ok(link) => link,
        Err(e) => return crate::WebError::from(e).into_response(),
    };
    match admin.authority.can_mutate_targets(&link.targets).await {
        Ok(crate::authority::Access::Allowed) => {}
        Ok(crate::authority::Access::Denied) => return crate::WebError::Forbidden.into_response(),
        _ => return crate::WebError::ConsoleAuthority.into_response(),
    }
    let access_preview = if state.config.access_removal_enabled {
        state
            .link_authority
            .prepare_access_removal(ghinvite_core::revocation::PrepareAccessRemoval {
                link_id,
                admin: admin_assertion(admin),
                operation_id: form.operation_id.clone(),
            })
            .await
            .ok()
    } else {
        None
    };
    let access_enabled = state.config.access_removal_enabled;
    let preview = state
        .storage
        .revoke_preview(admin.account.account_id, link_id)
        .await
        .ok()
        .flatten();
    let login = admin.account.account_login.clone();
    let status = if error.is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let signed_in_login = Some(admin.session.login.clone());
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::layouts::ConsoleLayout {
                signed_in_login: signed_in_login.clone(),
                title: "Revoke invitation link",
                account_login: Some(login.clone()),
                active_nav: Some("links".to_owned()),
                children: rsx! {
                    ghinvite_ui::revocation::RevokeConfirmation {
                        account_login:login.clone(),description:link.description.clone(),link_id:link_id.to_string(),operation_id:String::from(form.operation_id.clone()),
                        pending_requests:preview.map(|p|p.pending_requests),pending_invitations:preview.map(|p|p.pending_invitations),
                        decline_pending_requests:form.decline_pending_requests,confirmation:form.confirmation.clone(),error:error.clone(),
                        access_enabled, access_preview:access_preview.clone(), remove_granted_access:form.remove_granted_access,
                    }
                }
            }
        }
    });
    (status, Html(html)).into_response()
}
async fn revoke_link(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id)): axum::extract::Path<(String, String)>,
    CsrfForm(form): CsrfForm<RevokeForm>,
) -> axum::response::Response {
    let link_id = match path_id(&admin, &link_id) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let cascade = ghinvite_core::revocation::RevokeCascade {
        decline_pending_requests: form.decline_pending_requests,
        cancel_pending_invitations: form.cancel_pending_invitations,
        remove_granted_access: form.remove_granted_access,
    };
    if cascade.any() && form.confirmation != "REVOKE" {
        return render_revoke(
            &state,
            &admin,
            link_id,
            form,
            Some("Type REVOKE to confirm the selected cascade.".into()),
        )
        .await;
    }
    attempts::execute(
        &state,
        &admin,
        attempts::Command::Revoke(ghinvite_core::revocation::RevokeCommand {
            link_id,
            admin: admin_assertion(&admin),
            operation_id: form.operation_id,
            cascade,
        }),
    )
    .await
}

#[derive(serde::Deserialize)]
struct LinkLifecycleForm {
    operation_id: ghinvite_core::link_lifecycle::LifecycleOperationId,
}

async fn pause_link(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id)): axum::extract::Path<(String, String)>,
    CsrfForm(form): CsrfForm<LinkLifecycleForm>,
) -> axum::response::Response {
    change_link_lifecycle(
        state,
        admin,
        link_id,
        form,
        ghinvite_core::link_lifecycle::LifecycleAction::Pause,
    )
    .await
}

async fn resume_link(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id)): axum::extract::Path<(String, String)>,
    CsrfForm(form): CsrfForm<LinkLifecycleForm>,
) -> axum::response::Response {
    change_link_lifecycle(
        state,
        admin,
        link_id,
        form,
        ghinvite_core::link_lifecycle::LifecycleAction::Resume,
    )
    .await
}

async fn change_link_lifecycle(
    state: AppState,
    admin: RequireConsoleAdminOf,
    link_id: String,
    form: LinkLifecycleForm,
    action: ghinvite_core::link_lifecycle::LifecycleAction,
) -> axum::response::Response {
    let link_id = match path_id(&admin, &link_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    attempts::execute(
        &state,
        &admin,
        attempts::Command::Lifecycle {
            action,
            command: ghinvite_core::link_lifecycle::LifecycleCommand {
                link_id,
                admin: admin_assertion(&admin),
                operation_id: form.operation_id,
            },
        },
    )
    .await
}

#[derive(serde::Deserialize)]
struct RotationForm {
    operation_id: String,
}

async fn rotate_link(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, link_id)): axum::extract::Path<(String, String)>,
    CsrfForm(form): CsrfForm<RotationForm>,
) -> axum::response::Response {
    let link_id = match path_id(&admin, &link_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let operation_id =
        match ghinvite_core::admission::AdmissionOperationId::try_from(form.operation_id) {
            Ok(id) => id,
            Err(_) => {
                return crate::WebError::BadRequest("Invalid operation ID.".into()).into_response();
            }
        };
    if let Some(response) = attempts::recover_rotation(&state, &admin, link_id, &operation_id).await
    {
        return response;
    }
    let secret = match crate::link_secret::LinkSecret::generate() {
        Ok(secret) => secret,
        Err(_) => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Invitation URL could not be generated. Try again.",
            )
                .into_response();
        }
    };
    let command = attempts::Command::Rotate(ghinvite_core::invitation_link::RotateSecret {
        link_id,
        operation_id,
        admin: admin_assertion(&admin),
        new_sha256: secret.hash(),
    });
    attempts::submit_with_secret(&state, &admin, command, Some(secret))
        .await
        .unwrap_or_else(|_| crate::WebError::BadRequest("Invalid rotation.".into()).into_response())
}

async fn requests_queue(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Query(query): axum::extract::Query<QueueQuery>,
) -> impl IntoResponse {
    let scope = match admin.authority.console_scope().await {
        Ok(scope) => scope,
        Err(error) => return crate::middleware::auth::authority_unavailable(error),
    };

    use ghinvite_core::storage::pending_queue::PendingBoundary;
    let after = match query
        .after
        .as_deref()
        .map(|token| cursor::decode(token, |boundary: &PendingBoundary| boundary.created_at))
    {
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
        .scoped_list_pending_requests_page(&scope.scope, after)
        .await
    {
        Ok(page) => page,
        Err(e) => return crate::error::WebError::from(e).into_response(),
    };
    let next_href = page.next.map(|boundary| {
        format!(
            "/console/accounts/{}/requests?after={}",
            admin.account.account_login,
            cursor::encode(&boundary)
        )
    });
    let rows = page
        .rows
        .into_iter()
        .map(|row| ghinvite_ui::requests::PendingRequestRow {
            request_id: row.request_id.to_string(),
            link_description: row.link_description,
            link_id: row.link_id.map(|id| id.to_string()).unwrap_or_default(),
            avatar_href: row
                .requester_avatar_url
                .filter(|url| !url.is_empty())
                .map(|_| {
                    format!(
                        "/console/accounts/{}/requests/{}/avatar",
                        admin.account.account_login, row.request_id
                    )
                }),
            github_created_at: row.requester_github_created_at,
            requester_login: row.requester_login,
            justification: row.justification,
            created_at: row.created_at,
            decision_deadline: row.decision_deadline,
            targets: row.targets,
            expires_at: row.expires_at,
            approval_required: row.approval_required,
        })
        .collect::<Vec<_>>();

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();

    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::requests::RequestsQueuePage {
                authority_pending: scope.pending,
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
    axum::extract::Path((_login, request_id)): axum::extract::Path<(String, String)>,
    CsrfForm(form): CsrfForm<LifecycleForm>,
) -> impl IntoResponse {
    authoritative_decision(
        &state,
        &admin,
        &request_id,
        form,
        ghinvite_core::request_lifecycle::DecisionAction::Approve,
    )
    .await
}

async fn decline_request(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, request_id)): axum::extract::Path<(String, String)>,
    CsrfForm(mut form): CsrfForm<LifecycleForm>,
) -> impl IntoResponse {
    let requester_message = match ghinvite_core::request_lifecycle::normalize_requester_message(
        form.requester_message.take(),
    ) {
        Ok(message) => message,
        Err(message) => return crate::WebError::BadRequest(message.into()).into_response(),
    };
    authoritative_decision(
        &state,
        &admin,
        &request_id,
        form,
        ghinvite_core::request_lifecycle::DecisionAction::Decline {
            reason: None,
            requester_message,
        },
    )
    .await
}

#[derive(serde::Deserialize)]
struct LifecycleForm {
    requester_message: Option<String>,
    link_id: Option<ghinvite_core::InvitationLinkId>,
    operation_id: Option<ghinvite_core::request_lifecycle::LifecycleOperationId>,
}

async fn authoritative_decision(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    request_id: &str,
    form: LifecycleForm,
    action: ghinvite_core::request_lifecycle::DecisionAction,
) -> axum::response::Response {
    use ghinvite_core::request_lifecycle::DecideRequest;
    let request_id = match path_id(admin, request_id) {
        Ok(id) => id,
        Err(not_found) => return not_found,
    };
    let (Some(link_id), Some(operation_id)) = (form.link_id, form.operation_id) else {
        return crate::WebError::BadRequest(
            "Missing lifecycle command identity. Reload the queue.".into(),
        )
        .into_response();
    };
    let command = DecideRequest {
        link_id,
        request_id,
        operation_id,
        admin: ghinvite_core::invitation_link::AccountAdmin {
            account_id: admin.account.account_id,
            user_id: admin.session.user_id,
        },
        action,
    };
    attempts::execute(state, admin, attempts::Command::Decision(command)).await
}

fn admin_assertion(admin: &RequireConsoleAdminOf) -> ghinvite_core::invitation_link::AccountAdmin {
    ghinvite_core::invitation_link::AccountAdmin {
        account_id: admin.account.account_id,
        user_id: admin.session.user_id,
    }
}

async fn authoritative_link(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
) -> Result<ghinvite_core::InvitationLink, AuthorityError> {
    let link = state
        .link_authority
        .link_status(ghinvite_core::admission::AdminLinkCommand {
            link_id,
            admin: admin_assertion(admin),
        })
        .await?
        .as_link();
    if link.account_id != admin.account.account_id {
        return Err(AuthorityError::Missing);
    }
    match admin.authority.can_read_link(&link.targets).await {
        Ok(crate::authority::Access::Allowed) => Ok(link),
        Ok(crate::authority::Access::Denied) => Err(AuthorityError::Missing),
        Ok(crate::authority::Access::Pending) | Err(_) => {
            Err(AuthorityError::AuthorizationUnavailable)
        }
    }
}

async fn authorize_targets(
    admin: &RequireConsoleAdminOf,
    targets: &ghinvite_core::targets::Targets,
    mutation: bool,
) -> crate::Result<()> {
    let access = if mutation {
        admin.authority.can_mutate_targets(targets).await
    } else {
        admin.authority.can_read_link(targets).await
    };
    match access {
        Ok(crate::authority::Access::Allowed) => Ok(()),
        Ok(crate::authority::Access::Denied) => Err(crate::WebError::Forbidden),
        Ok(crate::authority::Access::Pending) | Err(_) => Err(crate::WebError::ConsoleAuthority),
    }
}
async fn authorize_link_mutation(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    id: ghinvite_core::InvitationLinkId,
) -> crate::Result<()> {
    let link = authoritative_link(state, admin, id)
        .await
        .map_err(crate::WebError::from)?;
    authorize_targets(admin, &link.targets, true).await
}

async fn settings_page(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
) -> impl IntoResponse {
    if !admin.authority.is_owner() {
        return crate::WebError::Forbidden.into_response();
    }
    let delegation = match state
        .storage
        .account_settings(admin.account.account_id)
        .await
    {
        Ok(settings) => settings.is_some_and(|settings| settings.repo_admin_delegation),
        Err(error) => return crate::WebError::from(error).into_response(),
    };
    let can_manage_delegation = matches!(
        admin.authority.role(),
        crate::authority::Role::InstallingOwner | crate::authority::Role::AccountAdmin
    );
    let delegation_operation = ghinvite_core::RequestId::new().to_string();
    let (org_cap, org_cap_error) = if admin.account.account_type
        == ghinvite_core::AccountType::Organization
        && admin.authority.role() == crate::authority::Role::AccountAdmin
    {
        match state
            .commands
            .org_cap_policy(admin.account.account_id)
            .await
        {
            Ok(view) => (Some(view), None),
            Err(_) => (
                None,
                Some("Invitation budget settings are unavailable. Try again later.".to_string()),
            ),
        }
    } else {
        (None, None)
    };
    let org_cap_operation = ghinvite_core::RequestId::new().to_string();
    // SQL snapshots are display-only. Never infer permission to write from them.
    let mut budgets = Vec::new();
    let mut share_limited = Vec::new();
    let mut budget_error = false;
    for app in [
        ghinvite_core::account::App::Repo,
        ghinvite_core::account::App::Org,
    ] {
        if app == ghinvite_core::account::App::Org
            && admin.authority.role() != crate::authority::Role::AccountAdmin
        {
            continue;
        }
        match state
            .storage
            .get_invite_budget(ghinvite_core::account::AccountInstallationRef {
                app,
                account_id: admin.account.account_id,
            })
            .await
        {
            Ok(Some(snapshot)) => budgets.push(snapshot),
            Ok(None) => (),
            Err(_) => budget_error = true,
        }
        match state
            .storage
            .share_limited_links(
                ghinvite_core::account::AccountInstallationRef {
                    app,
                    account_id: admin.account.account_id,
                },
                None,
            )
            .await
        {
            Ok(rows) => share_limited.extend(rows),
            Err(_) => budget_error = true,
        }
    }
    share_limited.sort_by_key(|link| link.link_id.to_string());
    share_limited.dedup_by_key(|link| link.link_id);

    let error = if admin.account.uninstalled_at.is_none()
        && admin.authority.has_repository_installation()
    {
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

    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::settings::SettingsPage {
                can_manage_delegation,
                delegation,
                delegation_operation: delegation_operation.clone(),
                org_cap: org_cap.clone(),
                org_cap_operation: org_cap_operation.clone(),
                org_cap_error: org_cap_error.clone(),
                budgets: budgets.clone(),
                budget_error,
                share_limited: share_limited.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account: account.clone(),
                availability_error: availability_error.clone(),
            }
        }
    });
    (status, Html(html)).into_response()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegationForm {
    #[serde(rename = "csrf_token")]
    _csrf_token: String,
    operation_id: String,
    #[serde(default)]
    on: bool,
}
async fn set_delegation(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    CsrfForm(form): CsrfForm<DelegationForm>,
) -> axum::response::Response {
    if !matches!(
        admin.authority.role(),
        crate::authority::Role::InstallingOwner | crate::authority::Role::AccountAdmin
    ) {
        return crate::WebError::Forbidden.into_response();
    }
    let operation_id =
        match ghinvite_core::admission::AdmissionOperationId::try_from(form.operation_id) {
            Ok(id) => id,
            Err(_) => {
                return crate::WebError::BadRequest(
                    "Invalid operation identity. Reload settings.".into(),
                )
                .into_response();
            }
        };
    let command = ghinvite_core::account::SetDelegationInput {
        operation_id,
        actor_user_id: admin.session.user_id,
        on: form.on,
    };
    match state
        .commands
        .set_delegation(admin.account.account_id, command)
        .await
    {
        Ok(Ok(())) => {
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Success,
                    message: "Repository admin delegation updated.".into(),
                },
            )
            .await;
            axum::response::Redirect::to(&format!(
                "/console/accounts/{}/settings",
                admin.account.account_login
            ))
            .into_response()
        }
        Ok(Err(_)) => crate::WebError::Forbidden.into_response(),
        Err(error) => error.into_response_with_recovery(
            format!("/console/accounts/{}/settings", admin.account.account_login),
            "Back to settings",
        ),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OrgCapForm {
    #[serde(rename = "csrf_token")]
    _csrf_token: String,
    operation_id: String,
    #[serde(default)]
    confirmed_established: bool,
}
async fn set_org_cap(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    CsrfForm(form): CsrfForm<OrgCapForm>,
) -> axum::response::Response {
    if admin.account.account_type != ghinvite_core::AccountType::Organization
        || admin.authority.role() != crate::authority::Role::AccountAdmin
    {
        return crate::WebError::Forbidden.into_response();
    }
    let Ok(operation_id) = form.operation_id.clone().try_into() else {
        return crate::WebError::BadRequest("Invalid operation identity. Reload settings.".into())
            .into_response();
    };
    let input = ghinvite_core::budget::SetOrgCap {
        operation_id,
        actor_user_id: admin.session.user_id,
        confirmed_established: form.confirmed_established,
    };
    match state
        .commands
        .set_org_cap(admin.account.account_id, input)
        .await
    {
        Ok(Ok(_)) => {
            let _ = session::set_flash(
                &admin.tower,
                session::Flash {
                    level: session::FlashLevel::Success,
                    message: "Organization invitation cap updated.".into(),
                },
            )
            .await;
            axum::response::Redirect::to(&format!(
                "/console/accounts/{}/settings",
                admin.account.account_login
            ))
            .into_response()
        }
        Ok(Err(
            ghinvite_core::budget::OrgCapRefusal::NotOwner
            | ghinvite_core::budget::OrgCapRefusal::OrgAppRequired,
        )) => crate::WebError::Forbidden.into_response(),
        Err(crate::WebError::Conflict) => crate::WebError::Conflict.into_response_with_recovery(
            format!("/console/accounts/{}/settings", admin.account.account_login),
            "Back to settings",
        ),
        Ok(Err(ghinvite_core::budget::OrgCapRefusal::BudgetUnavailable)) => {
            let html = render(admin.render_context(), move || {
                rsx! {
                    ghinvite_ui::layouts::ConsoleLayout {
                        signed_in_login: Some(admin.session.login.clone()), title: "Invitation cap", account_login: Some(admin.account.account_login.clone()), active_nav: Some("settings".into()), flash: None,
                        children: rsx! {
                            p { role: "alert", "Invitation budget settings are unavailable. This change was not applied." }
                            a { href: "/console/accounts/{admin.account.account_login}/settings", "Back to settings" }
                        },
                    }
                }
            });
            (axum::http::StatusCode::SERVICE_UNAVAILABLE, Html(html)).into_response()
        }
        Err(_) => {
            let message =
                "The result has not been confirmed. Retry this same change to recover its result.";
            let html = render(admin.render_context(), move || {
                rsx! {
                    ghinvite_ui::layouts::ConsoleLayout {
                        signed_in_login: Some(admin.session.login.clone()), title: "Invitation cap", account_login: Some(admin.account.account_login.clone()), active_nav: Some("settings".into()), flash: None,
                        children: rsx! { ghinvite_ui::settings::OrgCapForm { account_login: admin.account.account_login.clone(), operation_id: form.operation_id.clone(), confirmed: form.confirmed_established, error: Some(message.to_string()) } },
                    }
                }
            });
            (axum::http::StatusCode::SERVICE_UNAVAILABLE, Html(html)).into_response()
        }
    }
}

async fn not_found(admin: RequireConsoleAdminOf) -> impl IntoResponse {
    console_not_found_response(&admin)
}

async fn plain_not_found() -> impl IntoResponse {
    crate::error::WebError::NotFound
}

fn organization_from_targets(
    targets: &ghinvite_core::targets::Targets,
) -> ghinvite_ui::link_form::OrganizationOffer {
    use ghinvite_ui::link_form::{OrganizationChoice, OrganizationOffer, TeamChoice};
    targets
        .organization
        .as_ref()
        .map_or(OrganizationOffer::NotApplicable, |org| {
            OrganizationOffer::Available(OrganizationChoice {
                id: org.org_id,
                login: org.org_login.clone(),
                teams: targets
                    .teams
                    .iter()
                    .map(|team| TeamChoice {
                        id: team.team_id,
                        slug: team.team_slug.clone(),
                        name: team.team_slug.clone(),
                        externally_managed: false,
                    })
                    .collect(),
            })
        })
}
