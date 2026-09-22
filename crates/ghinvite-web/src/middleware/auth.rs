//! Session-validation tower layer + admin recheck cache.

use crate::error::WebError;
use crate::session::{AdminCheck, Session};
use crate::state::AppState;
use axum::extract::FromRef;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{Html, IntoResponse, Response};
use chrono::{Duration, Utc};
use dioxus::prelude::*;
use ghinvite_github::oauth::UserApiClient;

const ADMIN_CACHE_TTL_SECS: i64 = 60;

/// Check the signed-in user's authority over an installed account.
/// Personal ownership is bound to the authenticated GitHub user ID.
/// Organization membership checks are cached in the session for 60 seconds.
///
/// Returns `Ok(true)` if admin, `Ok(false)` if not, `Err` when GitHub could
/// not answer the membership check.
pub async fn check_admin(
    state: &AppState,
    session: &mut Session,
    account: &ghinvite_core::Account,
) -> Result<bool, ghinvite_github::Error> {
    if account.account_type == ghinvite_core::AccountType::User {
        return Ok(session.user_id == account.account_id);
    }
    let cache_key = format!("{}:{}", session.user_id, account.account_id);
    let now = Utc::now();
    if let Some(cached) = session.admin_checks.get(&cache_key)
        && cached.checked_at <= now
        && now - cached.checked_at < Duration::seconds(ADMIN_CACHE_TTL_SECS)
    {
        return Ok(cached.is_admin);
    }
    let user_api = UserApiClient::new(state.github_transport.clone(), session.access_token.clone());
    let is_admin = user_api
        .get_org_membership(&account.account_login)
        .await?
        .is_active_admin_of(account.account_id);
    session.admin_checks.insert(
        cache_key,
        AdminCheck {
            is_admin,
            checked_at: Utc::now(),
        },
    );
    Ok(is_admin)
}

/// Extractor for routes nested under `/console/accounts/{login}/...`. Loads the
/// session, resolves `:login` → `ghinvite_core::Account` via storage, and runs the
/// admin recheck (60s cache per `crate::session::AdminCheck`). On any
/// concealment path — no account history or not admin — surfaces
/// as a generic public 404. Unauthenticated console requests redirect to login
/// before account authorization checks run.
///
/// The handler MUST save the session after any mutation via
/// `session::save(&tower, &session)` to persist cache updates.
pub struct RequireConsoleAdminOf {
    pub session: Session,
    pub account: ghinvite_core::Account,
    pub tower: tower_sessions::Session,
}

/// Why [`RequireConsoleAdminOf`] refused a request.
///
/// Routes that recover from one cause — link creation keeps the submitted
/// form when GitHub cannot verify access — match the variant; every other
/// route lets axum render it.
pub enum ConsoleAdminRejection {
    /// Nobody is signed in. Sign in, then come back to `return_to`.
    SignIn { return_to: String },
    /// The account has no history under this login, or the signed-in user is
    /// not its admin. Both render the same generic public 404, so the answer
    /// does not say whether the account exists.
    Concealed {
        signed_in_login: String,
        csrf_token: Option<String>,
    },
    /// GitHub could not answer the membership check, so access is neither
    /// granted nor denied.
    AdminCheckUnavailable(ghinvite_github::Error),
    /// The account could not be read from storage.
    Storage(ghinvite_core::storage::Error),
    /// The browser session could not be loaded or saved.
    Session(String),
    /// The route's path did not carry a usable `:login`.
    MalformedPath(String),
}

impl IntoResponse for ConsoleAdminRejection {
    fn into_response(self) -> Response {
        match self {
            Self::SignIn { return_to } => {
                let encoded: String =
                    url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
                axum::response::Redirect::to(&format!("/login?return_to={encoded}")).into_response()
            }
            Self::Concealed {
                signed_in_login,
                csrf_token,
            } => {
                let signed_in_login = Some(signed_in_login);
                let html = crate::render::render_with_csrf(csrf_token, move || {
                    rsx! {
                        ghinvite_ui::not_found::PublicNotFoundPage {
                            signed_in_login: signed_in_login.clone(),
                        }
                    }
                });
                (axum::http::StatusCode::NOT_FOUND, Html(html)).into_response()
            }
            Self::AdminCheckUnavailable(error) => WebError::Github(error).into_response(),
            Self::Storage(error) => WebError::Storage(error).into_response(),
            Self::Session(message) => WebError::Session(message).into_response(),
            Self::MalformedPath(message) => WebError::BadRequest(message).into_response(),
        }
    }
}

impl<S> FromRequestParts<S> for RequireConsoleAdminOf
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ConsoleAdminRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        outer_state: &S,
    ) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(outer_state);

        let tower: tower_sessions::Session = parts
            .extensions
            .get::<tower_sessions::Session>()
            .cloned()
            .ok_or_else(|| {
                ConsoleAdminRejection::Session("no tower session in request extensions".into())
            })?;

        let mut session = crate::session::load(&tower)
            .await
            .map_err(|e| ConsoleAdminRejection::Session(e.to_string()))?;

        if !session.is_authenticated() {
            let return_to = parts
                .uri
                .path_and_query()
                .map(|value| value.as_str())
                .unwrap_or("/console")
                .to_owned();
            return Err(ConsoleAdminRejection::SignIn { return_to });
        }

        let axum::extract::Path(params): axum::extract::Path<
            std::collections::HashMap<String, String>,
        > = axum::extract::Path::from_request_parts(parts, outer_state)
            .await
            .map_err(|e| ConsoleAdminRejection::MalformedPath(format!("path: {e}")))?;
        let login = params
            .get("login")
            .ok_or_else(|| {
                ConsoleAdminRejection::MalformedPath("missing :login path param".into())
            })?
            .clone();

        let concealed = |session: &Session| ConsoleAdminRejection::Concealed {
            signed_in_login: session.login.clone(),
            csrf_token: session.csrf_token.clone(),
        };

        let account = match state.storage.get_active_installation_by_login(&login).await {
            Ok(Some(account)) => account,
            // An uninstalled account keeps its history readable to its admins.
            Ok(None) => match state.storage.get_latest_installation_by_login(&login).await {
                Ok(Some(account)) => account,
                Ok(None) => return Err(concealed(&session)),
                Err(error) => return Err(ConsoleAdminRejection::Storage(error)),
            },
            Err(error) => return Err(ConsoleAdminRejection::Storage(error)),
        };

        let is_admin = check_admin(&state, &mut session, &account)
            .await
            .map_err(ConsoleAdminRejection::AdminCheckUnavailable)?;
        if !is_admin {
            return Err(concealed(&session));
        }

        crate::session::save(&tower, &session)
            .await
            .map_err(|e| ConsoleAdminRejection::Session(e.to_string()))?;

        Ok(RequireConsoleAdminOf {
            session,
            account,
            tower,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;

    async fn body_text(response: Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn sign_in_returns_to_the_requested_console_page() {
        let response = ConsoleAdminRejection::SignIn {
            return_to: "/console/accounts/acme/links?filter=active".into(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            "/login?return_to=%2Fconsole%2Faccounts%2Facme%2Flinks%3Ffilter%3Dactive"
        );
    }

    /// Concealment is the public 404 in signed-in chrome, never the Console's
    /// own not-found page: that one would name the account.
    #[tokio::test]
    async fn concealed_renders_the_public_not_found_page() {
        let response = ConsoleAdminRejection::Concealed {
            signed_in_login: "octocat".into(),
            csrf_token: Some("token".into()),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let html = body_text(response).await;
        assert!(html.contains("octocat"), "{html}");
        assert!(!html.contains("/console/accounts/"), "{html}");
    }

    #[tokio::test]
    async fn an_unavailable_admin_check_keeps_the_github_dependency_status() {
        for (error, expected) in [
            (
                ghinvite_github::Error::Status {
                    status: 504,
                    body: "private".into(),
                },
                StatusCode::GATEWAY_TIMEOUT,
            ),
            (
                ghinvite_github::Error::Transport("private".into()),
                StatusCode::BAD_GATEWAY,
            ),
        ] {
            let response = ConsoleAdminRejection::AdminCheckUnavailable(error).into_response();
            assert_eq!(response.status(), expected);
            assert!(!body_text(response).await.contains("private"));
        }
    }

    #[tokio::test]
    async fn storage_session_and_path_failures_are_not_dependency_failures() {
        for (rejection, expected) in [
            (
                ConsoleAdminRejection::Storage(ghinvite_core::storage::Error::Database(
                    "private".into(),
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                ConsoleAdminRejection::Session("private".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                ConsoleAdminRejection::MalformedPath("missing :login path param".into()),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            assert_eq!(rejection.into_response().status(), expected);
        }
    }
}
