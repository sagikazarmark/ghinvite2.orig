//! Installed-app verification. No user OAuth credential enters this boundary.
use crate::state::AppState;
use axum::http::StatusCode;
use ghinvite_core::account::{App, InstallationFacts, InstallationPermission};
use ghinvite_github::{AppSlot, GithubApp, payloads::GhAppInstallation};

/// The app named `value` in routes and configuration.
pub(crate) fn app(value: &str) -> Option<App> {
    match value {
        "repo" => Some(App::Repo),
        "org" => Some(App::Org),
        _ => None,
    }
}

/// The installed app's `ghinvite-github` slot.
pub(crate) fn slot(app: App) -> AppSlot {
    match app {
        App::Repo => AppSlot::Repo,
        App::Org => AppSlot::Org,
    }
}

/// The configured app, if this deployment has it.
pub(crate) fn configured(state: &AppState, app: App) -> Option<GithubApp> {
    state.github_apps.app(slot(app)).ok()
}

pub(crate) fn facts(
    app: App,
    expected_app_id: u64,
    expected_installation_id: u64,
    installation: GhAppInstallation,
) -> Result<InstallationFacts, StatusCode> {
    if installation.id != expected_installation_id || installation.app_id != expected_app_id {
        return Err(StatusCode::NOT_FOUND);
    }
    ghinvite_core::account::InstallationRef {
        app,
        installation_id: installation.id,
    }
    .key()
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    ghinvite_core::account::AccountInstallationRef {
        app,
        account_id: installation.account.id,
    }
    .key()
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    let account_type = installation
        .account
        .account_type
        .as_deref()
        .and_then(|kind| kind.parse().ok())
        .ok_or(StatusCode::BAD_GATEWAY)?;
    Ok(InstallationFacts {
        app,
        account: ghinvite_core::Account {
            installation_id: installation.id,
            account_id: installation.account.id,
            account_login: installation.account.login,
            account_type,
            installed_at: installation.created_at,
            uninstalled_at: None,
            // Selection does not prove the list of reachable numeric repos.
            // The authority performs a complete installation-token refresh.
            selected_repos: ghinvite_core::SelectedRepos::Subset(vec![]),
        },
        suspended_at: installation.suspended_at,
        permissions: installation
            .permissions
            .into_iter()
            .map(|(name, permission)| {
                (
                    name,
                    match permission {
                        ghinvite_github::payloads::GhInstallationPermission::Read => {
                            InstallationPermission::Read
                        }
                        ghinvite_github::payloads::GhInstallationPermission::Write => {
                            InstallationPermission::Write
                        }
                    },
                )
            })
            .collect(),
    })
}

pub(crate) async fn verify(
    state: &AppState,
    app: App,
    installation_id: u64,
) -> Result<InstallationFacts, StatusCode> {
    let configured = configured(state, app).ok_or(StatusCode::NOT_FOUND)?;
    // One app-authenticated read through the shared transport; a 401 is
    // reported as unavailable, as before.
    let app_rest = configured
        .app_rest()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let installation =
        ghinvite_github::endpoints::get_installation(&app_rest.rest(), installation_id)
            .await
            .map_err(|error| {
                tracing::warn!(code = error.code(), "installation verification failed");
                match error {
                    ghinvite_github::ApiError::NotFound => StatusCode::NOT_FOUND,
                    _ => StatusCode::SERVICE_UNAVAILABLE,
                }
            })?;
    facts(app, configured.id().get(), installation_id, installation)
}

/// Complete app-scoped request listing. No raw upstream response enters a
/// session, retained command or error page. Callers must filter identities.
pub(crate) async fn installation_requests(
    state: &AppState,
    app: App,
) -> Result<Vec<ghinvite_github::endpoints::InstallationRequest>, StatusCode> {
    let configured = configured(state, app).ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let app_rest = configured
        .app_rest()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let values = ghinvite_github::endpoints::list_installation_requests(&app_rest.rest())
        .await
        .map_err(|error| {
            tracing::warn!(
                app = app.as_str(),
                code = error.code(),
                "installation requests unavailable"
            );
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    let mut ids = std::collections::BTreeSet::new();
    let numeric = |id| id > 0 && id <= ghinvite_core::storage::integer::MAX_EXACT;
    for value in &values {
        if !numeric(value.id)
            || !numeric(value.account.id)
            || !numeric(value.requester.id)
            || !ids.insert(value.id)
            || value.account.account_type.as_deref() != Some("Organization")
            || value.account.login.is_empty()
            || value.account.login.len() > 100
            || !value
                .account
                .login
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    }
    Ok(values)
}
