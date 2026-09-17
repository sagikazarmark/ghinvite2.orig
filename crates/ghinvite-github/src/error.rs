use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// All failure modes a GitHub-side call can produce. Callers in Plan 3 / Plan 4
/// will branch on these variants; in particular `Error::Status(404)` on a
/// `get_repo` is *not* fatal — it just means the App lost access.
#[derive(Debug, Error)]
pub enum Error {
    /// The HTTP transport itself failed (DNS, TLS, broken pipe, etc).
    #[error("transport error: {0}")]
    Transport(String),

    /// The server returned a non-2xx status. Holds the numeric status and the
    /// raw body (truncated to 4 KiB) for diagnostics.
    #[error("github returned status {status}: {body}")]
    Status { status: u16, body: String },

    /// The response body was not valid JSON or did not match the expected shape.
    #[error("response decode error: {0}")]
    Decode(String),

    /// The OAuth authorization-code exchange returned an error from GitHub
    /// (`error=...`/`error_description=...`).
    #[error("oauth error: {0}")]
    OAuth(String),

    /// Something is wrong with the calling code's inputs (e.g. bad URL,
    /// unparseable PEM, expired JWT key), or a well-formed response this crate
    /// refuses to act on: an incomplete repository refresh, a pending-invitation
    /// listing whose pages could not be walked, an identity mismatch. Never
    /// thrown by the transport itself. Callers treat it as unknown, not as a
    /// terminal answer.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// JWT minting / signing failed.
    #[error("jwt error: {0}")]
    Jwt(String),
}

impl Error {
    /// Convenience: extract the status code if this is a `Status` variant.
    /// Plan 3 handlers branch on specific codes (404 = lost access, 422 =
    /// already a member, etc.), so giving them this rather than `match`ing
    /// keeps call sites tight.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_extracts_only_for_status_variant() {
        assert_eq!(
            Error::Status {
                status: 404,
                body: "x".into(),
            }
            .status(),
            Some(404)
        );
        assert_eq!(Error::Decode("nope".into()).status(), None);
    }

    #[test]
    fn errors_render_useful_messages() {
        let s = Error::Status {
            status: 401,
            body: "bad creds".into(),
        }
        .to_string();
        assert!(s.contains("401"));
        assert!(s.contains("bad creds"));
    }
}
