//! [`with_token`]: one GitHub call with a provider's token, retried once with
//! a replacement after a 401.

use crate::{InstallationTokenProvider, TokenError};
use octoauth::{InstallationId, InstallationToken, TokenScope};
use std::future::Future;

/// Run `call` with a token for `installation` and `scope`; if it fails with
/// an error `is_unauthorized` accepts (GitHub answered 401), replace the
/// token and run `call` once more.
///
/// This is the one correct way to recover from a 401 with a provider: asking
/// [`token`](InstallationTokenProvider::token) again would hand back the same
/// cached token GitHub just rejected, while
/// [`replace`](InstallationTokenProvider::replace) returns a newer one (and
/// concurrent callers replacing the same token share one replacement). A
/// second 401 is returned as [`WithTokenError::Call`]; any other error is
/// returned without retrying, and nothing ever sleeps.
///
/// `call` must make **one GitHub request**, or only requests that are safe to
/// repeat: a 401 on its third request re-runs the first two. Re-running the
/// request GitHub answered with 401 is safe, since GitHub did not act on it.
///
/// Inside a Restate `ctx.run` (or any durable step), call this inside the step
/// and return only the outcome, never the token (ADR 0003).
///
/// ```no_run
/// use octotoken::{
///     Access, ApiError, ErrorBody, InstallationId, InstallationTokenProvider,
///     OrganizationPermission, Permissions, TokenScope, WithTokenError, with_token,
/// };
///
/// #[derive(Debug)]
/// enum CallError {
///     Api(ApiError),
///     Transport,
/// }
///
/// async fn invitations(
///     tokens: &impl InstallationTokenProvider,
///     client: &reqwest::Client,
///     scope: &TokenScope,
/// ) -> Result<Vec<u8>, WithTokenError<CallError>> {
///     let unauthorized = |e: &CallError| matches!(e, CallError::Api(e) if e.is_unauthorized());
///     with_token(tokens, InstallationId::new(42), scope, unauthorized, |token| async move {
///         let response = client
///             .get("https://api.github.com/orgs/acme/invitations")
///             .bearer_auth(token.expose_secret())
///             .send()
///             .await
///             .map_err(|_| CallError::Transport)?;
///         let (status, headers) = (response.status(), response.headers().clone());
///         let body = response.bytes().await.map_err(|_| CallError::Transport)?;
///         if !status.is_success() {
///             let error = ApiError::classify(status, &headers, ErrorBody::Bytes(&body));
///             return Err(CallError::Api(error));
///         }
///         Ok(body.to_vec())
///     })
///     .await
/// }
/// ```
pub async fn with_token<P, T, E, F, Fut>(
    provider: &P,
    installation: InstallationId,
    scope: &TokenScope,
    is_unauthorized: impl Fn(&E) -> bool,
    mut call: F,
) -> Result<T, WithTokenError<E>>
where
    P: InstallationTokenProvider + ?Sized,
    F: FnMut(InstallationToken) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let token = provider
        .token(installation, scope)
        .await
        .map_err(WithTokenError::Token)?;
    match call(token.clone()).await {
        Err(error) if is_unauthorized(&error) => {}
        result => return result.map_err(WithTokenError::Call),
    }
    let token = provider
        .replace(&token)
        .await
        .map_err(WithTokenError::Token)?;
    call(token).await.map_err(WithTokenError::Call)
}

/// Why [`with_token`] failed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum WithTokenError<E> {
    /// The provider could not supply a token (or a replacement); `call` did
    /// not run (again).
    #[error(transparent)]
    Token(TokenError),

    /// `call` failed: any error other than a 401, or a 401 with the
    /// replacement too. Returned as `call` produced it; if it carries
    /// upstream text, convert it before logging or journaling.
    #[error(transparent)]
    Call(E),
}
