//! Session validation and installation-derived Console authority.

use crate::error::WebError;
use crate::session::Session;
use crate::state::AppState;
use axum::extract::FromRef;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{Html, IntoResponse, Response};
use dioxus::prelude::*;
/// Open installation-derived authority for one request. No session OAuth token
/// or session-local role cache participates in Console authorization.
pub async fn account_authority(
    state: &AppState,
    session: &Session,
    account: &ghinvite_core::Account,
) -> Result<Option<crate::authority::AuthorityView>, crate::authority::AuthorityError> {
    crate::authority::AuthorityView::open(state, session.user_id, &session.login, account).await
}

/// Extractor for routes nested under `/console/accounts/{login}/...`. Loads the
/// session, resolves `:login` → `ghinvite_core::Account` via storage, and runs the
/// installation-derived authority check. On any
/// concealment path — no account history or not admin — surfaces
/// as a generic public 404. Unauthenticated console requests redirect to login
/// before account authorization checks run.
///
/// Authority evidence lives in the session-independent cache, never this session.
pub struct RequireConsoleAdminOf {
    pub authority: crate::authority::AuthorityView,
    pub session: Session,
    pub account: ghinvite_core::Account,
    pub tower: tower_sessions::Session,
    pending_requests: Option<u64>,
}

impl RequireConsoleAdminOf {
    /// Per-render facts from this request's authorized numeric account. This is
    /// never written to a session or reused as authorization evidence.
    pub fn render_context(&self) -> crate::render::RenderContext {
        use ghinvite_core::account::App;
        use ghinvite_ui::layouts::{ConsoleApps, InstalledAppState};
        let status = |app| match self.authority.installation_status(app) {
            None => InstalledAppState::NotInstalled,
            Some(true) => InstalledAppState::Suspended,
            Some(false) => InstalledAppState::Installed,
        };
        let owner = self.authority.is_owner();
        crate::render::RenderContext {
            csrf_token: self.session.csrf_token.clone(),
            pending_requests: Some(ghinvite_ui::layouts::ConsolePendingRequests {
                account_login: self.account.account_login.clone(),
                count: self.pending_requests,
            }),
            console_apps: Some(ConsoleApps {
                account_login: self.account.account_login.clone(),
                repository: status(App::Repo),
                organization: owner.then(|| status(App::Org)),
                can_manage: owner,
            }),
        }
    }
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
    AuthorityUnavailable(crate::authority::AuthorityError),
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
            Self::AuthorityUnavailable(error) => authority_unavailable(error),
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

        let session = crate::session::load(&tower)
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

        // The Console belongs to a numeric account, not specifically to the
        // repository app. Prefer current app-scoped installations; keep history
        // addressable after uninstall, without guessing across conflicting IDs.
        let mut repo = state
            .storage
            .active_app_installation_by_login(ghinvite_core::account::App::Repo, &login)
            .await
            .map_err(ConsoleAdminRejection::Storage)?;
        let mut org = state
            .storage
            .active_app_installation_by_login(ghinvite_core::account::App::Org, &login)
            .await
            .map_err(ConsoleAdminRejection::Storage)?;
        if repo.is_none()
            && let Some(current_org) = &org
        {
            // Keep repository-uninstall history visible while an org app still
            // supplies current owner authority. An unrelated historical login
            // cannot replace the active numeric account identity.
            let historical_repo = state
                .storage
                .latest_app_installation_by_login(ghinvite_core::account::App::Repo, &login)
                .await
                .map_err(ConsoleAdminRejection::Storage)?;
            repo = historical_repo.filter(|history| {
                history.account.account_id == current_org.account.account_id
                    && history.account.account_type == current_org.account.account_type
            });
        }
        if repo.is_none() && org.is_none() {
            repo = state
                .storage
                .latest_app_installation_by_login(ghinvite_core::account::App::Repo, &login)
                .await
                .map_err(ConsoleAdminRejection::Storage)?;
            org = state
                .storage
                .latest_app_installation_by_login(ghinvite_core::account::App::Org, &login)
                .await
                .map_err(ConsoleAdminRejection::Storage)?;
        }
        if let (Some(repo), Some(org)) = (&repo, &org)
            && (repo.account.account_id != org.account.account_id
                || repo.account.account_type != org.account.account_type)
        {
            return Err(ConsoleAdminRejection::AuthorityUnavailable(
                crate::authority::AuthorityError::Unavailable,
            ));
        }
        let account = repo
            .or(org)
            .map(|installation| installation.account)
            .ok_or_else(|| concealed(&session))?;

        let authority = account_authority(&state, &session, &account)
            .await
            .map_err(ConsoleAdminRejection::AuthorityUnavailable)?
            .ok_or_else(|| concealed(&session))?;
        // Search-first discovery exposes only installation repository choices.
        // Creation proves every selected target before any mutation. Other
        // account pages require some checked scope before exposing contents.
        let discovery = parts.uri.path().ends_with("/links/new")
            || parts.uri.path().ends_with("/links/new/selection")
            || parts.uri.path().ends_with("/bulk")
            || parts.uri.path().ends_with("/bulk/new")
            || (parts.method == axum::http::Method::POST
                && parts.uri.path().contains("/bulk/")
                && parts.uri.path().ends_with("/resolve"))
            || (parts.method == axum::http::Method::POST && parts.uri.path().ends_with("/links"));
        let navigation_scope =
            if authority.role() == crate::authority::Role::Delegated && !discovery {
                let scope = authority
                    .console_scope()
                    .await
                    .map_err(ConsoleAdminRejection::AuthorityUnavailable)?;
                if scope.authorized_repositories == 0 {
                    if scope.pending {
                        return Err(ConsoleAdminRejection::AuthorityUnavailable(
                            crate::authority::AuthorityError::Busy,
                        ));
                    }
                    return Err(concealed(&session));
                }
                Some(scope)
            } else if authority.is_owner() {
                authority.console_scope().await.ok()
            } else {
                // Search-first discovery never spends repository read budget on a
                // navigation count. Unknown is distinct from an empty queue.
                None
            };
        let pending_requests = match navigation_scope {
            Some(scope) if !scope.pending => state
                .storage
                .scoped_count_pending_requests_for_account(&scope.scope)
                .await
                .ok(),
            _ => None,
        };

        Ok(RequireConsoleAdminOf {
            pending_requests,
            authority,
            session,
            account,
            tower,
        })
    }
}

/// Dependency failures are not negative authority and do not disclose details.
pub fn authority_unavailable(error: crate::authority::AuthorityError) -> Response {
    let message = match error {
        crate::authority::AuthorityError::PendingSetup => "Finishing setup. Try again shortly.",
        _ => "GitHub access could not be checked. Try again in a minute.",
    };
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        [
            (axum::http::header::RETRY_AFTER, "60"),
            (axum::http::header::CACHE_CONTROL, "private, no-store"),
        ],
        message,
    )
        .into_response()
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
    async fn unavailable_authority_is_not_a_denial_or_session_role() {
        for error in [
            crate::authority::AuthorityError::Unavailable,
            crate::authority::AuthorityError::Busy,
            crate::authority::AuthorityError::Configuration,
            crate::authority::AuthorityError::PendingSetup,
        ] {
            let response = ConsoleAdminRejection::AuthorityUnavailable(error).into_response();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()["retry-after"], "60");
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
