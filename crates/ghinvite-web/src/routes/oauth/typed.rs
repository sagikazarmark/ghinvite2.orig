//! Token-free sign-in on octologin: PKCE, pending state in SQL/D1, and a
//! rotated session holding only the identity (and any domain evidence).
use super::*;
use crate::signin::{
    Callback, DomainContext, DomainRestriction, SignInError, SignInService,
    storage::SqlPendingSignInStore,
};

fn service(state: &AppState) -> Result<SignInService> {
    let app = state.config.signin_apps.as_ref().ok_or_else(unavailable)?;
    SignInService::new(app).map_err(error)
}
fn unavailable() -> WebError {
    WebError::Internal("Sign-in is unavailable. Try again later.".into())
}
fn error(error: SignInError) -> WebError {
    match error {
        SignInError::KeyringRetry => WebError::KeyringRetry,
        SignInError::NoPendingSignIn => WebError::OAuth(OAuthFailure::NoPendingSignIn),
        SignInError::InvalidState => WebError::OAuth(OAuthFailure::StateMismatch),
        SignInError::Declined => WebError::OAuth(OAuthFailure::from_callback("access_denied")),
        SignInError::Provider(code) => WebError::OAuth(OAuthFailure::from_callback(code.code())),
        _ => unavailable(),
    }
}
fn store(state: &AppState) -> SqlPendingSignInStore {
    SqlPendingSignInStore::new(state.storage.clone(), state.config.keyring.clone())
}
struct Context<'a> {
    authority: &'a crate::LinkAuthority,
    capability: Option<&'a session::PendingLink>,
}
#[async_trait::async_trait]
impl DomainContext for Context<'_> {
    async fn restriction(
        &self,
        user_id: u64,
    ) -> std::result::Result<Option<DomainRestriction>, SignInError> {
        let Some(capability) = self.capability else {
            return Ok(None);
        };
        let page = self
            .authority
            .requester_page(capability, user_id, None)
            .await
            .map_err(|_| SignInError::DomainUnavailable)?;
        if page.link_id != capability.link_id {
            return Err(SignInError::DomainUnavailable);
        }
        Ok(Some(DomainRestriction {
            link_id: page.link_id,
            rules: page.email_domains,
        }))
    }
}
pub(super) async fn login(
    state: &AppState,
    tower: &TowerSession,
    return_to: Option<&str>,
) -> Result<Redirect> {
    let run = login_inner(state, tower, return_to);
    // octologin's futures are not `Send` on wasm32 (reqwest's fetch backend);
    // a Worker isolate is single-threaded, so the wrapper's check never fires.
    #[cfg(target_arch = "wasm32")]
    let run = send_wrapper::SendWrapper::new(run);
    run.await
}
async fn login_inner(
    state: &AppState,
    tower: &TowerSession,
    return_to: Option<&str>,
) -> Result<Redirect> {
    let mut session = session::load(tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    session.domain_evidence = None;
    session.return_to = return_to.and_then(session::validate_return_to);
    session::save(tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    tower
        .save()
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    let id = tower.id().ok_or_else(unavailable)?.to_string();
    let url = service(state)?
        .begin(&store(state), &id)
        .await
        .map_err(error)?;
    Ok(Redirect::to(url.as_str()))
}
pub(super) async fn callback(
    state: &AppState,
    tower: &TowerSession,
    query: CallbackQuery,
) -> Result<Redirect> {
    let run = callback_inner(state, tower, query);
    // As in `login`: octologin's futures are not `Send` on wasm32.
    #[cfg(target_arch = "wasm32")]
    let run = send_wrapper::SendWrapper::new(run);
    run.await
}
async fn callback_inner(
    state: &AppState,
    tower: &TowerSession,
    query: CallbackQuery,
) -> Result<Redirect> {
    let mut session = session::load(tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    session.domain_evidence = None;
    session::save(tower, &session)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    // Middleware intentionally skips persistence on 5xx; retire prior evidence first.
    tower
        .save()
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    let id = tower.id().ok_or_else(unavailable)?.to_string();
    let supplied = query.state.unwrap_or_default();
    let callback = if let Some(error) = &query.error {
        Callback::denied(supplied, error)
    } else {
        Callback::code(supplied, query.code.unwrap_or_default())
    };
    let result = service(state)?
        .complete_scoped(
            &store(state),
            &id,
            callback,
            Some(&Context {
                authority: &state.link_authority,
                capability: session.pending_link.as_ref(),
            }),
        )
        .await
        .map_err(error)?;
    state
        .storage
        .upsert_user(&ghinvite_core::User {
            user_id: result.identity.github_user_id,
            login: result.identity.login.clone(),
            avatar_url: result.avatar_url,
            github_created_at: result.github_created_at,
            last_seen_at: Utc::now(),
        })
        .await?;
    tower
        .cycle_id()
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    let destination = session.return_to.take().unwrap_or_else(|| "/".to_owned());
    let pending_link = session.pending_link.take();
    let signin_starts = crate::rate_limit::signin_starts(tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    tower.clear().await;
    crate::rate_limit::restore_signin_starts(tower, &signin_starts)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    crate::session_store::establish_authenticated_lifetime(tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    session::save(
        tower,
        &session::Session {
            user_id: result.identity.github_user_id,
            login: result.identity.login,
            pending_link,
            domain_evidence: result.domain_evidence,
            csrf_token: Some(crate::middleware::csrf::generate_token()),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| WebError::Session(e.to_string()))?;
    Ok(Redirect::to(&destination))
}
