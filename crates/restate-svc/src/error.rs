//! Error type bridging Storage, GitHub, and Restate's terminal/transient model.

use restate_sdk::prelude::TerminalError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, HandlerError>;

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error(transparent)]
    Storage(#[from] storage::Error),

    #[error(transparent)]
    Github(#[from] github::Error),

    /// Domain invariant the caller should not have allowed.
    #[error("invariant violated: {0}")]
    Invariant(String),
}

impl HandlerError {
    /// Returns `true` if the underlying cause is transient and the caller
    /// should let Restate retry (i.e. NOT wrap this in `TerminalError`).
    /// Returns `false` for terminal errors that should bypass Restate retry.
    pub fn is_terminal(&self) -> bool {
        match self {
            HandlerError::Storage(e) => match e {
                storage::Error::Database(_) => false, // transient
                storage::Error::Conflict(_)
                | storage::Error::NotFound
                | storage::Error::Corrupt(_) => true, // terminal
            },
            HandlerError::Github(e) => match e {
                github::Error::Status { status, .. } => match *status {
                    429 | 500..=599 => false, // transient
                    _ => true,                // terminal (4xx)
                },
                github::Error::Transport(_)
                | github::Error::Decode(_)
                | github::Error::OAuth(_)
                | github::Error::InvalidInput(_)
                | github::Error::Jwt(_) => false, // transient
            },
            HandlerError::Invariant(_) => true, // bug — terminal
        }
    }

    /// Convert into Restate's terminal-error wrapper if this error should NOT
    /// retry; otherwise return the underlying message wrapped in
    /// `Err(restate_sdk::error::Error::other(...))` so Restate retries.
    ///
    /// In the current SDK shape, handler return types are
    /// `Result<T, TerminalError>`; transient errors are surfaced by leaving
    /// `ctx.run` to fail and propagate. Most handlers will use `to_terminal()`
    /// after a `match self.is_terminal()`.
    pub fn to_terminal(&self) -> TerminalError {
        TerminalError::new(self.to_string())
    }
}

/// Map our `HandlerError` to the SDK's `HandlerError`, honoring
/// `is_terminal()` classification:
///
/// - **Terminal** errors (4xx GitHub, conflicts, NotFound, invariant
///   violations, corruption) become `restate_sdk::errors::TerminalError`,
///   which Restate will NOT retry.
/// - **Transient** errors (5xx, rate-limit, transport, DB connectivity)
///   become a non-terminal `restate_sdk::errors::HandlerError`, which
///   Restate WILL retry with exponential backoff.
///
/// Implemented as a free function rather than `impl From<HandlerError>`
/// because the SDK provides a blanket
/// `impl<E: Into<Box<dyn StdError + Send + Sync>>> From<E> for HandlerError`
/// that conflicts with any user-provided `From` impl. Use as
/// `.map_err(crate::error::to_sdk_handler_error)`.
pub fn to_sdk_handler_error(e: HandlerError) -> ::restate_sdk::errors::HandlerError {
    if e.is_terminal() {
        // Terminal: Restate will NOT retry.
        e.to_terminal().into()
    } else {
        // Transient: the SDK's blanket `From<E: Into<Box<dyn StdError ...>>>`
        // produces a `HandlerErrorInner::Retryable`, which Restate retries.
        let boxed: Box<dyn std::error::Error + Send + Sync> = e.to_string().into();
        ::restate_sdk::errors::HandlerError::from(boxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::ConflictKind;

    #[test]
    fn storage_database_is_transient() {
        let e = HandlerError::Storage(storage::Error::Database(
            sqlx::Error::Configuration("test".into()),
        ));
        assert!(!e.is_terminal());
    }

    #[test]
    fn storage_conflict_is_terminal() {
        let e = HandlerError::Storage(storage::Error::Conflict(ConflictKind::DuplicateSlug));
        assert!(e.is_terminal());
    }

    #[test]
    fn storage_not_found_is_terminal() {
        let e = HandlerError::Storage(storage::Error::NotFound);
        assert!(e.is_terminal());
    }

    #[test]
    fn github_5xx_is_transient() {
        let e = HandlerError::Github(github::Error::Status {
            status: 502,
            body: "bad gateway".into(),
        });
        assert!(!e.is_terminal());
    }

    #[test]
    fn github_429_is_transient() {
        let e = HandlerError::Github(github::Error::Status {
            status: 429,
            body: "rate limited".into(),
        });
        assert!(!e.is_terminal());
    }

    #[test]
    fn github_404_is_terminal() {
        let e = HandlerError::Github(github::Error::Status {
            status: 404,
            body: "not found".into(),
        });
        assert!(e.is_terminal());
    }

    #[test]
    fn github_transport_is_transient() {
        let e = HandlerError::Github(github::Error::Transport("dns".into()));
        assert!(!e.is_terminal());
    }

    #[test]
    fn invariant_is_terminal() {
        let e = HandlerError::Invariant("test".into());
        assert!(e.is_terminal());
    }
}
