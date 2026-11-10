//! [`Installation`]: one installation of an [`App`] and its operations.

use crate::api::ApiError;
use crate::app::App;
use crate::error::{Error, ForbiddenReason, TransientKind};
use crate::id::InstallationId;
use crate::permissions::Permissions;
use crate::transport::Response;
use http::StatusCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod info;
mod token;

pub use info::{AccountKind, InstallationAccount, InstallationInfo, RepositorySelection};
pub use token::{InstallationToken, TokenId, TokenParts, TokenScope};

/// One installation of an [`App`]. Owns a clone of the `App`, so it can be
/// stored or moved into a task.
#[derive(Clone)]
pub struct Installation {
    app: App,
    id: InstallationId,
}

impl Installation {
    pub(crate) fn new(app: App, id: InstallationId) -> Self {
        Self { app, id }
    }

    /// The installation ID.
    pub fn id(&self) -> InstallationId {
        self.id
    }

    /// The app this installation belongs to.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Create an installation access token narrowed to `permissions`, for every
    /// repository the installation can access.
    ///
    /// One `POST /app/installations/{id}/access_tokens`. It is retried only
    /// after a 401 caused by the app's credential being invalid at GitHub's
    /// time (see [`Error::AppCredentialsRejected`]); a plain 403 is followed
    /// by one `GET /app/installations/{id}` to check for suspension (with the
    /// same 401 handling). At most four requests; never retried otherwise:
    ///
    /// | Response | Error |
    /// |---|---|
    /// | rate-limit evidence on 403/429 | [`Error::RateLimited`] |
    /// | 401 | [`Error::AppCredentialsRejected`] |
    /// | 403, and the installation's `suspended_at` is set | [`Error::InstallationSuspended`] |
    /// | other 403 | [`Error::Forbidden`] |
    /// | 404 | [`Error::InstallationGone`] |
    /// | 422 | [`Error::PermissionsNotGranted`] |
    /// | 5xx, transport failure | [`Error::Transient`] |
    /// | other status (incl. 3xx) | [`Error::Unexpected`] |
    /// | malformed 201, body over 10 MiB | [`Error::Decode`] |
    /// | a custom client followed a redirect | [`Error::Redirected`] |
    /// | local signing failed (nothing sent) | [`Error::Signing`] |
    pub async fn access_token(
        &self,
        permissions: &Permissions,
    ) -> Result<InstallationToken, Error> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            permissions: &'a Permissions,
        }
        let body = serde_json::to_vec(&Body { permissions })
            .expect("a map of string keys and string values serializes");
        let id = self.id.to_string();
        let response = self
            .app
            .call(
                reqwest::Method::POST,
                &["app", "installations", &id, "access_tokens"],
                Some(body),
            )
            .await?;
        if response.status == 201 {
            return InstallationToken::from_response(
                response,
                self.app.id(),
                self.id,
                TokenScope::new(permissions.clone()),
            );
        }
        let error = status_error(&response);
        // A plain 403 may mean "suspended". Any failure of the follow-up read
        // means "not confirmed", so the caller sees the original 403.
        if matches!(error, Error::Forbidden { .. }) && matches!(self.is_suspended().await, Ok(true))
        {
            return Err(Error::InstallationSuspended);
        }
        Err(error)
    }

    /// The installation as GitHub reports it: one `GET /app/installations/{id}`,
    /// authenticated as the app, with the same 401 handling as
    /// [`access_token`](Self::access_token):
    ///
    /// | Response | Error |
    /// |---|---|
    /// | rate-limit evidence on 403/429 | [`Error::RateLimited`] |
    /// | 401 | [`Error::AppCredentialsRejected`] |
    /// | other 403 | [`Error::Forbidden`] |
    /// | 404 | [`Error::InstallationGone`] |
    /// | 5xx, transport failure | [`Error::Transient`] |
    /// | other status (incl. 3xx) | [`Error::Unexpected`] |
    /// | malformed 200, body over 10 MiB, an `id` or `app_id` other than this installation's | [`Error::Decode`] |
    /// | a custom client followed a redirect | [`Error::Redirected`] |
    /// | local signing failed (nothing sent) | [`Error::Signing`] |
    pub async fn get(&self) -> Result<InstallationInfo, Error> {
        let id = self.id.to_string();
        let response = self
            .app
            .call(reqwest::Method::GET, &["app", "installations", &id], None)
            .await?;
        if response.status == 200 {
            InstallationInfo::from_response(&response, self.app.id(), self.id)
        } else {
            Err(status_error(&response))
        }
    }

    /// Whether GitHub reports this installation as suspended: whether
    /// [`get`](Self::get)'s `suspended_at` is set. Errors as for `get`.
    pub async fn is_suspended(&self) -> Result<bool, Error> {
        self.get().await.map(|info| info.suspended_at.is_some())
    }
}

impl std::fmt::Debug for Installation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installation")
            .field("app", &self.app.id())
            .field("id", &self.id)
            .finish()
    }
}

/// The error for a response that is not the endpoint's success status: the
/// generic classification, with these endpoints' meaning of 404 and 422.
fn status_error(response: &Response) -> Error {
    match response.api_error() {
        ApiError::RateLimited { scope, retry_after } => Error::RateLimited { scope, retry_after },
        // `App::call` turns every 401 into this error first; kept so the
        // mapping is total.
        ApiError::Unauthorized => Error::AppCredentialsRejected { clock_skew: None },
        ApiError::Forbidden { reason } => Error::Forbidden { reason },
        ApiError::NotFound => Error::InstallationGone,
        ApiError::Validation { .. } => Error::PermissionsNotGranted,
        // `spammed` is no documented answer of these endpoints: read it as
        // its status.
        ApiError::Spammed if response.status == StatusCode::UNPROCESSABLE_ENTITY => {
            Error::PermissionsNotGranted
        }
        ApiError::Spammed => Error::Forbidden {
            reason: ForbiddenReason::Other,
        },
        ApiError::Server { status } => Error::Transient {
            kind: TransientKind::Server { status },
        },
        _ => Error::Unexpected {
            status: response.status.as_u16(),
        },
    }
}

/// `UNIX_EPOCH + seconds`, if non-negative and representable.
fn unix_time(seconds: i64) -> Option<SystemTime> {
    u64::try_from(seconds)
        .ok()
        .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)))
}

#[cfg(test)]
mod tests;
