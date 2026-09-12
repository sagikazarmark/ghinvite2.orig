//! Web-binary error type with axum `IntoResponse` mapping.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, WebError>;

#[derive(Debug, Error)]
pub enum WebError {
    #[error("storage error: {0}")]
    Storage(#[from] ghinvite_core::storage::Error),

    #[error("github api error: {0}")]
    Github(#[from] ghinvite_github::Error),

    #[error("session error: {0}")]
    Session(String),

    #[error("oauth error: {0}")]
    OAuth(String),

    #[error("restate ingress error: {0}")]
    Restate(String),

    /// Resource not found / not authorized — surfaced as a generic 404 so we
    /// don't leak whether the resource exists. Used for invitation-link routes,
    /// account routes the user isn't an admin of, etc.
    #[error("not found")]
    NotFound,

    /// Caller is not authenticated. Surfaces as a 302 redirect to /login.
    #[error("unauthenticated")]
    Unauthenticated,

    /// Caller is authenticated but lacks permission for the resource.
    /// Per spec §10.3: surfaces as 404 to avoid leaking existence.
    #[error("forbidden")]
    Forbidden,

    /// Some input from the client was malformed (e.g. missing query param).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Catch-all for unexpected internal errors.
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            WebError::NotFound | WebError::Forbidden => {
                (StatusCode::NOT_FOUND, "Not Found".to_string())
            }
            WebError::Unauthenticated => {
                // Redirect to /login. axum's redirect helper makes this clean.
                return axum::response::Redirect::to("/login").into_response();
            }
            WebError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            WebError::OAuth(msg) => (StatusCode::BAD_REQUEST, format!("OAuth error: {msg}")),
            WebError::Session(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Session temporarily unavailable. Please try again later.".into(),
            ),
            WebError::Restate(msg) => (StatusCode::BAD_GATEWAY, format!("Restate error: {msg}")),
            WebError::Storage(_) | WebError::Github(_) | WebError::Internal(_) => {
                tracing::error!(error = ?self, "internal error rendering response");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error".to_string(),
                )
            }
        };
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn body_text(resp: Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn not_found_renders_404() {
        let resp = WebError::NotFound.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn forbidden_renders_404() {
        // Spec §10.3: forbidden → 404 to avoid information leak.
        let resp = WebError::Forbidden.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unauthenticated_redirects_to_login() {
        let resp = WebError::Unauthenticated.into_response();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp.headers().get("location").unwrap();
        assert_eq!(location.to_str().unwrap(), "/login");
    }

    #[tokio::test]
    async fn bad_request_passes_message() {
        let resp = WebError::BadRequest("missing 'code' param".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_text(resp).await;
        assert!(body.contains("missing 'code' param"));
    }

    #[tokio::test]
    async fn restate_error_renders_502() {
        let resp = WebError::Restate("timeout".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
