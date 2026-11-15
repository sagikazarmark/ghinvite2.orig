//! `tracing` events (feature `tracing`); without the feature these functions
//! do nothing.
//!
//! Target `octotoken`; field `event` names the event. Other fields are IDs
//! and stable error codes only, never a token. A token's
//! [`TokenId`](octoauth::TokenId) is a digest, safe to log.

use crate::TokenError;
use octoauth::{AppId, InstallationId, InstallationToken};

/// After creating a token for the cache: `info` `token_created` (a miss or an
/// expiring token) or `token_replaced` (after
/// [`replace`](crate::InstallationTokenProvider::replace), with the rejected
/// token's ID), or `warn` `token_failed` with the error's code.
pub(crate) fn created(
    app: AppId,
    installation: InstallationId,
    rejected: Option<&InstallationToken>,
    result: &Result<InstallationToken, TokenError>,
) {
    #[cfg(feature = "tracing")]
    match (result, rejected) {
        (Ok(token), None) => tracing::info!(
            target: "octotoken",
            event = "token_created",
            app_id = app.get(),
            installation_id = installation.get(),
            token_id = %token.id(),
        ),
        (Ok(token), Some(rejected)) => tracing::info!(
            target: "octotoken",
            event = "token_replaced",
            app_id = app.get(),
            installation_id = installation.get(),
            token_id = %token.id(),
            rejected_token_id = %rejected.id(),
        ),
        (Err(error), rejected) => failed(app, installation, rejected, error),
    }
    #[cfg(not(feature = "tracing"))]
    let _ = (app, installation, rejected, result);
}

/// `warn` `token_failed`: no token, with the error's code and whether it was
/// a replacement.
pub(crate) fn failed(
    app: AppId,
    installation: InstallationId,
    rejected: Option<&InstallationToken>,
    error: &TokenError,
) {
    #[cfg(feature = "tracing")]
    tracing::warn!(
        target: "octotoken",
        event = "token_failed",
        app_id = app.get(),
        installation_id = installation.get(),
        replacing = rejected.is_some(),
        code = error.code(),
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (app, installation, rejected, error);
}
