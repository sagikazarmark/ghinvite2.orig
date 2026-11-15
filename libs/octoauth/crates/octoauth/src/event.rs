//! `tracing` events (feature `tracing`); without the feature these functions
//! do nothing.
//!
//! Target `octoauth`; field `event` names the event. Other fields are IDs,
//! stable error codes and seconds only: never a token, a credential, key
//! material or upstream text. A token's [`TokenId`](crate::TokenId) is a
//! digest, safe to log.

use crate::error::Error;
use crate::id::{AppId, InstallationId};
use crate::installation::InstallationToken;

/// `debug`: a new app credential was signed (the cached one was too old,
/// or a rejection measured a clock correction).
pub(crate) fn credential_signed(app: AppId, correction: i64) {
    #[cfg(feature = "tracing")]
    tracing::debug!(
        target: "octoauth",
        event = "app_credential_signed",
        app_id = app.get(),
        correction,
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (app, correction);
}

/// `warn`: GitHub rejected the app credential because of a skewed clock;
/// the request is retried once with `correction`.
pub(crate) fn clock_skew_corrected(app: AppId, skew: i64, correction: i64) {
    #[cfg(feature = "tracing")]
    tracing::warn!(
        target: "octoauth",
        event = "clock_skew_corrected",
        app_id = app.get(),
        skew,
        correction,
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (app, skew, correction);
}

/// `info` `installation_token_created`, or `warn` `installation_token_failed`
/// with the error's code.
pub(crate) fn token_created(
    app: AppId,
    installation: InstallationId,
    result: &Result<InstallationToken, Error>,
) {
    #[cfg(feature = "tracing")]
    match result {
        Ok(token) => tracing::info!(
            target: "octoauth",
            event = "installation_token_created",
            app_id = app.get(),
            installation_id = installation.get(),
            token_id = %token.id(),
        ),
        Err(error) => tracing::warn!(
            target: "octoauth",
            event = "installation_token_failed",
            app_id = app.get(),
            installation_id = installation.get(),
            code = error.code(),
        ),
    }
    #[cfg(not(feature = "tracing"))]
    let _ = (app, installation, result);
}
