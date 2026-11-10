//! Sign in with GitHub, for GitHub Apps.
//!
//! The OAuth web flow that tells you who a user is, and nothing more:
//!
//! 1. [`SignIn::authorize`]: redirect the browser to GitHub; keep the
//!    [`PendingAuthorization`] server side.
//! 2. On the callback, [`PendingAuthorization::verify`] the `state` (in
//!    constant time, consuming it), then [`SignIn::exchange`] the `code`
//!    with the PKCE verifier for a [`UserToken`].
//! 3. [`SignIn::user`] (and [`SignIn::emails`] if you need verified
//!    addresses) to learn who signed in; key on [`User::id`], not the login.
//! 4. [`SignIn::revoke`] the token: signing in needs nothing longer-lived.
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
//! let token = sign_in.exchange(code, &verifier).await?;
//! let user = sign_in.user(&token).await;
//! sign_in.revoke(token).await?;
//! println!("signed in: {}", user?.id);
//! # Ok(()) }
//! ```
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

pub use authorize::{AuthorizeOptions, AuthorizeUrl, PendingAuthorization};
pub use error::{OAuthErrorCode, SignInError};
pub use sign_in::{DEFAULT_USER_AGENT, DEFAULT_WEB_BASE_URL, SignIn, SignInBuilder};
pub use user::{User, UserId, UserToken, VerifiedEmail};

/// Error types shared with `octoauth`.
pub use octoauth::{ApiError, ConfigError, ForbiddenReason, RateLimitScope, TransientKind};
