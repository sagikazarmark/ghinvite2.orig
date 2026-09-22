//! GitHub App Setup URL return handling.

use crate::commands::{SetupReturn, SetupReturnAction, handle_setup_return};
use crate::error::{OAuthFailure, Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::Utc;
use ghinvite_core::{AccountType, SelectedRepos};
use ghinvite_github::oauth::UserApiClient;
use ghinvite_github::payloads::GhUserInstallation;
use serde::Deserialize;
use tower_sessions::Session as TowerSession;

/// The `account.type` / `target_type` values GitHub documents for an
/// installation. Anything else is logged as unrecognised rather than echoed.
const ACCOUNT_TYPES: &[&str] = &["Bot", "Organization", "User"];

/// The `repository_selection` values GitHub documents for an installation.
const REPOSITORY_SELECTIONS: &[&str] = &["all", "selected"];

pub fn router() -> Router<AppState> {
    Router::new().route("/setup/github", get(handle_github_setup))
}

#[derive(Debug, Deserialize)]
struct SetupQuery {
    installation_id: Option<u64>,
    setup_action: Option<String>,
}

fn normalize_setup_action(action: Option<&str>) -> SetupReturnAction {
    match action {
        Some("update") => SetupReturnAction::Update,
        _ => SetupReturnAction::Install,
    }
}

fn setup_action_query_value(action: SetupReturnAction) -> &'static str {
    match action {
        SetupReturnAction::Install => "install",
        SetupReturnAction::Update => "update",
    }
}

fn setup_return_to(installation_id: u64, action: SetupReturnAction) -> String {
    let mut path = format!("/setup/github?installation_id={installation_id}");
    path.push_str("&setup_action=");
    path.push_str(setup_action_query_value(action));
    path
}

fn login_redirect(return_to: &str) -> Redirect {
    let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    Redirect::to(&format!("/login?return_to={encoded}"))
}

async fn handle_github_setup(
    State(state): State<AppState>,
    tower: TowerSession,
    Query(q): Query<SetupQuery>,
) -> Result<Response> {
    let installation_id = q
        .installation_id
        .ok_or_else(|| WebError::BadRequest("missing 'installation_id'".into()))?;
    let action = normalize_setup_action(q.setup_action.as_deref());
    let return_to = setup_return_to(installation_id, action);

    let session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    if !session.is_authenticated() {
        return Ok(login_redirect(&return_to).into_response());
    }

    let user_api = UserApiClient::new(state.github_transport.clone(), session.access_token.clone());
    let installations = user_api.list_user_installations().await?;
    let installation = installations
        .installations
        .into_iter()
        .find(|candidate| candidate.id == installation_id)
        .ok_or(WebError::OAuth(OAuthFailure::InstallationNotVisible))?;

    let account_type = account_type_for(&installation)?;
    let selected_repos = selected_repos_for(&user_api, &installation).await?;
    let account_login = installation.account.login.clone();

    handle_setup_return(
        &state.commands,
        SetupReturn {
            action,
            installation_id: installation.id,
            actor_user_id: session.user_id,
            account_id: installation.account.id,
            account_login: account_login.clone(),
            account_type,
            selected_repos,
            returned_at: Utc::now(),
        },
    )
    .await?;

    Ok(Redirect::to(&format!("/console/accounts/{account_login}")).into_response())
}

fn account_type_for(installation: &GhUserInstallation) -> Result<AccountType> {
    let raw = installation
        .account
        .account_type
        .as_deref()
        .unwrap_or(installation.target_type.as_str());
    raw.parse::<AccountType>().map_err(|_| {
        // `raw` is whatever GitHub put in the field. Bound it for the log and
        // keep it out of the error, which the browser renders.
        tracing::warn!(
            account_type = %ghinvite_github::bounded_upstream_code(raw, ACCOUNT_TYPES),
            "installation account type is not supported"
        );
        WebError::OAuth(OAuthFailure::UnsupportedInstallation {
            field: "account type",
        })
    })
}

async fn selected_repos_for(
    user_api: &UserApiClient,
    installation: &GhUserInstallation,
) -> Result<SelectedRepos> {
    match installation.repository_selection.as_str() {
        "all" => Ok(SelectedRepos::All),
        "selected" => {
            let repos = user_api
                .list_user_installation_repos(installation.id)
                .await?;
            Ok(SelectedRepos::Subset(
                repos.repositories.into_iter().map(|repo| repo.id).collect(),
            ))
        }
        other => {
            tracing::warn!(
                repository_selection = %ghinvite_github::bounded_upstream_code(other, REPOSITORY_SELECTIONS),
                "installation repository selection is not supported"
            );
            Err(WebError::OAuth(OAuthFailure::UnsupportedInstallation {
                field: "repository selection",
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_return_to_preserves_known_action() {
        assert_eq!(
            setup_return_to(77, SetupReturnAction::Install),
            "/setup/github?installation_id=77&setup_action=install"
        );
        assert_eq!(
            setup_return_to(77, SetupReturnAction::Update),
            "/setup/github?installation_id=77&setup_action=update"
        );
    }

    #[test]
    fn normalize_setup_action_defaults_unknown_values_to_install() {
        assert_eq!(
            normalize_setup_action(Some("install")),
            SetupReturnAction::Install
        );
        assert_eq!(
            normalize_setup_action(Some("update")),
            SetupReturnAction::Update
        );
        assert_eq!(
            normalize_setup_action(Some("install&return_to=//evil.test")),
            SetupReturnAction::Install
        );
        assert_eq!(normalize_setup_action(None), SetupReturnAction::Install);
    }

    #[test]
    fn login_redirect_url_encodes_return_to() {
        let redirect =
            login_redirect("/setup/github?installation_id=77&setup_action=install").into_response();
        let location = redirect
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            location,
            "/login?return_to=%2Fsetup%2Fgithub%3Finstallation_id%3D77%26setup_action%3Dinstall"
        );
    }
}
