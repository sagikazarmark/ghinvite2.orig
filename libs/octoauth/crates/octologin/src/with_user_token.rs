//! [`SignIn::with_user_token`]: exchange a code, use the user token, and
//! always revoke it.

use crate::{SignIn, SignInError, UserToken};

impl SignIn {
    /// Exchange the callback's `code` (as [`exchange`](Self::exchange)), run
    /// `read` with the user token, then [`revoke`](Self::revoke) the token,
    /// whether `read` succeeded or not.
    ///
    /// Signing in needs the token only for a few reads: who signed in
    /// ([`user`](Self::user)), perhaps their [`emails`](Self::emails), and
    /// anything else `read` asks another client for with
    /// [`expose_secret`](UserToken::expose_secret), such as
    /// `GET /user/installations`. Holding the token in this call's scope
    /// makes keeping it longer the hard path, and so does an early `?` in
    /// `read`: it cannot skip the revocation.
    ///
    /// The revocation is best effort. When `read` succeeds, its outcome is
    /// in [`Revoked::revocation`]: a failed revocation leaves the token to
    /// expire (8 hours for a GitHub App), so the sign-in can still go ahead.
    /// When `read` fails, its error is returned and a failed revocation is
    /// dropped.
    ///
    /// ```no_run
    /// # async fn example(sign_in: octologin::SignIn, pending: octologin::PendingAuthorization) -> Result<(), octologin::SignInError> {
    /// # let (code, state) = ("", "");
    /// let verifier = pending.verify(state)?;
    /// let signed_in = sign_in
    ///     .with_user_token(code, &verifier, async |token| sign_in.user(token).await)
    ///     .await
    ///     .map_err(|error| error.into_error())?;
    /// if let Err(error) = signed_in.revocation {
    ///     eprintln!("user token not revoked: {error}");
    /// }
    /// println!("signed in: {}", signed_in.value.id);
    /// # Ok(()) }
    /// ```
    pub async fn with_user_token<T, E>(
        &self,
        code: &str,
        pkce_verifier: &str,
        read: impl AsyncFnOnce(&UserToken) -> Result<T, E>,
    ) -> Result<Revoked<T>, WithUserTokenError<E>> {
        let token = self
            .exchange(code, pkce_verifier)
            .await
            .map_err(WithUserTokenError::SignIn)?;
        let outcome = read(&token).await;
        let revocation = self.revoke(token).await;
        match outcome {
            Ok(value) => Ok(Revoked { value, revocation }),
            Err(error) => Err(WithUserTokenError::Call(error)),
        }
    }
}

/// What [`SignIn::with_user_token`]'s `read` returned, after the token was
/// revoked.
#[derive(Debug)]
#[must_use = "check `revocation` to learn whether the token outlives the sign-in"]
pub struct Revoked<T> {
    /// `read`'s value.
    pub value: T,

    /// Whether revoking the token succeeded. On an error the token stays
    /// valid until it expires.
    pub revocation: Result<(), SignInError>,
}

/// Why [`SignIn::with_user_token`] failed. Complete, like octotoken's
/// `WithTokenError`; [`into_error`](Self::into_error) flattens it into the
/// caller's own error type.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WithUserTokenError<E> {
    /// The code exchange failed; `read` did not run and there is no token to
    /// revoke.
    #[error(transparent)]
    SignIn(SignInError),

    /// `read` failed; the token was revoked (or a failed attempt dropped).
    /// Returned as `read` produced it; if it carries upstream text, convert
    /// it before logging.
    #[error(transparent)]
    Call(E),
}

impl<E> WithUserTokenError<E> {
    /// Convert `read`'s error with `f` (`error.map(MyError::from).into_error()`).
    pub fn map<F>(self, f: impl FnOnce(E) -> F) -> WithUserTokenError<F> {
        match self {
            Self::SignIn(error) => WithUserTokenError::SignIn(error),
            Self::Call(error) => WithUserTokenError::Call(f(error)),
        }
    }
}

impl<E: From<SignInError>> WithUserTokenError<E> {
    /// The caller's own error: [`Call`](Self::Call) as it is,
    /// [`SignIn`](Self::SignIn) converted with `From`.
    pub fn into_error(self) -> E {
        match self {
            Self::SignIn(error) => error.into(),
            Self::Call(error) => error,
        }
    }
}
