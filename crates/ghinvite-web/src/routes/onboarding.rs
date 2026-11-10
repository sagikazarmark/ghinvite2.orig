//! App-JWT pending requests are app-wide upstream. Filter them by numeric
//! requester or an independently authorized account before building UI props.
use crate::{
    WebError, installed_apps, middleware::auth::RequireConsoleAdminOf, session, state::AppState,
};
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use dioxus::prelude::*;
use ghinvite_core::account::App;
use ghinvite_ui::onboarding::{AppStatus, PendingRequest};
use serde::Deserialize;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/onboarding", get(chooser))
        .route("/onboarding/requests", get(requests))
        .route(
            "/console/accounts/{login}/installations",
            get(account_installations),
        )
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("private, no-store"),
        ))
}
fn available(state: &AppState, app: App) -> bool {
    installed_apps::configured(state, app).is_some()
        && (app == App::Repo || state.config.github_org_install_url.is_some())
}
fn signin(path: &str) -> Response {
    let encoded: String = url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
    Redirect::to(&format!("/login?return_to={encoded}")).into_response()
}
async fn load(tower: &TowerSession) -> crate::Result<session::Session> {
    session::load(tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))
}
async fn chooser(State(state): State<AppState>, tower: TowerSession) -> crate::Result<Response> {
    let session = load(&tower).await?;
    if !session.is_authenticated() {
        return Ok(signin("/onboarding"));
    }
    let repo_available = available(&state, App::Repo);
    let org_available = available(&state, App::Org);
    Ok(Html(crate::render::render_with_csrf(session.csrf_token,move ||rsx!{
        ghinvite_ui::onboarding::Chooser { signed_in_login:session.login.clone(),repo_available,org_available }
    })).into_response())
}
#[derive(Default, Deserialize)]
struct RequestQuery {
    app: Option<String>,
}
async fn requests(
    State(state): State<AppState>,
    tower: TowerSession,
    Query(q): Query<RequestQuery>,
) -> crate::Result<Response> {
    let selected = match q.app.as_deref() {
        None => None,
        Some(value) => Some(installed_apps::app(value).ok_or(WebError::NotFound)?),
    };
    let path = selected.map_or_else(
        || "/onboarding/requests".to_owned(),
        |app| format!("/onboarding/requests?app={}", app.as_str()),
    );
    let session = load(&tower).await?;
    if !session.is_authenticated() {
        return Ok(signin(&path));
    }
    let (requests, unavailable) = pending(&state, selected, Some(session.user_id), None).await;
    Ok(page(
        None,
        session,
        None,
        vec![],
        requests,
        unavailable,
        path,
    ))
}
async fn account_installations(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
) -> crate::Result<Response> {
    if admin.authority.role() == crate::authority::Role::Delegated {
        return Err(WebError::NotFound);
    }
    let mut apps = vec![];
    for app in [App::Repo, App::Org] {
        let installation = state
            .storage
            .active_app_installation(app, admin.account.account_id)
            .await?;
        let status = match installation {
            Some(value) if value.suspended_at.is_some() => "Suspended",
            Some(_) => "Installed",
            None => "Not installed",
        };
        apps.push(AppStatus {
            name: label(app).into(),
            status: status.into(),
            install_path: if app == App::Repo {
                "/install"
            } else {
                "/install/org"
            }
            .into(),
            available: available(&state, app),
        });
    }
    let (requests, unavailable) = pending(&state, None, None, Some(admin.account.account_id)).await;
    let path = format!(
        "/console/accounts/{}/installations",
        admin.account.account_login
    );
    Ok(page(
        Some(admin.render_context()),
        admin.session,
        Some(admin.account.account_login),
        apps,
        requests,
        unavailable,
        path,
    ))
}
fn label(app: App) -> &'static str {
    match app {
        App::Repo => "Repo app",
        App::Org => "Org app",
    }
}
async fn pending(
    state: &AppState,
    selected: Option<App>,
    requester: Option<u64>,
    account: Option<u64>,
) -> (Vec<PendingRequest>, bool) {
    let mut rows = vec![];
    let mut unavailable = false;
    for app in [App::Repo, App::Org] {
        if selected.is_some_and(|selected| selected != app) {
            continue;
        }
        // A configured app without a browser install URL can still be read.
        if installed_apps::configured(state, app).is_none() {
            continue;
        }
        match installed_apps::installation_requests(state, app).await {
            Ok(found) => {
                for value in found {
                    if requester.is_some_and(|id| value.requester.id != id)
                        || account.is_some_and(|id| value.account.id != id)
                    {
                        continue;
                    }
                    rows.push(PendingRequest {
                        app: label(app).into(),
                        account: value.account.login,
                        requested_at: value.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
                    });
                }
            }
            Err(_) => unavailable = true,
        }
    }
    rows.sort_by(|a, b| {
        (&a.account, &a.app, &a.requested_at).cmp(&(&b.account, &b.app, &b.requested_at))
    });
    (rows, unavailable)
}
fn page(
    context: Option<crate::render::RenderContext>,
    session: session::Session,
    account_login: Option<String>,
    apps: Vec<AppStatus>,
    requests: Vec<PendingRequest>,
    unavailable: bool,
    refresh_path: String,
) -> Response {
    let html = crate::render::render_with_csrf(
        context.unwrap_or_else(|| session.csrf_token.clone().into()),
        move || {
            rsx! {
                ghinvite_ui::onboarding::Requests {signed_in_login:session.login.clone(),account_login:account_login.clone(),apps:apps.clone(),requests:requests.clone(),unavailable,refresh_path:refresh_path.clone()}
            }
        },
    );
    (
        if unavailable {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        },
        Html(html),
    )
        .into_response()
}
