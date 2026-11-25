//! octocrab with octoauth's credentials.
//!
//! octoauth wraps no GitHub API client; this crate is the glue for apps that
//! use [octocrab](https://docs.rs/octocrab):
//!
//! - [`builder`]: octocrab's builder for the API an [`App`] uses (its base
//!   URL), with bounded connect and read times and **no auth**. Build one
//!   base client per process and derive a client per credential with
//!   [`authenticated`]; configuring auth on the base client and then deriving
//!   would send two `Authorization` headers.
//! - [`Error`]: `octocrab::Error` reduced to octoauth's [`ApiError`] (fixed
//!   codes, GitHub's rate-limit wait, the reason for a 403) or
//!   [`Error::Client`]. `octocrab::Error`'s `Debug` carries GitHub's message
//!   (or the raw body) and every response header; convert it before it
//!   reaches a log, a durable journal or a webhook handler's error.
//! - [`is_unauthorized`] and [`unauthorized`]: the 401 signals for
//!   octotoken's `with_token` (replace the installation token once) and
//!   [`App::with_credential`] (correct a skewed clock once).
//!
//! ```no_run
//! # async fn example(app: octoauth::App) -> Result<(), Box<dyn std::error::Error>> {
//! use octoauth::WithCredentialError;
//! use octoauth_octocrab::{Error, authenticated, builder, unauthorized};
//!
//! let base = builder(&app).map_err(Error::from)?.build().map_err(Error::from)?;
//! let installations = app
//!     .with_credential(unauthorized, |credential| {
//!         let base = &base;
//!         async move {
//!             let github = authenticated(base, credential.expose_secret())?;
//!             github.apps().installations().send().await
//!         }
//!     })
//!     .await;
//! match installations {
//!     Ok(page) => println!("{} installations", page.items.len()),
//!     // Fixed codes only: safe to log.
//!     Err(WithCredentialError::Call(error)) => eprintln!("{}", Error::from(error)),
//!     Err(WithCredentialError::Credential(error)) => eprintln!("{error}"),
//! }
//! # Ok(()) }
//! ```
//!
//! # octocrab features
//!
//! The crate enables only octocrab's `timeout`. The application picks the
//! rest; [`builder`]'s `build` needs `default-client` and a TLS feature
//! (e.g. `rustls` with `rustls-ring`). Leave two of octocrab's default
//! features off:
//!
//! - `retry` retries 5xx, 429 and transport errors immediately, for every
//!   method including POST; a retried write can be a duplicate write.
//! - `follow-redirect` follows redirects, which can re-send a credential or a
//!   write to another location.
//!
//! Cargo unifies features, so check that no other dependency turns them on.
//! octocrab buffers response bodies with no size limit; behind an untrusted
//! proxy, cap them in a custom service (`OctocrabBuilder::with_service`).
//! octocrab's `all_pages` follows `Link` URLs without checking their host;
//! page by number instead when the credential matters.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use octoauth::{ApiError, App, ErrorBody};
use octocrab::{
    DefaultOctocrabBuilderConfig, NoAuth, NoSvc, NotLayerReady, Octocrab, OctocrabBuilder,
};
use std::time::Duration;

pub use http::HeaderMap;

/// octocrab's default builder, as [`builder`] returns it.
pub type Builder = OctocrabBuilder<NoSvc, DefaultOctocrabBuilderConfig, NoAuth, NotLayerReady>;

/// The connect timeout [`builder`] sets.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The read timeout [`builder`] sets.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// octocrab's builder for the REST API `app` uses ([`App::base_url`]), with
/// [`CONNECT_TIMEOUT`] and [`READ_TIMEOUT`] and no auth. Fails only if
/// octocrab cannot parse the base URL.
///
/// The `User-Agent` stays octocrab's: octocrab always sends its own, and
/// another one added with `add_header` would be a second header.
pub fn builder(app: &App) -> octocrab::Result<Builder> {
    Octocrab::builder()
        .set_connect_timeout(Some(CONNECT_TIMEOUT))
        .set_read_timeout(Some(READ_TIMEOUT))
        .base_uri(app.base_url().trim_end_matches('/'))
}

/// A client sending `secret` as its bearer token: an installation token's
/// or the app's credential (`expose_secret` of either), derived from a base
/// client without auth. Cheap; derive one per request.
pub fn authenticated(base: &Octocrab, secret: &str) -> octocrab::Result<Octocrab> {
    base.user_access_token(secret.to_owned())
}

/// Why an octocrab call failed, without any upstream text: safe to log and
/// journal.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// GitHub answered with an error status.
    #[error(transparent)]
    Api(ApiError),

    /// No (complete) answer, or one octocrab could not decode or build. The
    /// details are octocrab's, which may quote the response or the URL, so
    /// they are dropped.
    #[error("gh:octocrab:client")]
    Client,
}

impl Error {
    /// The stable error code: [`ApiError::code`] or `gh:octocrab:client`.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Api(error) => error.code(),
            Self::Client => "gh:octocrab:client",
        }
    }

    /// Whether GitHub answered 401.
    pub const fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Api(error) if error.is_unauthorized())
    }
}

impl From<octocrab::Error> for Error {
    fn from(error: octocrab::Error) -> Self {
        Self::from(&error)
    }
}

impl From<&octocrab::Error> for Error {
    fn from(error: &octocrab::Error) -> Self {
        api_error(error).map_or(Self::Client, Self::Api)
    }
}

/// `error` classified by [`ApiError::classify`] when GitHub answered with an
/// error status, else `None`. When the body was not JSON, octocrab puts the
/// raw text in `message`, which then simply matches no template.
pub fn api_error(error: &octocrab::Error) -> Option<ApiError> {
    let octocrab::Error::GitHub { source, .. } = error else {
        return None;
    };
    let headers = source.headers.clone().unwrap_or_default();
    Some(ApiError::classify(
        source.status_code,
        &headers,
        ErrorBody::Parsed {
            message: Some(&source.message),
            errors: source.errors.as_deref().unwrap_or_default(),
        },
    ))
}

/// Whether GitHub answered 401: `is_unauthorized` for octotoken's
/// `with_token`, whose call then gets one replacement token.
pub fn is_unauthorized(error: &octocrab::Error) -> bool {
    matches!(error, octocrab::Error::GitHub { source, .. } if source.status_code == 401)
}

/// The response headers of a 401, for [`App::with_credential`], which reads
/// GitHub's `Date` from them to correct a skewed clock. `None` for any other
/// error, and for a 401 octocrab kept no headers of.
pub fn unauthorized(error: &octocrab::Error) -> Option<&HeaderMap> {
    match error {
        octocrab::Error::GitHub { source, .. } if source.status_code == 401 => {
            source.headers.as_ref()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
