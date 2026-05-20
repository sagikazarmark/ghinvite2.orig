//! GitHub App Setup URL return handling.

use crate::commands::{
    OnboardInstallation, RecordRepositorySelectionChange, RepositorySelectionChangeSource,
};
use crate::error::{Result, WebError};
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::Utc;
use domain::{AccountType, SelectedRepos};
use github::oauth::UserApiClient;
use github::payloads::GhUserInstallation;
use serde::Deserialize;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new().route("/setup/github", get(handle_github_setup))
}

#[derive(Debug, Deserialize)]
struct SetupQuery {
    installation_id: Option<u64>,
    setup_action: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupAction {
    Install,
    Update,
}

impl SetupAction {
    fn as_query_value(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
        }
    }
}

fn normalize_setup_action(action: Option<&str>) -> SetupAction {
    match action {
        Some("update") => SetupAction::Update,
        _ => SetupAction::Install,
    }
}

fn setup_return_to(installation_id: u64, action: SetupAction) -> String {
    let mut path = format!("/setup/github?installation_id={installation_id}");
    path.push_str("&setup_action=");
    path.push_str(action.as_query_value());
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
        .ok_or_else(|| WebError::OAuth("installation is not visible to signed-in user".into()))?;

    let account_type = account_type_for(&installation)?;
    let selected_repos = selected_repos_for(&user_api, &installation).await?;
    let account_login = installation.account.login.clone();

    match action {
        SetupAction::Install => {
            state
                .commands
                .onboard_installation(OnboardInstallation {
                    installation_id: installation.id,
                    actor_user_id: session.user_id,
                    account_id: installation.account.id,
                    account_login: account_login.clone(),
                    account_type,
                    selected_repos,
                    installed_at: Utc::now(),
                })
                .await?;
        }
        SetupAction::Update => {
            state
                .commands
                .record_repository_selection_change(RecordRepositorySelectionChange {
                    installation_id: installation.id,
                    selected_repos,
                    source: RepositorySelectionChangeSource::SetupReturn,
                })
                .await?;
        }
    }

    Ok(Redirect::to(&format!("/accounts/{account_login}")).into_response())
}

fn account_type_for(installation: &GhUserInstallation) -> Result<AccountType> {
    let raw = installation
        .account
        .account_type
        .as_deref()
        .unwrap_or(installation.target_type.as_str());
    raw.parse::<AccountType>()
        .map_err(|_| WebError::BadRequest(format!("unsupported installation account type: {raw}")))
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
        other => Err(WebError::BadRequest(format!(
            "unsupported repository_selection: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_return_to_preserves_known_action() {
        assert_eq!(
            setup_return_to(77, SetupAction::Install),
            "/setup/github?installation_id=77&setup_action=install"
        );
        assert_eq!(
            setup_return_to(77, SetupAction::Update),
            "/setup/github?installation_id=77&setup_action=update"
        );
    }

    #[test]
    fn normalize_setup_action_defaults_unknown_values_to_install() {
        assert_eq!(
            normalize_setup_action(Some("install")),
            SetupAction::Install
        );
        assert_eq!(normalize_setup_action(Some("update")), SetupAction::Update);
        assert_eq!(
            normalize_setup_action(Some("install&return_to=//evil.test")),
            SetupAction::Install
        );
        assert_eq!(normalize_setup_action(None), SetupAction::Install);
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
