//! Sign in with GitHub, for GitHub Apps.
//!
//! The OAuth web flow that tells you who a user is, and nothing more:
//!
//! 1. [`SignIn::authorize`]: redirect the browser to GitHub; keep the
//!    [`PendingAuthorization`] server side.
//! 2. On the callback, [`PendingAuthorization::verify`] the `state` (in
//!    constant time, consuming it), then [`SignIn::with_user_token`]:
//!    exchange the `code` with the PKCE verifier for a [`UserToken`], read
//!    with it, and revoke it whatever the reads did. Signing in needs
//!    nothing longer-lived.
//! 3. In those reads, [`SignIn::user`] (and [`SignIn::emails`] if you need
//!    verified addresses) to learn who signed in; key on [`User::id`], not
//!    the login.
//!
//! [`SignIn::exchange`] and [`SignIn::revoke`] are the two halves of
//! `with_user_token`, for a caller that has to hold the token across
//! something a closure cannot span.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use octologin::{AuthorizeOptions, SignIn};
//!
//! let sign_in = SignIn::builder("Iv23.example", "client-secret", "https://app.example/callback")
//!     .build()?;
//!
//! // GET /login
//! let (url, pending) = sign_in.authorize(AuthorizeOptions::default())?;
//! // ... store `pending` with the session, redirect to `url.as_str()` ...
//!
//! // GET /callback?code=…&state=…
//! # let (code, state) = ("", "");
//! let verifier = pending.verify(state)?;
//! let signed_in = sign_in
//!     .with_user_token(code, &verifier, async |token| sign_in.user(token).await)
//!     .await
//!     .map_err(|error| error.into_error())?;
//! println!("signed in: {}", signed_in.value.id);
//! # Ok(()) }
//! ```
//!
//! # What else a user token reads
//!
//! A GitHub App's user token acts for the user, limited to what both the
//! user and the App may access. Endpoints this crate does not wrap take it
//! as a bearer token with any client ([`UserToken::expose_secret`]); with
//! octocrab, `octoauth_octocrab::authenticated`. The one an app usually
//! wants at sign-in is what the user may see of it: `GET /user/installations`
//! and, per installation, `GET /user/installations/{id}/repositories`. Read
//! them inside `with_user_token` and keep the IDs, not the token.
//!
//! # After an installation
//!
//! GitHub sends whoever installed (or changed) the App to its setup URL with
//! `installation_id` and `setup_action`. Neither proves anything: anyone can
//! open that URL with any ID. Read what changed back from GitHub, e.g. by
//! signing the user in again ([`SignIn::authorize`]; GitHub skips the
//! consent screen for a user who already authorized the App).
//!
//! With "Request user authorization (OAuth) during installation" on, GitHub
//! sends the installer to the **callback** URL instead, with a `code` but
//! **no `state`**: this browser never started that authorization, so there
//! is no [`PendingAuthorization`] to verify and no PKCE verifier to exchange
//! with. Don't exchange that code; start a sign-in instead.
//!
//! # Requests and errors
//!
//! The flow uses PKCE (S256) and requests no scopes. Requests go through
//! reqwest, as in `octoauth`: no redirects (natively), a 30 s deadline,
//! bounded responses, request and response bodies zeroized. Errors
//! ([`SignInError`]) carry fixed codes only; the client secret, codes,
//! verifiers and tokens never appear in `Debug` output.
//!
//! # Cloudflare Workers (wasm32)
//!
//! The crate builds for `wasm32-unknown-unknown`, with the same caveat as
//! `octoauth`: `fetch` follows redirects and reqwest cannot turn that off. A
//! redirected response is discarded as [`SignInError::Redirected`], but the
//! request was already re-sent, for the code exchange with the client secret
//! in its body (per the Fetch standard only a 307/308 re-sends a body). GitHub
//! does not redirect these endpoints; a redirect means something between you
//! and GitHub is not GitHub.
//!
//! As in `octoauth`, a response body is read whole there before its size bound
//! applies.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod authorize;
mod error;
mod sign_in;
mod transport;
mod user;
mod with_user_token;

pub use authorize::{AuthorizeOptions, AuthorizeUrl, PendingAuthorization};
pub use error::{OAuthErrorCode, SignInError};
pub use sign_in::{DEFAULT_USER_AGENT, DEFAULT_WEB_BASE_URL, SignIn, SignInBuilder};
pub use user::{User, UserId, UserToken, VerifiedEmail};
pub use with_user_token::{Revoked, WithUserTokenError};

/// Error types shared with `octoauth`.
pub use octoauth::{ApiError, ConfigError, ForbiddenReason, RateLimitScope, TransientKind};
